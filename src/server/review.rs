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
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SelectionInput {
    comment_ids: Vec<String>,
    selected: bool,
}
pub(super) async fn publish_selection(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Json(input): Json<SelectionInput>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    if input.comment_ids.is_empty()
        || input.comment_ids.len() > 500
        || input.comment_ids.iter().any(|id| id.is_empty())
    {
        return Err(ApiError::bad("commentIds must contain 1–500 IDs"));
    }
    let (changed, selection_revision) = state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?
        .set_publish_selection(&state.session.pr_id, &input.comment_ids, input.selected)
        .map_err(|e| {
            if e.to_string().contains("not selectable") {
                ApiError::new(
                    StatusCode::CONFLICT,
                    "not_selectable",
                    "comment is not selectable",
                )
            } else {
                db_error(e)
            }
        })?;
    let sequence = if changed.is_empty() {
        state
            .session
            .sequence
            .load(std::sync::atomic::Ordering::SeqCst)
    } else {
        state.session.notify("review.selection.changed", json!({"commentIds":input.comment_ids,"selected":input.selected,"selectionRevision":selection_revision})).sequence
    };
    Ok(Json(
        json!({"commentIds":input.comment_ids,"selected":input.selected,"changed":changed,"selectionRevision":selection_revision,"sequence":sequence}),
    ))
}

pub(super) async fn publish_preview(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    let plan = publish::preview(&state.session).map_err(|error| {
        if error.to_string().contains("snapshot is incomplete") {
            ApiError::new(
                StatusCode::CONFLICT,
                "snapshot_incomplete",
                "refresh before publishing",
            )
        } else {
            ApiError::new(
                StatusCode::CONFLICT,
                "review_not_ready",
                "finish the local review before publishing",
            )
        }
    })?;
    let blockers = publish::blockers(&state.session, &plan).map_err(db_error)?;
    Ok(Json(
        json!({"plan":plan,"blockers":blockers,"sequence":state.session.sequence.load(std::sync::atomic::Ordering::SeqCst)}),
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
        .map_err(|error| {
            let text = error.to_string();
            let code = if text.contains("plan changed") || text.contains("draft changed") {
                "plan_changed"
            } else if text.contains("publication is blocked")
                || text.contains("unresolved publication")
                || text.contains("already attempted")
            {
                "publish_blocked"
            } else if text.contains("PR base")
                || text.contains("PR revision")
                || text.contains("anchor")
                || text.contains("parent is missing")
            {
                "pr_changed"
            } else {
                "publish_conflict"
            };
            let mut api = ApiError::new(
                StatusCode::CONFLICT,
                code,
                "publication failed or changed; refresh and preview before retrying",
            );
            if code == "publish_blocked"
                && let Ok(plan) = publish::preview(&state.session)
                && let Ok(blockers) = publish::blockers(&state.session, &plan)
            {
                api.details = Some(json!({"blockers":blockers}));
            }
            api
        })?;
    let event = state
        .session
        .notify("review.published", json!({"outcome":outcome}));
    Ok(Json(json!({"outcome":outcome,"sequence":event.sequence})))
}
