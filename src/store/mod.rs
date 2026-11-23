//! SQLite persistence for rules, snapshots, review progress and discussions.
//!
//! The database lives in the OS data directory (never in a repository).
//! Set `ASIDE_DATA_DIR` to override. Demo sessions use an in-memory database.

mod analysis;
mod conversations;
mod publication;
mod repositories;
mod review;
mod rules;
mod schema;
pub(crate) mod snapshots;
mod threads;

// TODO(#33): the remaining re-exports are consumed by the analysis API.
#[allow(unused_imports)]
pub use analysis::{
    ANALYSIS_SNAPSHOT_RETENTION, AnalysisBatch, BatchOutcome, ImportBatch, ImportedSourceWrite,
    ObservationLabel, PruneReport, Raw, RevisionConflict, StoredFinding,
};
pub use publication::ReservedDraft;
pub use rules::RuleUpdate;
pub use threads::NewThread;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::Connection;

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
        if version < 0 || version as usize > schema::MIGRATIONS.len() {
            bail!(
                "database schema v{version} is newer than this Aside build supports (v{}); upgrade Aside",
                schema::MIGRATIONS.len()
            );
        }
        for (i, sql) in schema::MIGRATIONS.iter().enumerate().skip(version as usize) {
            let tx = self.conn.transaction()?;
            // PR13's unpublished v7 used the same version number as main's
            // released review-rules migration. Preserve its data and install
            // the missing released schema before running conversations v8.
            if i + 1 == 8 && version == 7 {
                let has_rules: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'review_exclusion_rules')",
                    [], |r| r.get(0),
                )?;
                if !has_rules {
                    tx.execute_batch(schema::MIGRATIONS[6])?;
                    // v8 normalizes only root drafts. Capture branch reply drafts
                    // before its conversion of old draft rows to private history.
                    tx.execute_batch("CREATE TABLE legacy_pending_replies (id TEXT PRIMARY KEY, thread_id TEXT NOT NULL, body TEXT NOT NULL, selected INTEGER NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, reserved_attempt_id TEXT, original_seq INTEGER NOT NULL);
                        INSERT INTO legacy_pending_replies SELECT c.id, c.thread_id, c.body, c.publish_selected, c.created_at, c.updated_at, r.attempt_id, c.seq
                        FROM review_comments c JOIN review_threads t ON t.id = c.thread_id
                        LEFT JOIN publication_reservations r ON r.comment_id = c.id
                        WHERE c.author_kind = 'human' AND c.publish_state = 'draft' AND trim(c.body) != ''
                        AND (t.source = 'github' OR c.seq > (SELECT MIN(root.seq) FROM review_comments root WHERE root.thread_id = c.thread_id));")?;
                }
            }
            if i + 1 == 4 {
                // Both branches released a different v3. Inspect under the write
                // transaction before applying only the missing half.
                migrate_colliding_v3(&tx)?;
            } else {
                if i + 1 == 5 || i + 1 == 6 {
                    migrate_rest_columns(&tx)?;
                }
                if i + 1 == 10 {
                    let mut stmt = tx.prepare("PRAGMA table_info(review_sessions)")?;
                    let columns = stmt
                        .query_map([], |r| r.get::<_, String>(1))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    if !columns.iter().any(|c| c == "selection_revision") {
                        tx.execute_batch(sql)?;
                    } else {
                        tx.execute_batch("ALTER TABLE review_comment_drafts ADD COLUMN publish_selected INTEGER NOT NULL DEFAULT 1;
                            ALTER TABLE review_threads ADD COLUMN legacy_reply_seq INTEGER;")?;
                    }
                } else {
                    tx.execute_batch(sql)
                        .with_context(|| format!("migration v{} failed", i + 1))?;
                }
                if i + 1 == 10 {
                    let mut stmt = tx.prepare("PRAGMA table_info(review_comments)")?;
                    let columns = stmt
                        .query_map([], |r| r.get::<_, String>(1))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    if columns.iter().any(|c| c == "publish_selected") {
                        tx.execute_batch("UPDATE review_comment_drafts SET publish_selected = (SELECT publish_selected FROM review_comments WHERE review_comments.id = review_comment_drafts.id) WHERE id IN (SELECT id FROM review_comments);")?;
                    }
                    let captured: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'legacy_pending_replies')", [], |r| r.get(0))?;
                    if captured {
                        tx.execute_batch("INSERT INTO review_threads (id, pr_id, source, path, side, line, start_line, line_text, diff_digest, head_sha, anchor_state, anchor_reason, resolved, created_at, updated_at, subject, linked_thread_id, legacy_reply_seq)
                            SELECT 'legacy_reply_' || p.id, t.pr_id, 'local', t.path, t.side, t.line, t.start_line, t.line_text, t.diff_digest, t.head_sha, t.anchor_state, t.anchor_reason, t.resolved, p.created_at, p.updated_at, t.subject, t.id, p.original_seq
                            FROM legacy_pending_replies p JOIN review_threads t ON t.id = p.thread_id;
                            INSERT INTO review_comment_drafts (id, thread_id, body, state, created_at, updated_at, publish_selected, reserved_attempt_id)
                            SELECT id, 'legacy_reply_' || id, body, 'draft', created_at, updated_at, selected, reserved_attempt_id FROM legacy_pending_replies;
                            DROP TABLE legacy_pending_replies;")?;
                    }
                }
            }
            if i + 1 > schema::MIGRATION_TABLE_VERSION {
                tx.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    rusqlite::params![(i + 1) as i64, crate::util::now_rfc3339()],
                )?;
            }
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
            tx.commit()?;
        }
        Ok(())
    }
}

