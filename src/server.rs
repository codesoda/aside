//! Loopback-only HTTP API and embedded SPA. The caller owns listener binding and browser launch.
use std::{convert::Infallible, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{HeaderMap, Method, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Redirect, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use rust_embed::RustEmbed;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    diff,
    model::{
        Acknowledgement, Actor, AnchorState, Comment, LineKind, PublishState, RuleAction, RuleKind,
        Side, ThreadSource, Verdict,
    },
    publish, rules,
    session::Session,
    store::{NewThread, NotFound, ReservedDraft, RuleUpdate},
    util::{new_id, now_rfc3339, secure_eq},
};

#[derive(RustEmbed)]
#[folder = "web/dist/"]
struct Assets;

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

fn host_ok(headers: &HeaderMap) -> bool {
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|host| {
            let Some((name, port)) = host.rsplit_once(':') else {
                return false;
            };
            (name == "127.0.0.1" || name == "localhost")
                && port.parse::<u16>().is_ok_and(|n| n != 0)
        })
}

fn same_origin(headers: &HeaderMap) -> bool {
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    origin == format!("{}://{host}", "http")
        || std::env::var("ASIDE_DEV_ORIGIN").is_ok_and(|dev| origin == dev)
}

async fn guard(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let path = request.uri().path();
    if !host_ok(request.headers()) {
        return ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_host",
            "invalid loopback host",
        )
        .into_response();
    }
    if path == "/auth" || path == "/api/v1/health" || !path.starts_with("/api/") {
        return next.run(request).await;
    }
    let bearer = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let cookie = request
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.split(';')
                .map(str::trim)
                .find_map(|part| part.strip_prefix("aside_ui="))
        });
    let actor = if bearer.is_some_and(|v| secure_eq(v, &state.agent_token)) {
        Some(Actor::Agent)
    } else if bearer.is_none() && cookie.is_some_and(|v| secure_eq(v, &state.ui_token)) {
        Some(Actor::Human)
    } else {
        None
    };
    let Some(actor) = actor else {
        return ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "authentication required",
        )
        .into_response();
    };
    if actor == Actor::Human
        && !matches!(
            *request.method(),
            Method::GET | Method::HEAD | Method::OPTIONS
        )
        && !same_origin(request.headers())
    {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            "invalid_origin",
            "a matching Origin is required",
        )
        .into_response();
    }
    request.extensions_mut().insert(actor);
    let response = next.run(request).await;
    if response.status().is_client_error()
        && response
            .headers()
            .get(header::CONTENT_TYPE)
            .is_none_or(|v| !v.as_bytes().starts_with(b"application/json"))
    {
        return ApiError::new(response.status(), "invalid_request", "invalid API request")
            .into_response();
    }
    response
}

#[derive(Deserialize)]
struct LoginQuery {
    token: String,
}

async fn login(
    State(state): State<AppState>,
    Query(query): Query<LoginQuery>,
) -> ApiResult<Response> {
    if !secure_eq(&query.token, &state.ui_token) {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "invalid login credential",
        ));
    }
    let mut response = Redirect::to("/").into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        format!(
            "aside_ui={}; HttpOnly; SameSite=Strict; Path=/",
            state.ui_token
        )
        .parse()
        .map_err(|_| ApiError::internal())?,
    );
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        "no-referrer".parse().map_err(|_| ApiError::internal())?,
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-store".parse().map_err(|_| ApiError::internal())?,
    );
    Ok(response)
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

