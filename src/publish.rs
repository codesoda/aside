//! Private-first publication plans and guarded GitHub review submission.
//!
//! An attempt is persisted as `partial` before its first remote operation. A
//! timeout may mean GitHub accepted a request, so the same plan is never sent
//! again automatically. A human checks GitHub and reconciles an uncertain attempt
//! (`reconcile`), which releases its unsent drafts for a new plan.

mod plan;

use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

pub use self::plan::PublishPlan;
use self::plan::{Selection, build_plan};
use crate::{
    github::{self, GhRunner, NewReviewComment, PrRef},
    session::Session,
};

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

/// Build the exact outbound review and reply operations. Every eligible local draft is
/// selected unless the human deselected it (`PUT /review/publish/selection`); deselected,
/// dependent and invalid drafts are omitted with a code and a reason. Callers must display
/// `omitted` as well as the selected operations before confirmation.
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
    if review.lifecycle != "finished" {
        bail!("finish the local review before publishing");
    }
    let verdict = review.verdict.context("review has no verdict")?;
    build_plan(
        &snapshot,
        verdict,
        &review.summary,
        &store.list_threads(&session.pr_id)?,
        &Selection {
            deselected: store.deselected_comment_ids(&session.pr_id)?,
            rootless_threads: store.rootless_thread_ids(&session.pr_id)?,
        },
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

/// Serializes publications and reconciliations, even when callers supply different plan IDs.
fn publication_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Record what the human found on GitHub for the PR's unresolved attempt, releasing its
/// unsent drafts for a later plan (`Store::reconcile_publication`). Waits for a send in
/// progress, so its drafts are never released mid-send. The HTTP layer must authorize a
/// human actor before calling this function.
pub async fn reconcile(
    session: &Session,
    attempt_id: &str,
    published: &[(String, Option<i64>)],
) -> Result<()> {
    let _guard = publication_lock().lock().await;
    lock_store(session)?.reconcile_publication(&session.pr_id, attempt_id, published)
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
    let _guard = publication_lock().lock().await;
    let (gh, pr) = github_context(session)?;
    let snapshot = session
        .snapshot
        .read()
        .map_err(|_| anyhow!("snapshot lock poisoned"))?
        .clone();
    if !snapshot.complete {
        bail!("snapshot is incomplete; refresh before publication");
    }
    let head = blocking({
        let gh = gh.clone();
        let pr = pr.clone();
        move || github::fetch_head_sha(gh.as_ref(), &pr)
    })
    .await?;
    if head != snapshot.pr.head_sha {
        bail!("PR head changed; refresh and preview again");
    }
    let plan = preview(session)?;
    if plan.head_sha != head || plan.plan_id != plan_id {
        bail!("publication plan changed; preview again");
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
        store.start_publication(&session.pr_id, plan_id, &plan_json, &head, &drafts)?
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
mod tests;

#[cfg(test)]
mod selection_tests;

#[cfg(test)]
mod reconcile_tests;
