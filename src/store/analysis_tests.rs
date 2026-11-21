//! Persistence tests for analysis runs, findings, evidence, imports and triage.

use std::path::Path;

use rusqlite::Connection;
use serde_json::json;

use super::*;
use crate::model::{
    ANALYSIS_SCHEMA_VERSION, Acknowledgement, Actor, AnalysisMode, AnchorKind, Applicability,
    AssertionKind, AttentionAssessment, AttentionLevel, CodeAnchor, Completeness, EvidenceId,
    EvidenceKind, FindingKeyInput, FindingKind, FindingSource, NativeSourceKey, RunKeyInput,
    Severity, SeverityRecord, SourceKind, SourceStatus, ValidationRecord, ValidationStatus,
};
use crate::store::schema::{self, MIGRATIONS};

const REPO: &str = "R_test";

struct Fixture {
    store: Store,
    pr: String,
}

fn fixture_at(store: Store) -> Fixture {
    let mut store = store;
    let repo = store
        .resolve_repository("github.com", "acme", "widget", Some(REPO))
        .unwrap();
    let pr = store.upsert_pull_request(&repo.id, 42, "t", "u").unwrap();
    Fixture { store, pr }
}

fn fixture() -> Fixture {
    fixture_at(Store::open_in_memory().unwrap())
}

fn snap(head: &str) -> SnapshotId {
    SnapshotId::derive(REPO, 42, "base", head)
}

impl Fixture {
    fn add_snapshot(&mut self, head: &str, current: bool) -> SnapshotId {
        let id = snap(head);
        self.store
            .register_analysis_snapshot(&self.pr, &id, "base", head, None)
            .unwrap();
        if current {
            self.store
                .set_current_analysis_snapshot(&self.pr, &id)
                .unwrap();
        }
        id
    }

    fn queue(&mut self, snapshot: &SnapshotId, provider: &str, attempt: u32) -> AnalysisRun {
        let run = AnalysisRun::queued(
            &RunKeyInput {
                snapshot_id: snapshot,
                provider_id: provider,
                engine_version: "1",
                configuration_digest: "cfg",
                input_digest: "in",
                attempt,
            },
            AnalysisMode::Static,
        );
        self.store
            .record_analysis_run(&self.pr, &Raw::new(run.clone()).unwrap())
            .unwrap();
        run
    }

