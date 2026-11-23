use anyhow::{Result, bail};
use rusqlite::{OptionalExtension, params};

use super::{NotFound, ReservedDraft, Store};

type DraftHistory = (String, Option<ReviewCommentDraft>, Vec<PublishedComment>);
use crate::model::{Actor, Comment, PublishState, PublishedComment, ReviewCommentDraft};
use crate::util::{new_id, now_rfc3339};

impl Store {
    /// Atomically append a human private message and reserve its agent turn.
    /// A pending turn rejects later replies rather than orphaning them.
    pub fn insert_human_request(&mut self, comment: &Comment) -> Result<bool> {
        if comment.author_kind != Actor::Human || comment.publish_state != PublishState::Private {
            bail!("human request must be a private human comment");
        }
        let tx = self.conn.transaction()?;
        let now = now_rfc3339();
        let changed = tx.execute(
            "UPDATE review_threads SET agent_state = 'waiting', agent_request_comment_id = ?2, agent_error = NULL, agent_updated_at = ?3
             WHERE id = ?1 AND agent_state NOT IN ('waiting', 'responding')",
            params![comment.thread_id, comment.id, now],
        )?;
        if changed != 1 {
            return Ok(false);
        }
        let inserted = tx.execute("INSERT OR IGNORE INTO agent_request_history (comment_id, thread_id, state, created_at, updated_at) VALUES (?1, ?2, 'waiting', ?3, ?3)", params![comment.id, comment.thread_id, now])?;
        if inserted != 1 {
            return Ok(false);
        }
        insert_private_comment(&tx, comment)?;
        tx.commit()?;
        Ok(true)
    }

    /// Consume exactly one waiting turn and append its reply as one transaction.
    /// A failed insert rolls the state back so retry remains possible.
    pub fn insert_agent_response(
        &mut self,
        thread_id: &str,
        request_comment_id: &str,
        comment: &Comment,
    ) -> Result<bool> {
        if comment.author_kind != Actor::Agent
            || comment.publish_state != PublishState::Private
            || comment.thread_id != thread_id
        {
            bail!("agent response must be a private agent comment in this thread");
        }
        let tx = self.conn.transaction()?;
        let n = tx.execute(
            "UPDATE review_threads SET agent_state = 'responding', agent_updated_at = ?3
             WHERE id = ?1 AND agent_request_comment_id = ?2 AND agent_state = 'waiting'",
            params![thread_id, request_comment_id, now_rfc3339()],
        )?;
        if n != 1 {
            return Ok(false);
        }
        let consumed = tx.execute("UPDATE agent_request_history SET state = 'completed', updated_at = ?3 WHERE comment_id = ?1 AND thread_id = ?2 AND state = 'waiting'", params![request_comment_id, thread_id, now_rfc3339()])?;
        if consumed != 1 {
            return Ok(false);
        }
        insert_private_comment(&tx, comment)?;
        tx.execute("UPDATE review_threads SET agent_state = 'idle', agent_error = NULL, agent_updated_at = ?2 WHERE id = ?1", params![thread_id, now_rfc3339()])?;
        tx.commit()?;
        Ok(true)
    }

