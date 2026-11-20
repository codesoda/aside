use std::collections::BTreeSet;

use anyhow::Result;
use rusqlite::{OptionalExtension, Row, params};

use crate::model::{Actor, AnchorState, Comment, PublishState, ReviewThread, Side, ThreadSource};
use crate::util::{new_id, now_rfc3339};

use super::{NotFound, ReservedDraft, Store};

/// Fields for a new thread row.
pub struct NewThread {
    pub id: String,
    pub source: ThreadSource,
    pub github_thread_id: Option<String>,
    pub path: String,
    pub side: Side,
    pub line: Option<u32>,
    pub start_line: Option<u32>,
    pub line_text: Option<String>,
    pub diff_digest: Option<String>,
    pub head_sha: String,
    pub anchor_state: AnchorState,
    pub anchor_reason: Option<String>,
    pub resolved: bool,
    pub created_at: Option<String>,
}

impl Store {
    pub fn insert_thread(&mut self, pr_id: &str, t: &NewThread) -> Result<()> {
        let now = now_rfc3339();
        self.conn.execute(
            "INSERT INTO review_threads (id, pr_id, source, github_thread_id, path, side, line, start_line, line_text,
                diff_digest, head_sha, anchor_state, anchor_reason, resolved, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?15)",
            params![
                t.id, pr_id, source_str(t.source), t.github_thread_id, t.path, t.side.as_str(), t.line, t.start_line,
                t.line_text, t.diff_digest, t.head_sha, t.anchor_state.as_str(), t.anchor_reason, t.resolved,
                t.created_at.as_deref().unwrap_or(&now)
            ],
        )?;
        Ok(())
    }

    pub fn insert_comment(&mut self, c: &Comment, github_node_id: Option<&str>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO review_comments (id, thread_id, author_kind, author, body, publish_state, github_comment_id,
                github_node_id, created_at, updated_at, seq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, (SELECT COALESCE(MAX(seq), 0) + 1 FROM review_comments))",
            params![
                c.id, c.thread_id, c.author_kind.as_str(), c.author, c.body, c.publish_state.as_str(),
                c.github_comment_id, github_node_id, c.created_at, c.updated_at
            ],
        )?;
        Ok(())
    }

    /// Merge GitHub threads into local storage (insert new, refresh known).
    pub fn sync_github_threads(
        &mut self,
        pr_id: &str,
        head_sha: &str,
        threads: &[crate::model::GithubThread],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        let now = now_rfc3339();
        for t in threads {
            let existing: Option<String> = tx
                .query_row(
                    "SELECT id FROM review_threads WHERE github_thread_id = ?1",
                    [&t.node_id],
                    |r| r.get(0),
                )
                .optional()?;
            let (state, reason) = if t.is_outdated {
                (
                    AnchorState::Outdated,
                    Some("GitHub marks this thread as outdated"),
                )
            } else {
                (AnchorState::Current, None)
            };
            let thread_id = match existing {
                Some(id) => {
                    tx.execute(
                        "UPDATE review_threads SET resolved = ?1, line = ?2, start_line = ?3, anchor_state = ?4, anchor_reason = ?5, updated_at = ?6 WHERE id = ?7",
                        params![t.is_resolved, t.line, t.start_line, state.as_str(), reason, now, id],
                    )?;
                    id
                }
                None => {
                    let id = new_id("thr");
                    tx.execute(
                        "INSERT INTO review_threads (id, pr_id, source, github_thread_id, path, side, line, start_line, line_text,
                            diff_digest, head_sha, anchor_state, anchor_reason, resolved, created_at, updated_at)
                         VALUES (?1, ?2, 'github', ?3, ?4, ?5, ?6, ?7, NULL, NULL, ?8, ?9, ?10, ?11, ?12, ?12)",
                        params![
                            id, pr_id, t.node_id, t.path, t.side.unwrap_or(Side::Right).as_str(),
                            t.line.or(t.original_line), t.start_line, head_sha, state.as_str(), reason, t.is_resolved,
                            t.comments.first().map(|c| c.created_at.clone()).unwrap_or_else(|| now.clone())
                        ],
                    )?;
                    id
                }
            };
            for c in &t.comments {
                tx.execute(
                    "INSERT INTO review_comments (id, thread_id, author_kind, author, body, publish_state, github_comment_id,
                        github_node_id, created_at, updated_at, seq)
                     VALUES (?1, ?2, 'github', ?3, ?4, 'imported', ?5, ?6, ?7, ?7, (SELECT COALESCE(MAX(seq), 0) + 1 FROM review_comments))
                     ON CONFLICT(github_node_id) DO UPDATE SET body = excluded.body",
                    params![new_id("cmt"), thread_id, c.author, c.body, c.database_id, c.node_id, c.created_at],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_threads(&self, pr_id: &str) -> Result<Vec<ReviewThread>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source, github_thread_id, path, side, line, start_line, line_text, anchor_state, anchor_reason,
                    resolved, head_sha, created_at
             FROM review_threads WHERE pr_id = ?1 ORDER BY created_at, id",
        )?;
        let mut threads: Vec<ReviewThread> = stmt
            .query_map([pr_id], |r| {
                Ok(ReviewThread {
                    id: r.get(0)?,
                    source: if r.get::<_, String>(1)? == "github" {
                        ThreadSource::Github
                    } else {
                        ThreadSource::Local
                    },
                    github_thread_id: r.get(2)?,
                    path: r.get(3)?,
                    file_id: None,
                    side: Side::parse(&r.get::<_, String>(4)?).unwrap_or(Side::Right),
                    line: r.get(5)?,
                    start_line: r.get(6)?,
                    line_text: r.get(7)?,
                    anchor_state: AnchorState::parse(&r.get::<_, String>(8)?),
                    anchor_reason: r.get(9)?,
                    resolved: r.get(10)?,
                    head_sha: r.get(11)?,
                    created_at: r.get(12)?,
                    comments: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut cstmt = self.conn.prepare(
            "SELECT c.id, c.thread_id, c.author_kind, c.author, c.body, c.created_at, c.updated_at, c.publish_state, c.github_comment_id
             FROM review_comments c JOIN review_threads t ON t.id = c.thread_id WHERE t.pr_id = ?1 ORDER BY c.seq",
        )?;
        let comments = cstmt.query_map([pr_id], comment_from_row)?;
        for c in comments {
            let c = c?;
            if let Some(t) = threads.iter_mut().find(|t| t.id == c.thread_id) {
                t.comments.push(c);
            }
        }
        Ok(threads)
    }

    pub fn get_thread(&self, pr_id: &str, id: &str) -> Result<ReviewThread> {
        self.list_threads(pr_id)?
            .into_iter()
            .find(|t| t.id == id)
            .ok_or_else(|| NotFound(format!("thread {id}")).into())
    }

    pub fn set_thread_resolved(&mut self, id: &str, resolved: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE review_threads SET resolved = ?1, updated_at = ?2 WHERE id = ?3",
            params![resolved, now_rfc3339(), id],
        )?;
        Ok(())
    }

    pub fn set_thread_anchor(
        &mut self,
        id: &str,
        state: AnchorState,
        reason: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE review_threads SET anchor_state = ?1, anchor_reason = ?2, updated_at = ?3 WHERE id = ?4",
            params![state.as_str(), reason, now_rfc3339(), id],
        )?;
        Ok(())
    }

    pub fn get_comment(&self, id: &str) -> Result<Comment> {
        self.conn
            .query_row(
                "SELECT id, thread_id, author_kind, author, body, created_at, updated_at, publish_state, github_comment_id
                 FROM review_comments WHERE id = ?1",
                [id],
                comment_from_row,
            )
            .optional()?
            .ok_or_else(|| NotFound(format!("comment {id}")).into())
    }

    pub fn update_comment_body(&mut self, id: &str, body: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE review_comments SET body = ?1, updated_at = ?2 WHERE id = ?3 AND publish_state = 'draft'
             AND NOT EXISTS (SELECT 1 FROM publication_reservations WHERE comment_id = ?3)",
            params![body, now_rfc3339(), id],
        )?;
        if n != 1 {
            return Err(ReservedDraft.into());
        }
        Ok(())
    }

    /// Delete a draft comment, and its thread if that leaves the thread empty. Deleting
    /// a local thread's first comment while replies remain marks the thread rootless.
    pub fn delete_comment(&mut self, id: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        let thread_id: String = tx.query_row(
            "SELECT thread_id FROM review_comments WHERE id = ?1",
            [id],
            |r| r.get(0),
        )?;
        let first: String = tx.query_row(
            "SELECT id FROM review_comments WHERE thread_id = ?1 ORDER BY seq LIMIT 1",
            [&thread_id],
            |r| r.get(0),
        )?;
        let n = tx.execute(
            "DELETE FROM review_comments WHERE id = ?1 AND publish_state = 'draft'
             AND NOT EXISTS (SELECT 1 FROM publication_reservations WHERE comment_id = ?1)",
            [id],
        )?;
        if n != 1 {
            return Err(ReservedDraft.into());
        }
        let left: i64 = tx.query_row(
            "SELECT COUNT(*) FROM review_comments WHERE thread_id = ?1",
            [&thread_id],
            |r| r.get(0),
        )?;
        if left == 0 {
            tx.execute("DELETE FROM review_threads WHERE id = ?1", [&thread_id])?;
        } else if first == id {
            tx.execute(
                "UPDATE review_threads SET root_deleted = 1 WHERE id = ?1 AND source = 'local'",
                [&thread_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Local threads of this PR whose first comment was deleted while replies remained.
    pub fn rootless_thread_ids(&self, pr_id: &str) -> Result<BTreeSet<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM review_threads WHERE pr_id = ?1 AND root_deleted = 1")?;
        let ids = stmt.query_map([pr_id], |r| r.get(0))?;
        ids.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    pub fn mark_comment_published(
        &mut self,
        id: &str,
        github_comment_id: Option<i64>,
    ) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE review_comments SET publish_state = 'published', github_comment_id = ?1, updated_at = ?2
             WHERE id = ?3 AND publish_state = 'draft'
             AND EXISTS (SELECT 1 FROM publication_reservations WHERE comment_id = ?3)",
            params![github_comment_id, now_rfc3339(), id],
        )?;
        if n != 1 {
            return Err(ReservedDraft.into());
        }
        Ok(())
    }
}

fn comment_from_row(r: &Row) -> rusqlite::Result<Comment> {
    Ok(Comment {
        id: r.get(0)?,
        thread_id: r.get(1)?,
        author_kind: Actor::parse(&r.get::<_, String>(2)?),
        author: r.get(3)?,
        body: r.get(4)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
        publish_state: PublishState::parse(&r.get::<_, String>(7)?),
        github_comment_id: r.get(8)?,
    })
}

fn source_str(s: ThreadSource) -> &'static str {
    match s {
        ThreadSource::Local => "local",
        ThreadSource::Github => "github",
    }
}
