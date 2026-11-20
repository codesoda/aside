use anyhow::{Context, Result, bail};
use rusqlite::params;

use crate::util::{new_id, now_rfc3339};

use super::Store;

/// A draft that was reserved for publication or is no longer editable.
#[derive(Debug)]
pub struct ReservedDraft;

impl std::fmt::Display for ReservedDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "draft is reserved for publication")
    }
}
impl std::error::Error for ReservedDraft {}

impl Store {
    pub fn start_publication(
        &mut self,
        pr_id: &str,
        plan_id: &str,
        plan_json: &str,
        head_sha: &str,
        drafts: &[(&str, &str)],
    ) -> Result<String> {
        let id = new_id("pub");
        // IMMEDIATE takes the writer lock before checking, across all processes.
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let attempted: i64 = tx.query_row(
            "SELECT COUNT(*) FROM publication_attempts WHERE pr_id = ?1 AND plan_id = ?2",
            params![pr_id, plan_id],
            |r| r.get(0),
        )?;
        if attempted != 0 {
            bail!("this publication plan was already attempted");
        }
        tx.execute(
            "INSERT INTO publication_attempts (id, pr_id, plan_id, plan_json, head_sha, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'partial', ?6)",
            params![id, pr_id, plan_id, plan_json, head_sha, now_rfc3339()],
        )?;
        tx.execute(
            "INSERT INTO publication_claims (pr_id, attempt_id) VALUES (?1, ?2)",
            params![pr_id, id],
        )
        .context("unresolved publication exists for this PR; reconcile on GitHub")?;
        for &(comment_id, body) in drafts {
            let n: i64 = tx.query_row(
                "SELECT COUNT(*) FROM review_comments c JOIN review_threads t ON t.id = c.thread_id
                 WHERE c.id = ?1 AND c.body = ?2 AND c.publish_state = 'draft' AND t.pr_id = ?3",
                params![comment_id, body, pr_id],
                |r| r.get(0),
            )?;
            if n != 1 {
                bail!("draft changed; preview again");
            }
            tx.execute(
                "INSERT INTO publication_reservations (comment_id, attempt_id) VALUES (?1, ?2)",
                params![comment_id, id],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    pub fn finish_publication(
        &mut self,
        id: &str,
        status: &str,
        error: Option<&str>,
        review_id: Option<i64>,
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let n = tx.execute(
            "UPDATE publication_attempts SET status = ?1, error = ?2, github_review_id = ?3, finished_at = ?4 WHERE id = ?5",
            params![status, error, review_id, now_rfc3339(), id],
        )?;
        if n != 1 {
            bail!("publication attempt missing");
        }
        if status == "succeeded" {
            tx.execute(
                "DELETE FROM publication_claims WHERE attempt_id = ?1
                 AND NOT EXISTS (SELECT 1 FROM publication_attempts a
                                 WHERE a.pr_id = publication_claims.pr_id AND a.status != 'succeeded')",
                [id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