    /// Create or replace the one open outgoing review comment. Only the human-facing
    /// caller may invoke this; an agent response is always a private message.
    pub fn upsert_review_comment(
        &mut self,
        pr_id: &str,
        thread_id: &str,
        body: &str,
        source_comment_id: Option<&str>,
    ) -> Result<ReviewCommentDraft> {
        if body.trim().is_empty() {
            bail!("review comment must not be empty");
        }
        let tx = self.conn.transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM review_threads WHERE id = ?1 AND pr_id = ?2)",
            params![thread_id, pr_id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(NotFound(format!("thread {thread_id}")).into());
        }
        if let Some(source) = source_comment_id {
            let owned: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM review_comments WHERE id = ?1 AND thread_id = ?2)",
                params![source, thread_id],
                |r| r.get(0),
            )?;
            if !owned {
                return Err(NotFound(format!("comment {source}")).into());
            }
        }
        let existing: Option<(String, Option<String>)> = tx.query_row("SELECT id, reserved_attempt_id FROM review_comment_drafts WHERE thread_id = ?1 AND state = 'draft'", [thread_id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        let now = now_rfc3339();
        let id = if let Some((id, reserved)) = existing {
            if reserved.is_some() {
                return Err(ReservedDraft.into());
            }
            tx.execute("UPDATE review_comment_drafts SET body = ?1, source_comment_id = ?2, updated_at = ?3 WHERE id = ?4 AND state = 'draft' AND reserved_attempt_id IS NULL", params![body, source_comment_id, now, id])?;
            id
        } else {
            let id = new_id("draft");
            tx.execute("INSERT INTO review_comment_drafts (id, thread_id, body, state, source_comment_id, created_at, updated_at) VALUES (?1, ?2, ?3, 'draft', ?4, ?5, ?5)", params![id, thread_id, body, source_comment_id, now])?;
            id
        };
        tx.execute("UPDATE review_sessions SET selection_revision = selection_revision + 1 WHERE pr_id = ?1", [pr_id])?;
        let selected: bool = tx.query_row(
            "SELECT publish_selected FROM review_comment_drafts WHERE id = ?1",
            [&id],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(ReviewCommentDraft {
            id,
            body: body.into(),
            source_comment_id: source_comment_id.map(str::to_owned),
            updated_at: now,
            locked: false,
            selected,
            target: None,
            blocker: None,
        })
    }

    pub fn clear_review_comment(&mut self, pr_id: &str, thread_id: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM review_threads WHERE id = ?1 AND pr_id = ?2)",
            params![thread_id, pr_id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(NotFound(format!("thread {thread_id}")).into());
        }
        let locked: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM review_comment_drafts WHERE thread_id = ?1 AND state = 'draft' AND reserved_attempt_id IS NOT NULL)", [thread_id], |r| r.get(0))?;
        if locked {
            return Err(ReservedDraft.into());
        }
        let deleted = tx.execute("DELETE FROM review_comment_drafts WHERE thread_id = ?1 AND state = 'draft' AND reserved_attempt_id IS NULL", [thread_id])?;
        if deleted != 0 {
            tx.execute("UPDATE review_sessions SET selection_revision = selection_revision + 1 WHERE pr_id = ?1", [pr_id])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn find_thread_by_finding(&self, pr_id: &str, finding_id: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT id FROM review_threads WHERE pr_id = ?1 AND finding_id = ?2 AND source = 'local'", params![pr_id, finding_id], |r| r.get(0)).optional()?)
    }

    /// Atomic deduplication for a previously inserted private human message.
    #[allow(dead_code)] // Retained for callers that schedule an existing message.
    pub fn open_agent_request(&mut self, thread_id: &str, comment_id: &str) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let now = now_rfc3339();
        let n = tx.execute("UPDATE review_threads SET agent_state = 'waiting', agent_request_comment_id = ?2, agent_error = NULL, agent_updated_at = ?3 WHERE id = ?1
            AND agent_state NOT IN ('waiting', 'responding')
            AND EXISTS (SELECT 1 FROM review_comments WHERE id = ?2 AND thread_id = ?1 AND author_kind = 'human' AND publish_state = 'private')",
            params![thread_id, comment_id, now])?;
        if n != 1 {
            return Ok(false);
        }
        let inserted = tx.execute("INSERT OR IGNORE INTO agent_request_history (comment_id, thread_id, state, created_at, updated_at) VALUES (?1, ?2, 'waiting', ?3, ?3)", params![comment_id, thread_id, now])?;
        if inserted != 1 {
            return Ok(false);
        }
        tx.commit()?;
        Ok(true)
    }

    pub fn transition_agent_request(
        &mut self,
        thread_id: &str,
        comment_id: &str,
        from: crate::model::AgentState,
        to: crate::model::AgentState,
        error: Option<&str>,
    ) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let n = tx.execute(
            "UPDATE review_threads SET agent_state = ?4, agent_error = ?5, agent_updated_at = ?6
            WHERE id = ?1 AND agent_request_comment_id = ?2 AND agent_state = ?3",
            params![
                thread_id,
                comment_id,
                from.as_str(),
                to.as_str(),
                error,
                now_rfc3339()
            ],
        )?;
        if n != 1 {
            return Ok(false);
        }
        let history_state = match to {
            crate::model::AgentState::Waiting => "waiting",
            crate::model::AgentState::Responding => "responding",
            crate::model::AgentState::Failed => "failed",
            crate::model::AgentState::Idle | crate::model::AgentState::NoAgent => "completed",
        };
        tx.execute("UPDATE agent_request_history SET state = ?3, updated_at = ?4 WHERE comment_id = ?1 AND thread_id = ?2", params![comment_id, thread_id, history_state, now_rfc3339()])?;
        tx.commit()?;
        Ok(true)
    }

    /// Mark only comments already persisted through this ID read. A stale read
    /// cannot skip over a later message from another participant.
    pub fn mark_thread_read(&mut self, thread_id: &str, through_comment_id: &str) -> Result<bool> {
        let n = self.conn.execute("UPDATE review_threads SET human_read_seq = MAX(human_read_seq, (SELECT seq FROM review_comments WHERE id = ?2 AND thread_id = ?1))
            WHERE id = ?1 AND EXISTS (SELECT 1 FROM review_comments WHERE id = ?2 AND thread_id = ?1)", params![thread_id, through_comment_id])?;
        Ok(n == 1)
    }

    pub(super) fn conversation_drafts(&self, pr_id: &str) -> Result<Vec<DraftHistory>> {
        let mut stmt = self.conn.prepare("SELECT d.thread_id, d.id, d.body, d.source_comment_id, d.updated_at, d.reserved_attempt_id, d.state, d.github_comment_id, d.github_review_id, d.published_at, d.publish_selected
            FROM review_comment_drafts d JOIN review_threads t ON t.id = d.thread_id WHERE t.pr_id = ?1 ORDER BY d.created_at, d.id")?;
        let mut entries: Vec<DraftHistory> = Vec::new();
        for row in stmt.query_map([pr_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Option<i64>>(7)?,
                r.get::<_, Option<i64>>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, bool>(10)?,
            ))
        })? {
            let (
                thread,
                id,
                body,
                source_comment_id,
                updated_at,
                reservation,
                state,
                github_comment_id,
                github_review_id,
                published_at,
                selected,
            ) = row?;
            let index = if let Some(i) = entries.iter().position(|entry| entry.0 == thread) {
                i
            } else {
                entries.push((thread, None, Vec::new()));
                entries.len() - 1
            };
            if state == "draft" {
                entries[index].1 = Some(ReviewCommentDraft {
                    id,
                    body,
                    source_comment_id,
                    updated_at,
                    locked: reservation.is_some(),
                    selected,
                    target: None,
                    blocker: None,
                });
            } else if let Some(published_at) = published_at {
                entries[index].2.push(PublishedComment {
                    id,
                    body,
                    github_comment_id,
                    github_review_id,
                    published_at,
                });
            }
        }
        Ok(entries)
    }
}

