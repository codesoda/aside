use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, ApiResult, AppState, db_error, human};
use crate::{
    model::{Actor, Verdict},
    publish,
    store::PublishingElsewhere,
};

/// The human-only publication routes, under `/api/v1`.
pub(super) fn publish_routes() -> Router<AppState> {
    Router::new()
        .route("/review/publish/preview", post(publish_preview))
        .route("/review/publish/selection", put(publish_selection))
        .route("/review/publish/unresolved", get(publish_unresolved))
        .route(
            "/review/publish/attempts/{id}/reconcile",
            post(publish_reconcile),
        )
        .route("/review/publish", post(publish_confirm))
}

pub(super) async fn refresh(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
) -> ApiResult<Json<Value>> {
    state.session.refresh().await.map_err(db_error)?;
    let view = state.session.view(actor).map_err(db_error)?;
    Ok(Json(json!({"sequence":view.sequence,"session":view})))
}

#[derive(Deserialize)]
pub(super) struct FinishInput {
    verdict: Verdict,
    summary: String,
}
pub(super) async fn finish(
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
struct SelectionInput {
    comment_ids: Vec<String>,
    selected: bool,
}
/// Choose which local drafts the next publication plan includes. Human only:
/// the selection decides what reaches GitHub, so an agent cannot change it.
async fn publish_selection(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Json(input): Json<SelectionInput>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    if input.comment_ids.is_empty() || input.comment_ids.len() > 1_000 {
        return Err(ApiError::bad("commentIds must list 1–1000 comment ids"));
    }
    let deselected = {
        let mut store = state
            .session
            .store
            .lock()
            .map_err(|_| ApiError::internal())?;
        store
            .set_publication_selection(&state.session.pr_id, &input.comment_ids, input.selected)
            .map_err(db_error)?;
        store
            .deselected_comment_ids(&state.session.pr_id)
            .map_err(db_error)?
    };
    let event = state.session.notify(
        "publication.selection_changed",
        json!({"commentIds":input.comment_ids,"selected":input.selected}),
    );
    Ok(Json(
        json!({"deselectedCommentIds":deselected,"sequence":event.sequence}),
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
    // A send keeps its locks until it ends, even if the browser drops this request.
    let session = state.session.clone();
    let send =
        tokio::spawn(
            async move { publish::publish(&session, &input.plan_id, input.confirm).await },
        );
    let outcome = send
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(|error| {
            if error.is::<PublishingElsewhere>() {
                return db_error(error);
            }
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
/// The PR's unresolved publication attempt, if any, with the drafts it has not published.
async fn publish_unresolved(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    let unresolved = state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?
        .unresolved_publication(&state.session.pr_id)
        .map_err(db_error)?;
    Ok(Json(json!({ "unresolved": unresolved })))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublishedComment {
    comment_id: String,
    github_comment_id: Option<i64>,
}
#[derive(Deserialize)]
struct ReconcileInput {
    published: Vec<PublishedComment>,
}
/// After checking GitHub, the human records which of an unresolved attempt's drafts
/// reached it. The rest become drafts again, and the PR can be published again.
async fn publish_reconcile(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(id): Path<String>,
    Json(input): Json<ReconcileInput>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    if input.published.len() > 1_000 {
        return Err(ApiError::bad("published must list at most 1000 comments"));
    }
    if input
        .published
        .iter()
        .any(|c| c.github_comment_id.is_some_and(|id| id <= 0))
    {
        return Err(ApiError::bad("githubCommentId must be positive"));
    }
    let published: Vec<_> = input
        .published
        .into_iter()
        .map(|c| (c.comment_id, c.github_comment_id))
        .collect();
    publish::reconcile(&state.session, &id, &published)
        .await
        .map_err(db_error)?;
    let event = state
        .session
        .notify("publication.reconciled", json!({ "attemptId": id }));
    Ok(Json(json!({ "attemptId": id, "sequence": event.sequence })))
}
