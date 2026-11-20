//! SQLite persistence for rules, snapshots, review progress and discussions.
//!
//! The database lives in the OS data directory (never in a repository).
//! Set `ASIDE_DATA_DIR` to override. Demo sessions use an in-memory database.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use crate::model::{
    Acknowledgement, Actor, AnchorState, Comment, ExclusionRule, PrSnapshot, PublishState,
    Repository, ReviewState, ReviewThread, RuleAction, RuleKind, Side, ThreadSource, Verdict,
};
use crate::util::{new_id, now_rfc3339};

const MIGRATIONS: &[&str] = &[
    // v1
    "
    CREATE TABLE repositories (
        id TEXT PRIMARY KEY,
        host TEXT NOT NULL,
        owner TEXT NOT NULL,
        name TEXT NOT NULL,
        provider_id TEXT UNIQUE,
        created_at TEXT NOT NULL
    );
    CREATE TABLE repository_aliases (
        alias TEXT PRIMARY KEY,
        repository_id TEXT NOT NULL REFERENCES repositories(id)
    );
    CREATE TABLE repository_exclusion_rules (
        id TEXT PRIMARY KEY,
        repository_id TEXT NOT NULL REFERENCES repositories(id),
        action TEXT NOT NULL CHECK (action IN ('include', 'exclude')),
        kind TEXT NOT NULL CHECK (kind IN ('file', 'directory', 'glob')),
        expression TEXT NOT NULL,
        enabled INTEGER NOT NULL DEFAULT 1,
        ord INTEGER NOT NULL,
        created_by TEXT NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE INDEX idx_rules_repo ON repository_exclusion_rules(repository_id, ord);
    CREATE TABLE pull_requests (
        id TEXT PRIMARY KEY,
        repository_id TEXT NOT NULL REFERENCES repositories(id),
        number INTEGER NOT NULL,
        title TEXT NOT NULL,
        url TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        UNIQUE (repository_id, number)
    );
    CREATE TABLE pr_snapshots (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        pr_id TEXT NOT NULL REFERENCES pull_requests(id),
        head_sha TEXT NOT NULL,
        base_sha TEXT NOT NULL,
        fetched_at TEXT NOT NULL,
        snapshot_json TEXT NOT NULL
    );
    CREATE TABLE snapshot_files (
        snapshot_id INTEGER NOT NULL REFERENCES pr_snapshots(id) ON DELETE CASCADE,
        path TEXT NOT NULL,
        previous_path TEXT,
        fingerprint_version INTEGER NOT NULL,
        diff_digest TEXT NOT NULL,
        content_digest TEXT,
        PRIMARY KEY (snapshot_id, path)
    );
    CREATE TABLE review_acknowledgements (
        id TEXT PRIMARY KEY,
        pr_id TEXT NOT NULL REFERENCES pull_requests(id),
        path TEXT NOT NULL,
        kind TEXT NOT NULL CHECK (kind IN ('reviewed', 'cleared')),
        fingerprint_version INTEGER NOT NULL,
        diff_digest TEXT NOT NULL,
        content_digest TEXT,
        head_sha TEXT NOT NULL,
        base_sha TEXT NOT NULL,
        actor TEXT NOT NULL,
        created_at TEXT NOT NULL,
        seq INTEGER NOT NULL
    );
    CREATE INDEX idx_acks_pr_path ON review_acknowledgements(pr_id, path);
    CREATE TABLE review_sessions (
        pr_id TEXT PRIMARY KEY REFERENCES pull_requests(id),
        lifecycle_state TEXT NOT NULL DEFAULT 'in-progress',
        verdict TEXT,
        summary TEXT NOT NULL DEFAULT '',
        finished_at TEXT,
        published_at TEXT,
        created_at TEXT NOT NULL,
        last_opened_at TEXT NOT NULL
    );
    CREATE TABLE review_threads (
        id TEXT PRIMARY KEY,
        pr_id TEXT NOT NULL REFERENCES pull_requests(id),
        source TEXT NOT NULL CHECK (source IN ('local', 'github')),
        github_thread_id TEXT UNIQUE,
        path TEXT NOT NULL,
        side TEXT NOT NULL,
        line INTEGER,
        start_line INTEGER,
        line_text TEXT,
        diff_digest TEXT,
        head_sha TEXT NOT NULL,
        anchor_state TEXT NOT NULL,
        anchor_reason TEXT,
        resolved INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE INDEX idx_threads_pr ON review_threads(pr_id);
    CREATE TABLE review_comments (
        id TEXT PRIMARY KEY,
        thread_id TEXT NOT NULL REFERENCES review_threads(id),
        author_kind TEXT NOT NULL,
        author TEXT NOT NULL,
        body TEXT NOT NULL,
        publish_state TEXT NOT NULL,
        github_comment_id INTEGER,
        github_node_id TEXT UNIQUE,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        seq INTEGER NOT NULL
    );
    CREATE INDEX idx_comments_thread ON review_comments(thread_id);
    CREATE TABLE publication_attempts (
        id TEXT PRIMARY KEY,
        pr_id TEXT NOT NULL REFERENCES pull_requests(id),
        plan_id TEXT NOT NULL,
        plan_json TEXT NOT NULL,
        head_sha TEXT NOT NULL,
        status TEXT NOT NULL,
        error TEXT,
        github_review_id INTEGER,
        created_at TEXT NOT NULL,
        finished_at TEXT
    );
    ",
    // v2: one durable unresolved claim per PR, including pre-migration attempts.
    "
    CREATE TABLE publication_claims (
        pr_id TEXT PRIMARY KEY REFERENCES pull_requests(id),
        attempt_id TEXT NOT NULL REFERENCES publication_attempts(id)
    );
    INSERT INTO publication_claims (pr_id, attempt_id)
    SELECT pr_id, MIN(id) FROM publication_attempts WHERE status != 'succeeded' GROUP BY pr_id;
    CREATE TABLE publication_reservations (
        comment_id TEXT PRIMARY KEY REFERENCES review_comments(id),
        attempt_id TEXT NOT NULL REFERENCES publication_attempts(id)
    );
    ",
];

/// How many snapshots to keep per PR.
const SNAPSHOT_RETENTION: i64 = 20;

pub struct Store {
    conn: Connection,
}

pub fn default_path() -> Result<PathBuf> {
    let dir = match std::env::var_os("ASIDE_DATA_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => dirs::data_dir()
            .context("could not find the OS data directory")?
            .join("aside"),
    };
    Ok(dir.join("aside.db"))
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
            restrict_dir(parent);
        }
        let conn = Connection::open(path)
            .with_context(|| format!("could not open database {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let mut store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<()> {
        let version: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version < 0 || version as usize > MIGRATIONS.len() {
            bail!(
                "database schema v{version} is newer than this Aside build supports (v{}); upgrade Aside",
                MIGRATIONS.len()
            );
        }
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql)
                .with_context(|| format!("migration v{} failed", i + 1))?;
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Find or create the repository, preferring the provider's immutable ID.
    /// Handles renames (same provider ID, new name) and name reuse (same name,
    /// different provider ID) without sharing rules across repositories.
    pub fn resolve_repository(
        &mut self,
        host: &str,
        owner: &str,
        name: &str,
        provider_id: Option<&str>,
    ) -> Result<Repository> {
        let alias = format!("{host}/{owner}/{name}").to_lowercase();
        let now = now_rfc3339();
        let tx = self.conn.transaction()?;
        let by_provider: Option<String> = match provider_id {
            Some(pid) => tx
                .query_row(
                    "SELECT id FROM repositories WHERE provider_id = ?1",
                    [pid],
                    |r| r.get(0),
                )
                .optional()?,
            None => None,
        };
        let by_alias: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT r.id, r.provider_id FROM repository_aliases a JOIN repositories r ON r.id = a.repository_id WHERE a.alias = ?1",
                [&alias],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;

        let id = match (by_provider, by_alias, provider_id) {
            (Some(id), _, _) => id,
            (None, Some((id, None)), Some(pid)) => {
                tx.execute(
                    "UPDATE repositories SET provider_id = ?1 WHERE id = ?2",
                    params![pid, id],
                )?;
                id
            }
            (None, Some((id, existing)), pid) if pid.is_none() || existing.as_deref() == pid => id,
            _ => {
                let id = new_id("repo");
                tx.execute(
                    "INSERT INTO repositories (id, host, owner, name, provider_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![id, host, owner, name, provider_id, now],
                )?;
                id
            }
        };
        tx.execute(
            "UPDATE repositories SET owner = ?1, name = ?2 WHERE id = ?3",
            params![owner, name, id],
        )?;
        tx.execute(
            "INSERT INTO repository_aliases (alias, repository_id) VALUES (?1, ?2)
             ON CONFLICT(alias) DO UPDATE SET repository_id = excluded.repository_id",
            params![alias, id],
        )?;
        tx.commit()?;
        Ok(Repository {
            id,
            host: host.into(),
            owner: owner.into(),
            name: name.into(),
            provider_id: provider_id.map(str::to_string),
        })
    }

    pub fn list_rules(&self, repository_id: &str) -> Result<Vec<ExclusionRule>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, action, kind, expression, enabled, ord, created_at, updated_at
             FROM repository_exclusion_rules WHERE repository_id = ?1 ORDER BY ord",
        )?;
        let rows = stmt.query_map([repository_id], rule_from_row)?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    /// Insert a rule at `position` (0-based; default: last, so it wins).
    pub fn create_rule(
        &mut self,
        repository_id: &str,
        action: RuleAction,
        kind: RuleKind,
        expression: &str,
        position: Option<usize>,
        actor: Actor,
    ) -> Result<ExclusionRule> {
        let id = new_id("rule");
        let now = now_rfc3339();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO repository_exclusion_rules (id, repository_id, action, kind, expression, enabled, ord, created_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, 1000000000, ?6, ?7, ?7)",
            params![id, repository_id, action_str(action), kind_str(kind), expression, actor.as_str(), now],
        )?;
        reorder(&tx, repository_id, &id, position)?;
        tx.commit()?;
        self.get_rule(repository_id, &id)
    }

    pub fn get_rule(&self, repository_id: &str, id: &str) -> Result<ExclusionRule> {
        self.conn
            .query_row(
                "SELECT id, action, kind, expression, enabled, ord, created_at, updated_at
                 FROM repository_exclusion_rules WHERE repository_id = ?1 AND id = ?2",
                params![repository_id, id],
                rule_from_row,
            )
            .optional()?
            .ok_or_else(|| NotFound(format!("rule {id}")).into())
    }

    pub fn update_rule(
        &mut self,
        repository_id: &str,
        id: &str,
        update: &RuleUpdate,
    ) -> Result<ExclusionRule> {
        self.get_rule(repository_id, id)?;
        let tx = self.conn.transaction()?;
        let now = now_rfc3339();
        if let Some(action) = update.action {
            tx.execute(
                "UPDATE repository_exclusion_rules SET action = ?1, updated_at = ?2 WHERE id = ?3",
                params![action_str(action), now, id],
            )?;
        }
        if let Some((kind, expr)) = &update.kind_expression {
            tx.execute(
                "UPDATE repository_exclusion_rules SET kind = ?1, expression = ?2, updated_at = ?3 WHERE id = ?4",
                params![kind_str(*kind), expr, now, id],
            )?;
        }
        if let Some(enabled) = update.enabled {
            tx.execute(
                "UPDATE repository_exclusion_rules SET enabled = ?1, updated_at = ?2 WHERE id = ?3",
                params![enabled, now, id],
            )?;
        }
        if let Some(position) = update.position {
            tx.execute(
                "UPDATE repository_exclusion_rules SET updated_at = ?1 WHERE id = ?2",
                params![now, id],
            )?;
            reorder(&tx, repository_id, id, Some(position))?;
        }
        tx.commit()?;
        self.get_rule(repository_id, id)
    }

    pub fn delete_rule(&mut self, repository_id: &str, id: &str) -> Result<()> {
        let n = self.conn.execute(
            "DELETE FROM repository_exclusion_rules WHERE repository_id = ?1 AND id = ?2",
            params![repository_id, id],
        )?;
        if n == 0 {
            return Err(NotFound(format!("rule {id}")).into());
        }
        Ok(())
    }

    pub fn upsert_pull_request(
        &mut self,
        repository_id: &str,
        number: u64,
        title: &str,
        url: &str,
    ) -> Result<String> {
        let now = now_rfc3339();
        let number = i64::try_from(number).context("PR number exceeds SQLite integer range")?;
        let tx = self.conn.transaction()?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT id FROM pull_requests WHERE repository_id = ?1 AND number = ?2",
                params![repository_id, number],
                |r| r.get(0),
            )
            .optional()?;
        let id = match existing {
            Some(id) => {
                tx.execute(
                    "UPDATE pull_requests SET title = ?1, url = ?2, updated_at = ?3 WHERE id = ?4",
                    params![title, url, now, id],
                )?;
                id
            }
            None => {
                let id = new_id("pr");
                tx.execute(
                    "INSERT INTO pull_requests (id, repository_id, number, title, url, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![id, repository_id, number, title, url, now],
                )?;
                id
            }
        };
        tx.execute(
            "INSERT INTO review_sessions (pr_id, created_at, last_opened_at) VALUES (?1, ?2, ?2)
             ON CONFLICT(pr_id) DO UPDATE SET last_opened_at = excluded.last_opened_at",
            params![id, now],
        )?;
        tx.commit()?;
        Ok(id)
    }

    pub fn save_snapshot(&mut self, pr_id: &str, snapshot: &PrSnapshot) -> Result<i64> {
        let json = serde_json::to_string(snapshot)?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO pr_snapshots (pr_id, head_sha, base_sha, fetched_at, snapshot_json) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![pr_id, snapshot.pr.head_sha, snapshot.pr.base_sha, snapshot.fetched_at, json],
        )?;
        let id = tx.last_insert_rowid();
        for f in &snapshot.files {
            tx.execute(
                "INSERT OR REPLACE INTO snapshot_files (snapshot_id, path, previous_path, fingerprint_version, diff_digest, content_digest)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, f.path, f.previous_path, f.fingerprint_version, f.diff_digest, f.content_digest],
            )?;
        }
        tx.execute(
            "DELETE FROM pr_snapshots WHERE pr_id = ?1 AND id NOT IN
             (SELECT id FROM pr_snapshots WHERE pr_id = ?1 ORDER BY id DESC LIMIT ?2)",
            params![pr_id, SNAPSHOT_RETENTION],
        )?;
        tx.commit()?;
        Ok(id)
    }

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
        self.conn
            .query_row(
                "SELECT lifecycle_state, verdict, summary, finished_at, published_at FROM review_sessions WHERE pr_id = ?1",
                [pr_id],
                |r| {
                    Ok(ReviewState {
                        lifecycle: r.get(0)?,
                        verdict: r.get::<_, Option<String>>(1)?.as_deref().and_then(Verdict::parse),
                        summary: r.get(2)?,
                        finished_at: r.get(3)?,
                        published_at: r.get(4)?,
                    })
                },
            )
            .map_err(Into::into)
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

