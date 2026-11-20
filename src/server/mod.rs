//! Loopback-only HTTP API and embedded SPA. The caller owns listener binding and browser launch.
mod analysis;
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
    review::{finish, publish_confirm, publish_preview, refresh},
    threads::{create_thread, delete_comment, reply, update_comment, update_thread},
};
use crate::{
    model::Actor,
    session::Session,
    store::{NotFound, ReservedDraft, RevisionConflict},
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
    } else if let Some(conflict) = error.downcast_ref::<RevisionConflict>() {
        // Recoverable: carries the authoritative state to retry against
        // (analyze.md §12.4). TODO(#26): the triage PATCH that raises it.
        let mut api = ApiError::new(
            StatusCode::CONFLICT,
            "revision_conflict",
            conflict.to_string(),
        );
        api.details = Some(json!({
            "retryable": true,
            "current": conflict.current,
            "currentRevision": conflict.current.as_ref().map_or(0, |s| s.revision),
        }));
        api
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
        .route("/analysis/summary", get(analysis::summary))
        .route("/analysis/runs", get(analysis::runs))
        .route("/analysis/findings", get(analysis::findings))
        .route("/analysis/findings/{id}/evidence", get(analysis::evidence))
        .route("/github-feedback/threads", get(analysis::feedback))
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

/// The session's single event channel. Every event carries its sequence as
/// the SSE `id`, so a reconnecting browser sends `Last-Event-ID`. Events are
/// notifications, not a durable log: if the client missed any (reconnect
/// behind the current sequence, or the broadcast buffer lagged) the first
/// thing it receives is `resync`, and it must reload the authoritative
/// `GET /session` rather than apply deltas on top of stale state.
async fn events(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    // Subscribe before reading the sequence so nothing falls in between.
    let rx = state.session.events.subscribe();
    let current = state
        .session
        .sequence
        .load(std::sync::atomic::Ordering::SeqCst);
    let missed = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().parse::<u64>().map_or(true, |seen| seen < current))
        .unwrap_or(false);
    let first = missed.then(|| Ok(resync_event(current)));
    let live = futures_util::stream::unfold(rx, move |mut rx| async move {
        match rx.recv().await {
            Ok(envelope) => {
                let sequence = envelope.sequence;
                let event = Event::default()
                    .event(&envelope.event_type)
                    .id(sequence.to_string())
                    .json_data(envelope)
                    .unwrap_or_else(|_| resync_event(sequence));
                Some((Ok(event), rx))
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                Some((Ok(resync_event(0)), rx))
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => None,
        }
    });
    let stream = futures_util::StreamExt::chain(futures_util::stream::iter(first), live);
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

fn resync_event(sequence: u64) -> Event {
    Event::default().event("resync").data(
        json!({
            "eventVersion": crate::model::LIFECYCLE_EVENT_VERSION,
            "sequence": sequence,
        })
        .to_string(),
    )
}

async fn api_not_found() -> Response {
    ApiError::missing().into_response()
}

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

    /// First SSE frame for a connection, or `None` if nothing arrives soon.
    async fn first_frame(session: Arc<Session>, last_event_id: Option<&str>) -> Option<String> {
        use futures_util::StreamExt;
        use tower::ServiceExt;
        let app = router(AppState::new(session, "ui".into(), "agent".into()));
        let mut builder = axum::http::Request::builder()
            .uri("/api/v1/events")
            .header(header::HOST, "127.0.0.1:1234")
            .header(header::AUTHORIZATION, "Bearer agent");
        if let Some(id) = last_event_id {
            builder = builder.header("last-event-id", id);
        }
        let response = app
            .oneshot(builder.body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body().into_data_stream();
        let chunk = tokio::time::timeout(Duration::from_millis(200), body.next())
            .await
            .ok()??
            .ok()?;
        Some(String::from_utf8_lossy(&chunk).into_owned())
    }

    #[tokio::test]
    async fn reconnect_behind_the_current_sequence_starts_with_resync() {
        let session = Session::new(
            crate::demo::snapshot(0),
            crate::store::Store::open_in_memory().unwrap(),
            None,
            None,
        )
        .unwrap();
        session.notify("file.reviewed", json!({}));
        session.notify("file.reviewed", json!({}));
        let frame = first_frame(Arc::clone(&session), Some("1")).await.unwrap();
        assert!(frame.contains("event: resync"), "{frame}");
        assert!(frame.contains("\"sequence\":2"), "{frame}");
        // Up to date, or a first connection: no spurious resync.
        assert_eq!(first_frame(Arc::clone(&session), Some("2")).await, None);
        assert_eq!(first_frame(Arc::clone(&session), None).await, None);
        // Unparseable cursor: resync rather than guess.
        let frame = first_frame(session, Some("garbage")).await.unwrap();
        assert!(frame.contains("event: resync"));
    }

    #[tokio::test]
    async fn live_events_carry_their_sequence_as_the_sse_id() {
        use futures_util::StreamExt;
        use tower::ServiceExt;
        let session = Session::new(
            crate::demo::snapshot(0),
            crate::store::Store::open_in_memory().unwrap(),
            None,
            None,
        )
        .unwrap();
        let app = router(AppState::new(
            Arc::clone(&session),
            "ui".into(),
            "agent".into(),
        ));
        let request = axum::http::Request::builder()
            .uri("/api/v1/events")
            .header(header::HOST, "127.0.0.1:1234")
            .header(header::AUTHORIZATION, "Bearer agent")
            .body(axum::body::Body::empty())
            .unwrap();
        let mut body = app
            .oneshot(request)
            .await
            .unwrap()
            .into_body()
            .into_data_stream();
        session.notify("analysis.run.queued", json!({"eventVersion": 1}));
        let chunk = tokio::time::timeout(Duration::from_secs(2), body.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let frame = String::from_utf8_lossy(&chunk);
        assert!(frame.contains("event: analysis.run.queued"), "{frame}");
        assert!(frame.contains("id: 1"), "{frame}");
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