fn migrate_colliding_v3(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    const ANALYSIS_TABLES: &[&str] = &[
        "analysis_snapshots",
        "analysis_current_snapshots",
        "analysis_runs",
        "review_findings",
        "finding_occurrences",
        "finding_evidence",
        "finding_evidence_links",
        "finding_relations",
        "finding_aliases",
        "imported_source_records",
        "imported_source_aliases",
        "finding_source_links",
        "finding_user_state",
    ];
    let exists = |name: &str| -> Result<bool> {
        Ok(tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [name],
            |r| r.get::<_, bool>(0),
        )?)
    };
    let mut analysis_count = 0;
    for table in ANALYSIS_TABLES {
        analysis_count += usize::from(exists(table)?);
    }
    let mut metadata_count = 0;
    for &(table, column, _) in schema::GITHUB_METADATA {
        let mut stmt = tx.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        metadata_count += usize::from(columns.iter().any(|c| c == column));
    }
    let ledger = exists("schema_migrations")?;
    if !(analysis_count == 0 || analysis_count == ANALYSIS_TABLES.len())
        || !(metadata_count == 0 || metadata_count == schema::GITHUB_METADATA.len())
        || (analysis_count == 0 && (metadata_count != schema::GITHUB_METADATA.len() || ledger))
        || (analysis_count == ANALYSIS_TABLES.len() && !ledger)
    {
        bail!("ambiguous v3 schema: refusing to modify partially migrated database");
    }
    if ledger {
        let versions: Vec<i64> = tx
            .prepare("SELECT version FROM schema_migrations ORDER BY version")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if versions != [1, 2, 3] {
            bail!("ambiguous v3 migration ledger: expected versions 1, 2, 3");
        }
    }
    if analysis_count == 0 {
        tx.execute_batch(schema::MIGRATIONS[2])?;
    }
    if metadata_count == 0 {
        for &(table, column, kind) in schema::GITHUB_METADATA {
            tx.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {kind}"))?;
        }
    }
    Ok(())
}

/// The PR branch also shipped a v4 with REST columns but without main's
/// analysis tables or GitHub metadata. Reconcile that shape transactionally.
fn migrate_rest_columns(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let has_column = |table: &str, column: &str| -> Result<bool> {
        let mut stmt = tx.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(columns.iter().any(|name| name == column))
    };
    let has_analysis: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'analysis_snapshots')",
        [], |r| r.get(0))?;
    if !has_analysis {
        // Only the legacy PR v4 has these columns without the main v3 schema.
        if !has_column("review_comments", "source_status")? {
            bail!("ambiguous v4 schema: missing analysis tables and REST status");
        }
        tx.execute_batch(schema::MIGRATIONS[2])?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (4, ?1)",
            [crate::util::now_rfc3339()],
        )?;
        for &(table, column, kind) in schema::GITHUB_METADATA {
            if !has_column(table, column)? {
                tx.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {kind}"))?;
            }
        }
    }
    if !has_column("review_comments", "in_reply_to_id")? {
        tx.execute_batch("ALTER TABLE review_comments ADD COLUMN in_reply_to_id INTEGER")?;
    }
    tx.execute_batch("CREATE INDEX IF NOT EXISTS idx_comments_github_database_id ON review_comments(github_comment_id)")?;
    if !has_column("review_comments", "source_status")? {
        tx.execute_batch("ALTER TABLE review_comments ADD COLUMN source_status TEXT NOT NULL DEFAULT 'available' CHECK (source_status IN ('available', 'source_deleted_or_unavailable'))")?;
    }
    Ok(())
}