    fn count(&self, table: &str) -> i64 {
        self.store
            .conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    fn labels(&self) -> Vec<(String, ObservationLabel)> {
        self.store
            .list_findings(&self.pr, true)
            .unwrap()
            .into_iter()
            .map(|f| (f.finding.value().title.clone(), f.label))
            .collect()
    }

    fn label_of(&self, title: &str) -> ObservationLabel {
        self.labels()
            .into_iter()
            .find(|(t, _)| t == title)
            .unwrap_or_else(|| panic!("{title} missing"))
            .1
    }
}

fn finished(mut run: AnalysisRun, status: RunStatus) -> Raw<AnalysisRun> {
    run.status = status;
    run.started_at = Some("2026-01-01T00:00:00Z".into());
    run.finished_at = Some("2026-01-01T00:00:01Z".into());
    run.coverage.completeness = Completeness::Complete;
    Raw::new(run).unwrap()
}

fn evidence(run: &AnalysisRun, label: &str) -> Evidence {
    Evidence {
        schema_version: ANALYSIS_SCHEMA_VERSION,
        id: EvidenceId::derive(&crate::model::EvidenceKeyInput {
            kind: EvidenceKind::Measurement,
            source_run_id: Some(&run.id),
            label,
            method: None,
            input_digest: None,
            value_json: None,
        }),
        kind: EvidenceKind::Measurement,
        source_run_id: Some(run.id.clone()),
        source_entity_id: None,
        anchor: None,
        label: label.into(),
        value: Some(json!(17)),
        unit: None,
        numerator: None,
        denominator: None,
        method: None,
        input_digest: None,
        observed_at: "2026-01-01T00:00:01Z".into(),
        limitations: vec![],
    }
}

/// A finding whose ID depends only on `claim` (stable across snapshots).
fn finding(run: &AnalysisRun, claim: &str, evidence: &[&Evidence]) -> ReviewFinding {
    let mut anchor = CodeAnchor::new(AnchorKind::File);
    anchor.file_id = Some("file_a".into());
    anchor.current_path = Some("src/a.rs".into());
    ReviewFinding {
        schema_version: ANALYSIS_SCHEMA_VERSION,
        id: FindingId::derive(&FindingKeyInput {
            canonical_repo_id: REPO,
            pr_number: 42,
            provider_id: &run.provider_id,
            rule_id: "rule",
            claim_identity: claim,
            subject_identity: None,
            code_fingerprint: None,
        }),
        repository_id: REPO.into(),
        pr_number: 42,
        observed_snapshot_id: run.snapshot_id.clone(),
        source: FindingSource {
            kind: SourceKind::AsideStatic,
            provider_id: run.provider_id.clone(),
            run_id: Some(run.id.clone()),
            rule_id: Some("rule".into()),
            rule_version: None,
            model_id: None,
            model_revision: None,
            prompt_version: None,
            native: None,
            github_actor: None,
        },
        assertion_kind: AssertionKind::AttentionHint,
        kinds: vec![FindingKind::Churn],
        title: claim.into(),
        body: "b".into(),
        rationale: "r".into(),
        suggested_action: None,
        anchor,
        related_anchors: vec![],
        evidence_ids: evidence.iter().map(|e| e.id.clone()).collect(),
        attention: AttentionAssessment {
            level: AttentionLevel::Investigate,
            score: None,
            score_version: None,
            reasons: vec![],
        },
        severity: SeverityRecord {
            normalized: Severity::Unknown,
            original: None,
        },
        confidence: None,
        validation: ValidationRecord {
            status: ValidationStatus::NotRequired,
            method: None,
            validated_at: None,
            note: None,
        },
        applicability: Applicability::CurrentExact,
        source_status: SourceStatus::Unknown,
        first_seen_at: "2026-01-01T00:00:01Z".into(),
        last_seen_at: "2026-01-01T00:00:01Z".into(),
        external_url: None,
        parent_finding_id: None,
        correlation_group_id: None,
        extensions: serde_json::Map::new(),
    }
}

fn batch(run: &AnalysisRun, findings: Vec<ReviewFinding>, ev: Vec<Evidence>) -> AnalysisBatch {
    AnalysisBatch {
        run: finished(run.clone(), RunStatus::Succeeded),
        findings: findings.into_iter().map(|f| Raw::new(f).unwrap()).collect(),
        evidence: ev.into_iter().map(|e| Raw::new(e).unwrap()).collect(),
        relations: vec![],
    }
}

fn simple_batch(run: &AnalysisRun, claims: &[&str]) -> AnalysisBatch {
    let ev = evidence(run, "support");
    let findings = claims.iter().map(|c| finding(run, c, &[&ev])).collect();
    batch(run, findings, vec![ev])
}

fn user_version(conn: &Connection) -> i64 {
    conn.pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap()
}

fn recorded_versions(conn: &Connection) -> Vec<i64> {
    let mut stmt = conn
        .prepare("SELECT version FROM schema_migrations ORDER BY version")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn migrates_empty_database_and_records_versions() {
    let s = Store::open_in_memory().unwrap();
    assert_eq!(user_version(&s.conn), MIGRATIONS.len() as i64);
    assert_eq!(
        recorded_versions(&s.conn),
        (1..=MIGRATIONS.len() as i64).collect::<Vec<_>>()
    );
    let fk_ok: Vec<String> = s
        .conn
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(fk_ok.is_empty());
}

/// Builds a database exactly as the released v2 schema left it.
fn released_v2_database(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.pragma_update(None, "foreign_keys", "ON").unwrap();
    for sql in &MIGRATIONS[..2] {
        conn.execute_batch(sql).unwrap();
    }
    conn.pragma_update(None, "user_version", 2).unwrap();
    conn.execute_batch(
        "INSERT INTO repositories VALUES ('repo1', 'github.com', 'acme', 'widget', 'R_test', '2025-01-01');
         INSERT INTO repository_aliases VALUES ('github.com/acme/widget', 'repo1');
         INSERT INTO pull_requests VALUES ('pr1', 'repo1', 42, 't', 'u', '2025-01-01');
         INSERT INTO review_sessions (pr_id, created_at, last_opened_at) VALUES ('pr1', '2025-01-01', '2025-01-01');
         INSERT INTO review_acknowledgements VALUES
           ('ack1', 'pr1', 'src/a.rs', 'reviewed', 1, 'd', NULL, 'h', 'b', 'human', '2025-01-01', 1);",
    )
    .unwrap();
}

#[test]
fn migrates_released_v2_schema_and_keeps_review_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v2.db");
    released_v2_database(&path);
    let mut s = Store::open(&path).unwrap();
    assert_eq!(user_version(&s.conn), MIGRATIONS.len() as i64);
    assert_eq!(recorded_versions(&s.conn), vec![1, 2, 3, 4, 5, 6]);
    let acks = s.list_acknowledgements("pr1").unwrap();
    assert_eq!(acks.len(), 1);
    assert_eq!(acks[0].path, "src/a.rs");
    // The migrated database accepts analysis data for the existing PR.
    let id = snap("h");
    s.register_analysis_snapshot("pr1", &id, "b", "h", None)
        .unwrap();
    s.set_current_analysis_snapshot("pr1", &id).unwrap();
    drop(s);
    // Re-opening is a no-op: migrations are forward-only and applied once.
    let s = Store::open(&path).unwrap();
    assert_eq!(recorded_versions(&s.conn), vec![1, 2, 3, 4, 5, 6]);
    assert_eq!(s.current_analysis_snapshot("pr1").unwrap(), Some(id));
}

#[test]
fn branch_v5_rest_schema_gains_main_import_memberships() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("branch-v5.db");
    released_v2_database(&path);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(MIGRATIONS[2]).unwrap();
    for &(table, column, kind) in schema::GITHUB_METADATA {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {kind}"))
            .unwrap();
    }
    conn.execute_batch(
        "INSERT INTO schema_migrations (version, applied_at) VALUES
         (4, '2025-01-01'), (5, '2025-01-01');
         ALTER TABLE review_comments ADD COLUMN in_reply_to_id INTEGER;
         CREATE INDEX idx_comments_github_database_id ON review_comments(github_comment_id);
         ALTER TABLE review_comments ADD COLUMN source_status TEXT NOT NULL DEFAULT 'available';",
    )
    .unwrap();
    conn.pragma_update(None, "user_version", 5).unwrap();
    drop(conn);
    let store = Store::open(&path).unwrap();
    assert_eq!(user_version(&store.conn), 6);
    assert_eq!(recorded_versions(&store.conn), vec![1, 2, 3, 4, 5, 6]);
    let count: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'imported_source_prs'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn colliding_v3_schemas_converge_without_losing_rows() {
    for main_v3 in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("collision.db");
        released_v2_database(&path);
        let conn = Connection::open(&path).unwrap();
        if main_v3 {
            for &(table, column, kind) in schema::GITHUB_METADATA {
                conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {kind}"))
                    .unwrap();
            }
        } else {
            conn.execute_batch(MIGRATIONS[2]).unwrap();
            conn.execute_batch(
                "INSERT INTO analysis_snapshots
                 (id, pr_id, base_sha, head_sha, registered_at, seq)
                 VALUES ('historical', 'pr1', 'b', 'h', '2025-01-01', 1);
                 INSERT INTO finding_user_state
                 (session_id, finding_id, pr_id, disposition, updated_at, revision)
                 VALUES ('session', 'finding', 'pr1', 'investigating', '2025-01-01', 1);",
            )
            .unwrap();
        }
        conn.execute_batch(
            "INSERT INTO review_threads
             (id, pr_id, source, path, side, head_sha, anchor_state, created_at, updated_at)
             VALUES ('thread', 'pr1', 'github', 'src/a.rs', 'RIGHT', 'h', 'current', '2025-01-01', '2025-01-01');
             INSERT INTO review_comments
             (id, thread_id, author_kind, author, body, publish_state, created_at, updated_at, seq)
             VALUES ('comment', 'thread', 'github', 'alice', 'original', 'published', '2025-01-01', '2025-01-01', 1);",
        ).unwrap();
        if main_v3 {
            conn.execute(
                "UPDATE review_threads SET original_line = 7 WHERE id = 'thread'",
                [],
            )
            .unwrap();
            conn.execute(
                "UPDATE review_comments SET author_node_id = 'author' WHERE id = 'comment'",
                [],
            )
            .unwrap();
        }
        conn.pragma_update(None, "user_version", 3).unwrap();
        drop(conn);
        for _ in 0..2 {
            let s = Store::open(&path).unwrap();
            assert_eq!(user_version(&s.conn), MIGRATIONS.len() as i64);
            assert_eq!(recorded_versions(&s.conn), vec![1, 2, 3, 4, 5, 6]);
            let body: String = s
                .conn
                .query_row(
                    "SELECT body FROM review_comments WHERE id = 'comment'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(body, "original");
            let line: Option<i64> = s
                .conn
                .query_row(
                    "SELECT original_line FROM review_threads WHERE id = 'thread'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(line, main_v3.then_some(7));
            let author: Option<String> = s
                .conn
                .query_row(
                    "SELECT author_node_id FROM review_comments WHERE id = 'comment'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(author.as_deref(), main_v3.then_some("author"));
            let count: i64 = s
                .conn
                .query_row("SELECT COUNT(*) FROM analysis_snapshots", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, usize::from(!main_v3) as i64);
            let triage: i64 = s
                .conn
                .query_row("SELECT COUNT(*) FROM finding_user_state", [], |r| r.get(0))
                .unwrap();
            assert_eq!(triage, count);
            assert_eq!(s.list_acknowledgements("pr1").unwrap().len(), 1);
        }
    }
}

#[test]
fn partial_v3_refuses_upgrade_without_changing_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("partial.db");
    released_v2_database(&path);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("ALTER TABLE review_threads ADD COLUMN original_line INTEGER;")
        .unwrap();
    conn.pragma_update(None, "user_version", 3).unwrap();
    drop(conn);
    assert!(Store::open(&path).is_err());
    let conn = Connection::open(&path).unwrap();
    assert_eq!(user_version(&conn), 3);
    let ledger: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'schema_migrations'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ledger, 0);
}

#[test]
fn analysis_tables_never_reference_review_state() {
    let s = Store::open_in_memory().unwrap();
    let analysis_tables = [
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
    for table in analysis_tables {
        let targets: Vec<String> = s
            .conn
            .prepare(&format!("PRAGMA foreign_key_list({table})"))
            .unwrap()
            .query_map([], |r| r.get(2))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        for t in targets {
            assert!(
                !matches!(
                    t.as_str(),
                    "review_acknowledgements"
                        | "review_sessions"
                        | "review_threads"
                        | "review_comments"
                ),
                "{table} references {t}"
            );
        }
    }
    // No trigger can write review state behind the store's back.
    let triggers: i64 = s
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(triggers, 0);
}

#[test]
fn restart_restores_current_and_historical_without_false_current_labels() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("labels.db");
    let (snap_a, snap_b);
    {
        let mut f = fixture_at(Store::open(&path).unwrap());
        snap_a = f.add_snapshot("head-a", true);
        let run_a = f.queue(&snap_a, "aside.churn", 0);
        let late = f.queue(&snap_a, "aside.slow", 0);
        f.store
            .commit_analysis_batch(&simple_batch(&run_a, &["only-on-a", "on-both"]))
            .unwrap();
        assert_eq!(f.label_of("only-on-a"), ObservationLabel::Current);

        snap_b = f.add_snapshot("head-b", true);
        let run_b = f.queue(&snap_b, "aside.churn", 0);
        f.store
            .commit_analysis_batch(&simple_batch(&run_b, &["on-both", "only-on-b"]))
            .unwrap();

        // Results for snapshot A arriving after B became current.
        let outcome = f
            .store
            .commit_analysis_batch(&simple_batch(&late, &["late-on-a"]))
            .unwrap();
        assert!(outcome.stale);
        assert_eq!(outcome.stored_status, RunStatus::Stale);
    }

    let f = Fixture {
        store: Store::open(&path).unwrap(),
        pr: {
            let s = Store::open(&path).unwrap();
            let id: String = s
                .conn
                .query_row("SELECT id FROM pull_requests", [], |r| r.get(0))
                .unwrap();
            id
        },
    };
    assert_eq!(
        f.store.current_analysis_snapshot(&f.pr).unwrap(),
        Some(snap_b.clone())
    );
    assert_eq!(f.label_of("only-on-a"), ObservationLabel::Historical);
    assert_eq!(f.label_of("late-on-a"), ObservationLabel::Historical);
    assert_eq!(f.label_of("on-both"), ObservationLabel::Current);
    assert_eq!(f.label_of("only-on-b"), ObservationLabel::Current);

    let current: Vec<String> = f
        .store
        .list_findings(&f.pr, false)
        .unwrap()
        .into_iter()
        .map(|s| s.finding.value().title.clone())
        .collect();
    assert_eq!(current.len(), 2);
    assert!(current.iter().all(|t| t == "on-both" || t == "only-on-b"));

    let both = f
        .store
        .list_findings(&f.pr, true)
        .unwrap()
        .into_iter()
        .find(|s| s.finding.value().title == "on-both")
        .unwrap();
    assert_eq!(both.latest_snapshot_id, snap_b);
    // The stored payload is the first one produced, never relabelled.
    assert_eq!(both.finding.value().observed_snapshot_id, snap_a);

    let late_run = f
        .store
        .list_analysis_runs(&f.pr)
        .unwrap()
        .into_iter()
        .find(|r| r.value().provider_id == "aside.slow")
        .unwrap();
    assert_eq!(late_run.value().status, RunStatus::Stale);
    let reported: String = f
        .store
        .conn
        .query_row(
            "SELECT reported_status FROM analysis_runs WHERE id = ?1",
            [late_run.value().id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(reported, "succeeded");
}

#[test]
fn non_current_applicability_and_unfinished_or_stale_runs_are_not_current() {
    let mut f = fixture();
    let s = f.add_snapshot("h", true);
    let run = f.queue(&s, "p", 0);
    let mut outdated = finding(&run, "outdated", &[]);
    outdated.applicability = Applicability::HistoricalOutdated;
    let mut unknown = finding(&run, "unknown", &[]);
    unknown.applicability = Applicability::Unknown;
    let fresh = finding(&run, "fresh", &[]);
    f.store
        .commit_analysis_batch(&batch(&run, vec![outdated, unknown, fresh], vec![]))
        .unwrap();
    assert_eq!(f.label_of("outdated"), ObservationLabel::Historical);
    assert_eq!(f.label_of("unknown"), ObservationLabel::Historical);
    assert_eq!(f.label_of("fresh"), ObservationLabel::Current);
}

#[test]
fn duplicate_delivery_is_idempotent() {
    let mut f = fixture();
    let s = f.add_snapshot("h", true);
    let run = f.queue(&s, "p", 0);
    let mut b = simple_batch(&run, &["x", "y"]);
    b.relations.push(FindingRelation {
        source_finding_id: b.findings[0].value().id.clone(),
        kind: crate::model::RelationKind::Supports,
        target: RelationTarget::Finding(b.findings[1].value().id.clone()),
        method: "test".into(),
        method_version: "1".into(),
        score: None,
        validated: false,
        created_at: "now".into(),
    });
    f.store.commit_analysis_batch(&b).unwrap();
    f.store.commit_analysis_batch(&b).unwrap();
    assert_eq!(f.count("review_findings"), 2);
    assert_eq!(f.count("finding_occurrences"), 2);
    assert_eq!(f.count("finding_evidence"), 1);
    assert_eq!(f.count("finding_evidence_links"), 2);
    assert_eq!(
        f.store
            .list_finding_relations(&b.findings[0].value().id)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        f.store
            .list_finding_evidence(&b.findings[0].value().id)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn records_scheduler_run_states_and_rejects_reopening_terminal_runs() {
    let mut f = fixture();
    let s = f.add_snapshot("h", true);
    let run = f.queue(&s, "p", 0);
    let mut running = run.clone();
    running.status = RunStatus::Running;
    f.store
        .record_analysis_run(&f.pr, &Raw::new(running.clone()).unwrap())
        .unwrap();
    f.store
        .record_analysis_run(&f.pr, &finished(run.clone(), RunStatus::Partial))
        .unwrap();
    assert_eq!(
        f.store.get_analysis_run(&run.id).unwrap().value().status,
        RunStatus::Partial
    );
    assert!(
        f.store
            .record_analysis_run(&f.pr, &Raw::new(running).unwrap())
            .is_err(),
        "a retry must be a new attempt"
    );
    for (attempt, status) in [(1, RunStatus::Skipped), (2, RunStatus::Failed)] {
        let retry = f.queue(&s, "p", attempt);
        f.store
            .record_analysis_run(&f.pr, &finished(retry.clone(), status))
            .unwrap();
        assert_eq!(
            f.store.get_analysis_run(&retry.id).unwrap().value().status,
            status
        );
    }
    let cancelled = f.queue(&s, "p", 3);
    f.store
        .record_analysis_run(&f.pr, &finished(cancelled.clone(), RunStatus::Cancelled))
        .unwrap();
    assert!(
        f.store
            .commit_analysis_batch(&simple_batch(&cancelled, &["z"]))
            .is_err()
    );

    // A run's ID must match its inputs, and it is pinned to its PR.
    let mut forged = run.clone();
    forged.attempt = 9;
    assert!(
        f.store
            .record_analysis_run(&f.pr, &Raw::new(forged).unwrap())
            .is_err()
    );
    let other_pr = {
        let repo = f
            .store
            .resolve_repository("github.com", "acme", "other", Some("R_other"))
            .unwrap();
        f.store.upsert_pull_request(&repo.id, 1, "t", "u").unwrap()
    };
    assert!(
        f.store
            .record_analysis_run(&other_pr, &Raw::new(run).unwrap())
            .is_err()
    );
    // Unregistered snapshots are rejected rather than creating orphans.
    let ghost = SnapshotId::derive(REPO, 42, "base", "never-registered");
    let orphan = AnalysisRun::queued(
        &RunKeyInput {
            snapshot_id: &ghost,
            provider_id: "p",
            engine_version: "1",
            configuration_digest: "c",
            input_digest: "i",
            attempt: 0,
        },
        AnalysisMode::Static,
    );
    assert!(
        f.store
            .record_analysis_run(&f.pr, &Raw::new(orphan).unwrap())
            .is_err()
    );
}

#[test]
fn queued_runs_on_old_snapshots_finish_as_stale() {
    let mut f = fixture();
    let a = f.add_snapshot("a", true);
    let run = f.queue(&a, "p", 0);
    f.add_snapshot("b", true);
    let mut running = run.clone();
    running.status = RunStatus::Running;
    let out = f
        .store
        .record_analysis_run(&f.pr, &Raw::new(running).unwrap())
        .unwrap();
    assert_eq!(out.stored_status, RunStatus::Stale);
    let out = f
        .store
        .commit_analysis_batch(&simple_batch(&run, &["late"]))
        .unwrap();
    assert_eq!(out.stored_status, RunStatus::Stale);
    assert_eq!(f.label_of("late"), ObservationLabel::Historical);
}

#[test]
fn failed_mid_batch_write_leaves_no_partial_rows() {
    let mut f = fixture();
    let s = f.add_snapshot("h", true);
    let run = f.queue(&s, "p", 0);
    let ev = evidence(&run, "present");
    let good = finding(&run, "good", &[&ev]);
    let missing = evidence(&run, "never-written");
    // Passes contract validation; fails on the evidence FK after the run,
    // the evidence row and the first finding were already written.
    let bad = finding(&run, "bad", &[&missing]);
    let err = f
        .store
        .commit_analysis_batch(&batch(&run, vec![good, bad], vec![ev]))
        .unwrap_err();
    assert!(format!("{err:#}").contains("missing evidence"), "{err:#}");
    for table in [
        "review_findings",
        "finding_occurrences",
        "finding_evidence",
        "finding_evidence_links",
        "finding_relations",
    ] {
        assert_eq!(f.count(table), 0, "{table} has partial rows");
    }
    assert_eq!(
        f.store.get_analysis_run(&run.id).unwrap().value().status,
        RunStatus::Queued,
        "the run status update rolled back too"
    );
}

#[test]
fn evidence_cannot_be_orphaned() {
    let mut f = fixture();
    let s = f.add_snapshot("h", true);
    let run = f.queue(&s, "p", 0);
    f.store
        .commit_analysis_batch(&simple_batch(&run, &["x"]))
        .unwrap();
    let insert_orphan = f.store.conn.execute(
        "INSERT INTO finding_evidence (id, owner_run_id, kind, observed_at, evidence_json)
         VALUES ('ev_x', 'run_missing', 'measurement', 'now', '{}')",
        [],
    );
    assert!(insert_orphan.is_err());
    let dangling_link = f.store.conn.execute(
        "INSERT INTO finding_evidence_links (finding_id, evidence_id) VALUES ('find_missing', 'ev_missing')",
        [],
    );
    assert!(dangling_link.is_err());
    // Deleting the run removes its evidence and links in one step.
    f.store
        .conn
        .execute("DELETE FROM analysis_runs WHERE id = ?1", [run.id.as_str()])
        .unwrap();
    assert_eq!(f.count("finding_evidence"), 0);
    assert_eq!(f.count("finding_evidence_links"), 0);
    assert_eq!(f.count("finding_occurrences"), 0);
}

#[test]
fn invalid_findings_are_rejected_before_writing() {
    let mut f = fixture();
    let a = f.add_snapshot("a", true);
    let b = f.add_snapshot("b", false);
    let run = f.queue(&a, "p", 0);
    let other_run = f.queue(&b, "p", 0);
    let mut wrong_snapshot = finding(&run, "x", &[]);
    wrong_snapshot.observed_snapshot_id = b;
    let mut high = finding(&run, "y", &[]);
    high.attention.level = AttentionLevel::High;
    let mut foreign = finding(&run, "z", &[]);
    foreign.source.run_id = Some(other_run.id);
    for bad in [wrong_snapshot, high, foreign] {
        assert!(
            f.store
                .commit_analysis_batch(&batch(&run, vec![bad], vec![]))
                .is_err()
        );
    }
    assert_eq!(f.count("review_findings"), 0);
}

#[test]
fn concurrent_writers_wait_for_the_lock_instead_of_failing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("concurrent.db");
    let mut f = fixture_at(Store::open(&path).unwrap());
    let s = f.add_snapshot("h", true);
    let runs: Vec<AnalysisRun> = (0..4).map(|i| f.queue(&s, &format!("p{i}"), 0)).collect();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(runs.len()));
    let handles: Vec<_> = runs
        .into_iter()
        .map(|run| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut store = Store::open(&path).unwrap();
                barrier.wait();
                for i in 0..10 {
                    let claim = format!("{}-{i}", run.provider_id);
                    store
                        .commit_analysis_batch(&simple_batch(&run, &[&claim]))
                        .unwrap();
                    // Readers interleave with writers.
                    store.list_findings_count_for_test();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(f.count("review_findings"), 40);
    assert!(
        f.store
            .list_findings(&f.pr, false)
            .unwrap()
            .iter()
            .all(|s| s.label == ObservationLabel::Current)
    );
}

impl Store {
    fn list_findings_count_for_test(&self) {
        let _: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM review_findings", [], |r| r.get(0))
            .unwrap();
    }
}

#[test]
fn concurrent_triage_updates_cannot_lose_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cas.db");
    let mut f = fixture_at(Store::open(&path).unwrap());
    let s = f.add_snapshot("h", true);
    let run = f.queue(&s, "p", 0);
    let b = simple_batch(&run, &["x"]);
    f.store.commit_analysis_batch(&b).unwrap();
    let id = b.findings[0].value().id.clone();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = [
        UserDisposition::Investigating,
        UserDisposition::DismissedLocal,
    ]
    .into_iter()
    .map(|d| {
        let (path, pr, id, barrier) = (path.clone(), f.pr.clone(), id.clone(), barrier.clone());
        std::thread::spawn(move || {
            let mut store = Store::open(&path).unwrap();
            let mut state = FindingUserState::untouched(id, "session", "now");
            state.disposition = d;
            barrier.wait();
            match store.put_finding_user_state(&pr, &state, 0) {
                Ok(_) => true,
                Err(e) => {
                    assert!(e.downcast_ref::<RevisionConflict>().is_some(), "{e:#}");
                    false
                }
            }
        })
    })
    .collect();
    let wins = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .filter(|w| *w)
        .count();
    assert_eq!(wins, 1);
    assert_eq!(
        f.store
            .get_finding_user_state("session", &id)
            .unwrap()
            .unwrap()
            .revision,
        1
    );
}

#[test]
fn retention_prunes_old_runs_but_keeps_current_unfinished_and_triage() {
    let mut f = fixture();
    let first = f.add_snapshot("h0", true);
    let first_run = f.queue(&first, "p", 0);
    let b = simple_batch(&first_run, &["claim-0"]);
    f.store.commit_analysis_batch(&b).unwrap();
    let pruned_finding = b.findings[0].value().id.clone();
    let mut state = FindingUserState::untouched(pruned_finding.clone(), "session", "now");
    state.disposition = UserDisposition::DismissedLocal;
    f.store.put_finding_user_state(&f.pr, &state, 0).unwrap();

    // A slow provider still running on snapshot 1.
    let slow_snap = f.add_snapshot("h1", true);
    let slow = f.queue(&slow_snap, "slow", 0);

    let mut last = None;
    for i in 2..12 {
        let s = f.add_snapshot(&format!("h{i}"), true);
        let run = f.queue(&s, "p", 0);
        f.store
            .commit_analysis_batch(&simple_batch(&run, &[&format!("claim-{i}")]))
            .unwrap();
        last = Some((s, run));
    }
    let (current, current_run) = last.unwrap();

    let snaps: i64 = f.count("analysis_snapshots");
    assert_eq!(
        snaps,
        ANALYSIS_SNAPSHOT_RETENTION as i64 + 1,
        "retained window plus the snapshot with a running run"
    );
    assert!(f.store.get_analysis_run(&first_run.id).is_err());
    assert!(f.store.get_analysis_run(&slow.id).is_ok());
    assert!(f.store.get_analysis_run(&current_run.id).is_ok());
    assert_eq!(
        f.store.current_analysis_snapshot(&f.pr).unwrap(),
        Some(current)
    );
    assert_eq!(f.label_of("claim-11"), ObservationLabel::Current);
    assert!(f.labels().iter().all(|(t, _)| t != "claim-0"));

    // Local triage outlives pruned analysis output.
    let kept = f
        .store
        .get_finding_user_state("session", &pruned_finding)
        .unwrap()
        .unwrap();
    assert_eq!(kept.disposition, UserDisposition::DismissedLocal);

    // Pruning to zero still keeps the current snapshot's data.
    let report = f.store.prune_analysis(&f.pr, 0).unwrap();
    assert!(report.runs > 0);
    assert_eq!(f.label_of("claim-11"), ObservationLabel::Current);
    assert!(f.store.get_analysis_run(&slow.id).is_ok());
}

#[test]
fn snapshot_json_retention_does_not_break_analysis_links() {
    let mut f = fixture();
    let pr_snap = f
        .store
        .save_snapshot(&f.pr, &crate::demo::snapshot(0))
        .unwrap();
    let s = snap("h");
    f.store
        .register_analysis_snapshot(&f.pr, &s, "base", "h", Some(pr_snap))
        .unwrap();
    f.store.set_current_analysis_snapshot(&f.pr, &s).unwrap();
    for _ in 0..(crate::store::snapshots::SNAPSHOT_RETENTION + 1) {
        f.store
            .save_snapshot(&f.pr, &crate::demo::snapshot(0))
            .unwrap();
    }
    let link: Option<i64> = f
        .store
        .conn
        .query_row(
            "SELECT pr_snapshot_id FROM analysis_snapshots WHERE id = ?1",
            [s.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(link, None);
    assert_eq!(f.store.current_analysis_snapshot(&f.pr).unwrap(), Some(s));
}

#[test]
fn records_from_newer_versions_round_trip_without_degrading() {
    let mut f = fixture();
    let s = f.add_snapshot("h", true);
    let run = f.queue(&s, "p", 0);
    let mut tree = serde_json::to_value(finding(&run, "future", &[])).unwrap();
    tree["sourceStatus"] = json!("superseded_upstream");
    tree["applicability"] = json!("current_via_future_mapping");
    tree["assertionKind"] = json!("future_assertion");
    tree["kinds"] = json!(["future_kind"]);
    tree["source"]["kind"] = json!("github_future_source");
    tree["severity"]["normalized"] = json!("catastrophic");
    let written = serde_json::to_string(&tree).unwrap();
    let raw = Raw::<ReviewFinding>::from_json(&written).unwrap();
    // This build decodes the unknown values to its fallbacks...
    assert_eq!(raw.value().source_status, SourceStatus::Unknown);
    assert_eq!(raw.value().source.kind, SourceKind::OtherAdapter);
    let mut b = batch(&run, vec![], vec![]);
    b.findings.push(raw);
    f.store.commit_analysis_batch(&b).unwrap();

    // ...but the stored record is what the newer version wrote.
    let read = f.store.list_findings(&f.pr, true).unwrap().remove(0);
    assert_eq!(read.finding.json(), written);
    assert_eq!(read.latest_applicability, "current_via_future_mapping");
    // An unknown applicability is never shown as current.
    assert_eq!(read.label, ObservationLabel::Historical);
    let cols: (String, String, String) = f
        .store
        .conn
        .query_row(
            "SELECT source_kind, source_status, assertion_kind FROM review_findings",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        cols,
        (
            "github_future_source".into(),
            "superseded_upstream".into(),
            "future_assertion".into()
        )
    );

    // Writing the read record back (as an older build would) keeps it intact.
    let mut again = batch(&run, vec![], vec![]);
    again.findings.push(read.finding);
    f.store.commit_analysis_batch(&again).unwrap();
    let reread = f.store.list_findings(&f.pr, true).unwrap().remove(0);
    assert_eq!(reread.finding.json(), written);
    assert!(!reread.finding.json().contains("\"unknown\""));
}

fn native(kind: &str, key: &str) -> Raw<NativeSourceRef> {
    Raw::from_json(
        &json!({
            "host": "github.com",
            "repositoryId": REPO,
            "sourceKind": kind,
            "key": {"kind": "upstream", "value": key},
            "externalUrl": null
        })
        .to_string(),
    )
    .unwrap()
}

fn import(resource: &str, complete: bool, records: Vec<ImportedSourceWrite>) -> ImportBatch {
    ImportBatch {
        resource_kind: resource.into(),
        complete,
        observed_at: crate::util::now_rfc3339(),
        records,
    }
}

#[test]
fn imported_records_keep_raw_source_and_unknown_kinds() {
    let mut f = fixture();
    let raw_annotation = r#"{"checkRunId":7,"key":{"kind":"upstream","value":"a1"},"path":"src/a.rs","startLine":1,"endLine":1,"annotationLevel":"catastrophe","message":"m","headSha":"h","observedAt":"now","futureField":{"x":1}}"#;
    let parsed: crate::model::ImportedCheckAnnotation =
        serde_json::from_str(raw_annotation).unwrap();
    assert_eq!(
        parsed.annotation_level,
        crate::model::AnnotationLevel::Unknown
    );
    let ids = f
        .store
        .commit_import_batch(
            &f.pr,
            &import(
                "check_annotations",
                true,
                vec![
                    ImportedSourceWrite {
                        native: native("github_check_annotation", "a1"),
                        raw_json: raw_annotation.into(),
                        aliases: vec![],
                    },
                    // Two different kinds unknown to this build keep distinct identities.
                    ImportedSourceWrite {
                        native: native("github_future_a", "same"),
                        raw_json: "{}".into(),
                        aliases: vec![],
                    },
                    ImportedSourceWrite {
                        native: native("github_future_b", "same"),
                        raw_json: "{}".into(),
                        aliases: vec![],
                    },
                ],
            ),
        )
        .unwrap();
    assert_eq!(ids.len(), 3);
    assert_ne!(ids[1], ids[2]);
    let stored = f
        .store
        .list_imported_sources(&f.pr, Some("check_annotations"))
        .unwrap();
    assert_eq!(stored[0].raw_json, raw_annotation);
    assert!(stored[1].native.json().contains("github_future_a"));
    assert_eq!(
        stored[1].native.value().source_kind,
        SourceKind::OtherAdapter
    );
}

#[test]
fn source_aliases_merge_representations_and_findings_share_records() {
    let mut f = fixture();
    let ids = f
        .store
        .commit_import_batch(
            &f.pr,
            &import(
                "check_runs",
                false,
                vec![ImportedSourceWrite {
                    native: native("github_check_run", "100"),
                    raw_json: r#"{"id":100}"#.into(),
                    aliases: vec![
                        ("rest_id".into(), "100".into()),
                        ("graphql_node_id".into(), "CR_100".into()),
                    ],
                }],
            ),
        )
        .unwrap();
    let rec = ids[0];
    assert_eq!(
        f.store.resolve_imported_alias("rest_id", "100").unwrap(),
        Some(rec)
    );
    assert_eq!(
        f.store
            .resolve_imported_alias("graphql_node_id", "CR_100")
            .unwrap(),
        Some(rec)
    );
    // Re-importing the same record through the other API is the same record.
    let again = f
        .store
        .commit_import_batch(
            &f.pr,
            &import(
                "check_runs",
                false,
                vec![ImportedSourceWrite {
                    native: native("github_check_run", "100"),
                    raw_json: r#"{"node_id":"CR_100"}"#.into(),
                    aliases: vec![("graphql_node_id".into(), "CR_100".into())],
                }],
            ),
        )
        .unwrap();
    assert_eq!(again, vec![rec]);
    // An alias cannot be silently reassigned to another record.
    assert!(
        f.store
            .commit_import_batch(
                &f.pr,
                &import(
                    "check_runs",
                    false,
                    vec![ImportedSourceWrite {
                        native: native("github_check_run", "200"),
                        raw_json: "{}".into(),
                        aliases: vec![("rest_id".into(), "100".into())],
                    }],
                ),
            )
            .is_err()
    );
    assert_eq!(f.store.list_imported_sources(&f.pr, None).unwrap().len(), 1);

    // Two providers report findings on the same native record.
    let s = f.add_snapshot("h", true);
    let mut finding_ids = vec![];
    for provider in ["github.import", "aside.ci-digest"] {
        let run = f.queue(&s, provider, 0);
        let mut fi = finding(&run, "ci failed", &[]);
        fi.source.kind = SourceKind::GithubCheckRun;
        fi.assertion_kind = AssertionKind::ExternalFeedback;
        fi.source.native = Some(native("github_check_run", "100").into_value());
        finding_ids.push(fi.id.clone());
        f.store
            .commit_analysis_batch(&batch(&run, vec![fi], vec![]))
            .unwrap();
    }
    assert_ne!(finding_ids[0], finding_ids[1]);
    assert_eq!(f.store.findings_for_imported_source(rec).unwrap(), {
        let mut v = finding_ids.clone();
        v.sort();
        v
    });
    f.store
        .add_finding_alias(&finding_ids[1], &finding_ids[0], "exact_native_source")
        .unwrap();
    assert_eq!(
        f.store.list_finding_aliases(&finding_ids[0]).unwrap(),
        vec![finding_ids[1].clone()]
    );
    assert!(
        f.store
            .add_finding_alias(&finding_ids[0], &finding_ids[1], "x")
            .is_err(),
        "no alias chains"
    );
    assert_eq!(f.count("review_findings"), 2, "both source findings kept");
}

#[test]
fn shared_native_head_is_available_independently_to_two_prs() {
    let mut f = fixture();
    let repo = f
        .store
        .resolve_repository("github.com", "acme", "widget", Some(REPO))
        .unwrap();
    let other = f.store.upsert_pull_request(&repo.id, 43, "t", "u").unwrap();
    let rec = |key: &str| ImportedSourceWrite {
        native: native("github_check_run", key),
        raw_json: "{}".into(),
        aliases: vec![("rest_id".into(), key.into())],
    };
    let first = f
        .store
        .commit_import_batch(&f.pr, &import("check_runs", true, vec![rec("100")]))
        .unwrap();
    let second = f
        .store
        .commit_import_batch(&other, &import("check_runs", true, vec![rec("100")]))
        .unwrap();
    assert_eq!(first, second, "native identity is shared");
    assert_eq!(
        f.store.resolve_imported_alias("rest_id", "100").unwrap(),
        Some(first[0])
    );
    f.store
        .commit_import_batch(&other, &import("check_runs", true, vec![]))
        .unwrap();
    assert!(f.store.list_imported_sources(&f.pr, None).unwrap()[0].available);
    assert!(!f.store.list_imported_sources(&other, None).unwrap()[0].available);
    f.store
        .commit_import_batch(&other, &import("check_runs", true, vec![rec("100")]))
        .unwrap();
    f.store
        .commit_import_batch(&f.pr, &import("check_runs", true, vec![]))
        .unwrap();
    assert!(!f.store.list_imported_sources(&f.pr, None).unwrap()[0].available);
    assert!(f.store.list_imported_sources(&other, None).unwrap()[0].available);
}

#[test]
fn only_complete_import_refreshes_mark_missing_records_unavailable() {
    let mut f = fixture();
    let rec = |k: &str| ImportedSourceWrite {
        native: native("github_commit_status", k),
        raw_json: "{}".into(),
        aliases: vec![],
    };
    f.store
        .commit_import_batch(
            &f.pr,
            &import("commit_statuses", true, vec![rec("1"), rec("2")]),
        )
        .unwrap();
    f.store
        .commit_import_batch(&f.pr, &import("commit_statuses", false, vec![rec("1")]))
        .unwrap();
    let avail = |f: &Fixture| {
        f.store
            .list_imported_sources(&f.pr, None)
            .unwrap()
            .iter()
            .filter(|r| r.available)
            .count()
    };
    assert_eq!(avail(&f), 2, "partial refresh cannot mark records gone");
    f.store
        .commit_import_batch(&f.pr, &import("commit_statuses", true, vec![rec("1")]))
        .unwrap();
    assert_eq!(avail(&f), 1);
}

#[test]
fn dispositions_survive_restart_and_use_revisions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("triage.db");
    let (pr, id) = {
        let mut f = fixture_at(Store::open(&path).unwrap());
        let s = f.add_snapshot("h", true);
        let run = f.queue(&s, "p", 0);
        let b = simple_batch(&run, &["x"]);
        f.store.commit_analysis_batch(&b).unwrap();
        let id = b.findings[0].value().id.clone();
        let mut st = FindingUserState::untouched(id.clone(), "session", "now");
        st.disposition = UserDisposition::Snoozed;
        assert!(
            f.store.put_finding_user_state(&f.pr, &st, 0).is_err(),
            "snooze needs a deadline"
        );
        st.snoozed_until = Some("2030-01-01T00:00:00Z".into());
        st.note = Some("later".into());
        let stored = f.store.put_finding_user_state(&f.pr, &st, 0).unwrap();
        assert_eq!(stored.revision, 1);
        let stale = f.store.put_finding_user_state(&f.pr, &st, 0).unwrap_err();
        let conflict = stale.downcast_ref::<RevisionConflict>().unwrap();
        assert_eq!(conflict.current.as_ref().unwrap().revision, 1);
        let unknown = FindingUserState::untouched(FindingId::new("find_nope"), "session", "now");
        assert!(f.store.put_finding_user_state(&f.pr, &unknown, 0).is_err());
        (f.pr, id)
    };
    let mut s = Store::open(&path).unwrap();
    let st = s.get_finding_user_state("session", &id).unwrap().unwrap();
    assert_eq!(st.disposition, UserDisposition::Snoozed);
    assert_eq!(st.note.as_deref(), Some("later"));
    let mut next = st.clone();
    next.disposition = UserDisposition::Investigating;
    next.snoozed_until = None;
    assert_eq!(s.put_finding_user_state(&pr, &next, 1).unwrap().revision, 2);
    s.delete_finding_user_state("session", &id).unwrap();
    assert!(s.get_finding_user_state("session", &id).unwrap().is_none());
}

#[test]
fn analysis_never_writes_review_state() {
    let mut f = fixture();
    f.store
        .insert_acknowledgement(
            &f.pr,
            &Acknowledgement {
                id: "ack1".into(),
                path: "src/a.rs".into(),
                kind: "reviewed".into(),
                fingerprint_version: 1,
                diff_digest: "d".into(),
                content_digest: None,
                head_sha: "h".into(),
                base_sha: "b".into(),
                actor: Actor::Human,
                created_at: "now".into(),
            },
        )
        .unwrap();
    let dump = |f: &Fixture| -> Vec<String> {
        [
            "review_acknowledgements",
            "review_sessions",
            "review_threads",
            "review_comments",
            "publication_attempts",
        ]
        .iter()
        .flat_map(|t| {
            let mut stmt = f.store.conn.prepare(&format!("SELECT * FROM {t}")).unwrap();
            let n = stmt.column_count();
            stmt.query_map([], |r| {
                let cols: Vec<String> = (0..n)
                    .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                    .collect();
                Ok(format!("{t}:{}", cols.join("|")))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
        })
        .collect()
    };
    let before = dump(&f);
    assert!(!before.is_empty());

    // Full analysis lifecycle on the same file, including a finding that
    // tries to smuggle review state through its JSON.
    let s = f.add_snapshot("h", true);
    let run = f.queue(&s, "p", 0);
    let mut sneaky = serde_json::to_value(finding(&run, "x", &[])).unwrap();
    sneaky["extensions"] = json!({"reviewed": true});
    let mut b = batch(&run, vec![], vec![]);
    b.findings
        .push(Raw::from_json(&sneaky.to_string()).unwrap());
    assert!(f.store.commit_analysis_batch(&b).is_err());
    let b = simple_batch(&run, &["x"]);
    f.store.commit_analysis_batch(&b).unwrap();
    let mut st = FindingUserState::untouched(b.findings[0].value().id.clone(), "s", "now");
    for d in [
        UserDisposition::AcceptedForReview,
        UserDisposition::DismissedLocal,
        UserDisposition::PromotedToDraft,
    ] {
        st.disposition = d;
        st = f
            .store
            .put_finding_user_state(&f.pr, &st, st.revision)
            .unwrap();
    }
    f.add_snapshot("h2", true);
    f.store.prune_analysis(&f.pr, 0).unwrap();

    assert_eq!(dump(&f), before, "review state changed");
    let acks = f.store.list_acknowledgements(&f.pr).unwrap();
    assert_eq!(acks.len(), 1);
    assert_eq!(acks[0].kind, "reviewed");
}

#[test]
fn raw_records_reject_contract_mismatches() {
    assert!(Raw::<ReviewFinding>::from_json("{\"title\":1}").is_err());
    assert!(Raw::<NativeSourceRef>::from_json("not json").is_err());
    let _ = NativeSourceKey::Upstream("x".into());
}
