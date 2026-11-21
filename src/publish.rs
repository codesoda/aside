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
        Actor, AnchorState, PrSnapshot, PublishState, ReviewThread, Side, ThreadSource, Verdict,
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
    pub verdict: Verdict,
    pub summary: String,
    pub inline: Vec<PlannedInline>,
    pub replies: Vec<PlannedReply>,
    pub omitted: Vec<OmittedDraft>,
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
) -> Result<PublishPlan> {
    let mut plan = PublishPlan {
        plan_id: String::new(),
        head_sha: snapshot.pr.head_sha.clone(),
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
        for (index, comment) in thread.comments.iter().enumerate() {
            if comment.publish_state != PublishState::Draft {
                continue;
            }
            let reason = if comment.author_kind == Actor::Github || comment.body.trim().is_empty() {
                Some("not a nonempty local draft")
            } else if index == 0 && thread.source == ThreadSource::Local {
                if current_anchor(thread, snapshot) {
                    None
                } else {
                    Some("anchor is stale, missing or incomplete")
                }
            } else if parent.is_none() {
                Some("no publishable parent comment")
            } else {
                None
            };
            if let Some(reason) = reason {
                plan.omitted.push(OmittedDraft {
                    comment_id: comment.id.clone(),
                    thread_id: thread.id.clone(),
                    reason: reason.into(),
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
                });
                parent = Some((comment.id.clone(), None));
            } else if let Some((parent_comment_id, github_parent_id)) = &parent {
                plan.replies.push(PlannedReply {
                    comment_id: comment.id.clone(),
                    thread_id: thread.id.clone(),
                    parent_comment_id: parent_comment_id.clone(),
                    github_parent_id: *github_parent_id,
                    body: comment.body.clone(),
                });
            }
        }
    }
    if matches!(verdict, Verdict::RequestChanges | Verdict::Comment)
        && summary.trim().is_empty()
        && plan.inline.is_empty()
    {
        bail!("REQUEST_CHANGES and COMMENT require a summary or inline comment");
    }
    plan.plan_id = sha256_hex(&serde_json::to_vec(&plan)?);
    Ok(plan)
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
    if review.lifecycle != "finished" {
        bail!("finish the local review before publishing");
    }
    let verdict = review.verdict.context("review has no verdict")?;
    build_plan(
        &snapshot,
        verdict,
        &review.summary,
        &store.list_threads(&session.pr_id)?,
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
            reviews: vec![],
            issue_comments: vec![],
            warnings: vec![],
            complete: true,
            fetched_at: String::new(),
        }
    }

    #[test]
    fn plan_id_binds_head_and_text() {
        let a = build_plan(&snapshot(), Verdict::Approve, "looks good", &[]).unwrap();
        assert_eq!(
            a.plan_id,
            build_plan(&snapshot(), Verdict::Approve, "looks good", &[])
                .unwrap()
                .plan_id
        );
        let mut changed = snapshot();
        changed.pr.head_sha = "later".into();
        assert_ne!(
            a.plan_id,
            build_plan(&changed, Verdict::Approve, "looks good", &[])
                .unwrap()
                .plan_id
        );
        assert_ne!(
            a.plan_id,
            build_plan(&snapshot(), Verdict::Approve, "different", &[])
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
        fake.on(&["pr", "view"], Ok(r#"{"headRefOid":"head"}"#));
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
    async fn unknown_remote_outcome_is_durable_and_never_retried() {
        let fake = Arc::new(FakeGh::default());
        fake.on(&["pr", "view"], Ok(r#"{"headRefOid":"head"}"#));
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
            }],
        };
        let plan = build_plan(&snapshot(), Verdict::Approve, "", &[t]).unwrap();
        assert!(plan.inline.is_empty());
        assert_eq!(plan.omitted[0].comment_id, "c");
    }
}
