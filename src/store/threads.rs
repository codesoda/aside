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
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO review_comments (id, thread_id, author_kind, author, body, publish_state, github_comment_id,
                github_node_id, created_at, updated_at, seq, publish_selected)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, (SELECT COALESCE(MAX(seq), 0) + 1 FROM review_comments), ?11)",
            params![
                c.id, c.thread_id, c.author_kind.as_str(), c.author, c.body, c.publish_state.as_str(),
                c.github_comment_id, github_node_id, c.created_at, c.updated_at, c.publish_selected
            ],
        )?;
        if c.publish_state == PublishState::Draft {
            tx.execute("UPDATE review_sessions SET selection_revision = selection_revision + 1 WHERE pr_id = (SELECT pr_id FROM review_threads WHERE id = ?1)", [&c.thread_id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Merge GitHub threads into local storage (insert new, refresh known).
    pub fn sync_github_threads(
        &mut self,
        pr_id: &str,
        head_sha: &str,
        threads: &[crate::model::GithubThread],
        enumeration_complete: bool,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        let now = now_rfc3339();
        for t in threads {
            let existing: Option<String> = tx
                .query_row(
                    "SELECT id FROM review_threads WHERE github_thread_id = ?1 OR
                     (source = 'github' AND github_thread_id = ?2)",
                    params![
                        t.node_id,
                        t.comments
                            .first()
                            .and_then(|c| c.database_id)
                            .map(|id| format!("rest:{id}"))
                    ],
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
                    if !t.rest_only {
                        tx.execute(
                        "UPDATE review_threads SET github_thread_id = ?22, path = CASE WHEN ?25 THEN path ELSE ?23 END, side = COALESCE(?24, side),
                            resolved = CASE WHEN ?13 THEN resolved ELSE ?1 END,
                            line = CASE WHEN ?14 THEN line ELSE ?2 END,
                            start_line = CASE WHEN ?15 THEN start_line ELSE ?3 END,
                            anchor_state = CASE WHEN ?16 THEN anchor_state ELSE ?4 END,
                            anchor_reason = CASE WHEN ?16 THEN anchor_reason ELSE ?5 END,
                            updated_at = ?6,
                            original_line = CASE WHEN ?17 THEN original_line ELSE ?8 END,
                            original_start_line = CASE WHEN ?18 THEN original_start_line ELSE ?9 END,
                            start_side = CASE WHEN ?19 THEN start_side ELSE ?10 END,
                            viewer_can_resolve = CASE WHEN ?20 THEN viewer_can_resolve ELSE ?11 END,
                            viewer_can_unresolve = CASE WHEN ?21 THEN viewer_can_unresolve ELSE ?12 END
                         WHERE id = ?7",
                        params![t.is_resolved, t.line, t.start_line, state.as_str(), reason, now, id,
                            t.original_line, t.original_start_line, t.start_side.as_deref(), t.viewer_can_resolve, t.viewer_can_unresolve,
                            unavailable(&t.unavailable_fields, "isResolved"), unavailable(&t.unavailable_fields, "line"),
                            unavailable(&t.unavailable_fields, "startLine"), unavailable(&t.unavailable_fields, "isOutdated"),
                            unavailable(&t.unavailable_fields, "originalLine"), unavailable(&t.unavailable_fields, "originalStartLine"),
                            unavailable(&t.unavailable_fields, "startDiffSide"), unavailable(&t.unavailable_fields, "viewerCanResolve"),
                            unavailable(&t.unavailable_fields, "viewerCanUnresolve"), t.node_id, t.path, t.side.map(Side::as_str),
                            unavailable(&t.unavailable_fields, "path")],
                    )?;
                    }
                    id
                }
                None => {
                    let id = new_id("thr");
                    tx.execute(
                        "INSERT INTO review_threads (id, pr_id, source, github_thread_id, path, side, line, start_line, line_text,
                            diff_digest, head_sha, anchor_state, anchor_reason, resolved, created_at, updated_at,
                            original_line, original_start_line, start_side, viewer_can_resolve, viewer_can_unresolve)
                         VALUES (?1, ?2, 'github', ?3, ?4, ?5, ?6, ?7, NULL, NULL, ?8, ?9, ?10, ?11, ?12, ?12, ?13, ?14, ?15, ?16, ?17)",
                        params![
                            id, pr_id, t.node_id, t.path, t.side.unwrap_or(Side::Right).as_str(),
                            t.line.or(t.original_line), t.start_line, head_sha,
                            if t.rest_only { AnchorState::Orphaned.as_str() } else { state.as_str() },
                            if t.rest_only { Some("REST has no verified thread anchor") } else { reason }, t.is_resolved,
                            t.comments.first().map(|c| c.created_at.clone()).unwrap_or_else(|| now.clone()),
                            t.original_line, t.original_start_line, t.start_side.as_deref(),
                            t.viewer_can_resolve, t.viewer_can_unresolve
                        ],
                    )?;
                    id
                }
            };
            for c in &t.comments {
                let found: Option<String> = tx.query_row(
                    "SELECT c.id FROM review_comments c JOIN review_threads t ON t.id = c.thread_id
                     WHERE t.pr_id = ?1 AND ((?2 IS NOT NULL AND c.github_comment_id = ?2)
                        OR (?3 != '' AND c.github_node_id = ?3)) LIMIT 1",
                    params![pr_id, c.database_id, c.node_id], |r| r.get(0),
                ).optional()?;
                if let Some(id) = found {
                    tx.execute(
                        "UPDATE review_comments SET github_comment_id = COALESCE(?1, github_comment_id),
                         in_reply_to_id = COALESCE(?2, in_reply_to_id), source_status = 'available',
                         github_node_id = CASE WHEN ?3 = '' THEN github_node_id ELSE ?3 END,
                         body = CASE WHEN ?4 THEN body ELSE ?5 END,
                         author = CASE WHEN ?6 THEN author ELSE ?7 END,
                         updated_at = CASE WHEN ?8 THEN updated_at ELSE COALESCE(?9, updated_at) END,
                         github_updated_at = CASE WHEN ?8 THEN github_updated_at ELSE ?9 END,
                         author_node_id = CASE WHEN ?6 THEN author_node_id ELSE ?11 END,
                         author_type = CASE WHEN ?6 THEN author_type ELSE ?12 END
                         WHERE id = ?10 AND publish_state = 'imported'",
                        params![c.database_id, c.in_reply_to_id, c.node_id,
                            unavailable(&c.unavailable_fields, "body"), c.body,
                            unavailable(&c.unavailable_fields, "author"), c.author,
                            unavailable(&c.unavailable_fields, "updatedAt"), c.updated_at, id,
                            c.author_id, c.author_type],
                    )?;
                    continue;
                }
                tx.execute(
                    "INSERT INTO review_comments (id, thread_id, author_kind, author, body, publish_state, github_comment_id,
                        github_node_id, created_at, updated_at, seq, author_node_id, author_type, github_updated_at, in_reply_to_id)
                     VALUES (?1, ?2, 'github', ?3, ?4, 'imported', ?5, ?6, ?7, COALESCE(?8, ?7),
                        (SELECT COALESCE(MAX(seq), 0) + 1 FROM review_comments), ?9, ?10, ?8, ?14)
                     ON CONFLICT(github_node_id) DO UPDATE SET
                        body = CASE WHEN ?11 THEN review_comments.body ELSE excluded.body END,
                        updated_at = CASE WHEN ?12 THEN review_comments.updated_at ELSE excluded.updated_at END,
                        github_updated_at = CASE WHEN ?12 THEN review_comments.github_updated_at ELSE excluded.github_updated_at END,
                        author = CASE WHEN ?13 THEN review_comments.author ELSE excluded.author END,
                        author_node_id = CASE WHEN ?13 THEN review_comments.author_node_id ELSE excluded.author_node_id END,
                        author_type = CASE WHEN ?13 THEN review_comments.author_type ELSE excluded.author_type END",
                    params![new_id("cmt"), thread_id, c.author, c.body, c.database_id, c.node_id, c.created_at,
                        c.updated_at, c.author_id, c.author_type,
                        unavailable(&c.unavailable_fields, "body"), unavailable(&c.unavailable_fields, "updatedAt"),
                        unavailable(&c.unavailable_fields, "author"), c.in_reply_to_id],
                )?;
            }
        }
        if enumeration_complete {
            let seen: std::collections::HashSet<i64> = threads
                .iter()
                .flat_map(|t| &t.comments)
                .filter_map(|c| c.database_id)
                .collect();
            let mut stmt = tx.prepare(
                "SELECT c.id, c.github_comment_id FROM review_comments c
                JOIN review_threads t ON t.id = c.thread_id WHERE t.pr_id = ?1
                AND c.publish_state = 'imported' AND c.github_comment_id IS NOT NULL",
            )?;
            let known = stmt
                .query_map([pr_id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(stmt);
            for (id, remote) in known {
                if !seen.contains(&remote) {
                    tx.execute("UPDATE review_comments SET source_status = 'source_deleted_or_unavailable' WHERE id = ?1", [&id])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_publish_selection(
        &mut self,
        pr_id: &str,
        ids: &[String],
        selected: bool,
    ) -> Result<(Vec<String>, u64)> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut changed = Vec::new();
        for id in ids {
            let row: Option<(String, String, String, bool, bool)> = tx.query_row(
                "SELECT t.pr_id, c.publish_state, c.author_kind, c.publish_selected, EXISTS(SELECT 1 FROM publication_reservations WHERE comment_id = c.id) FROM review_comments c JOIN review_threads t ON t.id = c.thread_id WHERE c.id = ?1",
                [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            ).optional()?;
            let (owner, state, author, previous, reserved) =
                row.ok_or_else(|| NotFound(format!("comment {id}")))?;
            if owner != pr_id {
                return Err(NotFound(format!("comment {id}")).into());
            }
            if reserved {
                return Err(ReservedDraft.into());
            }
            if state != "draft" || author == "github" {
                anyhow::bail!("comment is not selectable");
            }
            if previous != selected {
                tx.execute(
                    "UPDATE review_comments SET publish_selected = ?1 WHERE id = ?2",
                    params![selected, id],
                )?;
                changed.push(id.clone());
            }
        }
        if !changed.is_empty() {
            tx.execute("UPDATE review_sessions SET selection_revision = selection_revision + 1 WHERE pr_id = ?1", [pr_id])?;
        }
        let revision = tx.query_row(
            "SELECT selection_revision FROM review_sessions WHERE pr_id = ?1",
            [pr_id],
            |r| r.get::<_, i64>(0),
        )? as u64;
        tx.commit()?;
        Ok((changed, revision))
    }

    pub fn plan_inputs(&self, pr_id: &str) -> Result<(Vec<ReviewThread>, u64, Vec<String>)> {
        let tx = self.conn.unchecked_transaction()?;
        let threads = self.list_threads(pr_id)?;
        let revision = tx.query_row(
            "SELECT selection_revision FROM review_sessions WHERE pr_id = ?1",
            [pr_id],
            |r| r.get::<_, i64>(0),
        )? as u64;
        let mut stmt = tx.prepare("SELECT r.comment_id FROM publication_reservations r JOIN review_comments c ON c.id = r.comment_id JOIN review_threads t ON t.id = c.thread_id WHERE t.pr_id = ?1")?;
        let reserved = stmt
            .query_map([pr_id], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        drop(stmt);
        tx.commit()?;
        Ok((threads, revision, reserved))
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
            "SELECT c.id, c.thread_id, c.author_kind, c.author, c.body, c.created_at, c.updated_at, c.publish_state, c.github_comment_id, c.publish_selected
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
                "SELECT id, thread_id, author_kind, author, body, created_at, updated_at, publish_state, github_comment_id, publish_selected
                 FROM review_comments WHERE id = ?1",
                [id],
                comment_from_row,
            )
            .optional()?
            .ok_or_else(|| NotFound(format!("comment {id}")).into())
    }

    pub fn update_comment_body(&mut self, id: &str, body: &str) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing: Option<(String, bool)> = tx.query_row("SELECT body, EXISTS(SELECT 1 FROM publication_reservations WHERE comment_id = ?1) FROM review_comments WHERE id = ?1 AND publish_state = 'draft'", [id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        if let Some((previous, false)) = existing.as_ref()
            && previous == body
        {
            return Ok(());
        }
        let n = tx.execute(
            "UPDATE review_comments SET body = ?1, updated_at = ?2 WHERE id = ?3 AND publish_state = 'draft'
             AND NOT EXISTS (SELECT 1 FROM publication_reservations WHERE comment_id = ?3)",
            params![body, now_rfc3339(), id],
        )?;
        if n != 1 {
            return Err(ReservedDraft.into());
        }
        tx.execute("UPDATE review_sessions SET selection_revision = selection_revision + 1 WHERE pr_id = (SELECT t.pr_id FROM review_comments c JOIN review_threads t ON t.id = c.thread_id WHERE c.id = ?1)", [id])?;
        tx.commit()?;
        Ok(())
    }

    /// Delete a draft comment, and its thread if that leaves the thread empty.
    pub fn delete_comment(&mut self, id: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        let (thread_id, pr_id): (String, String) = tx.query_row(
            "SELECT c.thread_id, t.pr_id FROM review_comments c JOIN review_threads t ON t.id = c.thread_id WHERE c.id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
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
        }
        tx.execute("UPDATE review_sessions SET selection_revision = selection_revision + 1 WHERE pr_id = ?1", [&pr_id])?;
        tx.commit()?;
        Ok(())
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

fn unavailable(fields: &[String], name: &str) -> bool {
    fields.iter().any(|field| field == "*" || field == name)
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
        publish_selected: r.get(9)?,
    })
}

fn source_str(s: ThreadSource) -> &'static str {
    match s {
        ThreadSource::Local => "local",
        ThreadSource::Github => "github",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn github_metadata_roundtrips_and_refreshes() {
        let mut store = Store::open_in_memory().unwrap();
        let repo = store
            .resolve_repository("github.com", "o", "r", None)
            .unwrap();
        let pr = store
            .upsert_pull_request(&repo.id, 1, "title", "url")
            .unwrap();
        let thread: crate::model::GithubThread = serde_json::from_value(json!({
            "nodeId": "thread-node", "path": "src/a.rs", "side": "RIGHT", "line": 10,
            "startLine": 9, "originalLine": 7, "originalStartLine": 6,
            "startSide": "LEFT", "viewerCanResolve": true, "viewerCanUnresolve": false,
            "isOutdated": false, "isResolved": false,
            "comments": [{"nodeId": "comment-node", "databaseId": 42, "author": "alice",
                "authorNodeId": "user-node", "authorType": "User", "body": "first",
                "createdAt": "2024-01-01T00:00:00Z", "updatedAt": "2024-01-02T00:00:00Z"}]
        }))
        .unwrap();
        store
            .sync_github_threads(&pr, "head", std::slice::from_ref(&thread), false)
            .unwrap();
        type SavedMetadata = (
            Option<u32>,
            Option<u32>,
            Option<String>,
            Option<bool>,
            Option<bool>,
        );
        let saved: SavedMetadata = store.conn.query_row(
            "SELECT original_line, original_start_line, start_side, viewer_can_resolve, viewer_can_unresolve FROM review_threads WHERE github_thread_id = ?1",
            ["thread-node"], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        ).unwrap();
        assert_eq!(
            saved,
            (
                Some(7),
                Some(6),
                Some("LEFT".into()),
                Some(true),
                Some(false)
            )
        );
        let comments = store.list_threads(&pr).unwrap();
        assert_eq!(comments[0].comments[0].updated_at, "2024-01-02T00:00:00Z");
        let meta: (Option<String>, Option<String>, Option<String>) = store.conn.query_row(
            "SELECT author_node_id, author_type, github_updated_at FROM review_comments WHERE github_node_id = ?1",
            ["comment-node"], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        ).unwrap();
        assert_eq!(
            meta,
            (
                Some("user-node".into()),
                Some("User".into()),
                Some("2024-01-02T00:00:00Z".into())
            )
        );

        let mut updated = thread.clone();
        updated.original_start_line = Some(5);
        updated.viewer_can_resolve = Some(false);
        updated.comments[0].body = "edited".into();
        updated.comments[0].author_id = Some("new-user".into());
        updated.comments[0].author_type = Some("Bot".into());
        updated.comments[0].updated_at = Some("2024-01-03T00:00:00Z".into());
        store
            .sync_github_threads(&pr, "head", &[updated], false)
            .unwrap();
        let threads = store.list_threads(&pr).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].comments.len(), 1);
        assert_eq!(threads[0].comments[0].body, "edited");
        assert_eq!(threads[0].comments[0].updated_at, "2024-01-03T00:00:00Z");
        let comment_meta: (Option<String>, Option<String>, Option<String>) = store.conn.query_row(
            "SELECT author_node_id, author_type, github_updated_at FROM review_comments WHERE github_node_id = 'comment-node'",
            [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        ).unwrap();
        assert_eq!(
            comment_meta,
            (
                Some("new-user".into()),
                Some("Bot".into()),
                Some("2024-01-03T00:00:00Z".into())
            )
        );
        let refreshed: (Option<u32>, Option<bool>) = store.conn.query_row(
            "SELECT original_start_line, viewer_can_resolve FROM review_threads WHERE github_thread_id = ?1",
            ["thread-node"], |r| Ok((r.get(0)?, r.get(1)?))
        ).unwrap();
        assert_eq!(refreshed, (Some(5), Some(false)));

        let mut unavailable = thread.clone();
        unavailable.original_start_line = None;
        unavailable.viewer_can_resolve = None;
        unavailable.unavailable_fields =
            vec!["originalStartLine".into(), "viewerCanResolve".into()];
        store
            .sync_github_threads(&pr, "head", &[unavailable], false)
            .unwrap();
        let preserved: (Option<u32>, Option<bool>) = store.conn.query_row(
            "SELECT original_start_line, viewer_can_resolve FROM review_threads WHERE github_thread_id = ?1",
            ["thread-node"], |r| Ok((r.get(0)?, r.get(1)?))
        ).unwrap();
        assert_eq!(preserved, (Some(5), Some(false)));

        let mut cleared = thread;
        cleared.original_start_line = None;
        cleared.viewer_can_resolve = None;
        store
            .sync_github_threads(&pr, "head", &[cleared], false)
            .unwrap();
        let cleared_meta: (Option<u32>, Option<bool>) = store.conn.query_row(
            "SELECT original_start_line, viewer_can_resolve FROM review_threads WHERE github_thread_id = ?1",
            ["thread-node"], |r| Ok((r.get(0)?, r.get(1)?))
        ).unwrap();
        assert_eq!(cleared_meta, (None, None));
    }

    #[test]
    fn rest_first_comment_recovers_graphql_metadata() {
        let mut store = Store::open_in_memory().unwrap();
        let repo = store
            .resolve_repository("github.com", "o", "r", None)
            .unwrap();
        let pr = store
            .upsert_pull_request(&repo.id, 1, "title", "url")
            .unwrap();
        let graphql: crate::model::GithubThread = serde_json::from_value(json!({
            "nodeId": "thread-node", "path": "src/a.rs", "side": "RIGHT",
            "isOutdated": false, "isResolved": false,
            "comments": [{"nodeId": "comment-node", "databaseId": 42, "author": "alice",
                "authorNodeId": "user-node", "authorType": "User", "body": "first",
                "createdAt": "2024-01-01T00:00:00Z", "updatedAt": "2024-01-02T00:00:00Z"}]
        }))
        .unwrap();
        let mut rest = graphql.clone();
        rest.node_id = "rest:42".into();
        rest.rest_only = true;
        rest.comments[0].author_id = None;
        rest.comments[0].author_type = None;
        rest.comments[0].updated_at = None;
        store
            .sync_github_threads(&pr, "head", &[rest], false)
            .unwrap();
        store
            .sync_github_threads(&pr, "head", &[graphql], true)
            .unwrap();
        let threads = store.list_threads(&pr).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].comments.len(), 1);
        assert_eq!(threads[0].github_thread_id.as_deref(), Some("thread-node"));
        let meta: (Option<String>, Option<String>, Option<String>) = store.conn.query_row(
            "SELECT author_node_id, author_type, github_updated_at FROM review_comments WHERE github_node_id = 'comment-node'",
            [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        ).unwrap();
        assert_eq!(
            meta,
            (
                Some("user-node".into()),
                Some("User".into()),
                Some("2024-01-02T00:00:00Z".into())
            )
        );
    }

    #[test]
    fn unavailable_path_and_comment_metadata_keep_prior_values() {
        let mut store = Store::open_in_memory().unwrap();
        let repo = store
            .resolve_repository("github.com", "o", "r", None)
            .unwrap();
        let pr = store
            .upsert_pull_request(&repo.id, 1, "title", "url")
            .unwrap();
        let original: crate::model::GithubThread = serde_json::from_value(json!({
            "nodeId": "thread-node", "path": "src/original.rs", "side": "RIGHT",
            "isOutdated": false, "isResolved": false,
            "comments": [{"nodeId": "comment-node", "databaseId": 42, "author": "alice",
                "authorNodeId": "user-node", "authorType": "User", "body": "first",
                "createdAt": "2024-01-01T00:00:00Z", "updatedAt": "2024-01-02T00:00:00Z"}]
        }))
        .unwrap();
        store
            .sync_github_threads(&pr, "head", std::slice::from_ref(&original), true)
            .unwrap();
        for fields in [vec!["path", "author", "updatedAt"], vec!["*", "*"]] {
            let mut unavailable = original.clone();
            unavailable.path = String::new();
            unavailable.unavailable_fields = vec![fields[0].into()];
            unavailable.comments[0].author_id = None;
            unavailable.comments[0].author_type = None;
            unavailable.comments[0].updated_at = None;
            unavailable.comments[0].unavailable_fields =
                fields[1..].iter().map(|s| (*s).into()).collect();
            store
                .sync_github_threads(&pr, "head", &[unavailable], false)
                .unwrap();
            assert_eq!(store.list_threads(&pr).unwrap()[0].path, "src/original.rs");
            let meta: (Option<String>, Option<String>, Option<String>) = store.conn.query_row(
                "SELECT author_node_id, author_type, github_updated_at FROM review_comments WHERE github_node_id = 'comment-node'",
                [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            ).unwrap();
            assert_eq!(
                meta,
                (
                    Some("user-node".into()),
                    Some("User".into()),
                    Some("2024-01-02T00:00:00Z".into())
                )
            );
        }
    }

    #[test]
    fn v2_database_migrates_optional_metadata_without_guessing_values() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        for sql in &super::super::schema::MIGRATIONS[..2] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 2).unwrap();
        let store = Store::init(conn).unwrap();
        let version: i64 = store
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, super::super::schema::MIGRATIONS.len() as i64);
        let columns: Vec<String> = store
            .conn
            .prepare("PRAGMA table_info(review_comments)")
            .unwrap()
            .query_map([], |r| r.get(1))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(columns.contains(&"github_updated_at".to_string()));
        assert!(columns.contains(&"author_node_id".to_string()));
    }

    #[test]
    fn legacy_github_metadata_defaults_to_none() {
        let thread: crate::model::GithubThread = serde_json::from_value(json!({
            "nodeId": "t", "path": "a", "side": null, "line": null, "startLine": null,
            "originalLine": null, "isOutdated": false, "isResolved": false,
            "comments": [{"nodeId": "c", "databaseId": null, "author": "ghost",
                "body": "body", "createdAt": "2024-01-01T00:00:00Z"}]
        }))
        .unwrap();
        assert_eq!(thread.viewer_can_resolve, None);
        assert_eq!(thread.original_start_line, None);
        assert_eq!(thread.comments[0].author_id, None);
        assert_eq!(thread.comments[0].updated_at, None);
    }
}
