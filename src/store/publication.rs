use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::util::{new_id, now_rfc3339};

use super::{NotFound, Store};

/// A draft that was reserved for publication or is no longer editable.
#[derive(Debug)]
pub struct ReservedDraft;

impl std::fmt::Display for ReservedDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "draft is reserved for publication")
    }
}
impl std::error::Error for ReservedDraft {}

/// A comment that is not a local draft, so it cannot be selected for publication.
#[derive(Debug)]
pub struct NotADraft(pub String);

impl std::fmt::Display for NotADraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "comment {} is not a local draft", self.0)
    }
}
impl std::error::Error for NotADraft {}

/// The attempt holding a PR's publication claim: in flight, or partial and waiting
/// for the human to reconcile it against GitHub.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnresolvedPublication {
    pub attempt_id: String,
    pub error: Option<String>,
    pub github_review_id: Option<i64>,
    /// Drafts the attempt reserved that are not recorded as published.
    pub remaining_comment_ids: Vec<String>,
}

impl Store {
    /// Local drafts of this PR that the human held back from publication.
    pub fn deselected_comment_ids(&self, pr_id: &str) -> Result<BTreeSet<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT d.comment_id FROM publication_deselections d
             JOIN review_comments c ON c.id = d.comment_id
             JOIN review_threads t ON t.id = c.thread_id WHERE t.pr_id = ?1",
        )?;
        let ids = stmt.query_map([pr_id], |r| r.get(0))?;
        ids.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    /// Select or deselect local drafts for publication, all or nothing. Each id must
    /// be an unreserved local draft of this PR; published and imported comments and
    /// drafts reserved by an attempt keep their current state.
    pub fn set_publication_selection(
        &mut self,
        pr_id: &str,
        comment_ids: &[String],
        selected: bool,
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        for id in comment_ids {
            let row: Option<(String, String, bool)> = tx
                .query_row(
                    "SELECT c.publish_state, c.author_kind,
                            EXISTS (SELECT 1 FROM publication_reservations r WHERE r.comment_id = c.id)
                     FROM review_comments c JOIN review_threads t ON t.id = c.thread_id
                     WHERE c.id = ?1 AND t.pr_id = ?2",
                    params![id, pr_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let Some((state, author_kind, reserved)) = row else {
                return Err(NotFound(format!("comment {id}")).into());
            };
            if state != "draft" || author_kind == "github" {
                return Err(NotADraft(id.clone()).into());
            }
            if reserved {
                return Err(ReservedDraft.into());
            }
            if selected {
                tx.execute(
                    "DELETE FROM publication_deselections WHERE comment_id = ?1",
                    [id],
                )?;
            } else {
                tx.execute(
                    "INSERT OR IGNORE INTO publication_deselections (comment_id, created_at) VALUES (?1, ?2)",
                    params![id, now_rfc3339()],
                )?;
            }
        }
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
        let attempted: i64 = tx.query_row(
            "SELECT COUNT(*) FROM publication_attempts
             WHERE pr_id = ?1 AND plan_id = ?2 AND status != 'reconciled'",
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
            // A draft deselected after the preview no longer matches the confirmed plan.
            let n: i64 = tx.query_row(
                "SELECT COUNT(*) FROM review_comments c JOIN review_threads t ON t.id = c.thread_id
                 WHERE c.id = ?1 AND c.body = ?2 AND c.publish_state = 'draft' AND t.pr_id = ?3
                 AND NOT EXISTS (SELECT 1 FROM publication_deselections d WHERE d.comment_id = c.id)",
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
                                 WHERE a.pr_id = publication_claims.pr_id
                                 AND a.status NOT IN ('succeeded', 'reconciled'))",
                [id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn unresolved_publication(&self, pr_id: &str) -> Result<Option<UnresolvedPublication>> {
        let attempt = self
            .conn
            .query_row(
                "SELECT a.id, a.error, a.github_review_id FROM publication_claims c
                 JOIN publication_attempts a ON a.id = c.attempt_id WHERE c.pr_id = ?1",
                [pr_id],
                |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((attempt_id, error, github_review_id)) = attempt else {
            return Ok(None);
        };
        let mut stmt = self.conn.prepare(
            "SELECT c.id FROM publication_reservations r JOIN review_comments c ON c.id = r.comment_id
             WHERE r.attempt_id = ?1 AND c.publish_state = 'draft' ORDER BY c.seq",
        )?;
        let remaining_comment_ids = stmt
            .query_map([&attempt_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Some(UnresolvedPublication {
            attempt_id,
            error,
            github_review_id,
            remaining_comment_ids,
        }))
    }

    /// The human checked GitHub for the PR's unresolved attempt. Drafts in `published`
    /// reached GitHub (with their comment ids where known) and are marked published.
    /// The attempt's other drafts become editable, selectable drafts again, the attempt
    /// becomes `reconciled` and the PR's claim is released, so a new plan can send them.
    pub fn reconcile_publication(
        &mut self,
        pr_id: &str,
        attempt_id: &str,
        published: &[(String, Option<i64>)],
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let claimed: Option<String> = tx
            .query_row(
                "SELECT attempt_id FROM publication_claims WHERE pr_id = ?1",
                [pr_id],
                |r| r.get(0),
            )
            .optional()?;
        if claimed.as_deref() != Some(attempt_id) {
            return Err(NotFound(format!("unresolved publication {attempt_id}")).into());
        }
        let now = now_rfc3339();
        for (comment_id, github_id) in published {
            let reserved: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM publication_reservations
                                WHERE comment_id = ?1 AND attempt_id = ?2)",
                params![comment_id, attempt_id],
                |r| r.get(0),
            )?;
            if !reserved {
                return Err(NotFound(format!("comment {comment_id} in this publication")).into());
            }
            // A draft the attempt already recorded as published keeps its GitHub id.
            tx.execute(
                "UPDATE review_comments SET publish_state = 'published', github_comment_id = ?1, updated_at = ?2
                 WHERE id = ?3 AND publish_state = 'draft'",
                params![github_id, now, comment_id],
            )?;
        }
        tx.execute(
            "DELETE FROM publication_reservations WHERE attempt_id = ?1
             AND comment_id IN (SELECT id FROM review_comments WHERE publish_state = 'draft')",
            [attempt_id],
        )?;
        tx.execute(
            "UPDATE publication_attempts SET status = 'reconciled', finished_at = ?1 WHERE id = ?2",
            params![now, attempt_id],
        )?;
        tx.execute("DELETE FROM publication_claims WHERE pr_id = ?1", [pr_id])?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::super::{Store, schema::MIGRATIONS};

    #[test]
    fn deselections_migrate_onto_a_v2_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.db");
        {
            let conn = Connection::open(&path).unwrap();
            for sql in &MIGRATIONS[..2] {
                conn.execute_batch(sql).unwrap();
            }
            conn.pragma_update(None, "user_version", 2).unwrap();
        }
        drop(Store::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);
        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'publication_deselections'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 1);
    }
}
