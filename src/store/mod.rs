//! SQLite persistence for rules, snapshots, review progress and discussions.
//!
//! The database lives in the OS data directory (never in a repository).
//! Set `ASIDE_DATA_DIR` to override. Demo sessions use an in-memory database.

mod publication;
mod repositories;
mod review;
mod rules;
mod schema;
mod snapshots;
mod threads;

pub use publication::{NotADraft, ReservedDraft};
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
            tx.execute_batch(sql)
                .with_context(|| format!("migration v{} failed", i + 1))?;
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
            tx.commit()?;
        }
        Ok(())
    }
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