#[derive(Debug, Default)]
pub struct RuleUpdate {
    pub action: Option<RuleAction>,
    pub kind_expression: Option<(RuleKind, String)>,
    pub enabled: Option<bool>,
    pub position: Option<usize>,
}

/// A draft that was reserved for publication or is no longer editable.
#[derive(Debug)]
pub struct ReservedDraft;

impl std::fmt::Display for ReservedDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "draft is reserved for publication")
    }
}
impl std::error::Error for ReservedDraft {}

/// A missing record; the API maps it to 404.
#[derive(Debug)]
pub struct NotFound(pub String);

impl std::fmt::Display for NotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} not found", self.0)
    }
}

impl std::error::Error for NotFound {}

/// Move `id` to `position` and renumber all rules 1..n.
fn reorder(tx: &Transaction, repository_id: &str, id: &str, position: Option<usize>) -> Result<()> {
    let mut ids: Vec<String> = {
        let mut stmt = tx.prepare("SELECT id FROM repository_exclusion_rules WHERE repository_id = ?1 AND id != ?2 ORDER BY ord, created_at")?;
        stmt.query_map(params![repository_id, id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    let at = position.unwrap_or(ids.len()).min(ids.len());
    ids.insert(at, id.to_string());
    for (i, rid) in ids.iter().enumerate() {
        tx.execute(
            "UPDATE repository_exclusion_rules SET ord = ?1 WHERE id = ?2",
            params![i as i64 + 1, rid],
        )?;
    }
    Ok(())
}

fn rule_from_row(r: &Row) -> rusqlite::Result<ExclusionRule> {
    Ok(ExclusionRule {
        id: r.get(0)?,
        action: if r.get::<_, String>(1)? == "include" {
            RuleAction::Include
        } else {
            RuleAction::Exclude
        },
        kind: match r.get::<_, String>(2)?.as_str() {
            "file" => RuleKind::File,
            "directory" => RuleKind::Directory,
            _ => RuleKind::Glob,
        },
        expression: r.get(3)?,
        enabled: r.get(4)?,
        order: r.get(5)?,
        created_at: r.get(6)?,
        updated_at: r.get(7)?,
    })
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

fn action_str(a: RuleAction) -> &'static str {
    match a {
        RuleAction::Include => "include",
        RuleAction::Exclude => "exclude",
    }
}

fn kind_str(k: RuleKind) -> &'static str {
    match k {
        RuleKind::File => "file",
        RuleKind::Directory => "directory",
        RuleKind::Glob => "glob",
    }
}

fn source_str(s: ThreadSource) -> &'static str {
    match s {
        ThreadSource::Local => "local",
        ThreadSource::Github => "github",
    }
}

/// Keep the data directory private to the current user where supported.
pub fn restrict_dir(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    #[test]
    fn migrations_apply_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.db");
        let repo_id = {
            let mut s = Store::open(&path).unwrap();
            let repo = s
                .resolve_repository("github.com", "acme", "widget", Some("R1"))
                .unwrap();
            s.create_rule(
                &repo.id,
                RuleAction::Exclude,
                RuleKind::Directory,
                "vendor",
                None,
                Actor::Human,
            )
            .unwrap();
            repo.id
        };
        let s = Store::open(&path).unwrap();
        assert_eq!(s.list_rules(&repo_id).unwrap().len(), 1);
    }

    #[test]
    fn rejects_newer_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.db");
        Store::open(&path).unwrap();
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", 99)
            .unwrap();
        assert!(Store::open(&path).is_err());
    }

    #[test]
    fn repository_identity_is_stable_across_renames_and_distinct_on_reuse() {
        let mut s = store();
        let a = s
            .resolve_repository("github.com", "acme", "widget", Some("R1"))
            .unwrap();
        let same = s
            .resolve_repository("github.com", "Acme", "Widget", Some("R1"))
            .unwrap();
        assert_eq!(a.id, same.id);
        let renamed = s
            .resolve_repository("github.com", "acme", "gadget", Some("R1"))
            .unwrap();
        assert_eq!(a.id, renamed.id, "a rename keeps the same repository");
        let reused = s
            .resolve_repository("github.com", "acme", "widget", Some("R2"))
            .unwrap();
        assert_ne!(
            a.id, reused.id,
            "a new repo reusing the name must not share rules"
        );
        let no_pid = s
            .resolve_repository("github.com", "acme", "other", None)
            .unwrap();
        let later = s
            .resolve_repository("github.com", "acme", "other", Some("R3"))
            .unwrap();
        assert_eq!(no_pid.id, later.id, "provider ID is attached once known");
    }

    #[test]
    fn rules_insert_reorder_update_delete() {
        let mut s = store();
        let repo = s.resolve_repository("github.com", "o", "r", None).unwrap();
        let a = s
            .create_rule(
                &repo.id,
                RuleAction::Exclude,
                RuleKind::Directory,
                "a",
                None,
                Actor::Human,
            )
            .unwrap();
        let b = s
            .create_rule(
                &repo.id,
                RuleAction::Exclude,
                RuleKind::Directory,
                "b",
                None,
                Actor::Human,
            )
            .unwrap();
        let c = s
            .create_rule(
                &repo.id,
                RuleAction::Include,
                RuleKind::File,
                "a/x",
                Some(0),
                Actor::Human,
            )
            .unwrap();
        let order: Vec<String> = s
            .list_rules(&repo.id)
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(order, vec![c.id.clone(), a.id.clone(), b.id.clone()]);
        s.update_rule(
            &repo.id,
            &c.id,
            &RuleUpdate {
                position: Some(2),
                enabled: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
        let rules = s.list_rules(&repo.id).unwrap();
        assert_eq!(rules.last().unwrap().id, c.id);
        assert!(!rules.last().unwrap().enabled);
        assert_eq!(
            rules.iter().map(|r| r.order).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        s.delete_rule(&repo.id, &a.id).unwrap();
        assert!(s.delete_rule(&repo.id, &a.id).is_err());
        let other = s
            .resolve_repository("github.com", "o", "other", None)
            .unwrap();
        assert!(
            s.delete_rule(&other.id, &b.id).is_err(),
            "rules are scoped to their repository"
        );
    }

    #[test]
    fn snapshots_are_pruned() {
        let mut s = store();
        let repo = s.resolve_repository("github.com", "o", "r", None).unwrap();
        let pr = s.upsert_pull_request(&repo.id, 1, "t", "u").unwrap();
        let snap = crate::demo::snapshot(0);
        for _ in 0..(SNAPSHOT_RETENTION + 5) {
            s.save_snapshot(&pr, &snap).unwrap();
        }
        let n: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM pr_snapshots", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, SNAPSHOT_RETENTION);
    }

    #[test]
    fn unresolved_attempt_blocks_changed_plan() {
        let mut s = store();
        let repo = s.resolve_repository("github.com", "o", "r", None).unwrap();
        let pr = s.upsert_pull_request(&repo.id, 1, "t", "u").unwrap();
        s.start_publication(&pr, "original", "{}", "head", &[])
            .unwrap();
        assert!(
            s.start_publication(&pr, "edited", "{}", "head", &[])
                .is_err()
        );
    }

    #[test]
    fn concurrent_claims_allow_only_one_process() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claim.db");
        let mut s = Store::open(&path).unwrap();
        let repo = s.resolve_repository("github.com", "o", "r", None).unwrap();
        let pr = s.upsert_pull_request(&repo.id, 1, "t", "u").unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let tasks: Vec<_> = (0..2)
            .map(|i| {
                let path = path.clone();
                let pr = pr.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut store = Store::open(&path).unwrap();
                    barrier.wait();
                    store
                        .start_publication(&pr, &format!("plan-{i}"), "{}", "head", &[])
                        .is_ok()
                })
            })
            .collect();
        assert_eq!(
            tasks
                .into_iter()
                .map(|t| t.join().unwrap())
                .filter(|ok| *ok)
                .count(),
            1
        );
    }

    #[test]
    fn reservation_blocks_edit_delete_and_marks_only_once() {
        let mut s = store();
        let repo = s.resolve_repository("github.com", "o", "r", None).unwrap();
        let pr = s.upsert_pull_request(&repo.id, 1, "t", "u").unwrap();
        s.insert_thread(
            &pr,
            &NewThread {
                id: "t".into(),
                source: ThreadSource::Local,
                github_thread_id: None,
                path: "a".into(),
                side: Side::Right,
                line: Some(1),
                start_line: None,
                line_text: None,
                diff_digest: None,
                head_sha: "head".into(),
                anchor_state: AnchorState::Current,
                anchor_reason: None,
                resolved: false,
                created_at: None,
            },
        )
        .unwrap();
        let now = now_rfc3339();
        s.insert_comment(
            &Comment {
                id: "c".into(),
                thread_id: "t".into(),
                author_kind: Actor::Human,
                author: "me".into(),
                body: "original".into(),
                created_at: now.clone(),
                updated_at: now,
                publish_state: PublishState::Draft,
                github_comment_id: None,
            },
            None,
        )
        .unwrap();
        assert!(
            s.start_publication(&pr, "wrong", "{}", "head", &[("c", "edited")])
                .is_err()
        );
        // The successful claim below proves the failed reservation rolled back the claim.
        s.start_publication(&pr, "correct", "{}", "head", &[("c", "original")])
            .unwrap();
        assert!(s.update_comment_body("c", "edited").is_err());
        assert!(s.delete_comment("c").is_err());
        assert_eq!(s.get_comment("c").unwrap().body, "original");
        s.mark_comment_published("c", Some(42)).unwrap();
        assert!(s.mark_comment_published("c", Some(43)).is_err());
        assert_eq!(s.get_comment("c").unwrap().github_comment_id, Some(42));
    }

    #[test]
    fn deleting_last_comment_removes_thread() {
        let mut s = store();
        let repo = s.resolve_repository("github.com", "o", "r", None).unwrap();
        let pr = s.upsert_pull_request(&repo.id, 1, "t", "u").unwrap();
        s.insert_thread(
            &pr,
            &NewThread {
                id: "t1".into(),
                source: ThreadSource::Local,
                github_thread_id: None,
                path: "a".into(),
                side: Side::Right,
                line: Some(1),
                start_line: None,
                line_text: Some("x".into()),
                diff_digest: None,
                head_sha: "h".into(),
                anchor_state: AnchorState::Current,
                anchor_reason: None,
                resolved: false,
                created_at: None,
            },
        )
        .unwrap();
        let now = now_rfc3339();
        s.insert_comment(
            &Comment {
                id: "c1".into(),
                thread_id: "t1".into(),
                author_kind: Actor::Human,
                author: "me".into(),
                body: "hi".into(),
                created_at: now.clone(),
                updated_at: now,
                publish_state: PublishState::Draft,
                github_comment_id: None,
            },
            None,
        )
        .unwrap();
        assert_eq!(s.list_threads(&pr).unwrap().len(), 1);
        s.delete_comment("c1").unwrap();
        assert!(s.list_threads(&pr).unwrap().is_empty());
    }
}
