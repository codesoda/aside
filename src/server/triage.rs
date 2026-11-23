use std::sync::atomic::Ordering;

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, ApiResult, AppState, db_error, human};
use crate::{
    model::{
        Actor, AnchorKind, ChangedFile, CodeAnchor, FindingId, FindingUserState, LineKind,
        ReviewFinding, Side, UserDisposition,
    },
    rules,
    store::{PromotionAnchor, PromotionConflict, Store, StoredFinding},
    util::now_rfc3339,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct StateEdit {
    expected_revision: u64,
    disposition: UserDisposition,
    snoozed_until: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Promotion {
    expected_revision: u64,
    operation_id: String,
}

fn current_finding(store: &Store, pr_id: &str, id: &FindingId) -> ApiResult<StoredFinding> {
    store
        .list_findings(pr_id, false)
        .map_err(db_error)?
        .into_iter()
        .find(|stored| &stored.finding.value().id == id)
        .ok_or_else(ApiError::missing)
}

fn deadline(edit: &StateEdit) -> ApiResult<Option<String>> {
    match (edit.disposition, edit.snoozed_until.as_deref()) {
        (UserDisposition::PromotedToDraft, _) => {
            Err(ApiError::bad("use the promotion action to create a draft"))
        }
        (UserDisposition::Snoozed, Some(value)) => {
            let until = DateTime::parse_from_rfc3339(value)
                .map_err(|_| ApiError::bad("snoozedUntil must be an RFC 3339 time"))?;
            if until <= Utc::now() {
                return Err(ApiError::bad("snoozedUntil must be in the future"));
            }
            Ok(Some(
                until
                    .with_timezone(&Utc)
                    .to_rfc3339_opts(SecondsFormat::AutoSi, true),
            ))
        }
        (UserDisposition::Snoozed, None) => Err(ApiError::bad("a snooze requires snoozedUntil")),
        (_, Some(_)) => Err(ApiError::bad(
            "snoozedUntil requires the snoozed disposition",
        )),
        (_, None) => Ok(None),
    }
}

pub(super) async fn update(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
    Json(edit): Json<StateEdit>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    let until = deadline(&edit)?;
    let session = &state.session;
    let finding_id = FindingId::new(&id);
    let saved = {
        let _snapshot = session.snapshot.read().map_err(|_| ApiError::internal())?;
        let mut store = session.store.lock().map_err(|_| ApiError::internal())?;
        let mut user = store
            .get_finding_user_state(&session.pr_id, &finding_id)
            .map_err(db_error)?
            .unwrap_or_else(|| {
                FindingUserState::untouched(finding_id, &session.pr_id, &now_rfc3339())
            });
        user.disposition = edit.disposition;
        user.snoozed_until = until;
        store
            .put_current_finding_user_state(&session.pr_id, &user, edit.expected_revision)
            .map_err(db_error)?
    };
    let event = session.notify("analysis.finding.state_changed", json!({"findingId": id}));
    Ok(Json(json!({"state": saved, "sequence": event.sequence})))
}

/// Only coordinates verified against the current diff can seed a draft.
pub(super) fn promotion_anchor<'a>(
    finding: &'a ReviewFinding,
    head_sha: &str,
    files: &[ChangedFile],
) -> Option<&'a CodeAnchor> {
    let anchor = &finding.anchor;
    if anchor.github_thread_id.is_some()
        || finding.source.kind.is_imported()
        || anchor
            .commit_sha
            .as_deref()
            .is_some_and(|sha| sha != head_sha)
    {
        return None;
    }
    let file = files.iter().find(|file| {
        anchor.file_id.as_deref() == Some(&file.id)
            && anchor.current_path.as_deref() == Some(&file.path)
            && anchor.diff_digest.as_deref() == Some(&file.diff_digest)
    })?;
    match anchor.kind {
        AnchorKind::File if anchor.line.is_none() && anchor.start_line.is_none() => Some(anchor),
        AnchorKind::DiffLine | AnchorKind::DiffRange if file.patch_complete => {
            let side = anchor.side?;
            let end = anchor.line?;
            let start = match anchor.kind {
                AnchorKind::DiffRange => anchor.start_line?,
                _ if anchor.start_line.is_none() => end,
                _ => return None,
            };
            let in_same_hunk = start <= end
                && file.hunks.iter().any(|hunk| {
                    let has_line = |number| {
                        hunk.lines.iter().any(|line| {
                            line.kind != LineKind::Meta
                                && match side {
                                    Side::Left => line.old_line,
                                    Side::Right => line.new_line,
                                } == Some(number)
                        })
                    };
                    has_line(start) && has_line(end)
                });
            in_same_hunk.then_some(anchor)
        }
        _ => None,
    }
}

