use anyhow::Result;
use rusqlite::params;

use crate::model::{Acknowledgement, Actor, ReviewState, Verdict};
use crate::util::now_rfc3339;

use super::Store;

impl Store {
    pub fn insert_acknowledgement(&mut self, pr_id: &str, ack: &Acknowledgement) -> Result<()> {
        self.conn.execute(
            "INSERT INTO review_acknowledgements
             (id, pr_id, path, kind, fingerprint_version, diff_digest, content_digest, head_sha, base_sha, actor, created_at, seq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                     (SELECT COALESCE(MAX(seq), 0) + 1 FROM review_acknowledgements))",
            params![
                ack.id, pr_id, ack.path, ack.kind, ack.fingerprint_version, ack.diff_digest, ack.content_digest,
                ack.head_sha, ack.base_sha, ack.actor.as_str(), ack.created_at
            ],
        )?;
        Ok(())
    }

    /// All acknowledgements for a PR, oldest first.
    pub fn list_acknowledgements(&self, pr_id: &str) -> Result<Vec<Acknowledgement>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, kind, fingerprint_version, diff_digest, content_digest, head_sha, base_sha, actor, created_at
             FROM review_acknowledgements WHERE pr_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([pr_id], |r| {
            Ok(Acknowledgement {
                id: r.get(0)?,
                path: r.get(1)?,
                kind: r.get(2)?,
                fingerprint_version: r.get(3)?,
                diff_digest: r.get(4)?,
                content_digest: r.get(5)?,
                head_sha: r.get(6)?,
                base_sha: r.get(7)?,
                actor: Actor::parse(&r.get::<_, String>(8)?),
                created_at: r.get(9)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    pub fn review_state(&self, pr_id: &str) -> Result<ReviewState> {
        let mut state = self.conn
            .query_row(
                "SELECT lifecycle_state, verdict, summary, finished_at, published_at, selection_revision FROM review_sessions WHERE pr_id = ?1",
                [pr_id],
                |r| {
                    Ok(ReviewState {
                        selection_revision: r.get::<_, i64>(5)? as u64,
                        unresolved_publication: None,
                        lifecycle: r.get(0)?,
                        verdict: r.get::<_, Option<String>>(1)?.as_deref().and_then(Verdict::parse),
                        summary: r.get(2)?,
                        finished_at: r.get(3)?,
                        published_at: r.get(4)?,
                    })
                },
            )
            .map_err(anyhow::Error::from)?;
        state.unresolved_publication = self.unresolved_publication(pr_id)?;
        Ok(state)
    }

    pub fn finish_review(&mut self, pr_id: &str, verdict: Verdict, summary: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE review_sessions SET lifecycle_state = 'finished', verdict = ?1, summary = ?2, finished_at = ?3 WHERE pr_id = ?4",
            params![verdict.as_str(), summary, now_rfc3339(), pr_id],
        )?;
        Ok(())
    }

    pub fn mark_published(&mut self, pr_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE review_sessions SET lifecycle_state = 'published', published_at = ?1 WHERE pr_id = ?2",
            params![now_rfc3339(), pr_id],
        )?;
        Ok(())
    }

    /// A new PR head starts a new local review while preserving history.
    pub fn reopen_review(&mut self, pr_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE review_sessions SET lifecycle_state = 'in-progress', verdict = NULL, summary = '', finished_at = NULL, published_at = NULL
             WHERE pr_id = ?1 AND lifecycle_state IN ('finished', 'published')",
            [pr_id],
        )?;
        Ok(())
    }
}
