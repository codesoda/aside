//! Loopback-only HTTP API and embedded SPA. The caller owns listener binding and browser launch.
mod assets;
mod auth;
mod exclusions;
mod files;
mod review;
mod threads;

use std::{convert::Infallible, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    middleware,
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use serde_json::{Value, json};

use self::{
    assets::static_asset,
    auth::{guard, login},
    exclusions::{
        create_exclusion, delete_exclusion, exclusions, preview_exclusion, update_exclusion,
    },
    files::{clear_reviewed, file_history, mark_reviewed},
    review::{finish, publish_routes, refresh},
    threads::{create_thread, delete_comment, reply, update_comment, update_thread},
};
use crate::{
    model::Actor,
    session::Session,
    store::{NotADraft, NotFound, PublishingElsewhere, ReservedDraft},
};

/// Tokens are session-specific secrets. The UI token is never included in API responses.
#[derive(Clone)]
pub struct AppState {
    pub session: Arc<Session>,
    ui_token: Arc<str>,
    agent_token: Arc<str>,
}

impl AppState {
    pub fn new(session: Arc<Session>, ui_token: String, agent_token: String) -> Self {
        Self {
            session,
            ui_token: ui_token.into(),
            agent_token: agent_token.into(),
        }
    }
}

#[allow(dead_code)] // Main can use this when opening the browser after binding.
pub fn login_url(base: &str, ui_token: &str) -> String {
    format!("{}/auth?token={ui_token}", base.trim_end_matches('/'))
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    details: Option<Value>,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            details: None,
        }
    }
    fn bad(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", message)
    }
    fn forbidden() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "this action requires the human reviewer",
        )
    }
    fn missing() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "resource not found")
    }
    fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "the operation failed",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({"error": self.code, "message": self.message, "details": self.details})),
        )
            .into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

fn db_error(error: anyhow::Error) -> ApiError {
    if error.downcast_ref::<NotFound>().is_some() {
        ApiError::missing()
    } else if error.downcast_ref::<ReservedDraft>().is_some() {
        ApiError::new(
            StatusCode::CONFLICT,
            "draft_reserved",
            "draft is reserved for publication",
        )
    } else if error.downcast_ref::<PublishingElsewhere>().is_some() {
        ApiError::new(
            StatusCode::CONFLICT,
            "publishing_elsewhere",
            "another Aside process is publishing this PR; try again once it is done",
        )
    } else if error.downcast_ref::<NotADraft>().is_some() {
        ApiError::new(
            StatusCode::CONFLICT,
            "not_a_local_draft",
            "only local drafts can be selected for publication",
        )
    } else {
        ApiError::internal()
    }
}

fn human(actor: Actor) -> ApiResult<()> {
    if actor == Actor::Human {
        Ok(())
    } else {
        Err(ApiError::forbidden())
    }
}

fn body_text(body: &str) -> ApiResult<&str> {
    let body = body.trim();
    if body.is_empty() || body.len() > 100_000 {
        Err(ApiError::bad("body must contain 1–100000 characters"))
    } else {
        Ok(body)
    }
}

/// Route construction only: main binds 127.0.0.1:0 and calls axum::serve.
pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/health", get(health))
        .route("/session", get(session_view))
        .route("/events", get(events))
        .route(
            "/repositories/current/exclusions",
            get(exclusions).post(create_exclusion),
        )
        .route(
            "/repositories/current/exclusions/preview",
            post(preview_exclusion),
        )
        .route(
            "/repositories/current/exclusions/{id}",
            axum::routing::patch(update_exclusion).delete(delete_exclusion),
        )
        .route(
            "/files/{id}/reviewed",
            post(mark_reviewed).delete(clear_reviewed),
        )
        .route("/files/{id}/history", get(file_history))
        .route("/threads", post(create_thread))
        .route("/threads/{id}/replies", post(reply))
        .route("/threads/{id}", axum::routing::patch(update_thread))
        .route(
            "/comments/{id}",
            axum::routing::patch(update_comment).delete(delete_comment),
        )
        .route("/refresh", post(refresh))
        .route("/review/finish", post(finish))
        .merge(publish_routes())
        .fallback(api_not_found)
        .layer(DefaultBodyLimit::max(1_048_576));
    Router::new()
        .nest("/api/v1", api)
        .route("/auth", get(login))
        .route("/api", axum::routing::any(api_not_found))
        .route("/api/{*rest}", axum::routing::any(api_not_found))
        .fallback(static_asset)
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"ok":true,"sessionId":state.session.id}))
}

async fn session_view(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
) -> ApiResult<Json<crate::model::SessionView>> {
    Ok(Json(state.session.view(actor).map_err(db_error)?))
}

async fn events(
    State(state): State<AppState>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let rx = state.session.events.subscribe();
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        match rx.recv().await {
            Ok(envelope) => {
                let event = Event::default()
                    .event(&envelope.event_type)
                    .json_data(envelope)
                    .unwrap_or_else(|_| Event::default().event("resync").data("{}"));
                Some((Ok(event), rx))
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                Some((Ok(Event::default().event("resync").data("{}")), rx))
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => None,
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

async fn api_not_found() -> Response {
    ApiError::missing().into_response()
}

#[cfg(test)]
mod selection_tests;

#[cfg(test)]
mod tests;
