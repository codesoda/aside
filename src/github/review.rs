use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::model::Side;

use super::{GhRunner, PrRef, runner::run_json};

/// Current base and head SHAs, read fresh before publication.
pub fn fetch_revision(gh: &dyn GhRunner, pr_ref: &PrRef) -> Result<(String, String)> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Refs {
        base_ref_oid: String,
        head_ref_oid: String,
    }
    let refs: Refs = run_json(
        gh,
        &[
            "pr",
            "view",
            &pr_ref.url(),
            "--json",
            "baseRefOid,headRefOid",
        ],
    )?;
    if refs.base_ref_oid.is_empty() || refs.head_ref_oid.is_empty() {
        anyhow::bail!("PR revision is unavailable");
    }
    Ok((refs.base_ref_oid, refs.head_ref_oid))
}

/// One inline comment inside a new GitHub review.
#[derive(Debug, Clone)]
pub struct NewReviewComment {
    pub path: String,
    pub side: Side,
    pub line: u32,
    pub start_line: Option<u32>,
    pub body: String,
}

/// Create a review in one API call. Returns the review's database ID.
pub fn create_review(
    gh: &dyn GhRunner,
    pr_ref: &PrRef,
    commit_id: &str,
    event: &str,
    body: &str,
    comments: &[NewReviewComment],
) -> Result<i64> {
    let comments: Vec<Value> = comments
        .iter()
        .map(|c| {
            let mut v =
                json!({ "path": c.path, "side": c.side.as_str(), "line": c.line, "body": c.body });
            if let Some(start) = c.start_line {
                v["start_line"] = json!(start);
                v["start_side"] = json!(c.side.as_str());
            }
            v
        })
        .collect();
    let payload =
        json!({ "commit_id": commit_id, "event": event, "body": body, "comments": comments });
    let out = gh.run(
        &[
            "api",
            "--method",
            "POST",
            &format!("{}/reviews", pr_ref.api_base()),
            "--input",
            "-",
        ],
        Some(payload.to_string().as_bytes()),
    )?;
    let v: Value = serde_json::from_slice(&out).context("unexpected JSON from create review")?;
    v["id"].as_i64().context("create review response has no id")
}

/// A created review comment: (database id, path, line, body).
pub type ReviewCommentRecord = (i64, String, Option<u32>, String);

pub fn review_comments(
    gh: &dyn GhRunner,
    pr_ref: &PrRef,
    review_id: i64,
) -> Result<Vec<ReviewCommentRecord>> {
    let pages: Vec<Vec<Value>> = run_json(
        gh,
        &[
            "api",
            "--paginate",
            "--slurp",
            &format!(
                "{}/reviews/{review_id}/comments?per_page=100",
                pr_ref.api_base()
            ),
        ],
    )?;
    Ok(pages
        .iter()
        .flatten()
        .filter_map(|c| {
            Some((
                c["id"].as_i64()?,
                c["path"].as_str()?.to_string(),
                c["line"].as_u64().map(|n| n as u32),
                c["body"].as_str().unwrap_or_default().to_string(),
            ))
        })
        .collect())
}

/// Reply to an existing review comment. Returns the new comment's database ID.
pub fn reply_to_comment(
    gh: &dyn GhRunner,
    pr_ref: &PrRef,
    comment_id: i64,
    body: &str,
) -> Result<i64> {
    let payload = json!({ "body": body });
    let out = gh.run(
        &[
            "api",
            "--method",
            "POST",
            &format!("{}/comments/{comment_id}/replies", pr_ref.api_base()),
            "--input",
            "-",
        ],
        Some(payload.to_string().as_bytes()),
    )?;
    let v: Value = serde_json::from_slice(&out).context("unexpected JSON from reply")?;
    v["id"].as_i64().context("reply response has no id")
}
