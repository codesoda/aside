use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, ApiResult, AppState, db_error, human};
use crate::{
    model::{Actor, Verdict},
    publish,
};

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
pub(super) async fn publish_preview(
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
pub(super) struct SelectionInput {
    comment_ids: Vec<String>,
    selected: bool,
}
/// Choose which local drafts the next publication plan includes. Human only:
/// the selection decides what reaches GitHub, so an agent cannot change it.
pub(super) async fn publish_selection(
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
pub(super) struct PublishInput {
    plan_id: String,
    confirm: bool,
}
pub(super) async fn publish_confirm(
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
