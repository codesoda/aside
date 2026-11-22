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
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PublishPreviewInput {
    verdict: Verdict,
    body: String,
}

pub(super) async fn publish_preview(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    input: Result<Json<PublishPreviewInput>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    if state.session.demo {
        return Err(ApiError::forbidden());
    }
    let Json(input) = input.map_err(|_| ApiError::bad("invalid publish preview request"))?;
    if input.body.len() > 100_000 {
        return Err(ApiError::bad("body is too long"));
    }
    let plan =
        publish::preview_requested(&state.session, input.verdict, &input.body).map_err(|_| {
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
pub(super) struct PublishInput {
    plan_id: String,
    verdict: Verdict,
    body: String,
    confirm: bool,
}
/// Read only the unresolved claim for the current PR, never publication history or plan text.
pub(super) async fn publish_status(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    let active = state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?
        .active_publication(&state.session.pr_id)
        .map_err(db_error)?;
    Ok(Json(match active {
        Some((attempt_id, affected_draft_ids)) => {
            json!({"active": {"attemptId": attempt_id, "affectedDraftIds": affected_draft_ids}})
        }
        None => json!({"active": null}),
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ReconcileInput {
    attempt_id: String,
    /// Explicit human determination after checking the review on GitHub.
    outcome: String,
    confirm: bool,
}
pub(super) async fn publish_reconcile(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Json(input): Json<ReconcileInput>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    if state.session.demo {
        return Err(ApiError::forbidden());
    }
    if !input.confirm {
        return Err(ApiError::bad(
            "explicit reconciliation confirmation required",
        ));
    }
    let posted = match input.outcome.as_str() {
        "posted" => true,
        "not_posted" => false,
        _ => return Err(ApiError::bad("outcome must be posted or not_posted")),
    };
    // Never hold the synchronous store lock while waiting for a remote publication.
    let _publication_guard = publish::publication_mutex().lock().await;
    state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?
        .reconcile_publication(&state.session.pr_id, &input.attempt_id, posted)
        .map_err(db_error)?;
    let event = state.session.notify(
        "review.publication_reconciled",
        json!({"attemptId":input.attempt_id,"outcome":input.outcome}),
    );
    Ok(Json(json!({"sequence":event.sequence})))
}

pub(super) async fn publish_confirm(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    input: Result<Json<PublishInput>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    if state.session.demo {
        return Err(ApiError::forbidden());
    }
    let Json(input) = input.map_err(|_| ApiError::bad("invalid publish confirmation request"))?;
    if !input.confirm {
        return Err(ApiError::bad("explicit confirmation is required"));
    }
    if input.body.len() > 100_000 {
        return Err(ApiError::bad("body is too long"));
    }
    let outcome = publish::publish_requested(
        &state.session,
        &input.plan_id,
        input.verdict,
        &input.body,
        input.confirm,
    )
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
