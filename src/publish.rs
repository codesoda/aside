//! Private-first publication plans and guarded GitHub review submission.
//!
//! An attempt is persisted as `partial` before its first remote operation. A
//! timeout may mean GitHub accepted a request, so the same plan is never sent
//! again automatically. A human must reconcile uncertain attempts on GitHub.

use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::{
    diff,
    github::{self, GhRunner, NewReviewComment, PrRef},
    model::{
        AnchorState, LineKind, PrSnapshot, ReviewThread, Side, ThreadSource, ThreadSubject,
        ThreadTarget, ThreadTargetKind, Verdict,
    },
    session::Session,
    util::sha256_hex,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedInline {
    pub comment_id: String,
    pub thread_id: String,
    pub path: String,
    pub side: Side,
    pub line: u32,
    pub start_line: Option<u32>,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedReply {
    pub comment_id: String,
    pub thread_id: String,
    /// A published or imported GitHub comment, or the new inline comment in this plan.
    pub parent_comment_id: String,
    pub github_parent_id: Option<i64>,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OmittedDraft {
    pub comment_id: String,
    pub thread_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishPlan {
    pub plan_id: String,
    pub head_sha: String,
    pub base_sha: String,
    pub verdict: Verdict,
    pub summary: String,
    pub inline: Vec<PlannedInline>,
    pub replies: Vec<PlannedReply>,
    pub omitted: Vec<OmittedDraft>,
    pub open_count: usize,
    pub resolved_count: usize,
    pub prepared_count: usize,
    pub attempt_ordinal: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishOutcome {
    pub attempt_id: String,
    pub status: String,
    pub github_review_id: Option<i64>,
    pub published_comment_ids: Vec<String>,
    pub remaining_comment_ids: Vec<String>,
    pub error: Option<String>,
}

fn lock_store(session: &Session) -> Result<std::sync::MutexGuard<'_, crate::store::Store>> {
    session
        .store
        .lock()
        .map_err(|_| anyhow!("store lock poisoned"))
}

fn current_anchor(t: &ReviewThread, snapshot: &PrSnapshot) -> bool {
    if t.anchor_state != AnchorState::Current || t.head_sha != snapshot.pr.head_sha {
        return false;
    }
    let (Some(line), Some(text)) = (t.line, t.line_text.as_deref()) else {
        return false;
    };
    snapshot
        .files
        .iter()
        .find(|f| f.path == t.path && f.patch_complete && !f.is_binary)
        .and_then(|f| diff::find_line(f, t.side, line))
        .is_some_and(|l| l.text == text)
        && t.start_line.is_none_or(|start| {
            start <= line
                && snapshot
                    .files
                    .iter()
                    .find(|f| f.path == t.path)
                    .and_then(|f| diff::find_line(f, t.side, start))
                    .is_some()
        })
}

/// The first actual patch row, including deletion-only patches. Never invent a coordinate.
fn first_file_row(file: &crate::model::ChangedFile) -> Option<(Side, u32)> {
    if !file.patch_complete || file.is_binary {
        return None;
    }
    file.hunks
        .iter()
        .flat_map(|h| &h.lines)
        .find_map(|line| match line.kind {
            LineKind::Deletion => line.old_line.map(|n| (Side::Left, n)),
            LineKind::Addition | LineKind::Context => line.new_line.map(|n| (Side::Right, n)),
            LineKind::Meta => None,
        })
}

fn build_plan(
    snapshot: &PrSnapshot,
    verdict: Verdict,
    summary: &str,
    threads: &[ReviewThread],
    attempt_ordinal: i64,
) -> Result<PublishPlan> {
    let mut plan = PublishPlan {
        plan_id: String::new(),
        head_sha: snapshot.pr.head_sha.clone(),
        base_sha: snapshot.pr.base_sha.clone(),
        verdict,
        summary: summary.to_owned(),
        inline: Vec::new(),
        replies: Vec::new(),
        omitted: Vec::new(),
        open_count: threads.iter().filter(|t| !t.resolved).count(),
        resolved_count: threads.iter().filter(|t| t.resolved).count(),
        prepared_count: threads
            .iter()
            .filter(|t| t.review_comment.is_some())
            .count(),
        attempt_ordinal,
    };
    for thread in threads {
        let Some(draft) = &thread.review_comment else {
            continue;
        };
        let file = snapshot.files.iter().find(|f| f.path == thread.path);
        let file_row = file.and_then(first_file_row);
        let parent = thread
            .comments
            .iter()
            .find(|c| c.github_comment_id.is_some());
        let reason = if draft.locked {
            Some("draft reserved by an unresolved attempt")
        } else if draft.body.trim().is_empty() {
            Some("empty prepared comment")
        } else if thread.source == ThreadSource::Github || !thread.published_comments.is_empty() {
            if parent.is_none()
                && thread
                    .published_comments
                    .iter()
                    .all(|p| p.github_comment_id.is_none())
            {
                Some("no confirmed GitHub root comment")
            } else {
                None
            }
        } else if thread.subject == ThreadSubject::File {
            if thread.anchor_state != AnchorState::Current
                || thread.head_sha != snapshot.pr.head_sha
                || file_row.is_none()
            {
                Some("file is stale, binary, incomplete or has no commentable diff row")
            } else {
                None
            }
        } else if !current_anchor(thread, snapshot) {
            Some("anchor is stale, missing or incomplete")
        } else {
            None
        };
        if let Some(reason) = reason {
            plan.omitted.push(OmittedDraft {
                comment_id: draft.id.clone(),
                thread_id: thread.id.clone(),
                reason: reason.into(),
            });
            continue;
        }
        if thread.source == ThreadSource::Github || !thread.published_comments.is_empty() {
            let github_parent_id = parent.and_then(|c| c.github_comment_id).or_else(|| {
                thread
                    .published_comments
                    .iter()
                    .find_map(|p| p.github_comment_id)
            });
            plan.replies.push(PlannedReply {
                comment_id: draft.id.clone(),
                thread_id: thread.id.clone(),
                parent_comment_id: parent.map(|c| c.id.clone()).unwrap_or_default(),
                github_parent_id,
                body: draft.body.clone(),
            });
        } else {
            let (side, line, body) = if thread.subject == ThreadSubject::File {
                let (side, line) = file_row.context("file has no commentable row")?;
                (side, line, format!("File Comment:\n{}", draft.body))
            } else {
                (
                    thread.side,
                    thread.line.context("line anchor missing")?,
                    draft.body.clone(),
                )
            };
            plan.inline.push(PlannedInline {
                comment_id: draft.id.clone(),
                thread_id: thread.id.clone(),
                path: thread.path.clone(),
                side,
                line,
                start_line: if thread.subject == ThreadSubject::File {
                    None
                } else {
                    thread.start_line
                },
                body,
            });
        }
    }
    if matches!(verdict, Verdict::RequestChanges | Verdict::Comment) && summary.trim().is_empty() {
        bail!("REQUEST_CHANGES and COMMENT require a summary or inline comment");
    }
    plan.plan_id = sha256_hex(&serde_json::to_vec(&plan)?);
    Ok(plan)
}

/// Project the same publication eligibility used by preview into the session DTO.
/// Only prepared drafts receive a target; private conversation messages are untouched.
pub fn attach_draft_targets(snapshot: &PrSnapshot, threads: &mut [ReviewThread]) -> Result<()> {
    let plan = if snapshot.complete {
        Some(build_plan(snapshot, Verdict::Approve, "", threads, 0)?)
    } else {
        None
    };
    for thread in threads {
        let Some(draft) = thread.review_comment.as_mut() else {
            continue;
        };
        draft.target = None;
        draft.blocker = None;
        let Some(plan) = &plan else {
            draft.blocker = Some("snapshot is incomplete; refresh before publication".into());
            continue;
        };
        if let Some(omitted) = plan
            .omitted
            .iter()
            .find(|c| c.comment_id == draft.id && c.thread_id == thread.id)
        {
            draft.blocker = Some(omitted.reason.clone());
        } else if let Some(inline) = plan
            .inline
            .iter()
            .find(|c| c.comment_id == draft.id && c.thread_id == thread.id)
        {
            draft.target = Some(ThreadTarget {
                kind: if thread.subject == ThreadSubject::File {
                    ThreadTargetKind::FileFallback
                } else {
                    ThreadTargetKind::Inline
                },
                path: inline.path.clone(),
                side: inline.side,
                line: Some(inline.line),
                start_line: inline.start_line,
            });
        } else if plan
            .replies
            .iter()
            .any(|c| c.comment_id == draft.id && c.thread_id == thread.id)
        {
            draft.target = Some(ThreadTarget {
                kind: ThreadTargetKind::Reply,
                path: thread.path.clone(),
                side: thread.side,
                line: thread.line,
                start_line: thread.start_line,
            });
        } else {
            draft.blocker = Some("no verified publication target".into());
        }
    }
    Ok(())
}

/// Build the exact outbound review and reply operations; invalid drafts are omitted with reasons.
/// In v1 all eligible local drafts are selected; the API has no per-comment selection state.
/// Callers must display `omitted` as well as the selected operations before confirmation.
pub fn preview_requested(session: &Session, verdict: Verdict, body: &str) -> Result<PublishPlan> {
    if matches!(verdict, Verdict::Comment | Verdict::RequestChanges) && body.trim().is_empty() {
        bail!("COMMENT and REQUEST_CHANGES require a summary");
    }
    let snapshot = session
        .snapshot
        .read()
        .map_err(|_| anyhow!("snapshot lock poisoned"))?
        .clone();
    if !snapshot.complete {
        bail!("snapshot is incomplete; refresh before publication");
    }
    let store = lock_store(session)?;
    build_plan(
        &snapshot,
        verdict,
        body,
        &store.list_threads(&session.pr_id)?,
        store.next_publication_ordinal(&session.pr_id)?,
    )
}

#[cfg(test)]
pub fn preview(session: &Session) -> Result<PublishPlan> {
    let state = lock_store(session)?.review_state(&session.pr_id)?;
    preview_requested(
        session,
        state.verdict.context("review has no verdict")?,
        &state.summary,
    )
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .context("GitHub task failed")?
}

fn github_context(session: &Session) -> Result<(Arc<dyn GhRunner>, PrRef)> {
    Ok((
        session.gh.clone().context("GitHub is unavailable")?,
        session
            .pr_ref
            .clone()
            .context("PR reference is unavailable")?,
    ))
}

pub(crate) fn publication_mutex() -> &'static tokio::sync::Mutex<()> {
    static PUBLICATION_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    PUBLICATION_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Publish a confirmed plan. A partial/uncertain attempt is never blindly retried.
/// The HTTP layer must authorize a human actor before calling this function.
pub async fn publish_requested(
    session: &Session,
    plan_id: &str,
    verdict: Verdict,
    body: &str,
    confirm: bool,
) -> Result<PublishOutcome> {
    if !confirm {
        bail!("explicit human confirmation is required");
    }
    if session.demo {
        bail!("demo sessions cannot publish to GitHub");
    }
    // Serialize active publications and human reconciliation across the remote operation.
    let _guard = publication_mutex().lock().await;
    let (gh, pr) = github_context(session)?;
    let snapshot = session
        .snapshot
        .read()
        .map_err(|_| anyhow!("snapshot lock poisoned"))?
        .clone();
    if !snapshot.complete {
        bail!("snapshot is incomplete; refresh before publication");
    }
    let plan = preview_requested(session, verdict, body)?;
    if plan.plan_id != plan_id {
        bail!("publication plan changed; preview again");
    }
    // Fetch a fresh, internally consistent diff before claiming drafts or writing
    // anything remotely. A matching head alone does not protect against a base
    // update, changed patch, or removed review-comment parent.
    let current = blocking({
        let gh = gh.clone();
        let pr = pr.clone();
        move || github::fetch_snapshot(gh.as_ref(), &pr, &diff::Limits::default())
    })
    .await?;
    if !current.complete
        || current.pr.base_sha != plan.base_sha
        || current.pr.head_sha != plan.head_sha
    {
        bail!("PR base, head or diff changed; refresh and preview again");
    }
    // Existing pending reviews must not be silently submitted over. A viewer
    // identity is only needed to reject an approval of one's own PR.
    let safety = blocking({
        let gh = gh.clone();
        let pr = pr.clone();
        move || {
            let out = gh.run(
                &[
                    "api",
                    "--paginate",
                    "--slurp",
                    &format!("{}/reviews?per_page=100", pr.api_base()),
                ],
                None,
            )?;
            let pages: Vec<Vec<serde_json::Value>> = serde_json::from_slice(&out)?;
            Ok(pages
                .into_iter()
                .flatten()
                .any(|review| review["state"].as_str() == Some("PENDING")))
        }
    })
    .await?;
    if safety {
        bail!("a pending GitHub review exists; resolve it before publishing");
    }
    if plan.verdict == Verdict::Approve && !current.pr.author.is_empty() {
        let viewer = blocking({
            let gh = gh.clone();
            move || gh.run(&["api", "user", "--jq", ".login"], None)
        })
        .await?;
        if String::from_utf8_lossy(&viewer)
            .trim()
            .eq_ignore_ascii_case(&current.pr.author)
        {
            bail!("cannot approve your own pull request");
        }
    }
    let threads = lock_store(session)?.list_threads(&session.pr_id)?;
    for inline in &plan.inline {
        let thread = threads
            .iter()
            .find(|t| t.id == inline.thread_id)
            .context("planned inline thread disappeared; preview again")?;
        if thread.path != inline.path
            || (thread.subject == ThreadSubject::Line
                && (thread.side != inline.side
                    || thread.line != Some(inline.line)
                    || thread.start_line != inline.start_line
                    || !current_anchor(thread, &current)))
            || (thread.subject == ThreadSubject::File
                && current
                    .files
                    .iter()
                    .find(|f| f.path == inline.path)
                    .and_then(first_file_row)
                    != Some((inline.side, inline.line)))
        {
            bail!("inline anchor changed or disappeared; refresh and preview again");
        }
    }
    for reply in &plan.replies {
        if let Some(parent_id) = reply.github_parent_id {
            let thread = threads
                .iter()
                .find(|t| t.id == reply.thread_id)
                .context("reply thread disappeared; preview again")?;
            let valid = current.github_threads.iter().any(|remote| {
                !remote.rest_only
                    && remote.path == thread.path
                    && thread.github_thread_id.as_deref().map_or_else(
                        || {
                            thread
                                .published_comments
                                .iter()
                                .any(|p| p.github_comment_id == Some(parent_id))
                        },
                        |id| remote.node_id == id,
                    )
                    && !remote.is_outdated
                    && !remote.is_resolved
                    && remote
                        .comments
                        .iter()
                        .any(|c| c.database_id == Some(parent_id))
            });
            if !valid {
                bail!("reply parent is missing or outdated; refresh and preview again");
            }
        } else if !plan
            .inline
            .iter()
            .any(|c| c.comment_id == reply.parent_comment_id && c.thread_id == reply.thread_id)
        {
            bail!("reply parent is not a validated inline comment; preview again");
        }
    }
    // Close the window between the expensive snapshot fetch and the first
    // remote operation with a final lightweight revision check.
    let refs = blocking({
        let gh = gh.clone();
        let pr = pr.clone();
        move || github::fetch_revision(gh.as_ref(), &pr)
    })
    .await?;
    if refs.0 != plan.base_sha || refs.1 != plan.head_sha {
        bail!("PR base or head changed; refresh and preview again");
    }
    let plan_json = serde_json::to_string(&plan)?;
    let attempt_id = {
        let mut store = lock_store(session)?;
        // Rebuild and compare while holding the store lock, then reserve exactly
        // the confirmed texts. A concurrent edit after preflight must not be
        // substituted for the text the user approved.
        let threads = store.list_threads(&session.pr_id)?;
        let latest = build_plan(
            &snapshot,
            plan.verdict,
            &plan.summary,
            &threads,
            store.next_publication_ordinal(&session.pr_id)?,
        )?;
        if latest.plan_id != plan_id {
            bail!("publication plan changed; preview again");
        }
        let drafts: Vec<_> = plan
            .inline
            .iter()
            .map(|c| {
                let is_file = threads
                    .iter()
                    .any(|t| t.id == c.thread_id && t.subject == ThreadSubject::File);
                let body = if is_file {
                    c.body
                        .strip_prefix("File Comment:\n")
                        .context("invalid file comment prefix")?
                } else {
                    c.body.as_str()
                };
                Ok((c.comment_id.as_str(), body))
            })
            .chain(
                plan.replies
                    .iter()
                    .map(|c| Ok((c.comment_id.as_str(), c.body.as_str()))),
            )
            .collect::<Result<_>>()?;
        // The transactional claim and draft reservations commit before the remote call.
        store.start_publication(&session.pr_id, plan_id, &plan_json, &plan.head_sha, &drafts)?
    };
    let mut outcome = PublishOutcome {
        attempt_id: attempt_id.clone(),
        status: "partial".into(),
        github_review_id: None,
        published_comment_ids: Vec::new(),
        remaining_comment_ids: plan
            .inline
            .iter()
            .map(|c| c.comment_id.clone())
            .chain(plan.replies.iter().map(|c| c.comment_id.clone()))
            .collect(),
        error: None,
    };
    if let Err(error) = execute(session, &gh, &pr, &plan, &mut outcome).await {
        outcome.error = Some(error.to_string());
        lock_store(session)?.finish_publication(
            &attempt_id,
            "partial",
            outcome.error.as_deref(),
            outcome.github_review_id,
        )?;
        return Ok(outcome);
    }
    if outcome.status == "rejected" {
        outcome.error = Some("GitHub rejected the review without posting it".into());
        return Ok(outcome);
    }
    lock_store(session)?.mark_published(&session.pr_id)?;
    lock_store(session)?.finish_publication(
        &attempt_id,
        "succeeded",
        None,
        outcome.github_review_id,
    )?;
    outcome.status = "succeeded".into();
    Ok(outcome)
}

#[cfg(test)]
pub async fn publish(session: &Session, plan_id: &str, confirm: bool) -> Result<PublishOutcome> {
    let state = lock_store(session)?.review_state(&session.pr_id)?;
    publish_requested(
        session,
        plan_id,
        state.verdict.context("review has no verdict")?,
        &state.summary,
        confirm,
    )
    .await
}

async fn execute(
    session: &Session,
    gh: &Arc<dyn GhRunner>,
    pr: &PrRef,
    plan: &PublishPlan,
    outcome: &mut PublishOutcome,
) -> Result<()> {
    let comments: Vec<NewReviewComment> = plan
        .inline
        .iter()
        .map(|c| NewReviewComment {
            path: c.path.clone(),
            side: c.side,
            line: c.line,
            start_line: c.start_line,
            body: c.body.clone(),
        })
        .collect();
    // The final revision check runs before start_publication. Do not perform
    // fallible read-only preflight after claiming drafts: a failure there would
    // leave a partial attempt even though no remote write was made.
    let review_result = blocking({
        let gh = gh.clone();
        let pr = pr.clone();
        let head = plan.head_sha.clone();
        let event = plan.verdict.as_str().to_string();
        let summary = plan.summary.clone();
        move || github::create_review(gh.as_ref(), &pr, &head, &event, &summary, &comments)
    })
    .await;
    let review_id = match review_result {
        Ok(id) => id,
        Err(error) => {
            if error.chain().any(|cause| {
                cause
                    .to_string()
                    .starts_with("GitHub rejected request (HTTP 4")
            }) {
                lock_store(session)?.reject_publication(&outcome.attempt_id)?;
                outcome.status = "rejected".into();
                return Ok(());
            }
            return Err(error
                .context("GitHub review creation failed; do not retry without checking GitHub"));
        }
    };
    outcome.github_review_id = Some(review_id);
    // Keep the claim until each remote operation is confirmed.
    lock_store(session)?.finish_publication(
        &outcome.attempt_id,
        "partial",
        Some("review created; comments and replies pending"),
        Some(review_id),
    )?;
    let remote = if plan.inline.is_empty() {
        Vec::new()
    } else {
        blocking({
            let gh = gh.clone();
            let pr = pr.clone();
            move || github::review_comments(gh.as_ref(), &pr, review_id)
        })
        .await?
    };
    let mut assigned = std::collections::HashSet::new();
    let mut root_ids = std::collections::HashMap::new();
    for c in &plan.inline {
        let matches: Vec<_> = remote
            .iter()
            .filter(|(id, path, line, body)| {
                !assigned.contains(id)
                    && path == &c.path
                    && *line == Some(c.line)
                    && body == &c.body
            })
            .collect();
        if matches.len() != 1 {
            bail!(
                "cannot uniquely match GitHub comment {} to its local draft",
                c.comment_id
            );
        }
        let id = matches[0].0;
        lock_store(session)?.mark_prepared_published(
            &c.comment_id,
            &outcome.attempt_id,
            Some(id),
            review_id,
            &plan.head_sha,
        )?;
        assigned.insert(id);
        root_ids.insert(c.comment_id.clone(), id);
        outcome.published_comment_ids.push(c.comment_id.clone());
        outcome.remaining_comment_ids.retain(|x| x != &c.comment_id);
    }
    for c in &plan.replies {
        let parent = c
            .github_parent_id
            .or_else(|| root_ids.get(&c.parent_comment_id).copied())
            .context("reply parent is not confirmed on GitHub")?;
        if c.github_parent_id.is_some() {
            let fresh = blocking({
                let gh = gh.clone();
                let pr = pr.clone();
                move || github::fetch_snapshot(gh.as_ref(), &pr, &diff::Limits::default())
            })
            .await?;
            let thread = lock_store(session)?
                .list_threads(&session.pr_id)?
                .into_iter()
                .find(|t| t.id == c.thread_id)
                .context("reply thread disappeared")?;
            if !fresh.complete
                || fresh.pr.base_sha != plan.base_sha
                || fresh.pr.head_sha != plan.head_sha
                || !fresh.github_threads.iter().any(|remote| {
                    !remote.rest_only
                        && remote.path == thread.path
                        && thread.github_thread_id.as_deref().map_or_else(
                            || {
                                thread
                                    .published_comments
                                    .iter()
                                    .any(|p| p.github_comment_id == Some(parent))
                            },
                            |id| remote.node_id == id,
                        )
                        && !remote.is_outdated
                        && !remote.is_resolved
                        && remote
                            .comments
                            .iter()
                            .any(|comment| comment.database_id == Some(parent))
                })
            {
                bail!("reply parent is missing or outdated; reconcile this attempt");
            }
        }
        let refs = blocking({
            let gh = gh.clone();
            let pr = pr.clone();
            move || github::fetch_revision(gh.as_ref(), &pr)
        })
        .await?;
        if refs.0 != plan.base_sha || refs.1 != plan.head_sha {
            bail!("PR revision changed before reply; reconcile this attempt");
        }
        let id = blocking({
            let gh = gh.clone();
            let pr = pr.clone();
            let body = c.body.clone();
            move || github::reply_to_comment(gh.as_ref(), &pr, parent, &body)
        })
        .await
        .with_context(|| {
            format!(
                "reply {} has unknown remote outcome; do not retry blindly",
                c.comment_id
            )
        })?;
        lock_store(session)?.mark_prepared_published(
            &c.comment_id,
            &outcome.attempt_id,
            Some(id),
            review_id,
            &plan.head_sha,
        )?;
        outcome.published_comment_ids.push(c.comment_id.clone());
        outcome.remaining_comment_ids.retain(|x| x != &c.comment_id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        github::{PrRef, fake::FakeGh},
        model::{Actor, Comment, PublishState, PullRequest, ReviewCommentDraft, SCHEMA_VERSION},
        store::{NewThread, Store},
    };

    fn snapshot() -> PrSnapshot {
        PrSnapshot {
            schema_version: SCHEMA_VERSION,
            pr: PullRequest {
                owner: "o".into(),
                repo: "r".into(),
                number: 1,
                title: "t".into(),
                body: String::new(),
                author: "a".into(),
                url: "u".into(),
                state: "OPEN".into(),
                is_draft: false,
                base_ref: "main".into(),
                base_sha: "base".into(),
                head_ref: "feature".into(),
                head_sha: "head".into(),
            },
            provider_repository_id: None,
            files: vec![],
            github_threads: vec![],
            github_threads_complete: false,
            github_threads_completeness: crate::model::Completeness::Complete,
            github_threads_problems: Vec::new(),
            reviews: vec![],
            reviews_completeness: crate::model::Completeness::Complete,
            reviews_problems: Vec::new(),
            issue_comments: vec![],
            issue_comments_completeness: crate::model::Completeness::Complete,
            issue_comments_problems: Vec::new(),
            checks: Default::default(),
            warnings: vec![],
            complete: true,
            fetched_at: String::new(),
        }
    }

    fn script_preflight(fake: &FakeGh, base: &str, diff_text: &str, files: &str) {
        let metadata = serde_json::json!({
            "number": 1, "title": "t", "url": "u", "author": {"login": "a"},
            "baseRefName": "main", "baseRefOid": base,
            "headRefName": "feature", "headRefOid": "head", "changedFiles": if files == "[[]]" { 0 } else { 1 }
        }).to_string();
        let refs = serde_json::json!({"baseRefOid": base, "headRefOid": "head"}).to_string();
        fake.on(
            &[
                "pr",
                "view",
                "https://github.com/o/r/pull/1",
                "--json",
                "baseRefOid,headRefOid",
            ],
            Ok(&refs),
        );
        fake.on(&["pr", "view"], Ok(&metadata));
        fake.on(&["repo", "view"], Ok(r#"{"id":"repo"}"#));
        fake.on(
            &[
                "api",
                "--paginate",
                "--slurp",
                "repos/o/r/pulls/1/reviews?per_page=100",
            ],
            Ok("[[]]"),
        );
        fake.on(&["api", "user", "--jq", ".login"], Ok("reviewer"));
        fake.on(&["pr", "diff"], Ok(diff_text));
        fake.on(
            &[
                "api",
                "--paginate",
                "--slurp",
                "repos/o/r/pulls/1/files?per_page=100",
            ],
            Ok(files),
        );
        fake.on(&["api", "graphql"], Ok(r#"{"data":{"repository":{"pullRequest":{"reviewThreads":{"pageInfo":{"hasNextPage":false},"nodes":[]}}}}}"#));
    }

    #[test]
    fn plan_id_binds_head_and_text() {
        let a = build_plan(&snapshot(), Verdict::Approve, "looks good", &[], 1).unwrap();
        assert_eq!(
            a.plan_id,
            build_plan(&snapshot(), Verdict::Approve, "looks good", &[], 1)
                .unwrap()
                .plan_id
        );
        assert_ne!(
            a.plan_id,
            build_plan(&snapshot(), Verdict::Approve, "looks good", &[], 2)
                .unwrap()
                .plan_id
        );
        let mut changed = snapshot();
        changed.pr.head_sha = "later".into();
        assert_ne!(
            a.plan_id,
            build_plan(&changed, Verdict::Approve, "looks good", &[], 1)
                .unwrap()
                .plan_id
        );
        changed.pr.head_sha = "head".into();
        changed.pr.base_sha = "later-base".into();
        assert_ne!(
            a.plan_id,
            build_plan(&changed, Verdict::Approve, "looks good", &[], 1)
                .unwrap()
                .plan_id
        );
        assert_ne!(
            a.plan_id,
            build_plan(&snapshot(), Verdict::Approve, "different", &[], 1)
                .unwrap()
                .plan_id
        );
    }

    #[tokio::test]
    async fn fake_publication_marks_each_comment_and_blocks_duplicate() {
        let mut snapshot = snapshot();
        snapshot.files = diff::parse_unified(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
            "head",
            &diff::Limits::default(),
        )
        .files;
        let fake = Arc::new(FakeGh::default());
        script_preflight(
            &fake,
            "base",
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
            r#"[[{"filename":"a.rs","status":"modified","patch":null,"additions":1,"deletions":1}]]"#,
        );
        fake.on(
            &["api", "--method", "POST", "repos/o/r/pulls/1/reviews"],
            Ok(r#"{"id":7}"#),
        );
        fake.on(
            &[
                "api",
                "--paginate",
                "--slurp",
                "repos/o/r/pulls/1/reviews/7/comments?per_page=100",
            ],
            Ok(r#"[[{"id":70,"path":"a.rs","line":1,"body":"fix this"}]]"#),
        );
        fake.on(
            &[
                "api",
                "--method",
                "POST",
                "repos/o/r/pulls/1/comments/70/replies",
            ],
            Ok(r#"{"id":71}"#),
        );
        let session = Session::new(
            snapshot,
            Store::open_in_memory().unwrap(),
            Some(fake.clone()),
            Some(PrRef {
                owner: "o".into(),
                repo: "r".into(),
                number: 1,
            }),
        )
        .unwrap();
        {
            let mut store = session.store.lock().unwrap();
            store
                .insert_thread(
                    &session.pr_id,
                    &NewThread {
                        id: "t".into(),
                        source: ThreadSource::Local,
                        github_thread_id: None,
                        path: "a.rs".into(),
                        side: Side::Right,
                        line: Some(1),
                        start_line: None,
                        line_text: Some("new".into()),
                        diff_digest: None,
                        head_sha: "head".into(),
                        anchor_state: AnchorState::Current,
                        anchor_reason: None,
                        resolved: false,
                        created_at: None,
                        subject: ThreadSubject::Line,
                        finding_id: None,
                    },
                )
                .unwrap();
            for (id, body) in [("c1", "private discussion"), ("c2", "agent context")] {
                store
                    .insert_comment(
                        &Comment {
                            id: id.into(),
                            thread_id: "t".into(),
                            author_kind: Actor::Human,
                            author: "me".into(),
                            body: body.into(),
                            created_at: id.into(),
                            updated_at: id.into(),
                            publish_state: PublishState::Draft,
                            github_comment_id: None,
                        },
                        None,
                    )
                    .unwrap();
            }
            store
                .upsert_review_comment(&session.pr_id, "t", "fix this", Some("c1"))
                .unwrap();
            store
                .finish_review(&session.pr_id, Verdict::Comment, "review body")
                .unwrap();
        }
        let plan = preview(&session).unwrap();
        assert_eq!(plan.inline.len(), 1);
        assert_eq!(plan.replies.len(), 0);
        assert_eq!(plan.inline[0].body, "fix this");
        let result = publish(&session, &plan.plan_id, true).await.unwrap();
        assert_eq!(result.status, "succeeded");
        assert_eq!(
            result.published_comment_ids,
            [plan.inline[0].comment_id.clone()]
        );
        let stored = session
            .store
            .lock()
            .unwrap()
            .list_threads(&session.pr_id)
            .unwrap();
        assert!(
            stored[0]
                .comments
                .iter()
                .all(|c| c.github_comment_id.is_none())
        );
        assert_eq!(stored[0].published_comments[0].github_comment_id, Some(70));
        let writes = fake.calls_matching(&["api", "--method", "POST"]);
        assert_eq!(writes.len(), 1);
        let payload = writes[0].1.as_ref().unwrap();
        assert!(!payload.contains("private discussion"));
        assert!(!payload.contains("agent context"));
        assert!(publish(&session, &plan.plan_id, true).await.is_err());
        assert_eq!(fake.calls_matching(&["api", "--method", "POST"]).len(), 1);
    }

    #[tokio::test]
    async fn unknown_remote_outcome_is_durable_and_never_retried() {
        let fake = Arc::new(FakeGh::default());
        script_preflight(&fake, "base", "", "[[]]");
        fake.on(
            &["api", "--method", "POST"],
            Err("connection lost after submit"),
        );
        let session = Session::new(
            snapshot(),
            Store::open_in_memory().unwrap(),
            Some(fake.clone()),
            Some(PrRef {
                owner: "o".into(),
                repo: "r".into(),
                number: 1,
            }),
        )
        .unwrap();
        session
            .store
            .lock()
            .unwrap()
            .finish_review(&session.pr_id, Verdict::Approve, "okay")
            .unwrap();
        let plan = preview(&session).unwrap();
        let first = publish(&session, &plan.plan_id, true).await.unwrap();
        assert_eq!(first.status, "partial");
        assert!(first.error.unwrap().contains("do not retry"));
        assert!(publish(&session, &plan.plan_id, true).await.is_err());
        session
            .store
            .lock()
            .unwrap()
            .finish_review(&session.pr_id, Verdict::Approve, "edited summary")
            .unwrap();
        let changed = preview(&session).unwrap();
        assert_ne!(plan.plan_id, changed.plan_id);
        assert!(publish(&session, &changed.plan_id, true).await.is_err());
        assert_eq!(fake.calls_matching(&["api", "--method", "POST"]).len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reconcile_waits_for_in_flight_post_and_preserves_reservations() {
        use crate::github::GhRunner;
        use axum::{
            body::Body,
            http::{Request, StatusCode},
        };
        use std::sync::{Mutex, mpsc};
        use tower::ServiceExt;

        struct BlockedGh {
            fake: Arc<FakeGh>,
            entered: Mutex<Option<mpsc::Sender<()>>>,
            release: Mutex<mpsc::Receiver<()>>,
        }
        impl GhRunner for BlockedGh {
            fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>> {
                if args.starts_with(&["api", "--method", "POST"])
                    && let Some(sender) = self.entered.lock().unwrap().take()
                {
                    sender.send(()).unwrap();
                    self.release.lock().unwrap().recv().unwrap();
                }
                self.fake.run(args, stdin)
            }
        }

        let mut snap = snapshot();
        snap.files = diff::parse_unified(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
            "head",
            &diff::Limits::default(),
        )
        .files;
        for succeeds in [false, true] {
            let fake = Arc::new(FakeGh::default());
            script_preflight(
                &fake,
                "base",
                "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
                r#"[[{"filename":"a.rs","status":"modified","patch":null,"additions":1,"deletions":1}]]"#,
            );
            if succeeds {
                fake.on(&["api", "--method", "POST"], Ok(r#"{"id":7}"#));
                fake.on(
                    &[
                        "api",
                        "--paginate",
                        "--slurp",
                        "repos/o/r/pulls/1/reviews/7/comments?per_page=100",
                    ],
                    Ok(r#"[[{"id":70,"path":"a.rs","line":1,"body":"fix this"}]]"#),
                );
            } else {
                fake.on(
                    &["api", "--method", "POST"],
                    Err("connection lost after submit"),
                );
            }
            let (entered_tx, entered_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let gh = Arc::new(BlockedGh {
                fake: fake.clone(),
                entered: Mutex::new(Some(entered_tx)),
                release: Mutex::new(release_rx),
            });
            let session = Session::new(
                snap.clone(),
                Store::open_in_memory().unwrap(),
                Some(gh),
                Some(PrRef {
                    owner: "o".into(),
                    repo: "r".into(),
                    number: 1,
                }),
            )
            .unwrap();
            {
                let mut store = session.store.lock().unwrap();
                store
                    .insert_thread(
                        &session.pr_id,
                        &NewThread {
                            id: "t".into(),
                            source: ThreadSource::Local,
                            github_thread_id: None,
                            path: "a.rs".into(),
                            side: Side::Right,
                            line: Some(1),
                            start_line: None,
                            line_text: Some("new".into()),
                            diff_digest: None,
                            head_sha: "head".into(),
                            anchor_state: AnchorState::Current,
                            anchor_reason: None,
                            resolved: false,
                            created_at: None,
                            subject: ThreadSubject::Line,
                            finding_id: None,
                        },
                    )
                    .unwrap();
                store
                    .upsert_review_comment(&session.pr_id, "t", "fix this", None)
                    .unwrap();
                store
                    .finish_review(&session.pr_id, Verdict::Comment, "summary")
                    .unwrap();
            }
            let plan = preview(&session).unwrap();
            let draft_id = plan.inline[0].comment_id.clone();
            let publishing = tokio::spawn({
                let session = session.clone();
                let plan_id = plan.plan_id.clone();
                async move {
                    publish_requested(&session, &plan_id, Verdict::Comment, "summary", true)
                        .await
                        .unwrap()
                }
            });
            tokio::task::spawn_blocking(move || entered_rx.recv())
                .await
                .unwrap()
                .unwrap();
            let attempt = session
                .store
                .lock()
                .unwrap()
                .active_publication(&session.pr_id)
                .unwrap()
                .unwrap();
            assert_eq!(attempt.1, vec![draft_id.clone()]);
            let app = crate::server::router(crate::server::AppState::new(
                session.clone(),
                "ui-secret".into(),
                "agent-secret".into(),
            ));
            let request = |id: &str| {
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/review/publish/reconcile")
                    .header("host", "127.0.0.1:1234")
                    .header("origin", "http://127.0.0.1:1234")
                    .header("cookie", "aside_ui=ui-secret")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"attemptId":id,"outcome":"not_posted","confirm":true})
                            .to_string(),
                    ))
                    .unwrap()
            };
            let reconciling = tokio::spawn(app.clone().oneshot(request(&attempt.0)));
            // The handler would complete immediately without the publication mutex.
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(100), async {
                    while !reconciling.is_finished() {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .is_err()
            );
            assert_eq!(
                session
                    .store
                    .lock()
                    .unwrap()
                    .active_publication(&session.pr_id)
                    .unwrap()
                    .unwrap()
                    .1,
                vec![draft_id.clone()]
            );
            assert_eq!(fake.calls_matching(&["api", "--method", "POST"]).len(), 0);
            release_tx.send(()).unwrap();
            assert_eq!(
                publishing.await.unwrap().status,
                if succeeds { "succeeded" } else { "partial" }
            );
            assert_eq!(
                reconciling.await.unwrap().unwrap().status(),
                if succeeds {
                    StatusCode::INTERNAL_SERVER_ERROR
                } else {
                    StatusCode::OK
                }
            );
            assert!(
                session
                    .store
                    .lock()
                    .unwrap()
                    .active_publication(&session.pr_id)
                    .unwrap()
                    .is_none()
            );
            assert_eq!(fake.calls_matching(&["api", "--method", "POST"]).len(), 1);
            assert_ne!(
                app.oneshot(request(&attempt.0)).await.unwrap().status(),
                StatusCode::OK
            );
            let thread = session
                .store
                .lock()
                .unwrap()
                .list_threads(&session.pr_id)
                .unwrap()
                .remove(0);
            if succeeds {
                assert_eq!(thread.published_comments[0].id, draft_id);
            } else {
                assert_eq!(thread.review_comment.unwrap().id, draft_id);
            }
        }
    }

    #[tokio::test]
    async fn edit_during_final_preflight_cannot_reserve_different_text() {
        use std::sync::Mutex;
        struct EditingGh {
            fake: Arc<FakeGh>,
            edit: Mutex<Option<Box<dyn FnOnce() + Send>>>,
        }
        impl GhRunner for EditingGh {
            fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>> {
                let out = self.fake.run(args, stdin)?;
                if args
                    == [
                        "pr",
                        "view",
                        "https://github.com/o/r/pull/1",
                        "--json",
                        "baseRefOid,headRefOid",
                    ]
                    && let Some(edit) = self.edit.lock().unwrap().take()
                {
                    edit();
                }
                Ok(out)
            }
        }
        let mut snap = snapshot();
        snap.files = diff::parse_unified(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
            "head",
            &diff::Limits::default(),
        )
        .files;
        let fake = Arc::new(FakeGh::default());
        script_preflight(
            &fake,
            "base",
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
            r#"[[{"filename":"a.rs","status":"modified","patch":null,"additions":1,"deletions":1}]]"#,
        );
        let gh = Arc::new(EditingGh {
            fake: fake.clone(),
            edit: Mutex::new(None),
        });
        let session = Session::new(
            snap,
            Store::open_in_memory().unwrap(),
            Some(gh.clone()),
            Some(PrRef {
                owner: "o".into(),
                repo: "r".into(),
                number: 1,
            }),
        )
        .unwrap();
        {
            let mut store = session.store.lock().unwrap();
            store
                .insert_thread(
                    &session.pr_id,
                    &NewThread {
                        id: "t".into(),
                        source: ThreadSource::Local,
                        github_thread_id: None,
                        path: "a.rs".into(),
                        side: Side::Right,
                        line: Some(1),
                        start_line: None,
                        line_text: Some("new".into()),
                        diff_digest: None,
                        head_sha: "head".into(),
                        anchor_state: AnchorState::Current,
                        anchor_reason: None,
                        resolved: false,
                        created_at: None,
                        subject: ThreadSubject::Line,
                        finding_id: None,
                    },
                )
                .unwrap();
            store
                .upsert_review_comment(&session.pr_id, "t", "approved", None)
                .unwrap();
        }
        let plan = preview_requested(&session, Verdict::Comment, "summary").unwrap();
        let store = session.store.clone();
        let pr_id = session.pr_id.clone();
        *gh.edit.lock().unwrap() = Some(Box::new(move || {
            store
                .lock()
                .unwrap()
                .upsert_review_comment(&pr_id, "t", "changed after confirmation", None)
                .unwrap();
        }));
        assert!(
            publish_requested(&session, &plan.plan_id, Verdict::Comment, "summary", true)
                .await
                .unwrap_err()
                .to_string()
                .contains("plan changed")
        );
        assert!(fake.calls_matching(&["api", "--method", "POST"]).is_empty());
        let after = session
            .store
            .lock()
            .unwrap()
            .list_threads(&session.pr_id)
            .unwrap();
        assert_eq!(
            after[0].review_comment.as_ref().unwrap().body,
            "changed after confirmation"
        );
        assert!(!after[0].review_comment.as_ref().unwrap().locked);
    }

    #[tokio::test]
    async fn known_rejection_releases_claim_and_allows_new_plan() {
        let fake = Arc::new(FakeGh::default());
        script_preflight(&fake, "base", "", "[[]]");
        fake.on(
            &["api", "--method", "POST"],
            Err("GitHub rejected request (HTTP 422)"),
        );
        let session = Session::new(
            snapshot(),
            Store::open_in_memory().unwrap(),
            Some(fake),
            Some(PrRef {
                owner: "o".into(),
                repo: "r".into(),
                number: 1,
            }),
        )
        .unwrap();
        session
            .store
            .lock()
            .unwrap()
            .finish_review(&session.pr_id, Verdict::Approve, "okay")
            .unwrap();
        let plan = preview(&session).unwrap();
        let outcome = publish(&session, &plan.plan_id, true).await.unwrap();
        assert_eq!(outcome.status, "rejected");
        let next = preview(&session).unwrap();
        assert_ne!(next.plan_id, plan.plan_id);
        assert_eq!(next.attempt_ordinal, 2);
    }

    #[tokio::test]
    async fn base_only_change_fails_before_claim_or_write() {
        let fake = Arc::new(FakeGh::default());
        script_preflight(&fake, "new-base", "", "[[]]");
        let session = Session::new(
            snapshot(),
            Store::open_in_memory().unwrap(),
            Some(fake.clone()),
            Some(PrRef {
                owner: "o".into(),
                repo: "r".into(),
                number: 1,
            }),
        )
        .unwrap();
        session
            .store
            .lock()
            .unwrap()
            .finish_review(&session.pr_id, Verdict::Approve, "okay")
            .unwrap();
        let plan = preview(&session).unwrap();
        assert!(
            publish(&session, &plan.plan_id, true)
                .await
                .unwrap_err()
                .to_string()
                .contains("base")
        );
        assert!(fake.calls_matching(&["api", "--method", "POST"]).is_empty());
        // No claim was made: a corrected remote revision can use the same plan.
        let corrected = Arc::new(FakeGh::default());
        script_preflight(&corrected, "base", "", "[[]]");
        corrected.on(&["api", "--method", "POST"], Err("unknown result"));
        let retry = Session::new(
            snapshot(),
            Store::open_in_memory().unwrap(),
            Some(corrected),
            Some(PrRef {
                owner: "o".into(),
                repo: "r".into(),
                number: 1,
            }),
        )
        .unwrap();
        retry
            .store
            .lock()
            .unwrap()
            .finish_review(&retry.pr_id, Verdict::Approve, "okay")
            .unwrap();
        assert_eq!(preview(&retry).unwrap().plan_id, plan.plan_id);
        assert_eq!(
            publish(&retry, &plan.plan_id, true).await.unwrap().status,
            "partial"
        );
    }

    #[tokio::test]
    async fn deleted_anchor_fails_before_claim_and_keeps_draft() {
        let mut local = snapshot();
        local.files = diff::parse_unified(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
            "head",
            &diff::Limits::default(),
        )
        .files;
        let fake = Arc::new(FakeGh::default());
        script_preflight(
            &fake,
            "base",
            "diff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-old\n+new\n",
            r#"[[{"filename":"b.rs","status":"modified","patch":null,"additions":1,"deletions":1}]]"#,
        );
        let session = Session::new(
            local,
            Store::open_in_memory().unwrap(),
            Some(fake.clone()),
            Some(PrRef {
                owner: "o".into(),
                repo: "r".into(),
                number: 1,
            }),
        )
        .unwrap();
        {
            let mut store = session.store.lock().unwrap();
            store
                .insert_thread(
                    &session.pr_id,
                    &NewThread {
                        id: "t".into(),
                        source: ThreadSource::Local,
                        github_thread_id: None,
                        path: "a.rs".into(),
                        side: Side::Right,
                        line: Some(1),
                        start_line: None,
                        line_text: Some("new".into()),
                        diff_digest: None,
                        head_sha: "head".into(),
                        anchor_state: AnchorState::Current,
                        anchor_reason: None,
                        resolved: false,
                        created_at: None,
                        subject: ThreadSubject::Line,
                        finding_id: None,
                    },
                )
                .unwrap();
            store
                .insert_comment(
                    &Comment {
                        id: "c".into(),
                        thread_id: "t".into(),
                        author_kind: Actor::Human,
                        author: "me".into(),
                        body: "fix".into(),
                        created_at: "now".into(),
                        updated_at: "now".into(),
                        publish_state: PublishState::Draft,
                        github_comment_id: None,
                    },
                    None,
                )
                .unwrap();
            store
                .upsert_review_comment(&session.pr_id, "t", "fix", Some("c"))
                .unwrap();
            store
                .finish_review(&session.pr_id, Verdict::Comment, "body")
                .unwrap();
        }
        let plan = preview(&session).unwrap();
        assert_eq!(plan.inline.len(), 1);
        assert!(
            publish(&session, &plan.plan_id, true)
                .await
                .unwrap_err()
                .to_string()
                .contains("anchor")
        );
        assert!(fake.calls_matching(&["api", "--method", "POST"]).is_empty());
        assert_eq!(
            session
                .store
                .lock()
                .unwrap()
                .list_threads(&session.pr_id)
                .unwrap()[0]
                .review_comment
                .as_ref()
                .unwrap()
                .body,
            "fix"
        );
        // A preflight rejection must not reserve the draft or claim the PR.
        let store = session.store.lock().unwrap();
        assert!(
            !store.list_threads(&session.pr_id).unwrap()[0]
                .review_comment
                .as_ref()
                .unwrap()
                .locked
        );
    }

    #[test]
    fn stale_draft_is_omitted_without_moving_its_anchor() {
        let t = ReviewThread {
            id: "t".into(),
            source: ThreadSource::Local,
            github_thread_id: None,
            path: "lost.rs".into(),
            file_id: None,
            side: Side::Right,
            line: Some(4),
            start_line: None,
            line_text: Some("x".into()),
            anchor_state: AnchorState::Current,
            anchor_reason: None,
            resolved: false,
            head_sha: "head".into(),
            created_at: String::new(),
            comments: vec![],
            subject: ThreadSubject::Line,
            finding_id: None,
            review_comment: Some(ReviewCommentDraft {
                id: "c".into(),
                body: "fix".into(),
                source_comment_id: None,
                updated_at: String::new(),
                locked: false,
                target: None,
                blocker: None,
            }),
            published_comments: vec![],
            linked_thread_id: None,
            agent: Default::default(),
            unread_count: 0,
            last_participant: None,
        };
        let plan = build_plan(&snapshot(), Verdict::Approve, "", &[t], 1).unwrap();
        assert!(plan.inline.is_empty());
        assert_eq!(plan.omitted[0].comment_id, "c");
    }

    #[test]
    fn session_json_exposes_verified_file_reply_and_blocked_targets() {
        let mut snap = snapshot();
        snap.files = diff::parse_unified(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +0,0 @@\n-old\n",
            "head",
            &diff::Limits::default(),
        )
        .files;
        let session = Session::new(snap, Store::open_in_memory().unwrap(), None, None).unwrap();
        {
            let mut store = session.store.lock().unwrap();
            for (id, source, path, subject) in [
                ("file", ThreadSource::Local, "a.rs", ThreadSubject::File),
                (
                    "missing",
                    ThreadSource::Local,
                    "lost.rs",
                    ThreadSubject::File,
                ),
                ("remote", ThreadSource::Github, "a.rs", ThreadSubject::Line),
            ] {
                store
                    .insert_thread(
                        &session.pr_id,
                        &NewThread {
                            id: id.into(),
                            source,
                            github_thread_id: if source == ThreadSource::Github {
                                Some("native-thread".into())
                            } else {
                                None
                            },
                            path: path.into(),
                            side: Side::Left,
                            line: if source == ThreadSource::Github {
                                Some(1)
                            } else {
                                None
                            },
                            start_line: None,
                            line_text: None,
                            diff_digest: None,
                            head_sha: "head".into(),
                            anchor_state: AnchorState::Current,
                            anchor_reason: None,
                            resolved: false,
                            created_at: None,
                            subject,
                            finding_id: None,
                        },
                    )
                    .unwrap();
                if id == "remote" {
                    store
                        .insert_comment(
                            &Comment {
                                id: "native-root".into(),
                                thread_id: id.into(),
                                author_kind: Actor::Github,
                                author: "reviewer".into(),
                                body: "existing".into(),
                                created_at: "now".into(),
                                updated_at: "now".into(),
                                publish_state: PublishState::Imported,
                                github_comment_id: Some(42),
                            },
                            None,
                        )
                        .unwrap();
                }
                store
                    .upsert_review_comment(&session.pr_id, id, "prepared", None)
                    .unwrap();
            }
        }
        let view = serde_json::to_value(session.view(Actor::Human).unwrap()).unwrap();
        let threads = view["threads"].as_array().unwrap();
        let get = |id: &str| &threads.iter().find(|t| t["id"] == id).unwrap()["reviewComment"];
        assert_eq!(get("file")["target"]["kind"], "file_fallback");
        assert_eq!(get("file")["target"]["side"], "LEFT");
        assert_eq!(get("file")["target"]["line"], 1);
        assert_eq!(get("remote")["target"]["kind"], "reply");
        assert!(get("missing")["target"].is_null());
        assert!(
            get("missing")["blocker"]
                .as_str()
                .unwrap()
                .contains("no commentable")
        );
        session.snapshot.write().unwrap().files[0].is_binary = true;
        let binary = serde_json::to_value(session.view(Actor::Human).unwrap()).unwrap();
        let file = binary["threads"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == "file")
            .unwrap();
        assert!(file["reviewComment"]["target"].is_null());
        assert!(
            file["reviewComment"]["blocker"]
                .as_str()
                .unwrap()
                .contains("binary")
        );
    }

    #[test]
    fn file_draft_requires_commentable_patch_and_prefixes_exactly_once() {
        let mut snap = snapshot();
        let mut thread = ReviewThread {
            id: "file".into(),
            source: ThreadSource::Local,
            github_thread_id: None,
            path: "a.rs".into(),
            file_id: None,
            side: Side::Right,
            line: None,
            start_line: None,
            line_text: None,
            anchor_state: AnchorState::Current,
            anchor_reason: None,
            resolved: true,
            head_sha: "head".into(),
            created_at: String::new(),
            comments: vec![],
            subject: ThreadSubject::File,
            finding_id: None,
            review_comment: Some(ReviewCommentDraft {
                id: "d".into(),
                body: "message".into(),
                source_comment_id: None,
                updated_at: String::new(),
                locked: false,
                target: None,
                blocker: None,
            }),
            published_comments: vec![],
            linked_thread_id: None,
            agent: Default::default(),
            unread_count: 0,
            last_participant: None,
        };
        let blocked = build_plan(&snap, Verdict::Comment, "summary", &[thread.clone()], 1).unwrap();
        assert_eq!(blocked.omitted.len(), 1);
        assert!(blocked.inline.is_empty());
        snap.files = diff::parse_unified(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
            "head",
            &diff::Limits::default(),
        )
        .files;
        let plan = build_plan(&snap, Verdict::Comment, "summary", &[thread.clone()], 1).unwrap();
        assert_eq!(plan.inline[0].body, "File Comment:\nmessage");
        assert_eq!(plan.inline[0].line, 1);
        assert_eq!(
            plan.inline[0].side,
            Side::Left,
            "replacement starts with a deletion"
        );
        snap.files = diff::parse_unified(
            "diff --git a/a.rs b/a.rs\ndeleted file mode 100644\n--- a/a.rs\n+++ /dev/null\n@@ -3,2 +0,0 @@\n-old\n-older\n",
            "head", &diff::Limits::default()).files;
        let deletion =
            build_plan(&snap, Verdict::Comment, "summary", &[thread.clone()], 1).unwrap();
        assert_eq!(deletion.inline.len(), 1);
        assert_eq!(deletion.inline[0].body, "File Comment:\nmessage");
        assert_eq!(
            (deletion.inline[0].side, deletion.inline[0].line),
            (Side::Left, 3)
        );
        assert_eq!(
            (plan.open_count, plan.resolved_count, plan.prepared_count),
            (0, 1, 1)
        );
        thread.review_comment.as_mut().unwrap().locked = true;
        assert!(
            build_plan(&snap, Verdict::Comment, "summary", &[thread], 1)
                .unwrap()
                .inline
                .is_empty()
        );
    }
}
