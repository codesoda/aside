//! Building the exact publication plan from local drafts and the human's selection.

use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    diff,
    model::{
        Actor, AnchorState, Comment, PrSnapshot, PublishState, ReviewThread, Side, ThreadSource,
        Verdict,
    },
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

/// Why a local draft is left out of a publication plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OmitCode {
    /// Imported from GitHub, or an empty body.
    NotALocalDraft,
    /// The human deselected it; it stays a draft for a later review.
    NotSelected,
    /// The thread's new first comment is deselected, so a reply has nothing to attach to.
    ParentNotSelected,
    /// The thread has no comment on GitHub, and its first comment cannot be published
    /// or was deleted.
    ParentNotPublishable,
    /// The thread's anchor is stale, missing or incomplete.
    StaleAnchor,
}

impl OmitCode {
    fn reason(self) -> &'static str {
        match self {
            Self::NotALocalDraft => "not a nonempty local draft",
            Self::NotSelected => "not selected for this review",
            Self::ParentNotSelected => "its thread's first comment is not selected",
            Self::ParentNotPublishable => "no publishable parent comment",
            Self::StaleAnchor => "anchor is stale, missing or incomplete",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OmittedDraft {
    pub comment_id: String,
    pub thread_id: String,
    pub code: OmitCode,
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

/// What a thread's replies attach to on GitHub: replies hang off the first comment.
enum Parent {
    /// The first comment exists on GitHub, or is the new inline comment in this plan.
    Ready(String, Option<i64>),
    /// The first comment is a local draft the human deselected.
    NotSelected,
    /// Nothing publishable to attach to.
    Missing,
}

/// Why `comment` must stay local, or `None` if it can be planned.
fn omission(
    comment: &Comment,
    is_root: bool,
    parent: &Parent,
    anchored: bool,
    deselected: &BTreeSet<String>,
) -> Option<OmitCode> {
    if comment.author_kind == Actor::Github || comment.body.trim().is_empty() {
        Some(OmitCode::NotALocalDraft)
    } else if deselected.contains(&comment.id) {
        Some(OmitCode::NotSelected)
    } else if is_root {
        (!anchored).then_some(OmitCode::StaleAnchor)
    } else {
        match parent {
            Parent::Ready(..) => None,
            Parent::NotSelected => Some(OmitCode::ParentNotSelected),
            Parent::Missing => Some(OmitCode::ParentNotPublishable),
        }
    }
}

/// What, besides each draft itself, decides whether a local draft is planned.
#[derive(Debug, Default)]
pub(super) struct Selection {
    /// Drafts the human held back.
    pub deselected: BTreeSet<String>,
    /// Local threads whose first comment was deleted: their remaining drafts were
    /// replies, so none of them may become the thread's new inline comment.
    pub rootless_threads: BTreeSet<String>,
}

/// Plan one thread's drafts. A deselected, deleted or unpublishable first comment
/// leaves the thread's draft replies local too, each with its reason.
fn plan_thread(
    plan: &mut PublishPlan,
    thread: &ReviewThread,
    snapshot: &PrSnapshot,
    selection: &Selection,
) -> Result<()> {
    let rootless = selection.rootless_threads.contains(&thread.id);
    let mut parent = match thread.comments.first() {
        Some(c) if c.github_comment_id.is_some() && !rootless => {
            Parent::Ready(c.id.clone(), c.github_comment_id)
        }
        _ => Parent::Missing,
    };
    let anchored = current_anchor(thread, snapshot);
    let deselected = &selection.deselected;
    for (index, comment) in thread.comments.iter().enumerate() {
        if comment.publish_state != PublishState::Draft {
            continue;
        }
        let is_root = index == 0 && thread.source == ThreadSource::Local && !rootless;
        if let Some(code) = omission(comment, is_root, &parent, anchored, deselected) {
            if is_root && code == OmitCode::NotSelected {
                parent = Parent::NotSelected;
            }
            plan.omitted.push(OmittedDraft {
                comment_id: comment.id.clone(),
                thread_id: thread.id.clone(),
                code,
                reason: code.reason().into(),
            });
        } else if is_root {
            plan.inline.push(PlannedInline {
                comment_id: comment.id.clone(),
                thread_id: thread.id.clone(),
                path: thread.path.clone(),
                side: thread.side,
                line: thread.line.context("current anchor has no line")?,
                start_line: thread.start_line,
                body: comment.body.clone(),
            });
            parent = Parent::Ready(comment.id.clone(), None);
        } else if let Parent::Ready(parent_comment_id, github_parent_id) = &parent {
            plan.replies.push(PlannedReply {
                comment_id: comment.id.clone(),
                thread_id: thread.id.clone(),
                parent_comment_id: parent_comment_id.clone(),
                github_parent_id: *github_parent_id,
                body: comment.body.clone(),
            });
        }
    }
    Ok(())
}

pub(super) fn build_plan(
    snapshot: &PrSnapshot,
    verdict: Verdict,
    summary: &str,
    threads: &[ReviewThread],
    selection: &Selection,
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
        plan_thread(&mut plan, thread, snapshot, selection)?;
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
