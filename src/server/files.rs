use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, ApiResult, AppState, db_error, human};
use crate::{
    model::{Acknowledgement, Actor},
    util::{new_id, now_rfc3339},
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Fingerprint {
    diff_digest: String,
}

pub(super) fn checked_file<'a>(
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

pub(super) async fn mark_reviewed(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(id): Path<String>,
    Json(input): Json<Fingerprint>,
) -> ApiResult<Json<Value>> {
    acknowledgement(state, actor, id, input.diff_digest, "reviewed").await
}
pub(super) async fn clear_reviewed(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(id): Path<String>,
    Json(input): Json<Fingerprint>,
) -> ApiResult<Json<Value>> {
    acknowledgement(state, actor, id, input.diff_digest, "cleared").await
}
pub(super) async fn file_history(
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