fn insert_private_comment(tx: &rusqlite::Transaction<'_>, comment: &Comment) -> Result<()> {
    let seq: i64 = tx.query_row("UPDATE review_threads SET last_comment_seq = last_comment_seq + 1 WHERE id = ?1 RETURNING last_comment_seq", [&comment.thread_id], |r| r.get(0))?;
    tx.execute("INSERT INTO review_comments (id, thread_id, author_kind, author, body, publish_state, github_comment_id, created_at, updated_at, seq)
        VALUES (?1, ?2, ?3, ?4, ?5, 'private', NULL, ?6, ?7, ?8)",
        params![comment.id, comment.thread_id, comment.author_kind.as_str(), comment.author, comment.body, comment.created_at, comment.updated_at, seq])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fresh_and_legacy_migration_preserves_reservation_and_privacy() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        for sql in &super::super::schema::MIGRATIONS[..7] {
            conn.execute_batch(sql).unwrap();
        }
        // v5 installs REST columns procedurally in the migration runner.
        conn.execute_batch("ALTER TABLE review_comments ADD COLUMN in_reply_to_id INTEGER")
            .unwrap();
        let mut s = Store { conn };
        let repo = s.resolve_repository("github.com", "o", "r", None).unwrap();
        let pr = s.upsert_pull_request(&repo.id, 1, "t", "u").unwrap();
        s.conn.execute("INSERT INTO review_threads (id,pr_id,source,path,side,head_sha,anchor_state,created_at,updated_at) VALUES ('t',?1,'local','a','RIGHT','h','current','now','now')", [&pr]).unwrap();
        s.conn.execute("INSERT INTO review_comments (id,thread_id,author_kind,author,body,publish_state,created_at,updated_at,seq) VALUES ('c','t','human','me','root','draft','now','now',1),('a','t','agent','bot','private','draft','now','now',2),('p','t','human','me','published','published','now','now',3)", []).unwrap();
        s.conn.execute("INSERT INTO publication_attempts (id,pr_id,plan_id,plan_json,head_sha,status,created_at) VALUES ('attempt',?1,'p','{}','h','pending','now')", [&pr]).unwrap();
        s.conn
            .execute(
                "INSERT INTO publication_reservations VALUES ('c','attempt')",
                [],
            )
            .unwrap();
        s.conn
            .execute_batch(include_str!("conversations.sql"))
            .unwrap();
        s.conn.execute("INSERT INTO review_comments (id,thread_id,author_kind,author,body,publish_state,created_at,updated_at,seq) VALUES ('later','t','human','me','later','private','now','now',4)", []).unwrap();
        s.conn.execute("UPDATE review_threads SET agent_request_comment_id = 'later', agent_state = 'idle' WHERE id = 't'", []).unwrap();
        s.conn.pragma_update(None, "user_version", 8).unwrap();
        let mut s = Store::init(s.conn).unwrap();
        let t = s.list_threads(&pr).unwrap();
        assert_eq!(t[0].review_comment.as_ref().unwrap().body, "root");
        assert!(t[0].review_comment.as_ref().unwrap().locked);
        assert_eq!(
            t[0].comments[1].publish_state,
            crate::model::PublishState::Private
        );
        assert_eq!(t[0].unread_count, 0);
        assert_eq!(
            t[0].comments[2].publish_state,
            crate::model::PublishState::Published
        );
        assert_eq!(t[0].published_comments.len(), 1);
        assert_eq!(t[0].published_comments[0].id, "p");
        assert!(s.clear_review_comment(&pr, "t").is_err());
        assert!(!s.open_agent_request("t", "c").unwrap());
        assert_eq!(s.find_thread_by_finding(&pr, "missing").unwrap(), None);
    }

    #[test]
    fn review_comment_is_pr_scoped_and_cannot_edit_reserved_draft() {
        let mut s = Store::open_in_memory().unwrap();
        let repo = s.resolve_repository("github.com", "o", "r", None).unwrap();
        let pr = s.upsert_pull_request(&repo.id, 1, "t", "u").unwrap();
        let other = s.upsert_pull_request(&repo.id, 2, "t", "u").unwrap();
        s.conn.execute("INSERT INTO review_threads (id,pr_id,source,path,side,head_sha,anchor_state,created_at,updated_at) VALUES ('t',?1,'local','a','RIGHT','h','current','now','now')", [&pr]).unwrap();
        assert!(s.upsert_review_comment(&other, "t", "body", None).is_err());
        assert!(s.upsert_review_comment(&pr, "t", "  ", None).is_err());
        assert!(
            s.upsert_review_comment(&pr, "t", "body", Some("not-here"))
                .is_err()
        );
        let initial = s.upsert_review_comment(&pr, "t", "body", None).unwrap();
        let edited = s.upsert_review_comment(&pr, "t", "updated", None).unwrap();
        assert_eq!(initial.id, edited.id);
        s.conn.execute("INSERT INTO publication_attempts (id,pr_id,plan_id,plan_json,head_sha,status,created_at) VALUES ('attempt',?1,'p','{}','h','pending','now')", [&pr]).unwrap();
        s.conn
            .execute(
                "UPDATE review_comment_drafts SET reserved_attempt_id = 'attempt' WHERE id = ?1",
                [&initial.id],
            )
            .unwrap();
        assert!(
            s.upsert_review_comment(&pr, "t", "not allowed", None)
                .is_err()
        );
        assert!(s.clear_review_comment(&pr, "t").is_err());
        assert_eq!(
            s.get_thread(&pr, "t").unwrap().review_comment.unwrap().body,
            "updated"
        );
    }

    #[test]
    fn response_is_atomic_and_pending_human_turn_is_rejected() {
        let mut s = Store::open_in_memory().unwrap();
        let repo = s.resolve_repository("github.com", "o", "r", None).unwrap();
        let pr = s.upsert_pull_request(&repo.id, 1, "t", "u").unwrap();
        s.conn.execute("INSERT INTO review_threads (id,pr_id,source,path,side,head_sha,anchor_state,created_at,updated_at) VALUES ('t',?1,'local','a','RIGHT','h','current','now','now')", [&pr]).unwrap();
        let make = |id: &str, actor| Comment {
            id: id.into(),
            thread_id: "t".into(),
            author_kind: actor,
            author: "author".into(),
            body: id.into(),
            created_at: "now".into(),
            updated_at: "now".into(),
            publish_state: PublishState::Private,
            github_comment_id: None,
        };
        assert!(s.insert_human_request(&make("h1", Actor::Human)).unwrap());
        assert!(!s.insert_human_request(&make("h2", Actor::Human)).unwrap());
        assert!(s.get_comment("h2").is_err());
        let duplicate = make("h1", Actor::Agent);
        assert!(s.insert_agent_response("t", "h1", &duplicate).is_err());
        assert_eq!(
            s.get_thread(&pr, "t").unwrap().agent.state,
            crate::model::AgentState::Waiting
        );
        assert!(
            s.insert_agent_response("t", "h1", &make("a1", Actor::Agent))
                .unwrap()
        );
        assert!(
            !s.insert_agent_response("t", "h1", &make("a2", Actor::Agent))
                .unwrap()
        );
        assert!(s.get_comment("a2").is_err());
        assert!(s.insert_human_request(&make("h2", Actor::Human)).unwrap());
    }

    #[test]
    fn consumed_request_stays_consumed_after_another_turn_and_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let mut s = Store::open(&path).unwrap();
        let repo = s.resolve_repository("github.com", "o", "r", None).unwrap();
        let pr = s.upsert_pull_request(&repo.id, 1, "t", "u").unwrap();
        s.conn.execute("INSERT INTO review_threads (id,pr_id,source,path,side,head_sha,anchor_state,created_at,updated_at) VALUES ('t',?1,'local','a','RIGHT','h','current','now','now')", [&pr]).unwrap();
        let make = |id: &str, actor| Comment {
            id: id.into(),
            thread_id: "t".into(),
            author_kind: actor,
            author: "me".into(),
            body: id.into(),
            created_at: "now".into(),
            updated_at: "now".into(),
            publish_state: PublishState::Private,
            github_comment_id: None,
        };
        assert!(s.insert_human_request(&make("A", Actor::Human)).unwrap());
        assert!(
            s.insert_agent_response("t", "A", &make("replyA", Actor::Agent))
                .unwrap()
        );
        assert!(s.insert_human_request(&make("B", Actor::Human)).unwrap());
        assert!(
            s.insert_agent_response("t", "B", &make("replyB", Actor::Agent))
                .unwrap()
        );
        drop(s);
        let mut s = Store::open(&path).unwrap();
        assert!(!s.open_agent_request("t", "A").unwrap());
        assert!(
            !s.insert_agent_response("t", "A", &make("late", Actor::Agent))
                .unwrap()
        );
        assert_eq!(
            s.get_thread(&pr, "t").unwrap().agent.state,
            crate::model::AgentState::Idle
        );
        assert!(s.get_comment("late").is_err());
    }

    #[test]
    fn deleting_read_top_keeps_new_agent_reply_unread_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("seq.db");
        let mut s = Store::open(&path).unwrap();
        let repo = s.resolve_repository("github.com", "o", "r", None).unwrap();
        let pr = s.upsert_pull_request(&repo.id, 1, "t", "u").unwrap();
        s.conn.execute("INSERT INTO review_threads (id,pr_id,source,path,side,head_sha,anchor_state,created_at,updated_at) VALUES ('t',?1,'local','a','RIGHT','h','current','now','now')", [&pr]).unwrap();
        s.conn
            .execute(
                "UPDATE review_threads SET finding_id = 'f' WHERE id = 't'",
                [],
            )
            .unwrap();
        let make = |id: &str, actor| Comment {
            id: id.into(),
            thread_id: "t".into(),
            author_kind: actor,
            author: "me".into(),
            body: id.into(),
            created_at: "now".into(),
            updated_at: "now".into(),
            publish_state: PublishState::Private,
            github_comment_id: None,
        };
        s.insert_comment(&make("top", Actor::Human), None).unwrap();
        assert!(s.mark_thread_read("t", "top").unwrap());
        s.delete_comment("top").unwrap();
        drop(s);
        let mut s = Store::open(&path).unwrap();
        s.insert_comment(&make("new", Actor::Agent), None).unwrap();
        let thread = s.get_thread(&pr, "t").unwrap();
        assert_eq!(thread.unread_count, 1);
        let seq: i64 = s
            .conn
            .query_row(
                "SELECT seq FROM review_comments WHERE id = 'new'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(seq, 2);
    }

    #[test]
    fn deleting_last_active_request_keeps_empty_thread_and_consumes_turn() {
        let mut s = Store::open_in_memory().unwrap();
        let repo = s.resolve_repository("github.com", "o", "r", None).unwrap();
        let pr = s.upsert_pull_request(&repo.id, 1, "t", "u").unwrap();
        s.conn.execute("INSERT INTO review_threads (id,pr_id,source,path,side,head_sha,anchor_state,created_at,updated_at) VALUES ('t',?1,'local','a','RIGHT','h','current','now','now')", [&pr]).unwrap();
        let request = Comment {
            id: "human".into(),
            thread_id: "t".into(),
            author_kind: Actor::Human,
            author: "me".into(),
            body: "ask".into(),
            created_at: "now".into(),
            updated_at: "now".into(),
            publish_state: PublishState::Private,
            github_comment_id: None,
        };
        assert!(s.insert_human_request(&request).unwrap());
        s.delete_comment("human").unwrap();
        let thread = s.get_thread(&pr, "t").unwrap();
        assert!(thread.comments.is_empty());
        assert_eq!(thread.agent.state, crate::model::AgentState::Idle);
        assert!(!s.open_agent_request("t", "human").unwrap());
        let state: String = s
            .conn
            .query_row(
                "SELECT state FROM agent_request_history WHERE comment_id = 'human'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "completed");
    }

    #[test]
    fn deleting_active_request_with_draft_allows_next_turn_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cancel.db");
        let mut s = Store::open(&path).unwrap();
        let repo = s.resolve_repository("github.com", "o", "r", None).unwrap();
        let pr = s.upsert_pull_request(&repo.id, 1, "t", "u").unwrap();
        s.conn.execute("INSERT INTO review_threads (id,pr_id,source,path,side,head_sha,anchor_state,created_at,updated_at) VALUES ('t',?1,'local','a','RIGHT','h','current','now','now')", [&pr]).unwrap();
        let make = |id: &str, actor| Comment {
            id: id.into(),
            thread_id: "t".into(),
            author_kind: actor,
            author: "me".into(),
            body: id.into(),
            created_at: "now".into(),
            updated_at: "now".into(),
            publish_state: PublishState::Private,
            github_comment_id: None,
        };
        assert!(s.insert_human_request(&make("old", Actor::Human)).unwrap());
        let draft = s
            .upsert_review_comment(&pr, "t", "independent body", Some("old"))
            .unwrap();
        s.delete_comment("old").unwrap();
        drop(s);
        let mut s = Store::open(&path).unwrap();
        let thread = s.get_thread(&pr, "t").unwrap();
        assert_eq!(thread.agent.state, crate::model::AgentState::Idle);
        assert_eq!(thread.review_comment.unwrap().body, "independent body");
        assert!(
            !s.insert_agent_response("t", "old", &make("late", Actor::Agent))
                .unwrap()
        );
        assert!(s.insert_human_request(&make("next", Actor::Human)).unwrap());
        assert_eq!(
            s.get_thread(&pr, "t").unwrap().review_comment.unwrap().id,
            draft.id
        );
        assert!(!s.open_agent_request("t", "old").unwrap());
        assert!(
            s.insert_agent_response("t", "next", &make("reply", Actor::Agent))
                .unwrap()
        );
    }

    #[test]
    fn deleting_private_source_retains_outgoing_body_and_publication() {
        let mut s = Store::open_in_memory().unwrap();
        let repo = s.resolve_repository("github.com", "o", "r", None).unwrap();
        let pr = s.upsert_pull_request(&repo.id, 1, "t", "u").unwrap();
        s.conn.execute("INSERT INTO review_threads (id,pr_id,source,path,side,head_sha,anchor_state,created_at,updated_at) VALUES ('t',?1,'local','a','RIGHT','h','current','now','now')", [&pr]).unwrap();
        for id in ["source", "published-source"] {
            s.insert_comment(
                &Comment {
                    id: id.into(),
                    thread_id: "t".into(),
                    author_kind: Actor::Human,
                    author: "me".into(),
                    body: id.into(),
                    created_at: "now".into(),
                    updated_at: "now".into(),
                    publish_state: PublishState::Private,
                    github_comment_id: None,
                },
                None,
            )
            .unwrap();
        }
        let draft = s
            .upsert_review_comment(&pr, "t", "prepared", Some("source"))
            .unwrap();
        s.conn.execute("INSERT INTO review_comment_drafts (id,thread_id,body,state,source_comment_id,github_comment_id,created_at,updated_at,published_at) VALUES ('published','t','history','published','published-source',42,'now','now','now')", []).unwrap();
        s.delete_comment("source").unwrap();
        s.delete_comment("published-source").unwrap();
        let thread = s.get_thread(&pr, "t").unwrap();
        assert_eq!(thread.review_comment.unwrap().id, draft.id);
        assert_eq!(thread.published_comments[0].github_comment_id, Some(42));
        let references: i64 = s
            .conn
            .query_row(
                "SELECT COUNT(*) FROM review_comment_drafts WHERE source_comment_id IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(references, 0);
    }

    #[test]
    fn agent_cas_and_read_cursor_persist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conversation.db");
        let mut s = Store::open(&path).unwrap();
        let repo = s.resolve_repository("github.com", "o", "r", None).unwrap();
        let pr = s.upsert_pull_request(&repo.id, 1, "t", "u").unwrap();
        s.conn.execute("INSERT INTO review_threads (id,pr_id,source,path,side,head_sha,anchor_state,created_at,updated_at) VALUES ('t',?1,'local','a','RIGHT','h','current','now','now')", [&pr]).unwrap();
        s.conn.execute("INSERT INTO review_comments (id,thread_id,author_kind,author,body,publish_state,created_at,updated_at,seq) VALUES ('h','t','human','me','ask','private','now','now',1),('a','t','agent','bot','answer','private','now','now',2)", []).unwrap();
        assert!(s.open_agent_request("t", "h").unwrap());
        assert!(!s.open_agent_request("t", "h").unwrap());
        assert!(
            !s.transition_agent_request(
                "t",
                "a",
                crate::model::AgentState::Waiting,
                crate::model::AgentState::Responding,
                None
            )
            .unwrap()
        );
        assert!(
            s.transition_agent_request(
                "t",
                "h",
                crate::model::AgentState::Waiting,
                crate::model::AgentState::Responding,
                None
            )
            .unwrap()
        );
        assert_eq!(s.list_threads(&pr).unwrap()[0].unread_count, 1);
        assert!(s.mark_thread_read("t", "a").unwrap());
        drop(s);
        let mut s = Store::open(&path).unwrap();
        assert_eq!(s.list_threads(&pr).unwrap()[0].unread_count, 0);
        assert!(!s.mark_thread_read("t", "missing").unwrap());
    }
}
