//! End-to-end cancellation of the real structural provider's worker through
//! the scheduler. The checkpoint holds the worker after it reads pinned objects
//! and before it enters the structural engine; no timer decides when to cancel.
use super::*;
use crate::{
    analysis::providers::DiffFactsProvider,
    analysis::{AnalysisInput, Scheduler, SchedulerConfig, SystemClock},
    demo,
    github::context::{AnalysisContext, HistoryObjects},
    model::RunStatus,
    session::Session,
    store::Store,
};
use std::{process::Command, sync::Arc, time::Duration};

fn git(root: &std::path::Path, args: &[&str]) -> String {
    let result = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    String::from_utf8(result.stdout).unwrap().trim().to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_active_structure_worker_does_not_block_or_publish_late_output() {
    let guard = tempfile::tempdir().unwrap();
    let root = guard.path().to_owned();
    let object_dir = root.join(".git");
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.name", "Fixture"]);
    git(&root, &["config", "user.email", "fixture@example.com"]);
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn changed() {}\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-qm", "pinned fixture"]);
    let head = git(&root, &["rev-parse", "HEAD"]);

    let mut snapshot = demo::snapshot(0);
    snapshot.pr.base_sha = head.clone();
    snapshot.pr.head_sha = head.clone();
    let database = tempfile::tempdir().unwrap();
    let db_path = database.path().join("review.sqlite");
    let session =
        Session::new(snapshot.clone(), Store::open(&db_path).unwrap(), None, None).unwrap();
    let mut input = AnalysisInput::new(&session.pr_id, &session.repository.id, snapshot);
    input.history_context = Some(AnalysisContext {
        source: "fixture".into(),
        base_sha: head.clone(),
        head_sha: head.clone(),
        comparison_base: Some(head),
        history_count: 1,
        shallow: false,
        complete: true,
        reason: None,
    });
    input.history_objects = Some(HistoryObjects::fixture(guard, object_dir));

    let reached = Arc::new(tokio::sync::Notify::new());
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = std::sync::Mutex::new(release_rx);
    let checkpoint = {
        let reached = Arc::clone(&reached);
        Arc::new(move || {
            reached.notify_one();
            release_rx.lock().unwrap().recv().unwrap();
        })
    };
    let provider = StructureProvider::with_test_checkpoint(checkpoint);
    let scheduler = Scheduler::new(
        Arc::clone(&session.store),
        vec![Arc::new(provider), Arc::new(DiffFactsProvider::default())],
        SchedulerConfig {
            max_concurrency: 2,
            ..SchedulerConfig::default()
        },
        Arc::new(SystemClock),
    )
    .unwrap();
    let snapshot_id = input.snapshot_id.clone();
    let handles = scheduler.begin_snapshot(input).unwrap();
    let structure = handles
        .iter()
        .find(|h| h.provider_id == ID)
        .unwrap()
        .clone();
    let diff = handles
        .iter()
        .find(|h| h.provider_id == crate::diff::facts::PROVIDER_ID)
        .unwrap()
        .clone();
    tokio::time::timeout(Duration::from_secs(10), reached.notified())
        .await
        .unwrap();
    // This must complete while the worker is *still* held at the checkpoint.
    let cancellation =
        tokio::time::timeout(Duration::from_secs(10), scheduler.cancel(&snapshot_id, ID)).await;
    release_tx.send(()).unwrap();
    let report = cancellation
        .expect("cancel blocked on the worker")
        .unwrap()
        .unwrap();
    assert_eq!(report.stored_status, RunStatus::Cancelled);
    assert_eq!(structure.wait().await.stored_status, RunStatus::Cancelled);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), diff.wait())
            .await
            .unwrap()
            .stored_status,
        RunStatus::Partial // Demo diff fixture has incomplete source rows, but sibling finishes independently.
    );
    drop(scheduler);

    // The retained object snapshot is held by the worker's ProviderContext.
    // Once its final reference exits, TempDir removes the pinned object tree.
    tokio::time::timeout(Duration::from_secs(10), async {
        while root.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("structure worker retained its pinned objects after cancellation");
    let store = session.store.lock().unwrap();
    let runs = store.list_analysis_runs(&session.pr_id).unwrap();
    assert_eq!(
        runs.iter()
            .find(|r| r.value().id == report.run_id)
            .unwrap()
            .value()
            .status,
        RunStatus::Cancelled
    );
    let structure_findings: Vec<_> = store
        .list_findings(&session.pr_id, true)
        .unwrap()
        .into_iter()
        .filter(|f| f.finding.value().source.provider_id == ID)
        .collect();
    assert!(
        structure_findings.is_empty(),
        "cancelled worker published late findings"
    );
    let db = rusqlite::Connection::open(db_path).unwrap();
    for (table, column) in [
        ("finding_evidence", "owner_run_id"),
        ("finding_relations", "run_id"),
        ("finding_occurrences", "run_id"),
    ] {
        let count: i64 = db
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE {column} = ?1"),
                [report.run_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "cancelled run persisted {table}");
    }
}
