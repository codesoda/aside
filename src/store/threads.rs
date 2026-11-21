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
        enumeration_complete: bool,
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
                .optional()?
                .or_else(|| {
                    // Promote a REST-only synthetic thread when GraphQL supplies
                    // the real thread ID. Never adopt a local draft thread.
                    t.comments.iter().filter_map(|c| c.database_id).find_map(|id| {
                        tx.query_row(
                            "SELECT t.id FROM review_threads t JOIN review_comments c ON c.thread_id = t.id
                             WHERE t.pr_id = ?1 AND t.source = 'github'
                               AND (?3 = 1 OR t.github_thread_id LIKE 'rest:%')
                               AND c.github_comment_id = ?2 LIMIT 1",
                            params![pr_id, id, t.rest_only], |r| r.get(0),
                        ).optional().ok().flatten()
                    })
                });
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
                            "UPDATE review_threads SET github_thread_id = ?1, path = ?2, side = COALESCE(?3, side), resolved = ?4, line = ?5, start_line = ?6,
                             anchor_state = ?7, anchor_reason = ?8, updated_at = ?9 WHERE id = ?10 AND source = 'github'",
                            params![t.node_id, t.path, t.side.map(Side::as_str), t.is_resolved, t.line.or(t.original_line), t.start_line, state.as_str(), reason, now, id],
                        )?;
                    }
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
                            t.line.or(t.original_line), t.start_line, head_sha,
                            if t.rest_only { AnchorState::Orphaned.as_str() } else { state.as_str() },
                            if t.rest_only { Some("REST has no verified thread anchor") } else { reason },
                            t.is_resolved,
                            t.comments.first().map(|c| c.created_at.clone()).unwrap_or_else(|| now.clone())
                        ],
                    )?;
                    id
                }
            };
            for c in &t.comments {
                let found: Option<String> = tx.query_row(
                    "SELECT c.id FROM review_comments c JOIN review_threads t ON t.id = c.thread_id
                     WHERE t.pr_id = ?1 AND ((?2 IS NOT NULL AND c.github_comment_id = ?2)
                         OR (?3 != '' AND c.github_node_id = ?3))
                     ORDER BY CASE WHEN c.github_comment_id = ?2 THEN 0 ELSE 1 END LIMIT 1",
                    params![pr_id, c.database_id, c.node_id], |r| r.get(0),
                ).optional()?;
                if let Some(id) = found {
                    // A published local comment keeps its local identity, but
                    // gains the remote author and aliases once observed.
                    tx.execute(
                        "UPDATE review_comments SET thread_id = ?1, author_kind = 'github', author = ?2,
                         body = ?3, publish_state = 'imported', github_comment_id = COALESCE(?4, github_comment_id),
                         github_node_id = COALESCE(NULLIF(?5, ''), github_node_id), updated_at = ?6,
                         in_reply_to_id = COALESCE(?7, in_reply_to_id), source_status = 'available'
                         WHERE id = ?8 AND publish_state != 'draft'",
                        params![thread_id, c.author, c.body, c.database_id, c.node_id,
                            c.updated_at.as_deref().unwrap_or(&now), c.in_reply_to_id, id],
                    )?;
                } else {
                    tx.execute(
                        "INSERT INTO review_comments (id, thread_id, author_kind, author, body, publish_state,
                         github_comment_id, github_node_id, created_at, updated_at, seq, in_reply_to_id)
                         VALUES (?1, ?2, 'github', ?3, ?4, 'imported', ?5, NULLIF(?6, ''), ?7, ?8,
                           (SELECT COALESCE(MAX(seq), 0) + 1 FROM review_comments), ?9)",
                        params![new_id("cmt"), thread_id, c.author, c.body, c.database_id, c.node_id,
                            c.created_at, c.updated_at.as_deref().unwrap_or(&c.created_at), c.in_reply_to_id],
                    )?;
                }
            }
        }
        // Missing remote comments can only be removed after both APIs and
        // all pages were enumerated. Drafts are never eligible.
        if enumeration_complete && threads.iter().all(|t| t.enumeration_complete) {
            let mut seen = std::collections::HashSet::new();
            for t in threads {
                for c in &t.comments {
                    if let Some(id) = c.database_id {
                        seen.insert(id);
                    }
                }
            }
            let mut stmt = tx.prepare(
                "SELECT c.id, c.github_comment_id FROM review_comments c JOIN review_threads t ON t.id = c.thread_id
                 WHERE t.pr_id = ?1 AND t.source = 'github' AND c.publish_state = 'imported'",
            )?;
            let known = stmt
                .query_map([pr_id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(stmt);
            for (id, remote_id) in known {
                if remote_id.is_some_and(|n| !seen.contains(&n)) {
                    tx.execute("UPDATE review_comments SET source_status = 'source_deleted_or_unavailable' WHERE id = ?1", [&id])?;
                }
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
             FROM review_comments c JOIN review_threads t ON t.id = c.thread_id WHERE t.pr_id = ?1
             ORDER BY CASE WHEN c.publish_state = 'imported' THEN 0 ELSE 1 END,
                      CASE WHEN c.publish_state = 'imported' THEN c.created_at END, c.seq",
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
            "UPDATE review_threads SET anchor_state = ?1, anchor_reason = ?2, updated_at = ?3
             WHERE id = ?4 AND (github_thread_id IS NULL OR github_thread_id NOT LIKE 'rest:%')",
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

    /// Delete a draft comment, and its thread if that leaves the thread empty.
    pub fn delete_comment(&mut self, id: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        let thread_id: String = tx.query_row(
            "SELECT thread_id FROM review_comments WHERE id = ?1",
            [id],
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
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GithubComment, GithubThread};

    fn comment(id: i64, author: &str) -> GithubComment {
        GithubComment {
            node_id: format!("N{id}"),
            database_id: Some(id),
            author: author.into(),
            body: "same text".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: None,
            in_reply_to_id: None,
        }
    }

    fn thread(
        id: &str,
        rest_only: bool,
        complete: bool,
        comments: Vec<GithubComment>,
    ) -> GithubThread {
        GithubThread {
            node_id: id.into(),
            path: "a.rs".into(),
            side: Some(Side::Right),
            line: if rest_only { None } else { Some(3) },
            start_line: None,
            original_line: None,
            is_outdated: false,
            is_resolved: false,
            comments,
            rest_only,
            enumeration_complete: complete,
        }
    }

    #[test]
    fn recovered_imports_sort_by_creation_without_reordering_drafts() -> Result<()> {
        let mut s = Store::open_in_memory()?;
        let repo = s.resolve_repository("github.com", "o", "r", None)?;
        let pr = s.upsert_pull_request(&repo.id, 1, "PR", "url")?;
        let mut newest = comment(3, "newest");
        newest.created_at = "2026-01-03T00:00:00Z".into();
        s.sync_github_threads(
            &pr,
            "head",
            &[thread("G", false, false, vec![newest.clone()])],
            false,
        )?;
        let thread_id = s.list_threads(&pr)?[0].id.clone();
        for id in ["draft-a", "draft-b"] {
            s.insert_comment(
                &Comment {
                    id: id.into(),
                    thread_id: thread_id.clone(),
                    author_kind: Actor::Human,
                    author: "me".into(),
                    body: id.into(),
                    publish_state: PublishState::Draft,
                    github_comment_id: None,
                    created_at: "now".into(),
                    updated_at: "now".into(),
                },
                None,
            )?;
        }
        let mut oldest = comment(1, "oldest");
        oldest.created_at = "2026-01-01T00:00:00Z".into();
        let mut middle = comment(2, "middle");
        middle.created_at = "2026-01-02T00:00:00Z".into();
        let full = thread(
            "G",
            false,
            true,
            vec![oldest.clone(), middle.clone(), newest.clone()],
        );
        s.sync_github_threads(&pr, "head", std::slice::from_ref(&full), true)?;
        let first = s.list_threads(&pr)?[0].comments.clone();
        assert_eq!(
            first.iter().map(|c| c.author.as_str()).collect::<Vec<_>>(),
            vec!["oldest", "middle", "newest", "me", "me"]
        );
        let ids: Vec<_> = first.iter().map(|c| c.id.clone()).collect();
        assert_eq!(&ids[3..], &["draft-a", "draft-b"]);
        s.sync_github_threads(&pr, "head", &[full], true)?;
        assert_eq!(
            s.list_threads(&pr)?[0]
                .comments
                .iter()
                .map(|c| c.id.clone())
                .collect::<Vec<_>>(),
            ids
        );
        Ok(())
    }

    #[test]
    fn published_database_id_reconciles_without_touching_drafts() -> Result<()> {
        let mut s = Store::open_in_memory()?;
        let repo = s.resolve_repository("github.com", "o", "r", None)?;
        let pr = s.upsert_pull_request(&repo.id, 1, "PR", "url")?;
        s.insert_thread(
            &pr,
            &NewThread {
                id: "local".into(),
                source: ThreadSource::Local,
                github_thread_id: None,
                path: "a.rs".into(),
                side: Side::Right,
                line: Some(5),
                start_line: None,
                line_text: None,
                diff_digest: None,
                head_sha: "head".into(),
                anchor_state: AnchorState::Current,
                anchor_reason: None,
                resolved: false,
                created_at: None,
            },
        )?;
        for (id, state, remote) in [
            ("published", PublishState::Published, Some(42)),
            ("draft", PublishState::Draft, None),
        ] {
            s.insert_comment(
                &Comment {
                    id: id.into(),
                    thread_id: "local".into(),
                    author_kind: Actor::Human,
                    author: "me".into(),
                    body: "original".into(),
                    publish_state: state,
                    github_comment_id: remote,
                    created_at: "now".into(),
                    updated_at: "now".into(),
                },
                None,
            )?;
        }
        s.sync_github_threads(
            &pr,
            "head",
            &[thread(
                "G",
                false,
                false,
                vec![comment(42, "github-author")],
            )],
            false,
        )?;
        let threads = s.list_threads(&pr)?;
        assert_eq!(threads.len(), 2);
        let imported = threads
            .iter()
            .find(|t| t.github_thread_id.as_deref() == Some("G"))
            .unwrap();
        assert_eq!(imported.comments.len(), 1);
        assert_eq!(imported.comments[0].id, "published");
        assert_eq!(imported.comments[0].author, "github-author");
        let local = threads.iter().find(|t| t.id == "local").unwrap();
        assert_eq!(local.comments.len(), 1);
        assert_eq!(local.comments[0].id, "draft");
        assert_eq!(local.line, Some(5));
        Ok(())
    }

    #[test]
    fn rest_failure_keeps_graphql_identity_and_drafts() -> Result<()> {
        let mut s = Store::open_in_memory()?;
        let repo = s.resolve_repository("github.com", "o", "r", None)?;
        let pr = s.upsert_pull_request(&repo.id, 1, "PR", "url")?;
        let graph = thread("G", false, false, vec![comment(1, "alice")]);
        s.sync_github_threads(&pr, "head", std::slice::from_ref(&graph), false)?;
        let id = s.list_threads(&pr)?[0].id.clone();
        s.insert_comment(
            &Comment {
                id: "draft".into(),
                thread_id: id.clone(),
                author_kind: Actor::Human,
                author: "me".into(),
                body: "draft".into(),
                publish_state: PublishState::Draft,
                github_comment_id: None,
                created_at: "now".into(),
                updated_at: "now".into(),
            },
            None,
        )?;
        let mut fallback = thread("rest:1", true, false, vec![comment(1, "alice")]);
        fallback.path = "other.rs".into();
        s.sync_github_threads(&pr, "head", &[fallback], false)?;
        let threads = s.list_threads(&pr)?;
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].id, id);
        assert_eq!(threads[0].github_thread_id.as_deref(), Some("G"));
        assert_eq!(threads[0].path, "a.rs");
        assert_eq!(threads[0].line, Some(3));
        assert_eq!(threads[0].comments.len(), 2);
        assert!(threads[0].comments.iter().any(|c| c.id == "draft"));
        Ok(())
    }

    #[test]
    fn complete_empty_marks_imported_unavailable_but_keeps_published() -> Result<()> {
        let mut s = Store::open_in_memory()?;
        let repo = s.resolve_repository("github.com", "o", "r", None)?;
        let pr = s.upsert_pull_request(&repo.id, 1, "PR", "url")?;
        s.sync_github_threads(
            &pr,
            "head",
            &[thread("G", false, false, vec![comment(1, "alice")])],
            false,
        )?;
        let imported = s.list_threads(&pr)?[0].comments[0].id.clone();
        s.sync_github_threads(&pr, "head", &[], false)?;
        let status = |s: &Store, id: &str| -> Result<String> {
            Ok(s.conn.query_row(
                "SELECT source_status FROM review_comments WHERE id = ?1",
                [id],
                |r| r.get(0),
            )?)
        };
        assert_eq!(status(&s, &imported)?, "available");
        s.sync_github_threads(&pr, "head", &[], true)?;
        assert_eq!(status(&s, &imported)?, "source_deleted_or_unavailable");
        assert_eq!(s.list_threads(&pr)?[0].comments[0].id, imported);
        s.sync_github_threads(
            &pr,
            "head",
            &[thread("G", false, true, vec![comment(1, "alice")])],
            true,
        )?;
        assert_eq!(status(&s, &imported)?, "available");
        Ok(())
    }

    #[test]
    fn partial_graphql_refresh_retains_rest_reply_parent() -> Result<()> {
        let mut s = Store::open_in_memory()?;
        let repo = s.resolve_repository("github.com", "o", "r", None)?;
        let pr = s.upsert_pull_request(&repo.id, 1, "PR", "url")?;
        let mut reply = comment(2, "bob");
        reply.in_reply_to_id = Some(1);
        s.sync_github_threads(
            &pr,
            "head",
            &[thread(
                "G",
                false,
                true,
                vec![comment(1, "alice"), reply.clone()],
            )],
            true,
        )?;
        reply.in_reply_to_id = None;
        s.sync_github_threads(
            &pr,
            "head",
            &[thread("G", false, false, vec![reply])],
            false,
        )?;
        let parent: Option<i64> = s.conn.query_row(
            "SELECT in_reply_to_id FROM review_comments WHERE github_comment_id = 2",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(parent, Some(1));
        Ok(())
    }

    #[test]
    fn aliases_promotion_edits_and_partial_deletion() -> Result<()> {
        let mut s = Store::open_in_memory()?;
        let repo = s.resolve_repository("github.com", "o", "r", None)?;
        let pr = s.upsert_pull_request(&repo.id, 1, "PR", "url")?;
        let initial = thread(
            "rest:1",
            true,
            false,
            vec![comment(1, "alice"), comment(2, "bob")],
        );
        s.sync_github_threads(&pr, "head", &[initial], false)?;
        let original = s.list_threads(&pr)?;
        assert_eq!(original[0].anchor_state, AnchorState::Orphaned);
        assert_eq!(original[0].comments.len(), 2);
        assert_eq!(original[0].comments[0].author, "alice");
        assert_eq!(original[0].comments[1].author, "bob");
        let ids: Vec<_> = original[0].comments.iter().map(|c| c.id.clone()).collect();

        let mut edited = comment(1, "renamed");
        edited.body = "edited".into();
        let mut partial = thread("G1", false, false, vec![edited.clone()]);
        partial.path = "renamed.rs".into();
        partial.side = Some(Side::Left);
        s.sync_github_threads(&pr, "head", &[partial], false)?;
        let promoted = s.list_threads(&pr)?;
        assert_eq!(promoted.len(), 1);
        assert_eq!(promoted[0].github_thread_id.as_deref(), Some("G1"));
        assert_eq!(promoted[0].anchor_state, AnchorState::Current);
        assert_eq!(promoted[0].path, "renamed.rs");
        assert_eq!(promoted[0].side, Side::Left);
        assert_eq!(
            promoted[0].comments.len(),
            2,
            "partial enumeration cannot delete bob"
        );
        assert_eq!(promoted[0].comments[0].id, ids[0]);
        assert_eq!(promoted[0].comments[0].author, "renamed");
        assert_eq!(promoted[0].comments[0].body, "edited");

        let mut resolved = thread("G1", false, true, vec![edited]);
        resolved.is_resolved = true;
        s.sync_github_threads(&pr, "head", &[resolved], true)?;
        let final_threads = s.list_threads(&pr)?;
        assert!(final_threads[0].resolved);
        assert_eq!(final_threads[0].comments.len(), 2);
        let status: String = s.conn.query_row(
            "SELECT source_status FROM review_comments WHERE id = ?1",
            [&ids[1]],
            |r| r.get(0),
        )?;
        assert_eq!(status, "source_deleted_or_unavailable");
        Ok(())
    }
}
