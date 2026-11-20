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
    review::{finish, publish_confirm, publish_preview, publish_selection, refresh},
    threads::{create_thread, delete_comment, reply, update_comment, update_thread},
};
use crate::{
    model::Actor,
    session::Session,
    store::{NotADraft, NotFound, ReservedDraft},
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
        .route("/review/publish/preview", post(publish_preview))
        .route(
            "/review/publish/selection",
            axum::routing::put(publish_selection),
        )
        .route("/review/publish", post(publish_confirm))
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
mod tests {
    use super::auth::host_ok;
    use super::*;
    use axum::http::{HeaderMap, Method, header};
    fn app() -> Router {
        let session = Session::new(
            crate::demo::snapshot(0),
            crate::store::Store::open_in_memory().unwrap(),
            None,
            None,
        )
        .unwrap();
        router(AppState::new(
            session,
            "ui-secret".into(),
            "agent-secret".into(),
        ))
    }

    async fn request(
        method: Method,
        path: &str,
        credential: Option<&str>,
        origin: Option<&str>,
        body: Option<Value>,
    ) -> Response {
        use tower::ServiceExt;
        let mut builder = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "127.0.0.1:1234");
        if let Some(value) = credential {
            builder = builder.header(
                if value.starts_with("Bearer ") {
                    header::AUTHORIZATION
                } else {
                    header::COOKIE
                },
                value,
            );
        }
        if let Some(value) = origin {
            builder = builder.header(header::ORIGIN, value);
        }
        let bytes = if let Some(body) = body {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            body.to_string()
        } else {
            String::new()
        };
        app()
            .oneshot(builder.body(axum::body::Body::from(bytes)).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn auth_and_permissions() {
        let path = "/api/v1/session";
        assert_eq!(
            request(Method::GET, path, None, None, None).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(Method::GET, path, Some("Bearer agent-secret"), None, None)
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            request(Method::GET, path, Some("aside_ui=ui-secret"), None, None)
                .await
                .status(),
            StatusCode::OK
        );
        let rule = json!({"action":"exclude","kind":"glob","expression":"*.rs"});
        let route = "/api/v1/repositories/current/exclusions";
        assert_eq!(
            request(
                Method::POST,
                route,
                Some("Bearer agent-secret"),
                None,
                Some(rule.clone())
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(
                Method::POST,
                route,
                Some("aside_ui=ui-secret"),
                None,
                Some(rule.clone())
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(
                Method::POST,
                route,
                Some("aside_ui=ui-secret"),
                Some("http://127.0.0.1:1234"),
                Some(rule)
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            request(
                Method::POST,
                "/api/v1/review/publish",
                Some("Bearer agent-secret"),
                None,
                Some(json!({"planId":"x","confirm":true}))
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(
                Method::POST,
                "/api/v1/review/publish/preview",
                Some("aside_ui=ui-secret"),
                Some("http://127.0.0.1:1234"),
                None
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn stale_digest_and_host_rejection() {
        use tower::ServiceExt;
        let id = crate::demo::snapshot(0).files[0].id.clone();
        let path = format!("/api/v1/files/{id}/reviewed");
        let response = request(
            Method::POST,
            &path,
            Some("aside_ui=ui-secret"),
            Some("http://127.0.0.1:1234"),
            Some(json!({"diffDigest":"stale"})),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = axum::body::to_bytes(response.into_body(), 100_000)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["error"],
            "stale_fingerprint"
        );
        let request = axum::http::Request::builder()
            .uri("/api/v1/health")
            .header(header::HOST, "evil.example:1234")
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(
            app().oneshot(request).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn login_url_keeps_port() {
        assert_eq!(
            login_url("http://127.0.0.1:1234/", "secret"),
            "http://127.0.0.1:1234/auth?token=secret"
        );
    }
    #[test]
    fn loopback_hosts_only() {
        for host in ["localhost:1234", "127.0.0.1:54321"] {
            let mut h = HeaderMap::new();
            h.insert(header::HOST, host.parse().unwrap());
            assert!(host_ok(&h));
        }
        for host in [
            "example.com:1234",
            "127.0.0.1:0",
            "localhost",
            "localhost:1234.evil.com",
            "[::1]:1234",
        ] {
            let mut h = HeaderMap::new();
            h.insert(header::HOST, host.parse().unwrap());
            assert!(!host_ok(&h));
        }
    }
}
