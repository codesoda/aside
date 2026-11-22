//! Private-first publication plans and guarded GitHub review submission.
//!
//! An attempt is persisted as `partial` before its first remote operation. A
//! timeout may mean GitHub accepted a request, so the same plan is never sent
//! again automatically. A human must reconcile uncertain attempts on GitHub.

use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    diff,
    github::{self, GhRunner, NewReviewComment, PrRef},
    model::{
        Actor, AnchorState, PrSnapshot, PublishState, ReviewThread, Side, ThreadSource, Verdict,
    },
    session::Session,
    util::sha256_hex,
};

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PlannedInline {
    pub comment_id: String,
    pub thread_id: String,
    pub path: String,
    pub side: Side,
    pub line: u32,
    pub start_line: Option<u32>,
    pub body: String,
    pub author_kind: Actor,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PlannedReply {
    pub comment_id: String,
    pub thread_id: String,
    // A published or imported GitHub comment, or the new inline comment in this plan.
    pub parent_comment_id: String,
    pub github_parent_id: Option<i64>,
    pub path: String,
    pub side: Side,
    pub line: Option<u32>,
    pub author_kind: Actor,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct OmittedDraft {
    pub comment_id: String,
    pub thread_id: String,
    pub reason: String,
    pub kind: String,
    pub selected: bool,
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PublishPlan {
    pub plan_id: String,
    #[ts(type = "number")]
    pub selection_revision: u64,
    pub head_sha: String,
    pub base_sha: String,
    pub verdict: Verdict,
    pub summary: String,
    pub inline: Vec<PlannedInline>,
    pub replies: Vec<PlannedReply>,
    pub omitted: Vec<OmittedDraft>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PublishOutcome {
    pub attempt_id: String,
    pub status: String,
    pub github_review_id: Option<i64>,
    pub published_comment_ids: Vec<String>,
    pub remaining_comment_ids: Vec<String>,
    pub omitted_comment_ids: Vec<String>,
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
        .find(|f| f.path == t.path && f.patch_complete)
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

fn build_plan(
    snapshot: &PrSnapshot,
    verdict: Verdict,
    summary: &str,
    threads: &[ReviewThread],
    selection_revision: u64,
    reserved: &[String],
) -> Result<PublishPlan> {
    let mut plan = PublishPlan {
        plan_id: String::new(),
        selection_revision,
        head_sha: snapshot.pr.head_sha.clone(),
        base_sha: snapshot.pr.base_sha.clone(),
        verdict,
        summary: summary.to_owned(),
        inline: Vec::new(),
        replies: Vec::new(),
        omitted: Vec::new(),
    };
    for thread in threads {
        let root = thread.comments.first();
        let mut parent = root
            .filter(|c| c.github_comment_id.is_some())
            .map(|c| (c.id.clone(), c.github_comment_id));
        // Only a valid local root can make deselection the sole parent blocker.
        let parent_deselected = thread.source == ThreadSource::Local
            && root.is_some_and(|c| {
                c.publish_state == PublishState::Draft
                    && !c.publish_selected
                    && c.author_kind != Actor::Github
                    && !c.body.trim().is_empty()
                    && !reserved.contains(&c.id)
            })
            && current_anchor(thread, snapshot);
        for (index, comment) in thread.comments.iter().enumerate() {
            if comment.publish_state != PublishState::Draft {
                continue;
            }
            let reason = if comment.author_kind == Actor::Github || comment.body.trim().is_empty() {
                Some("not_local_draft")
            } else if reserved.contains(&comment.id) {
                Some("reserved")
            } else if index == 0
                && thread.source == ThreadSource::Local
                && !current_anchor(thread, snapshot)
            {
                Some("stale_anchor")
            } else if parent.is_none()
                && !(index == 0 && thread.source == ThreadSource::Local)
                && !parent_deselected
            {
                Some("parent_unpublishable")
            } else if !comment.publish_selected {
                Some("not_selected")
            } else if index != 0 && parent_deselected {
                Some("parent_not_selected")
            } else {
                None
            };
            if let Some(reason) = reason {
                plan.omitted.push(OmittedDraft {
                    comment_id: comment.id.clone(),
                    thread_id: thread.id.clone(),
                    reason: reason.into(),
                    code: reason.into(),
                    kind: if index == 0 && thread.source == ThreadSource::Local {
                        "inline"
                    } else {
                        "reply"
                    }
                    .into(),
                    selected: comment.publish_selected,
                });
                continue;
            }
            if index == 0 && thread.source == ThreadSource::Local {
                plan.inline.push(PlannedInline {
                    comment_id: comment.id.clone(),
                    thread_id: thread.id.clone(),
                    path: thread.path.clone(),
                    side: thread.side,
                    line: thread.line.context("current anchor has no line")?,
                    start_line: thread.start_line,
                    body: comment.body.clone(),
                    author_kind: comment.author_kind,
                });
                parent = Some((comment.id.clone(), None));
            } else if let Some((parent_comment_id, github_parent_id)) = &parent {
                plan.replies.push(PlannedReply {
                    comment_id: comment.id.clone(),
                    thread_id: thread.id.clone(),
                    parent_comment_id: parent_comment_id.clone(),
                    github_parent_id: *github_parent_id,
                    path: thread.path.clone(),
                    side: thread.side,
                    line: thread.line,
                    author_kind: comment.author_kind,
                    body: comment.body.clone(),
                });
            }
        }
    }
    plan.plan_id = sha256_hex(&serde_json::to_vec(&plan)?);
    Ok(plan)
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PublishBlocker {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github_review_id: Option<i64>,
}

pub fn blockers(session: &Session, plan: &PublishPlan) -> Result<Vec<PublishBlocker>> {
    let store = lock_store(session)?;
    let mut result = Vec::new();
    if plan.verdict != Verdict::Approve && plan.summary.trim().is_empty() && plan.inline.is_empty()
    {
        result.push(PublishBlocker {
            code: "empty_review".into(),
            message: "A summary or inline comment is required".into(),
            attempt_id: None,
            github_review_id: None,
        });
    }
    if let Some(attempt) = store.unresolved_publication(&session.pr_id)? {
        result.push(PublishBlocker {
            code: "unresolved_attempt".into(),
            message: "Check the previous attempt on GitHub; Aside will not retry it".into(),
            attempt_id: Some(attempt.attempt_id),
            github_review_id: attempt.github_review_id,
        });
    }
    if store.plan_attempted(&session.pr_id, &plan.plan_id)? {
        result.push(PublishBlocker {
            code: "plan_already_attempted".into(),
            message: "This plan was already attempted".into(),
            attempt_id: None,
            github_review_id: None,
        });
    }
    Ok(result)
}

/// Build the exact outbound review and reply operations; invalid drafts are omitted with reasons.
/// In v1 all eligible local drafts are selected; the API has no per-comment selection state.
/// Callers must display `omitted` as well as the selected operations before confirmation.
pub fn preview(session: &Session) -> Result<PublishPlan> {
    let snapshot = session
        .snapshot
        .read()
        .map_err(|_| anyhow!("snapshot lock poisoned"))?
        .clone();
    if !snapshot.complete {
        bail!("snapshot is incomplete; refresh before publication");
    }
    let store = lock_store(session)?;
    let review = store.review_state(&session.pr_id)?;
    if review.lifecycle != "finished"
        && !(review.lifecycle == "published" && review.unresolved_publication.is_some())
    {
        bail!("finish the local review before publishing");
    }
    let verdict = review.verdict.context("review has no verdict")?;
    let (threads, revision, reserved) = store.plan_inputs(&session.pr_id)?;
    build_plan(
        &snapshot,
        verdict,
        &review.summary,
        &threads,
        revision,
        &reserved,
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

/// Publish a confirmed plan. A partial/uncertain attempt is never blindly retried.
/// The HTTP layer must authorize a human actor before calling this function.
pub async fn publish(session: &Session, plan_id: &str, confirm: bool) -> Result<PublishOutcome> {
    if !confirm {
        bail!("explicit human confirmation is required");
    }
    if session.demo {
        bail!("demo sessions cannot publish to GitHub");
    }
    // Serialize active publications even when callers supply different plan IDs.
    static PUBLICATION_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    let _guard = PUBLICATION_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let (gh, pr) = github_context(session)?;
    let snapshot = session
        .snapshot
        .read()
        .map_err(|_| anyhow!("snapshot lock poisoned"))?
        .clone();
    if !snapshot.complete {
        bail!("snapshot is incomplete; refresh before publication");
    }
    let plan = preview(session)?;
    if plan.plan_id != plan_id {
        bail!("publication plan changed; preview again");
    }
    if !blockers(session, &plan)?.is_empty() {
        bail!("publication is blocked");
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
    let threads = lock_store(session)?.list_threads(&session.pr_id)?;
    for inline in &plan.inline {
        let thread = threads
            .iter()
            .find(|t| t.id == inline.thread_id)
            .context("planned inline thread disappeared; preview again")?;
        if thread.path != inline.path
            || thread.side != inline.side
            || thread.line != Some(inline.line)
            || thread.start_line != inline.start_line
            || !current_anchor(thread, &current)
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
                (thread.github_thread_id.as_deref() == Some(remote.node_id.as_str())
                    || thread
                        .comments
                        .first()
                        .is_some_and(|c| c.github_comment_id == Some(parent_id)))
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
        let drafts: Vec<_> = plan
            .inline
            .iter()
            .map(|c| (c.comment_id.as_str(), c.body.as_str()))
            .chain(
                plan.replies
                    .iter()
                    .map(|c| (c.comment_id.as_str(), c.body.as_str())),
            )
            .collect();
        // The transactional claim and draft reservations commit before the remote call.
        store.start_publication(
            &session.pr_id,
            plan_id,
            &plan_json,
            &plan.head_sha,
            &drafts,
            plan.selection_revision,
        )?
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
        omitted_comment_ids: plan.omitted.iter().map(|c| c.comment_id.clone()).collect(),
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
    let review_id = blocking({
        let gh = gh.clone();
        let pr = pr.clone();
        let head = plan.head_sha.clone();
        let event = plan.verdict.as_str().to_string();
        let summary = plan.summary.clone();
        move || github::create_review(gh.as_ref(), &pr, &head, &event, &summary, &comments)
    })
    .await
    .context("GitHub review creation failed; do not retry without checking GitHub")?;
    outcome.github_review_id = Some(review_id);
    // A review now exists even if comment mapping or replies later fail. Keep
    // further plans from creating a second review while this one is reconciled.
    lock_store(session)?.mark_published(&session.pr_id)?;
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
        lock_store(session)?.mark_comment_published(&c.comment_id, Some(id))?;
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
                    (thread.github_thread_id.as_deref() == Some(remote.node_id.as_str())
                        || thread
                            .comments
                            .first()
                            .is_some_and(|root| root.github_comment_id == Some(parent)))
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
        lock_store(session)?.mark_comment_published(&c.comment_id, Some(id))?;
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
        model::{Comment, PullRequest, SCHEMA_VERSION},
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
        let a = build_plan(&snapshot(), Verdict::Approve, "looks good", &[], 0, &[]).unwrap();
        assert_eq!(
            a.plan_id,
            build_plan(&snapshot(), Verdict::Approve, "looks good", &[], 0, &[])
                .unwrap()
                .plan_id
        );
        let mut changed = snapshot();
        changed.pr.head_sha = "later".into();
        assert_ne!(
            a.plan_id,
            build_plan(&changed, Verdict::Approve, "looks good", &[], 0, &[])
                .unwrap()
                .plan_id
        );
        changed.pr.head_sha = "head".into();
        changed.pr.base_sha = "later-base".into();
        assert_ne!(
            a.plan_id,
            build_plan(&changed, Verdict::Approve, "looks good", &[], 0, &[])
                .unwrap()
                .plan_id
        );
        assert_ne!(
            a.plan_id,
            build_plan(&snapshot(), Verdict::Approve, "different", &[], 0, &[])
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
                    },
                )
                .unwrap();
            for (id, body) in [("c1", "fix this"), ("c2", "follow up")] {
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
                            publish_selected: true,
                        },
                        None,
                    )
                    .unwrap();
            }
            store
                .finish_review(&session.pr_id, Verdict::Comment, "review body")
                .unwrap();
        }
        let plan = preview(&session).unwrap();
        assert_eq!(plan.inline.len(), 1);
        assert_eq!(plan.replies.len(), 1);
        let result = publish(&session, &plan.plan_id, true).await.unwrap();
        assert_eq!(result.status, "succeeded");
        assert_eq!(result.published_comment_ids, ["c1", "c2"]);
        let stored = session
            .store
            .lock()
            .unwrap()
            .list_threads(&session.pr_id)
            .unwrap();
        assert_eq!(stored[0].comments[0].github_comment_id, Some(70));
        assert_eq!(stored[0].comments[1].github_comment_id, Some(71));
        assert_eq!(fake.calls_matching(&["api", "--method", "POST"]).len(), 2);
        assert!(publish(&session, &plan.plan_id, true).await.is_err());
        assert_eq!(fake.calls_matching(&["api", "--method", "POST"]).len(), 2);
    }

    #[tokio::test]
    async fn deferred_reply_uses_published_local_parent_in_later_review() {
        let mut local = snapshot();
        local.files = diff::parse_unified(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
            "head",
            &diff::Limits::default(),
        )
        .files;
        let fake = Arc::new(FakeGh::default());
        let remote = serde_json::json!({"data":{"repository":{"pullRequest":{"reviewThreads":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[{"id":"remote-t","isOutdated":false,"isResolved":false,"path":"a.rs","line":1,"originalLine":1,"startLine":null,"originalStartLine":null,"diffSide":"RIGHT","startDiffSide":null,"subjectType":"LINE","viewerCanReply":true,"viewerCanResolve":false,"viewerCanUnresolve":false,"resolvedBy":null,"comments":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[{"id":"C70","databaseId":70,"body":"root","createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-01T00:00:00Z","url":"url","outdated":false,"line":1,"originalLine":1,"commit":{"oid":"head"},"originalCommit":{"oid":"head"},"author":{"__typename":"User","login":"me","id":"U1"}}]}}]}}}}}).to_string();
        fake.on(&["api", "graphql"], Ok(&remote));
        script_preflight(
            &fake,
            "base",
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
            r#"[[{"filename":"a.rs","status":"modified","patch":null,"additions":1,"deletions":1}]]"#,
        );
        fake.on(
            &["api", "--method", "POST", "repos/o/r/pulls/1/reviews"],
            Ok(r#"{"id":8}"#),
        );
        fake.on(
            &[
                "api",
                "--paginate",
                "--slurp",
                "repos/o/r/pulls/1/reviews/8/comments?per_page=100",
            ],
            Ok(r#"[[{"id":70,"path":"a.rs","line":1,"body":"root"}]]"#),
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
                    },
                )
                .unwrap();
            for (id, body) in [("root", "root"), ("reply", "later")] {
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
                            publish_selected: true,
                        },
                        None,
                    )
                    .unwrap();
            }
            store
                .set_publish_selection(&session.pr_id, &["reply".into()], false)
                .unwrap();
            store
                .finish_review(&session.pr_id, Verdict::Comment, "first review")
                .unwrap();
        }
        let stale_plan = preview(&session).unwrap();
        session
            .store
            .lock()
            .unwrap()
            .set_publish_selection(&session.pr_id, &["reply".into()], true)
            .unwrap();
        assert!(
            publish(&session, &stale_plan.plan_id, true)
                .await
                .unwrap_err()
                .to_string()
                .contains("plan changed")
        );
        assert!(fake.calls_matching(&["api", "--method", "POST"]).is_empty());
        session
            .store
            .lock()
            .unwrap()
            .set_publish_selection(&session.pr_id, &["reply".into()], false)
            .unwrap();
        let first_plan = preview(&session).unwrap();
        assert_eq!(
            first_plan
                .inline
                .iter()
                .map(|c| c.body.as_str())
                .collect::<Vec<_>>(),
            ["root"]
        );
        assert!(first_plan.replies.is_empty());
        assert_eq!(
            first_plan
                .omitted
                .iter()
                .map(|c| c.comment_id.as_str())
                .collect::<Vec<_>>(),
            ["reply"]
        );
        let first = publish(&session, &first_plan.plan_id, true).await.unwrap();
        assert_eq!(first.status, "succeeded");
        assert_eq!(first.published_comment_ids, ["root"]);
        assert_eq!(first.omitted_comment_ids, ["reply"]);
        {
            let mut store = session.store.lock().unwrap();
            let reply = store.get_comment("reply").unwrap();
            assert_eq!(reply.publish_state, PublishState::Draft);
            assert!(!reply.publish_selected);
            store
                .set_publish_selection(&session.pr_id, &["reply".into()], true)
                .unwrap();
            store
                .finish_review(&session.pr_id, Verdict::Comment, "later review")
                .unwrap();
        }
        let plan = preview(&session).unwrap();
        assert!(plan.inline.is_empty());
        assert_eq!(plan.replies.len(), 1);
        assert_eq!(plan.replies[0].github_parent_id, Some(70));
        let result = publish(&session, &plan.plan_id, true).await.unwrap();
        assert_eq!(result.status, "succeeded");
        assert_eq!(result.published_comment_ids, ["reply"]);
        let posts = fake.calls_matching(&["api", "--method", "POST"]);
        assert_eq!(posts.len(), 3);
        let review_posts =
            fake.calls_matching(&["api", "--method", "POST", "repos/o/r/pulls/1/reviews"]);
        assert_eq!(review_posts.len(), 2);
        let first_payload: serde_json::Value =
            serde_json::from_str(review_posts[0].1.as_ref().unwrap()).unwrap();
        let second_payload: serde_json::Value =
            serde_json::from_str(review_posts[1].1.as_ref().unwrap()).unwrap();
        assert_eq!(first_payload["comments"][0]["body"], "root");
        assert_eq!(first_payload["comments"].as_array().unwrap().len(), 1);
        assert!(
            second_payload["comments"]
                .as_array()
                .is_none_or(Vec::is_empty)
        );
        assert_eq!(
            session
                .store
                .lock()
                .unwrap()
                .get_comment("reply")
                .unwrap()
                .github_comment_id,
            Some(71)
        );
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
        let blocked = preview(&session).unwrap();
        assert!(
            blockers(&session, &blocked)
                .unwrap()
                .iter()
                .any(|b| b.code == "unresolved_attempt")
        );
        assert!(
            session
                .store
                .lock()
                .unwrap()
                .review_state(&session.pr_id)
                .unwrap()
                .unresolved_publication
                .is_some()
        );
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

    #[tokio::test]
    async fn empty_review_is_previewable_but_blocked_before_network_or_claim() {
        let fake = Arc::new(FakeGh::default());
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
            .finish_review(&session.pr_id, Verdict::Comment, "")
            .unwrap();
        let plan = preview(&session).unwrap();
        assert!(plan.inline.is_empty() && plan.replies.is_empty());
        assert_eq!(blockers(&session, &plan).unwrap()[0].code, "empty_review");
        assert!(publish(&session, &plan.plan_id, true).await.is_err());
        assert!(fake.calls.lock().unwrap().is_empty());
        assert!(
            session
                .store
                .lock()
                .unwrap()
                .unresolved_publication(&session.pr_id)
                .unwrap()
                .is_none()
        );
        // An approval with no comment or summary is a valid outbound review.
        session
            .store
            .lock()
            .unwrap()
            .finish_review(&session.pr_id, Verdict::Approve, "")
            .unwrap();
        assert!(
            blockers(&session, &preview(&session).unwrap())
                .unwrap()
                .is_empty()
        );
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
                        publish_selected: true,
                    },
                    None,
                )
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
                .comments[0]
                .publish_state,
            PublishState::Draft
        );
        // A preflight rejection must not reserve the draft or claim the PR.
        let mut store = session.store.lock().unwrap();
        assert!(
            store
                .start_publication(&session.pr_id, "other", "{}", "head", &[("c", "fix")], 1)
                .is_ok()
        );
    }

    #[test]
    fn deselected_root_keeps_selected_reply_local_and_empty_review_is_inspectable() {
        let mut local = snapshot();
        local.files = diff::parse_unified(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
            "head",
            &diff::Limits::default(),
        )
        .files;
        let t = ReviewThread {
            id: "t".into(),
            source: ThreadSource::Local,
            github_thread_id: None,
            path: "a.rs".into(),
            file_id: None,
            side: Side::Right,
            line: Some(1),
            start_line: None,
            line_text: Some("new".into()),
            anchor_state: AnchorState::Current,
            anchor_reason: None,
            resolved: false,
            head_sha: "head".into(),
            created_at: String::new(),
            comments: [("root", false), ("reply", true)]
                .into_iter()
                .map(|(id, selected)| Comment {
                    id: id.into(),
                    thread_id: "t".into(),
                    author_kind: Actor::Human,
                    author: "me".into(),
                    body: id.into(),
                    created_at: id.into(),
                    updated_at: id.into(),
                    publish_state: PublishState::Draft,
                    github_comment_id: None,
                    publish_selected: selected,
                })
                .collect(),
        };
        let plan = build_plan(&local, Verdict::Comment, "", &[t], 1, &[]).unwrap();
        assert!(plan.inline.is_empty() && plan.replies.is_empty());
        assert_eq!(
            plan.omitted
                .iter()
                .map(|c| c.code.as_str())
                .collect::<Vec<_>>(),
            ["not_selected", "parent_not_selected"]
        );
    }

    #[test]
    fn omission_precedence_covers_all_codes() {
        let mut local = snapshot();
        local.files = diff::parse_unified(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
            "head",
            &diff::Limits::default(),
        )
        .files;
        let root = Comment {
            id: "root".into(),
            thread_id: "t".into(),
            author_kind: Actor::Human,
            author: "me".into(),
            body: "root".into(),
            created_at: "now".into(),
            updated_at: "now".into(),
            publish_state: PublishState::Draft,
            github_comment_id: None,
            publish_selected: false,
        };
        let mut t = ReviewThread {
            id: "t".into(),
            source: ThreadSource::Local,
            github_thread_id: None,
            path: "a.rs".into(),
            file_id: None,
            side: Side::Right,
            line: Some(1),
            start_line: None,
            line_text: Some("new".into()),
            anchor_state: AnchorState::Current,
            anchor_reason: None,
            resolved: false,
            head_sha: "head".into(),
            created_at: String::new(),
            comments: vec![root.clone()],
        };
        let code = |thread: &ReviewThread, reserved: &[String]| {
            build_plan(
                &local,
                Verdict::Approve,
                "",
                std::slice::from_ref(thread),
                0,
                reserved,
            )
            .unwrap()
            .omitted[0]
                .code
                .clone()
        };
        assert_eq!(code(&t, &[]), "not_selected");
        t.comments[0].author_kind = Actor::Github;
        assert_eq!(code(&t, &["root".into()]), "not_local_draft");
        t.comments[0].author_kind = Actor::Human;
        assert_eq!(code(&t, &["root".into()]), "reserved");
        t.path = "gone.rs".into();
        assert_eq!(code(&t, &[]), "stale_anchor");
        t.path = "a.rs".into();
        let mut reply = root.clone();
        reply.id = "reply".into();
        reply.publish_selected = true;
        t.comments.push(reply);
        assert_eq!(
            build_plan(&local, Verdict::Approve, "", &[t.clone()], 0, &[])
                .unwrap()
                .omitted[1]
                .code,
            "parent_not_selected"
        );
        // Deselection is not the cause when the root is invalid for another reason.
        t.path = "gone.rs".into();
        assert_eq!(
            build_plan(&local, Verdict::Approve, "", &[t.clone()], 0, &[])
                .unwrap()
                .omitted
                .iter()
                .map(|c| c.code.as_str())
                .collect::<Vec<_>>(),
            ["stale_anchor", "parent_unpublishable"]
        );
        t.path = "a.rs".into();
        for (body, reserved) in [("root", vec!["root".into()]), (" ", vec![])] {
            t.comments[0].body = body.into();
            assert_eq!(
                build_plan(&local, Verdict::Approve, "", &[t.clone()], 0, &reserved)
                    .unwrap()
                    .omitted[1]
                    .code,
                "parent_unpublishable"
            );
        }
        t.comments[0].publish_selected = true;
        assert_eq!(
            build_plan(&local, Verdict::Approve, "", &[t], 0, &[])
                .unwrap()
                .omitted[1]
                .code,
            "parent_unpublishable"
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
            comments: vec![Comment {
                id: "c".into(),
                thread_id: "t".into(),
                author_kind: Actor::Human,
                author: "a".into(),
                body: "fix".into(),
                created_at: String::new(),
                updated_at: String::new(),
                publish_state: PublishState::Draft,
                github_comment_id: None,
                publish_selected: true,
            }],
        };
        let plan = build_plan(&snapshot(), Verdict::Approve, "", &[t], 0, &[]).unwrap();
        assert!(plan.inline.is_empty());
        assert_eq!(plan.omitted[0].comment_id, "c");
    }
}
