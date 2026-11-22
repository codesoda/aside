use super::{ApiError, ApiResult, AppState, human};
use crate::{
    explain::{
        ExplainError,
        context::{self, Question},
        providers,
    },
    model::Actor,
};
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde_json::{Value, json};
use std::time::Duration;

fn error(error: ExplainError) -> ApiError {
    let (status, code, message) = match error {
        ExplainError::Stale => (
            StatusCode::CONFLICT,
            "stale_selection",
            "The review or revision changed. Select the change again; this answer was not retained.",
        ),
        ExplainError::Busy => (
            StatusCode::TOO_MANY_REQUESTS,
            "explanation_busy",
            "An explanation is still running. Cancel it or wait before asking again.",
        ),
        ExplainError::Cancelled => (
            StatusCode::CONFLICT,
            "explanation_cancelled",
            "Explanation cancelled.",
        ),
        ExplainError::Timeout => (
            StatusCode::GATEWAY_TIMEOUT,
            "explanation_timeout",
            "The provider timed out after 90 seconds. You can retry; no fallback was used.",
        ),
        ExplainError::Invalid(message) => (StatusCode::BAD_REQUEST, "invalid_explanation", message),
        ExplainError::Provider(message) => (StatusCode::BAD_GATEWAY, "provider_error", message),
    };
    ApiError::new(status, code, message)
}

pub(super) async fn options(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    Ok(Json(
        json!({"providers": providers::list(&state.explanations.config).await}),
    ))
}

pub(super) async fn ask(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<Question>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    let mut running = state
        .explanations
        .start(&request.request_id)
        .map_err(error)?;
    let selected = {
        let snapshot = state
            .session
            .snapshot
            .read()
            .map_err(|_| ApiError::internal())?;
        context::context(&state.session.id, &snapshot, &request.selection).map_err(error)?
    };
    let prompt = context::prompt(selected, &request).map_err(error)?;
    let answer = running
        .run(
            providers::answer(&state.explanations.config, &request.provider, &prompt),
            Duration::from_secs(90),
        )
        .await
        .map_err(error)?;
    // Refresh can happen while the model is running, even for unchanged file digests.
    {
        let snapshot = state
            .session
            .snapshot
            .read()
            .map_err(|_| ApiError::internal())?;
        context::context(&state.session.id, &snapshot, &request.selection).map_err(error)?;
    }
    Ok(Json(
        json!({"requestId": request.request_id, "selection": request.selection, "provider": request.provider, "answer": answer}),
    ))
}

pub(super) async fn cancel(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    state.explanations.cancel(&id).map_err(error)?;
    Ok(Json(json!({"cancelled": true})))
}
