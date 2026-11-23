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
    /// Only the active claim for this PR can identify an unresolved publication.
    pub fn active_publication(&self, pr_id: &str) -> Result<Option<(String, Vec<String>)>> {
        let mut claims = self
            .conn
            .prepare("SELECT attempt_id FROM publication_claims WHERE pr_id = ?1")?;
        let mut rows = claims.query([pr_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let attempt_id: String = row.get(0)?;
        let mut drafts = self.conn.prepare(
            "SELECT id FROM review_comment_drafts WHERE reserved_attempt_id = ?1 ORDER BY id",
        )?;
        let ids = drafts
            .query_map([&attempt_id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(Some((attempt_id, ids)))
    }

    pub fn next_publication_ordinal(&self, pr_id: &str) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) + 1 FROM publication_attempts WHERE pr_id = ?1",
            [pr_id],
            |r| r.get(0),
        )?)
    }

    pub fn mark_prepared_published(
        &mut self,
        id: &str,
        attempt_id: &str,
        github_comment_id: Option<i64>,
        github_review_id: i64,
        head_sha: &str,
    ) -> Result<()> {
        let n = self.conn.execute("UPDATE review_comment_drafts SET state = 'published', github_comment_id = ?3, github_review_id = ?4, published_head_sha = ?5, published_at = ?6, updated_at = ?6 WHERE id = ?1 AND reserved_attempt_id = ?2 AND state = 'draft'", params![id, attempt_id, github_comment_id, github_review_id, head_sha, now_rfc3339()])?;
        if n != 1 {
            bail!("prepared draft reservation changed");
        }
        Ok(())
    }

    pub fn reconcile_publication(
        &mut self,
        pr_id: &str,
        attempt_id: &str,
        posted: bool,
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // Check claim and status in the same write transaction that releases drafts.
        // A stale attempt must not clear a newer attempt's claim or reservations.
        let status: String = tx.query_row(
            "SELECT a.status FROM publication_attempts a
             JOIN publication_claims c ON c.pr_id = a.pr_id AND c.attempt_id = a.id
             WHERE a.id = ?1 AND a.pr_id = ?2",
            params![attempt_id, pr_id],
            |r| r.get(0),
        )?;
        if status != "partial" {
            bail!("attempt is not unresolved");
        }
        if posted {
            // Keep unknown portions locked: acknowledgement is not a GitHub ID mapping.
            tx.execute("UPDATE publication_attempts SET status = 'reconciled', finished_at = ?2 WHERE id = ?1", params![attempt_id, now_rfc3339()])?;
        } else {
            let has_remote: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM publication_attempts WHERE id = ?1 AND github_review_id IS NOT NULL) OR EXISTS(SELECT 1 FROM review_comment_drafts WHERE reserved_attempt_id = ?1 AND state = 'published')", [attempt_id], |r| r.get(0))?;
            if has_remote {
                bail!("review or comment already posted; cannot mark entire attempt not_posted");
            }
            tx.execute("UPDATE review_comment_drafts SET reserved_attempt_id = NULL WHERE reserved_attempt_id = ?1 AND state = 'draft'", [attempt_id])?;
            tx.execute("UPDATE publication_attempts SET status = 'rejected', finished_at = ?2 WHERE id = ?1", params![attempt_id, now_rfc3339()])?;
        }
        tx.execute(
            "DELETE FROM publication_claims WHERE attempt_id = ?1",
            [attempt_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// A typed HTTP rejection proves the review operation did not post.
    pub fn reject_publication(&mut self, attempt_id: &str) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute("UPDATE review_comment_drafts SET reserved_attempt_id = NULL WHERE reserved_attempt_id = ?1 AND state = 'draft'", [attempt_id])?;
        let n = tx.execute("UPDATE publication_attempts SET status = 'rejected', finished_at = ?2 WHERE id = ?1 AND status = 'partial' AND github_review_id IS NULL", params![attempt_id, now_rfc3339()])?;
        if n != 1 {
            bail!("attempt was not safely rejectable");
        }
        tx.execute(
            "DELETE FROM publication_claims WHERE attempt_id = ?1",
            [attempt_id],
        )?;
        tx.commit()?;
        Ok(())
    }

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
        let ordinal: i64 = tx.query_row(
            "SELECT COUNT(*) + 1 FROM publication_attempts WHERE pr_id = ?1",
            [pr_id],
            |r| r.get(0),
        )?;
        let planned: serde_json::Value = serde_json::from_str(plan_json)?;
        if planned["attemptOrdinal"].as_i64() != Some(ordinal) {
            bail!("publication ordinal changed; preview again");
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
                "SELECT COUNT(*) FROM review_comment_drafts c JOIN review_threads t ON t.id = c.thread_id
                 WHERE c.id = ?1 AND c.body = ?2 AND c.state = 'draft' AND c.reserved_attempt_id IS NULL AND t.pr_id = ?3",
                params![comment_id, body, pr_id],
                |r| r.get(0),
            )?;
            if n != 1 {
                bail!("draft changed; preview again");
            }
            tx.execute("UPDATE review_comment_drafts SET reserved_attempt_id = ?2 WHERE id = ?1 AND reserved_attempt_id IS NULL", params![comment_id, id])?;
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
        if status == "succeeded" || status == "rejected" {
            tx.execute("DELETE FROM publication_claims WHERE attempt_id = ?1", [id])?;
        }
        tx.commit()?;
        Ok(())
    }
}