/// A missing record; the API maps it to 404.
#[derive(Debug)]
pub struct NotFound(pub String);

impl std::fmt::Display for NotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} not found", self.0)
    }
}

impl std::error::Error for NotFound {}

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
    use crate::model::{
        Actor, AnchorState, Comment, PublishState, RuleAction, RuleKind, Side, ThreadSource,
    };
    use crate::util::now_rfc3339;
    use snapshots::SNAPSHOT_RETENTION;

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
        s.start_publication(&pr, "original", r#"{"attemptOrdinal":1}"#, "head", &[])
            .unwrap();
        assert!(
            s.start_publication(&pr, "edited", r#"{"attemptOrdinal":2}"#, "head", &[])
                .is_err()
        );
    }

    #[test]
    fn stale_reconciliation_cannot_clear_a_new_claim() {
        let mut s = store();
        let repo = s.resolve_repository("github.com", "o", "r", None).unwrap();
        let pr = s.upsert_pull_request(&repo.id, 1, "t", "u").unwrap();
        let old = s
            .start_publication(&pr, "old", r#"{"attemptOrdinal":1}"#, "head", &[])
            .unwrap();
        s.reconcile_publication(&pr, &old, false).unwrap();
        let current = s
            .start_publication(&pr, "current", r#"{"attemptOrdinal":2}"#, "head", &[])
            .unwrap();
        assert!(s.reconcile_publication(&pr, &old, false).is_err());
        assert!(s.reconcile_publication(&pr, &old, true).is_err());
        assert_eq!(s.active_publication(&pr).unwrap().unwrap().0, current);
        // Only an explicitly reconciled partial attempt may release this claim.
        s.reconcile_publication(&pr, &current, false).unwrap();
        assert!(s.active_publication(&pr).unwrap().is_none());
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
                        .start_publication(
                            &pr,
                            &format!("plan-{i}"),
                            r#"{"attemptOrdinal":1}"#,
                            "head",
                            &[],
                        )
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
                subject: crate::model::ThreadSubject::Line,
                finding_id: None,
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
        let draft = s
            .upsert_review_comment(&pr, "t", "original", Some("c"))
            .unwrap();
        assert!(
            s.start_publication(
                &pr,
                "wrong",
                r#"{"attemptOrdinal":1}"#,
                "head",
                &[(draft.id.as_str(), "edited")]
            )
            .is_err()
        );
        // Failed reservation rolls back; only the explicit prepared draft is claimed.
        let attempt = s
            .start_publication(
                &pr,
                "correct",
                r#"{"attemptOrdinal":1}"#,
                "head",
                &[(draft.id.as_str(), "original")],
            )
            .unwrap();
        assert!(s.upsert_review_comment(&pr, "t", "edited", None).is_err());
        assert!(s.clear_review_comment(&pr, "t").is_err());
        assert_eq!(s.get_comment("c").unwrap().body, "original");
        s.mark_prepared_published(&draft.id, &attempt, Some(42), 7, "head")
            .unwrap();
        assert!(
            s.mark_prepared_published(&draft.id, &attempt, Some(43), 7, "head")
                .is_err()
        );
        assert_eq!(
            s.list_threads(&pr).unwrap()[0].published_comments[0].github_comment_id,
            Some(42)
        );
    }

    #[test]
    fn deleting_last_private_comment_removes_thread() {
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
                subject: crate::model::ThreadSubject::Line,
                finding_id: None,
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
                publish_state: PublishState::Private,
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
