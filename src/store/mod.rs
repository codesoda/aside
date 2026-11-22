//! SQLite persistence for rules, snapshots, review progress and discussions.
//!
//! The database lives in the OS data directory (never in a repository).
//! Set `ASIDE_DATA_DIR` to override. Demo sessions use an in-memory database.

mod analysis;
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
            if i + 1 == 4 {
                // Both branches released a different v3. Inspect under the write
                // transaction before applying only the missing half.
                migrate_colliding_v3(&tx)?;
            } else {
                if i + 1 == 5 || i + 1 == 6 {
                    migrate_rest_columns(&tx)?;
                }
                tx.execute_batch(sql)
                    .with_context(|| format!("migration v{} failed", i + 1))?;
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
        s.start_publication(&pr, "original", "{}", "head", &[], 0)
            .unwrap();
        assert!(
            s.start_publication(&pr, "edited", "{}", "head", &[], 0)
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
                        .start_publication(&pr, &format!("plan-{i}"), "{}", "head", &[], 0)
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
                publish_selected: true,
            },
            None,
        )
        .unwrap();
        assert!(
            s.start_publication(&pr, "wrong", "{}", "head", &[("c", "edited")], 1)
                .is_err()
        );
        // The successful claim below proves the failed reservation rolled back the claim.
        s.start_publication(&pr, "correct", "{}", "head", &[("c", "original")], 1)
            .unwrap();
        assert!(s.update_comment_body("c", "edited").is_err());
        assert!(s.delete_comment("c").is_err());
        assert_eq!(s.get_comment("c").unwrap().body, "original");
        s.mark_comment_published("c", Some(42)).unwrap();
        assert!(s.mark_comment_published("c", Some(43)).is_err());
        assert_eq!(s.get_comment("c").unwrap().github_comment_id, Some(42));
    }

    #[test]
    fn selection_changes_are_atomic_and_invalidate_claims_in_both_directions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("selection.db");
        let mut s = Store::open(&path).unwrap();
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
                head_sha: "h".into(),
                anchor_state: AnchorState::Current,
                anchor_reason: None,
                resolved: false,
                created_at: None,
            },
        )
        .unwrap();
        for id in ["a", "b"] {
            s.insert_comment(
                &Comment {
                    id: id.into(),
                    thread_id: "t".into(),
                    author_kind: Actor::Human,
                    author: "me".into(),
                    body: id.into(),
                    created_at: "now".into(),
                    updated_at: "now".into(),
                    publish_state: PublishState::Draft,
                    github_comment_id: None,
                    publish_selected: true,
                },
                None,
            )
            .unwrap();
        }
        assert_eq!(
            s.set_publish_selection(&pr, &["b".into()], false)
                .unwrap()
                .1,
            3
        );
        assert_eq!(
            s.set_publish_selection(&pr, &["b".into()], false)
                .unwrap()
                .1,
            3
        );
        assert!(
            s.set_publish_selection(&pr, &["a".into(), "missing".into()], false)
                .is_err()
        );
        assert!(s.get_comment("a").unwrap().publish_selected);
        let mut other = Store::open(&path).unwrap();
        // Included -> omitted after preview; claim on another connection rolls back.
        s.set_publish_selection(&pr, &["a".into()], false).unwrap();
        assert!(
            other
                .start_publication(&pr, "one", "{}", "h", &[("a", "a")], 3)
                .is_err()
        );
        // Omitted -> included after preview is equally invalid, even if the
        // originally included draft is unchanged.
        s.set_publish_selection(&pr, &["a".into()], true).unwrap();
        s.set_publish_selection(&pr, &["b".into()], true).unwrap();
        assert!(
            other
                .start_publication(&pr, "two", "{}", "h", &[("a", "a")], 5)
                .is_err()
        );
        let count: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM publication_attempts", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
        let revision = s.review_state(&pr).unwrap().selection_revision;
        let claim = other
            .start_publication(&pr, "three", "{}", "h", &[("a", "a")], revision)
            .unwrap();
        assert!(s.set_publish_selection(&pr, &["a".into()], false).is_err());
        assert!(s.set_publish_selection(&pr, &["b".into()], false).is_ok());
        assert_eq!(
            s.unresolved_publication(&pr).unwrap().unwrap().attempt_id,
            claim
        );
        let revision = s.review_state(&pr).unwrap().selection_revision;
        drop(s);
        drop(other);
        let reopened = Store::open(&path).unwrap();
        assert!(!reopened.get_comment("b").unwrap().publish_selected);
        assert_eq!(
            reopened.review_state(&pr).unwrap().selection_revision,
            revision
        );
        assert_eq!(
            reopened
                .review_state(&pr)
                .unwrap()
                .unresolved_publication
                .unwrap()
                .attempt_id,
            claim
        );
    }

    #[test]
    fn thread_selection_batch_changes_each_draft_once_and_one_revision() {
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
                line: None,
                start_line: None,
                line_text: None,
                diff_digest: None,
                head_sha: "h".into(),
                anchor_state: AnchorState::Current,
                anchor_reason: None,
                resolved: false,
                created_at: None,
            },
        )
        .unwrap();
        for id in ["root", "reply"] {
            s.insert_comment(
                &Comment {
                    id: id.into(),
                    thread_id: "t".into(),
                    author_kind: Actor::Human,
                    author: "me".into(),
                    body: id.into(),
                    created_at: "now".into(),
                    updated_at: "now".into(),
                    publish_state: PublishState::Draft,
                    github_comment_id: None,
                    publish_selected: true,
                },
                None,
            )
            .unwrap();
        }
        let (changed, revision) = s
            .set_publish_selection(&pr, &["root".into(), "reply".into()], false)
            .unwrap();
        assert_eq!(changed, ["root", "reply"]);
        assert_eq!(revision, 3);
        assert!(
            s.list_threads(&pr).unwrap()[0]
                .comments
                .iter()
                .all(|c| !c.publish_selected)
        );
        assert_eq!(
            s.set_publish_selection(&pr, &["root".into(), "reply".into()], false)
                .unwrap()
                .1,
            3
        );
        s.conn
            .execute(
                "UPDATE review_comments SET publish_state = 'imported' WHERE id = 'root'",
                [],
            )
            .unwrap();
        assert!(
            s.set_publish_selection(&pr, &["reply".into(), "root".into()], true)
                .is_err()
        );
        assert!(!s.get_comment("reply").unwrap().publish_selected);
        s.conn
            .execute(
                "UPDATE review_comments SET publish_state = 'published' WHERE id = 'root'",
                [],
            )
            .unwrap();
        assert!(
            s.set_publish_selection(&pr, &["root".into()], true)
                .is_err()
        );
        assert_eq!(s.review_state(&pr).unwrap().selection_revision, 3);
    }

    #[test]
    fn concurrent_selection_setter_and_claim_serialize_across_connections() {
        for flip_selected in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("race.db");
            let mut setup = Store::open(&path).unwrap();
            let repo = setup
                .resolve_repository("github.com", "o", "r", None)
                .unwrap();
            let pr = setup.upsert_pull_request(&repo.id, 1, "t", "u").unwrap();
            setup
                .insert_thread(
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
                        head_sha: "h".into(),
                        anchor_state: AnchorState::Current,
                        anchor_reason: None,
                        resolved: false,
                        created_at: None,
                    },
                )
                .unwrap();
            for id in ["a", "b"] {
                setup
                    .insert_comment(
                        &Comment {
                            id: id.into(),
                            thread_id: "t".into(),
                            author_kind: Actor::Human,
                            author: "me".into(),
                            body: id.into(),
                            created_at: "now".into(),
                            updated_at: "now".into(),
                            publish_state: PublishState::Draft,
                            github_comment_id: None,
                            publish_selected: true,
                        },
                        None,
                    )
                    .unwrap();
            }
            if flip_selected {
                setup
                    .set_publish_selection(&pr, &["b".into()], false)
                    .unwrap();
            }
            let revision = setup.review_state(&pr).unwrap().selection_revision;
            drop(setup);
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let setter = {
                let (path, pr, barrier) = (path.clone(), pr.clone(), barrier.clone());
                std::thread::spawn(move || {
                    let mut s = Store::open(&path).unwrap();
                    barrier.wait();
                    s.set_publish_selection(&pr, &["b".into()], flip_selected)
                        .unwrap();
                })
            };
            let claim = {
                let (path, pr, barrier) = (path.clone(), pr.clone(), barrier.clone());
                std::thread::spawn(move || {
                    let mut s = Store::open(&path).unwrap();
                    barrier.wait();
                    s.start_publication(&pr, "plan", "{}", "h", &[("a", "a")], revision)
                        .is_ok()
                })
            };
            setter.join().unwrap();
            let claimed = claim.join().unwrap();
            let s = Store::open(&path).unwrap();
            let count: i64 = s
                .conn
                .query_row("SELECT COUNT(*) FROM publication_attempts", [], |r| {
                    r.get(0)
                })
                .unwrap();
            let reservations: i64 = s
                .conn
                .query_row("SELECT COUNT(*) FROM publication_reservations", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(count, i64::from(claimed));
            assert_eq!(reservations, i64::from(claimed));
            assert_eq!(
                s.review_state(&pr).unwrap().selection_revision,
                revision + 1
            );
            assert_eq!(s.get_comment("b").unwrap().publish_selected, flip_selected);
        }
    }

    #[test]
    fn draft_creation_and_deletion_invalidate_previous_selection_revision() {
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
                line: None,
                start_line: None,
                line_text: None,
                diff_digest: None,
                head_sha: "h".into(),
                anchor_state: AnchorState::Current,
                anchor_reason: None,
                resolved: false,
                created_at: None,
            },
        )
        .unwrap();
        assert_eq!(s.review_state(&pr).unwrap().selection_revision, 0);
        s.insert_comment(
            &Comment {
                id: "c".into(),
                thread_id: "t".into(),
                author_kind: Actor::Human,
                author: "me".into(),
                body: "body".into(),
                created_at: "now".into(),
                updated_at: "now".into(),
                publish_state: PublishState::Draft,
                github_comment_id: None,
                publish_selected: true,
            },
            None,
        )
        .unwrap();
        assert_eq!(s.review_state(&pr).unwrap().selection_revision, 1);
        assert!(
            s.start_publication(&pr, "stale", "{}", "h", &[], 0)
                .is_err()
        );
        s.delete_comment("c").unwrap();
        assert_eq!(s.review_state(&pr).unwrap().selection_revision, 2);
        assert!(
            s.start_publication(&pr, "stale-again", "{}", "h", &[], 1)
                .is_err()
        );
        assert_eq!(
            s.conn
                .query_row("SELECT COUNT(*) FROM publication_attempts", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
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
                publish_selected: true,
            },
            None,
        )
        .unwrap();
        assert_eq!(s.list_threads(&pr).unwrap().len(), 1);
        s.delete_comment("c1").unwrap();
        assert!(s.list_threads(&pr).unwrap().is_empty());
    }
}