pub(super) async fn promote(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
    Json(input): Json<Promotion>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    if input.operation_id.is_empty()
        || input.operation_id.len() > 128
        || !input
            .operation_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(ApiError::bad("invalid operationId"));
    }
    let session = &state.session;
    let finding_id = FindingId::new(&id);
    let (promoted, thread) = {
        let snapshot = session.snapshot.read().map_err(|_| ApiError::internal())?;
        let mut store = session.store.lock().map_err(|_| ApiError::internal())?;
        let map_promotion_error = |error: anyhow::Error| {
            if let Some(conflict) = error.downcast_ref::<PromotionConflict>() {
                ApiError::new(
                    StatusCode::CONFLICT,
                    "promotion_conflict",
                    conflict.to_string(),
                )
            } else {
                db_error(error)
            }
        };
        let promoted = if let Some(replay) = store
            .replay_finding_promotion(
                &session.pr_id,
                &finding_id,
                &input.operation_id,
                input.expected_revision,
            )
            .map_err(map_promotion_error)?
        {
            replay
        } else {
            let stored = current_finding(&store, &session.pr_id, &finding_id)?;
            let anchor = promotion_anchor(
                stored.finding.value(),
                &snapshot.pr.head_sha,
                &snapshot.files,
            )
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::CONFLICT,
                    "finding_not_promotable",
                    "finding has no current draft anchor",
                )
            })?;
            let effective = store
                .effective_rules(&session.repository.id, &session.pr_id)
                .map_err(db_error)?;
            if anchor
                .current_path
                .as_deref()
                .is_some_and(|path| rules::evaluate(&effective, path).ignored)
            {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "finding_not_promotable",
                    "finding is on an ignored file",
                ));
            }
            let line_text = anchor.line.zip(anchor.side).and_then(|(number, side)| {
                let file = snapshot
                    .files
                    .iter()
                    .find(|file| anchor.file_id.as_deref() == Some(&file.id))?;
                crate::diff::find_line(file, side, number).map(|line| line.text.as_str())
            });
            store
                .promote_finding(
                    &session.pr_id,
                    &finding_id,
                    &input.operation_id,
                    input.expected_revision,
                    PromotionAnchor {
                        anchor,
                        head_sha: &snapshot.pr.head_sha,
                        line_text,
                    },
                )
                .map_err(map_promotion_error)?
        };
        let mut thread = store
            .get_thread(&session.pr_id, &promoted.thread_id)
            .map_err(db_error)?;
        thread.file_id = snapshot
            .files
            .iter()
            .find(|file| file.path == thread.path)
            .map(|file| file.id.clone());
        (promoted, thread)
    };
    let sequence = if promoted.changed {
        session
            .notify(
                "thread.changed",
                json!({"threadId": promoted.thread_id, "findingId": id}),
            )
            .sequence
    } else {
        session.sequence.load(Ordering::SeqCst)
    };
    let draft = thread
        .review_comment
        .as_ref()
        .filter(|draft| draft.id == promoted.draft.id && !draft.locked)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "promotion_conflict",
                "the promoted draft is no longer editable",
            )
        })?;
    Ok(Json(
        json!({"state": promoted.state, "thread": thread, "draft": draft, "sequence": sequence}),
    ))
}

#[cfg(test)]
#[path = "triage_tests.rs"]
mod tests;
