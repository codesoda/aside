use anyhow::{Result, bail};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use crate::model::{
    AnchorKind, CodeAnchor, FindingId, FindingUserState, ReviewCommentDraft, ReviewFinding, Side,
    UserDisposition,
};
use crate::util::{new_id, now_rfc3339};

use super::{
    Store,
    analysis::{RevisionConflict, read_user_state},
};

#[derive(Debug)]
pub struct PromotionConflict(pub &'static str);
impl std::fmt::Display for PromotionConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "finding promotion conflict: {}", self.0)
    }
}
impl std::error::Error for PromotionConflict {}

pub struct PromotionAnchor<'a> {
    pub anchor: &'a CodeAnchor,
    pub head_sha: &'a str,
    pub line_text: Option<&'a str>,
}

#[derive(Debug)]
pub struct PromotedFinding {
    pub changed: bool,
    pub thread_id: String,
    pub draft: ReviewCommentDraft,
    pub state: FindingUserState,
}

/// Read a recorded promotion without checking the finding's current anchor or
/// applicability. The operation captures the original server-derived target.
fn replay_promotion(
    tx: &Transaction<'_>,
    pr_id: &str,
    finding_id: &FindingId,
    operation_id: &str,
    expected_revision: u64,
    target: Option<(&str, &str)>,
) -> Result<Option<PromotedFinding>> {
    let existing: Option<(String, i64, String, String, String)> = tx.query_row(
        "SELECT finding_id, expected_revision, anchor_json, head_sha, draft_id FROM finding_promotion_operations WHERE pr_id = ?1 AND session_id = ?2 AND operation_id = ?3",
        params![pr_id, pr_id, operation_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    ).optional()?;
    let Some((old_finding, old_revision, old_anchor, old_head, draft_id)) = existing else {
        return Ok(None);
    };
    if old_finding != finding_id.as_str()
        || old_revision != expected_revision as i64
        || target.is_some_and(|(anchor, head)| old_anchor != anchor || old_head != head)
    {
        return Err(PromotionConflict("operation ID has different inputs").into());
    }
    let live: Option<(String, String, String, bool, Option<String>)> = tx.query_row(
        "SELECT d.thread_id, d.body, d.updated_at, d.publish_selected, d.reserved_attempt_id FROM review_comment_drafts d JOIN review_threads t ON t.id = d.thread_id WHERE d.id = ?1 AND t.pr_id = ?2 AND d.state = 'draft'",
        params![draft_id, pr_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    ).optional()?;
    let Some((thread_id, body, updated_at, selected, reservation)) = live else {
        return Err(PromotionConflict("promoted draft was discarded or published").into());
    };
    if reservation.is_some() {
        return Err(PromotionConflict("promoted draft is reserved").into());
    }
    let state = read_user_state(tx, pr_id, finding_id)?
        .ok_or(PromotionConflict("promoted state is missing"))?;
    if state.promoted_draft_id.as_deref() != Some(draft_id.as_str()) {
        return Err(PromotionConflict("promoted state has changed").into());
    }
    Ok(Some(PromotedFinding {
        changed: false,
        thread_id,
        draft: ReviewCommentDraft {
            id: draft_id,
            body,
            updated_at,
            selected,
            locked: false,
            source_comment_id: None,
            target: None,
            blocker: None,
        },
        state,
    }))
}

impl Store {
    /// Replay a persisted promotion using a consistent read snapshot. This does
    /// not require the finding to remain current or its original head to match.
    pub fn replay_finding_promotion(
        &mut self,
        pr_id: &str,
        finding_id: &FindingId,
        operation_id: &str,
        expected_revision: u64,
    ) -> Result<Option<PromotedFinding>> {
        if operation_id.trim().is_empty() {
            bail!("promotion needs an operation");
        }
        let tx = self.conn.transaction()?;
        let result = replay_promotion(
            &tx,
            pr_id,
            finding_id,
            operation_id,
            expected_revision,
            None,
        )?;
        tx.commit()?;
        Ok(result)
    }

    /// One local, all-or-nothing promotion. The caller must independently
    /// validate the supplied canonical anchor against the live diff.
    pub fn promote_finding(
        &mut self,
        pr_id: &str,
        finding_id: &FindingId,
        operation_id: &str,
        expected_revision: u64,
        target: PromotionAnchor<'_>,
    ) -> Result<PromotedFinding> {
        let PromotionAnchor {
            anchor: expected_anchor,
            head_sha,
            line_text,
        } = target;
        let session_id = pr_id;
        if operation_id.trim().is_empty() || head_sha.is_empty() {
            bail!("promotion needs an operation and head");
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let anchor_json = serde_json::to_string(expected_anchor)?;
        if let Some(replayed) = replay_promotion(
            &tx,
            pr_id,
            finding_id,
            operation_id,
            expected_revision,
            Some((&anchor_json, head_sha)),
        )? {
            return Ok(replayed);
        }
        let json: String = tx
            .query_row(
                "SELECT finding_json FROM review_findings WHERE id = ?1 AND pr_id = ?2",
                params![finding_id.as_str(), pr_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(PromotionConflict("finding is not in this PR"))?;
        let finding: ReviewFinding = serde_json::from_str(&json)?;
        let owned: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM pull_requests p JOIN repositories r ON r.id = p.repository_id WHERE p.id = ?1 AND r.provider_id = ?2 AND p.number = ?3)",
            params![pr_id, finding.repository_id, finding.pr_number as i64], |r| r.get(0))?;
        if !owned {
            return Err(PromotionConflict("finding owner does not match PR").into());
        }
        let current: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM finding_occurrences o JOIN analysis_runs r ON r.id = o.run_id JOIN analysis_current_snapshots c ON c.snapshot_id = o.snapshot_id AND c.pr_id = ?2 WHERE o.finding_id = ?1 AND r.status IN ('running','succeeded','partial') AND o.applicability IN ('current_exact','current_related'))",
            params![finding_id.as_str(), pr_id], |r| r.get(0),
        )?;
        if !current {
            return Err(PromotionConflict("finding is historical").into());
        }
        let a = &finding.anchor;
        let side = a.side.unwrap_or(Side::Right);
        let subject = if a.kind == AnchorKind::File {
            "file"
        } else {
            "line"
        };
        if a != expected_anchor
            || !matches!(
                a.kind,
                AnchorKind::DiffLine | AnchorKind::DiffRange | AnchorKind::File
            )
            || a.github_thread_id.is_some()
            || finding.source.kind.is_imported()
            || a.current_path.as_deref().is_none_or(str::is_empty)
            || (a.kind != AnchorKind::File
                && (a.side.is_none() || a.line.is_none_or(|n| n == 0) || line_text.is_none()))
            || (a.kind == AnchorKind::File
                && (a.line.is_some() || a.start_line.is_some() || line_text.is_some()))
            || a.commit_sha.as_deref().is_some_and(|sha| sha != head_sha)
            || a.start_line
                .is_some_and(|start| start == 0 || start > a.line.unwrap_or(0))
        {
            return Err(PromotionConflict("finding anchor is invalid or stale").into());
        }
        let prior = read_user_state(&tx, session_id, finding_id)?;
        if prior.as_ref().map_or(0, |s| s.revision) != expected_revision {
            return Err(RevisionConflict { current: prior }.into());
        }
        let owner: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM review_sessions WHERE pr_id = ?1)",
            [pr_id],
            |r| r.get(0),
        )?;
        if !owner {
            return Err(PromotionConflict("review session is missing").into());
        }
        let thread: Option<String> = tx.query_row(
            "SELECT id FROM review_threads WHERE pr_id = ?1 AND source = 'local' AND finding_id = ?2 ORDER BY created_at LIMIT 1",
            params![pr_id, finding_id.as_str()], |r| r.get(0),
        ).optional()?;
        let now = now_rfc3339();
        let thread_id = if let Some(id) = thread {
            let matches: bool = tx.query_row("SELECT path = ?2 AND side = ?3 AND line IS ?4 AND start_line IS ?5 AND diff_digest IS ?6 AND head_sha = ?7 AND anchor_state = 'current' AND subject = ?8 FROM review_threads WHERE id = ?1",
                params![id, a.current_path, side.as_str(), a.line, a.start_line, a.diff_digest, head_sha, subject], |r| r.get(0))?;
            if !matches {
                return Err(
                    PromotionConflict("matching finding thread has a different anchor").into(),
                );
            }
            id
        } else {
            let id = new_id("thread");
            tx.execute("INSERT INTO review_threads (id, pr_id, source, path, side, line, start_line, line_text, diff_digest, head_sha, anchor_state, subject, finding_id, created_at, updated_at) VALUES (?1, ?2, 'local', ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'current', ?12, ?10, ?11, ?11)",
                params![id, pr_id, a.current_path, side.as_str(), a.line, a.start_line, line_text, a.diff_digest, head_sha, finding_id.as_str(), now, subject])?;
            id
        };
        let draft: Option<(String, String, String, bool, Option<String>)> = tx.query_row(
            "SELECT id, body, updated_at, publish_selected, reserved_attempt_id FROM review_comment_drafts WHERE thread_id = ?1 AND state = 'draft'", [&thread_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        ).optional()?;
        let (draft_id, body, updated_at, selected) = if let Some((
            id,
            body,
            at,
            selected,
            reservation,
        )) = draft
        {
            if reservation.is_some() {
                return Err(PromotionConflict("matching draft is reserved").into());
            }
            (id, body, at, selected)
        } else {
            let id = new_id("draft");
            let body = finding.body.trim();
            let body = if body.is_empty() || body.len() > 100_000 {
                finding.title.trim()
            } else {
                body
            };
            if body.is_empty() || body.len() > 100_000 {
                return Err(PromotionConflict("finding has no usable draft text").into());
            }
            tx.execute("INSERT INTO review_comment_drafts (id, thread_id, body, state, publish_selected, created_at, updated_at) VALUES (?1, ?2, ?3, 'draft', 0, ?4, ?4)", params![id, thread_id, body, now])?;
            tx.execute("UPDATE review_sessions SET selection_revision = selection_revision + 1 WHERE pr_id = ?1", [pr_id])?;
            (id, body.to_owned(), now.clone(), false)
        };
        let mut state = prior
            .unwrap_or_else(|| FindingUserState::untouched(finding_id.clone(), session_id, &now));
        state.disposition = UserDisposition::PromotedToDraft;
        state.snoozed_until = None;
        state.promoted_draft_id = Some(draft_id.clone());
        state.revision = expected_revision + 1;
        state.updated_at = now.clone();
        tx.execute("INSERT INTO finding_user_state (session_id, finding_id, pr_id, disposition, snoozed_until, note, promoted_draft_id, updated_at, revision) VALUES (?1, ?2, ?3, 'promoted_to_draft', NULL, ?4, ?5, ?6, ?7) ON CONFLICT(session_id, finding_id) DO UPDATE SET disposition = excluded.disposition, snoozed_until = NULL, promoted_draft_id = excluded.promoted_draft_id, updated_at = excluded.updated_at, revision = excluded.revision",
            params![session_id, finding_id.as_str(), pr_id, state.note, draft_id, now, state.revision as i64])?;
        tx.execute("INSERT INTO finding_promotion_operations (pr_id, session_id, operation_id, finding_id, expected_revision, anchor_json, head_sha, draft_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![pr_id, session_id, operation_id, finding_id.as_str(), expected_revision as i64, anchor_json, head_sha, draft_id])?;
        tx.commit()?;
        Ok(PromotedFinding {
            changed: true,
            thread_id,
            draft: ReviewCommentDraft {
                id: draft_id,
                body,
                updated_at,
                selected,
                locked: false,
                source_comment_id: None,
                target: None,
                blocker: None,
            },
            state,
        })
    }
}
