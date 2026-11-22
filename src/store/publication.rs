use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, params};

use crate::{
    model::UnresolvedPublication,
    util::{new_id, now_rfc3339},
};

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
    pub fn plan_attempted(&self, pr_id: &str, plan_id: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM publication_attempts WHERE pr_id = ?1 AND plan_id = ?2)",
            params![pr_id, plan_id],
            |r| r.get(0),
        )?)
    }

    pub fn unresolved_publication(&self, pr_id: &str) -> Result<Option<UnresolvedPublication>> {
        type AttemptRow = (String, String, Option<i64>, Option<String>, String);
        let row: Option<AttemptRow> = self.conn.query_row(
            "SELECT a.id, a.status, a.github_review_id, a.error, a.created_at FROM publication_attempts a WHERE a.pr_id = ?1 AND a.status != 'succeeded' ORDER BY a.created_at DESC LIMIT 1",
            [pr_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        ).optional()?;
        let Some((attempt_id, status, github_review_id, error, created_at)) = row else {
            return Ok(None);
        };
        let mut stmt = self.conn.prepare("SELECT comment_id FROM publication_reservations WHERE attempt_id = ?1 ORDER BY comment_id")?;
        let reserved_comment_ids = stmt
            .query_map([&attempt_id], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some(UnresolvedPublication {
            attempt_id,
            status,
            github_review_id,
            error,
            created_at,
            reserved_comment_ids,
        }))
    }

    pub fn start_publication(
        &mut self,
        pr_id: &str,
        plan_id: &str,
        plan_json: &str,
        head_sha: &str,
        drafts: &[(&str, &str)],
        expected_selection_revision: u64,
    ) -> Result<String> {
        let id = new_id("pub");
        // IMMEDIATE takes the writer lock before checking, across all processes.
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let revision: i64 = tx.query_row(
            "SELECT selection_revision FROM review_sessions WHERE pr_id = ?1",
            [pr_id],
            |r| r.get(0),
        )?;
        if revision != expected_selection_revision as i64 {
            bail!("publication plan changed; preview again");
        }
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
                 WHERE c.id = ?1 AND c.body = ?2 AND c.publish_state = 'draft' AND c.publish_selected = 1 AND t.pr_id = ?3",
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