async fn exclusions(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let view = state.session.view(Actor::Human).map_err(db_error)?;
    Ok(Json(
        json!({"rules":view.rules,"fileStates":view.file_states,"sequence":view.sequence}),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuleInput {
    action: RuleAction,
    kind: RuleKind,
    expression: String,
    position: Option<usize>,
}

async fn preview_exclusion(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Json(input): Json<RuleInput>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    let expression = rules::normalize(input.kind, &input.expression)
        .map_err(|e| ApiError::bad(e.to_string()))?;
    let mut view = state.session.view(actor).map_err(db_error)?;
    let rule = crate::model::ExclusionRule {
        id: "preview".into(),
        action: input.action,
        kind: input.kind,
        expression,
        enabled: true,
        order: 0,
        created_at: String::new(),
        updated_at: String::new(),
    };
    let at = input
        .position
        .unwrap_or(view.rules.len())
        .min(view.rules.len());
    view.rules.insert(at, rule);
    for (i, rule) in view.rules.iter_mut().enumerate() {
        rule.order = i as i64;
    }
    let matches: Vec<_> = view.files.iter().map(|file| json!({"fileId":file.id,"path":file.path,"scope":rules::evaluate(&view.rules, &file.path)})).collect();
    Ok(Json(json!({"matches":matches,"sequence":view.sequence})))
}

async fn create_exclusion(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Json(input): Json<RuleInput>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    let expression = rules::normalize(input.kind, &input.expression)
        .map_err(|e| ApiError::bad(e.to_string()))?;
    let rule = state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?
        .create_rule(
            &state.session.repository.id,
            input.action,
            input.kind,
            &expression,
            input.position,
            actor,
        )
        .map_err(db_error)?;
    let event = state
        .session
        .notify("exclusions.changed", json!({"ruleId":rule.id}));
    Ok(Json(
        json!({"rule":rule,"sequence":event.sequence,"fileStates":state.session.view(actor).map_err(db_error)?.file_states}),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RulePatch {
    action: Option<RuleAction>,
    kind: Option<RuleKind>,
    expression: Option<String>,
    enabled: Option<bool>,
    position: Option<usize>,
}

async fn update_exclusion(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(id): Path<String>,
    Json(input): Json<RulePatch>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    let mut store = state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?;
    let old = store
        .get_rule(&state.session.repository.id, &id)
        .map_err(db_error)?;
    let kind = input.kind.unwrap_or(old.kind);
    let expression = input.expression.as_deref().unwrap_or(&old.expression);
    let update = RuleUpdate {
        action: input.action,
        kind_expression: Some((
            kind,
            rules::normalize(kind, expression).map_err(|e| ApiError::bad(e.to_string()))?,
        )),
        enabled: input.enabled,
        position: input.position,
    };
    let rule = store
        .update_rule(&state.session.repository.id, &id, &update)
        .map_err(db_error)?;
    drop(store);
    let event = state
        .session
        .notify("exclusions.changed", json!({"ruleId":id}));
    Ok(Json(
        json!({"rule":rule,"sequence":event.sequence,"fileStates":state.session.view(actor).map_err(db_error)?.file_states}),
    ))
}

async fn delete_exclusion(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?
        .delete_rule(&state.session.repository.id, &id)
        .map_err(db_error)?;
    let event = state
        .session
        .notify("exclusions.changed", json!({"ruleId":id}));
    Ok(Json(
        json!({"sequence":event.sequence,"fileStates":state.session.view(actor).map_err(db_error)?.file_states}),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fingerprint {
    diff_digest: String,
}

fn checked_file<'a>(
    snapshot: &'a crate::model::PrSnapshot,
    id: &str,
    digest: &str,
) -> ApiResult<&'a crate::model::ChangedFile> {
    let file = snapshot
        .files
        .iter()
        .find(|file| file.id == id)
        .ok_or_else(ApiError::missing)?;
    if file.diff_digest != digest {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            code: "stale_fingerprint",
            message: "file changed; reload the session".into(),
            details: Some(json!({"file":file})),
        });
    }
    Ok(file)
}

async fn acknowledgement(
    state: AppState,
    actor: Actor,
    id: String,
    digest: String,
    kind: &str,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    let snapshot = state
        .session
        .snapshot
        .read()
        .map_err(|_| ApiError::internal())?;
    let file = checked_file(&snapshot, &id, &digest)?;
    let ack = Acknowledgement {
        id: new_id("ack"),
        path: file.path.clone(),
        kind: kind.into(),
        fingerprint_version: file.fingerprint_version,
        diff_digest: file.diff_digest.clone(),
        content_digest: file.content_digest.clone(),
        head_sha: snapshot.pr.head_sha.clone(),
        base_sha: snapshot.pr.base_sha.clone(),
        actor,
        created_at: now_rfc3339(),
    };
    state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?
        .insert_acknowledgement(&state.session.pr_id, &ack)
        .map_err(db_error)?;
    drop(snapshot);
    let event = state
        .session
        .notify("file.reviewed", json!({"fileId":id,"kind":kind}));
    Ok(Json(
        json!({"acknowledgement":ack,"sequence":event.sequence,"fileStates":state.session.view(actor).map_err(db_error)?.file_states}),
    ))
}

async fn mark_reviewed(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(id): Path<String>,
    Json(input): Json<Fingerprint>,
) -> ApiResult<Json<Value>> {
    acknowledgement(state, actor, id, input.diff_digest, "reviewed").await
}
async fn clear_reviewed(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(id): Path<String>,
    Json(input): Json<Fingerprint>,
) -> ApiResult<Json<Value>> {
    acknowledgement(state, actor, id, input.diff_digest, "cleared").await
}
async fn file_history(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let snapshot = state
        .session
        .snapshot
        .read()
        .map_err(|_| ApiError::internal())?;
    let file = snapshot
        .files
        .iter()
        .find(|f| f.id == id)
        .ok_or_else(ApiError::missing)?;
    let history: Vec<_> = state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?
        .list_acknowledgements(&state.session.pr_id)
        .map_err(db_error)?
        .into_iter()
        .filter(|a| a.path == file.path || file.previous_path.as_deref() == Some(&a.path))
        .collect();
    Ok(Json(json!({"file":file,"history":history})))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadInput {
    file_id: String,
    side: Side,
    line: u32,
    start_line: Option<u32>,
    body: String,
    diff_digest: String,
}

async fn create_thread(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Json(input): Json<ThreadInput>,
) -> ApiResult<Json<Value>> {
    body_text(&input.body)?;
    let snapshot = state
        .session
        .snapshot
        .read()
        .map_err(|_| ApiError::internal())?;
    let file = checked_file(&snapshot, &input.file_id, &input.diff_digest)?;
    if !file.patch_complete {
        return Err(ApiError::bad("file patch is incomplete"));
    }
    let line = diff::find_line(file, input.side, input.line)
        .ok_or_else(|| ApiError::bad("line is not in the current diff"))?;
    if line.kind == LineKind::Meta {
        return Err(ApiError::bad("cannot anchor to metadata"));
    }
    if let Some(start) = input.start_line
        && (start > input.line || diff::find_line(file, input.side, start).is_none())
    {
        return Err(ApiError::bad("invalid startLine"));
    }
    let id = new_id("thr");
    let thread = NewThread {
        id: id.clone(),
        source: ThreadSource::Local,
        github_thread_id: None,
        path: file.path.clone(),
        side: input.side,
        line: Some(input.line),
        start_line: input.start_line,
        line_text: Some(line.text.clone()),
        diff_digest: Some(file.diff_digest.clone()),
        head_sha: snapshot.pr.head_sha.clone(),
        anchor_state: AnchorState::Current,
        anchor_reason: None,
        resolved: false,
        created_at: None,
    };
    let now = now_rfc3339();
    let comment = Comment {
        id: new_id("cmt"),
        thread_id: id.clone(),
        author_kind: actor,
        author: actor.as_str().into(),
        body: input.body.trim().into(),
        created_at: now.clone(),
        updated_at: now,
        publish_state: PublishState::Draft,
        github_comment_id: None,
    };
    let mut store = state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?;
    store
        .insert_thread(&state.session.pr_id, &thread)
        .map_err(db_error)?;
    store.insert_comment(&comment, None).map_err(db_error)?;
    drop(store);
    drop(snapshot);
    let event = state
        .session
        .notify("thread.created", json!({"threadId":id}));
    Ok(Json(
        json!({"thread":state.session.store.lock().map_err(|_| ApiError::internal())?.get_thread(&state.session.pr_id, &id).map_err(db_error)?,"sequence":event.sequence}),
    ))
}

#[derive(Deserialize)]
struct ReplyInput {
    body: String,
}
async fn reply(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(id): Path<String>,
    Json(input): Json<ReplyInput>,
) -> ApiResult<Json<Value>> {
    body_text(&input.body)?;
    state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?
        .get_thread(&state.session.pr_id, &id)
        .map_err(db_error)?;
    let now = now_rfc3339();
    let comment = Comment {
        id: new_id("cmt"),
        thread_id: id.clone(),
        author_kind: actor,
        author: actor.as_str().into(),
        body: input.body.trim().into(),
        created_at: now.clone(),
        updated_at: now,
        publish_state: PublishState::Draft,
        github_comment_id: None,
    };
    state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?
        .insert_comment(&comment, None)
        .map_err(db_error)?;
    let event = state.session.notify(
        "thread.replied",
        json!({"threadId":id,"commentId":comment.id}),
    );
    Ok(Json(json!({"comment":comment,"sequence":event.sequence})))
}

#[derive(Deserialize)]
struct ThreadPatch {
    resolved: bool,
}
async fn update_thread(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(id): Path<String>,
    Json(input): Json<ThreadPatch>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    let mut store = state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?;
    let thread = store
        .get_thread(&state.session.pr_id, &id)
        .map_err(db_error)?;
    if thread.source != ThreadSource::Local {
        return Err(ApiError::forbidden());
    }
    store
        .set_thread_resolved(&id, input.resolved)
        .map_err(db_error)?;
    drop(store);
    let event = state
        .session
        .notify("thread.changed", json!({"threadId":id}));
    Ok(Json(
        json!({"thread":state.session.store.lock().map_err(|_| ApiError::internal())?.get_thread(&state.session.pr_id, &id).map_err(db_error)?,"sequence":event.sequence}),
    ))
}

fn editable_comment(state: &AppState, id: &str, actor: Actor) -> ApiResult<Comment> {
    let store = state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?;
    let comment = store.get_comment(id).map_err(db_error)?;
    store
        .get_thread(&state.session.pr_id, &comment.thread_id)
        .map_err(db_error)?;
    if comment.publish_state != PublishState::Draft || comment.author_kind != actor {
        return Err(ApiError::forbidden());
    }
    Ok(comment)
}
async fn update_comment(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(id): Path<String>,
    Json(input): Json<ReplyInput>,
) -> ApiResult<Json<Value>> {
    body_text(&input.body)?;
    editable_comment(&state, &id, actor)?;
    state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?
        .update_comment_body(&id, input.body.trim())
        .map_err(db_error)?;
    let event = state
        .session
        .notify("comment.changed", json!({"commentId":id}));
    Ok(Json(
        json!({"comment":state.session.store.lock().map_err(|_| ApiError::internal())?.get_comment(&id).map_err(db_error)?,"sequence":event.sequence}),
    ))
}
async fn delete_comment(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    editable_comment(&state, &id, actor)?;
    state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?
        .delete_comment(&id)
        .map_err(db_error)?;
    let event = state
        .session
        .notify("comment.deleted", json!({"commentId":id}));
    Ok(Json(json!({"sequence":event.sequence})))
}

async fn refresh(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
) -> ApiResult<Json<Value>> {
    state.session.refresh().await.map_err(db_error)?;
    let view = state.session.view(actor).map_err(db_error)?;
    Ok(Json(json!({"sequence":view.sequence,"session":view})))
}

#[derive(Deserialize)]
struct FinishInput {
    verdict: Verdict,
    summary: String,
}
async fn finish(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Json(input): Json<FinishInput>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    if input.summary.len() > 100_000 {
        return Err(ApiError::bad("summary is too long"));
    }
    if input.verdict != Verdict::Approve && input.summary.trim().is_empty() {
        return Err(ApiError::bad("a summary is required for this verdict"));
    }
    state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?
        .finish_review(&state.session.pr_id, input.verdict, input.summary.trim())
        .map_err(db_error)?;
    let event = state
        .session
        .notify("review.finished", json!({"verdict":input.verdict}));
    Ok(Json(
        json!({"review":state.session.store.lock().map_err(|_| ApiError::internal())?.review_state(&state.session.pr_id).map_err(db_error)?,"sequence":event.sequence}),
    ))
}
async fn publish_preview(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    if state.session.demo {
        return Err(ApiError::forbidden());
    }
    let plan = publish::preview(&state.session).map_err(|_| {
        ApiError::new(
            StatusCode::CONFLICT,
            "review_not_ready",
            "review is not ready to publish",
        )
    })?;
    Ok(Json(
        json!({"plan":plan,"sequence":state.session.sequence.load(std::sync::atomic::Ordering::SeqCst)}),
    ))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublishInput {
    plan_id: String,
    confirm: bool,
}
async fn publish_confirm(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Json(input): Json<PublishInput>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    if state.session.demo {
        return Err(ApiError::forbidden());
    }
    if !input.confirm {
        return Err(ApiError::bad("explicit confirmation is required"));
    }
    let outcome = publish::publish(&state.session, &input.plan_id, input.confirm)
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::CONFLICT,
                "publish_conflict",
                "publication failed or changed; refresh and preview before retrying",
            )
        })?;
    let event = state
        .session
        .notify("review.published", json!({"outcome":outcome}));
    Ok(Json(json!({"outcome":outcome,"sequence":event.sequence})))
}

async fn api_not_found() -> Response {
    ApiError::missing().into_response()
}

async fn static_asset(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path
        .split('/')
        .any(|part| part == ".." || part.starts_with('.'))
    {
        return ApiError::missing().into_response();
    }
    if let Some(file) = Assets::get(path).filter(|_| !path.is_empty()) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        let cache = if path.starts_with("assets/") {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };
        return (
            [
                (header::CONTENT_TYPE, mime.as_ref().to_string()),
                (header::CACHE_CONTROL, cache.into()),
            ],
            file.data,
        )
            .into_response();
    }
    match Assets::get("index.html") {
        Some(index) => (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
                (header::CACHE_CONTROL, "no-cache".into()),
            ],
            index.data,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "frontend not embedded").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
