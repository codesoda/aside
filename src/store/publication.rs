use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, params};

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
                                 WHERE a.pr_id = publication_claims.pr_id AND a.status != 'succeeded')",
                [id],
            )?;
        }
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
        assert_eq!(version, 3);
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
