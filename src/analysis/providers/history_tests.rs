use super::{HistoryProvider, history::Worker};
use crate::{
    analysis::{
        AnalysisInput, AnalysisProvider, ProviderContext, ProviderLimits, Scheduler,
        SchedulerConfig, SystemClock,
    },
    demo,
    github::context::{AnalysisContext, HistoryObjects},
    model::{AnalysisRunId, RunKeyInput, RunStatus},
    session::Session,
    store::Store,
};
use std::{
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[test]
fn cancelled_worker_is_reaped_without_blocking_caller() {
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let worker = Worker {
        handle: Some(std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            while !worker_cancel.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(5));
            }
            // Simulate cooperative cleanup after the cancellation checkpoint.
            std::thread::sleep(Duration::from_millis(200));
            done_tx.send(()).unwrap();
        })),
        cancel,
    };
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let start = Instant::now();
    drop(worker);
    assert!(start.elapsed() < Duration::from_millis(100));
    done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
}

#[tokio::test]
async fn missing_context_is_partial_with_unknown_file_coverage() {
    let input = AnalysisInput::new("pr", "repo", demo::snapshot(0));
    let run_id = AnalysisRunId::new("run_missing_context");
    let output = HistoryProvider::default()
        .run(ProviderContext {
            run_id,
            attempt: 0,
            limits: ProviderLimits::default(),
            input: std::sync::Arc::new(input),
        })
        .await
        .unwrap();
    assert_eq!(output.status, RunStatus::Partial);
    assert!(output.findings.is_empty());
    assert!(
        output
            .coverage
            .entries
            .iter()
            .all(|e| matches!(e.outcome, crate::model::ScopeOutcome::Unknown { .. }))
    );
}

#[tokio::test]
async fn addition_only_and_empty_baseline_keep_persisted_corpus() {
    for empty in [false, true] {
        let guard = tempfile::tempdir().unwrap();
        let root = guard.path().to_path_buf();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Fixture"]);
        git(&["config", "user.email", "fixture@example.com"]);
        if !empty {
            std::fs::write(root.join("old.txt"), "baseline").unwrap();
            git(&["add", "."]);
        }
        git(&["commit", "--allow-empty", "-qm", "base"]);
        let base = git(&["rev-parse", "HEAD"]);
        std::fs::write(root.join("addition.rs"), "fn addition() {}\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "PR addition"]);
        let head = git(&["rev-parse", "HEAD"]);
        let mut snapshot = demo::snapshot(0);
        snapshot.pr.base_sha = base.clone();
        snapshot.pr.head_sha = head.clone();
        snapshot.files.truncate(1);
        snapshot.files[0].path = "addition.rs".into();
        snapshot.files[0].previous_path = None;
        let session = Session::new(
            snapshot.clone(),
            Store::open_in_memory().unwrap(),
            None,
            None,
        )
        .unwrap();
        let mut input = AnalysisInput::new(&session.pr_id, &session.repository.id, snapshot);
        input.history_context = Some(AnalysisContext {
            source: "checkout".into(),
            base_sha: base.clone(),
            head_sha: head,
            comparison_base: Some(base.clone()),
            history_count: 1,
            shallow: false,
            complete: true,
            reason: None,
        });
        input.history_objects = Some(HistoryObjects::fixture(guard, root.join(".git")));
        let scheduler = Scheduler::new(
            std::sync::Arc::clone(&session.store),
            vec![std::sync::Arc::new(HistoryProvider::default())],
            SchedulerConfig::default(),
            std::sync::Arc::new(SystemClock),
        )
        .unwrap();
        let report = scheduler
            .begin_snapshot(input)
            .unwrap()
            .remove(0)
            .wait()
            .await;
        assert_eq!(report.stored_status, RunStatus::Partial);
        let store = session.store.lock().unwrap();
        let findings = store.list_findings(&session.pr_id, false).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].finding.value().title,
            "Pinned baseline history corpus"
        );
        let evidence = store
            .list_finding_evidence(&findings[0].finding.value().id)
            .unwrap();
        assert_eq!(evidence.len(), 1);
        assert_eq!(
            findings[0].finding.value().evidence_ids,
            vec![evidence[0].value().id.clone()]
        );
        let value = evidence[0].value().value.as_ref().unwrap();
        assert_eq!(value["baseSha"], base);
        assert_eq!(value["walked"], 1);
        assert_eq!(value["counted"], 1);
        assert_eq!(value["mergeSkipped"], 0);
        assert_eq!(value["baselinePopulation"], if empty { 0 } else { 1 });
        assert_eq!(value["rustPopulation"], 0);
        assert!(value["anchorUnix"].as_i64().is_some());
        assert!(value["windowStartUnix"].as_i64().is_some());
    }
}

#[tokio::test]
async fn persisted_baseline_evidence_excludes_pr_commits() {
    let guard = tempfile::tempdir().unwrap();
    let root = guard.path().to_path_buf();
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "-q"]);
    git(&["config", "user.name", "Fixture"]);
    git(&["config", "user.email", "fixture@example.com"]);
    let target = "internal/identity/store.go";
    std::fs::create_dir_all(root.join("internal/identity")).unwrap();
    std::fs::write(root.join(target), "0").unwrap();
    for n in 0..19 {
        std::fs::write(root.join(format!("zero-{n:02}.txt")), "0").unwrap();
    }
    git(&["add", "."]);
    git(&["commit", "-qm", "root"]);
    for n in 1..6 {
        std::fs::write(root.join(target), n.to_string()).unwrap();
        git(&["commit", "-qam", "historical touch"]);
    }
    let base = git(&["rev-parse", "HEAD"]);
    std::fs::write(root.join(target), "PR-only").unwrap();
    git(&["commit", "-qam", "PR-only touch"]);
    let head = git(&["rev-parse", "HEAD"]);
    let mut snapshot = demo::snapshot(0);
    snapshot.pr.base_sha = base.clone();
    snapshot.pr.head_sha = head.clone();
    snapshot.files.retain(|f| f.path == target);
    let session = Session::new(
        snapshot.clone(),
        Store::open_in_memory().unwrap(),
        None,
        None,
    )
    .unwrap();
    let mut input = AnalysisInput::new(&session.pr_id, &session.repository.id, snapshot);
    input.history_context = Some(AnalysisContext {
        source: "checkout".into(),
        base_sha: base.clone(),
        head_sha: head,
        comparison_base: Some(base),
        history_count: 6,
        shallow: false,
        complete: true,
        reason: None,
    });
    input.history_objects = Some(HistoryObjects::fixture(guard, root.join(".git")));
    let descriptor = HistoryProvider::default();
    let digest = input.provider_digest(descriptor.descriptor().kind);
    let run_id = AnalysisRunId::derive(&RunKeyInput {
        snapshot_id: &input.snapshot_id,
        provider_id: &descriptor.descriptor().id,
        engine_version: &descriptor.descriptor().engine_version,
        configuration_digest: &descriptor.descriptor().configuration_digest,
        input_digest: &digest,
        attempt: 0,
    });
    let persisted_input = input.clone();
    let ctx = ProviderContext {
        run_id,
        attempt: 0,
        limits: ProviderLimits {
            timeout: Duration::from_secs(100),
            ..ProviderLimits::default()
        },
        input: std::sync::Arc::new(input),
    };
    let output = descriptor.run(ctx).await.unwrap();
    assert_eq!(output.status, RunStatus::Partial); // Go complexity is unknown; churn remains valid.
    assert_eq!(output.findings.len(), 2);
    assert_eq!(output.evidence.len(), 2);
    let corpus = output
        .evidence
        .iter()
        .find(|e| e.value().label == "Pinned baseline corpus")
        .unwrap();
    assert_eq!(corpus.value().value.as_ref().unwrap()["walked"], 6);
    let value = output
        .evidence
        .iter()
        .find(|e| e.value().label == "Pinned baseline history")
        .unwrap()
        .value()
        .value
        .as_ref()
        .unwrap();
    assert_eq!(value["touchingCommits"], 6);
    assert_eq!(value["churnRank"]["below"], 19);
    assert_eq!(value["churnRank"]["population"], 20);
    assert_eq!(value["baseline"]["walked"], 6);
    assert_eq!(value["baseline"]["rustPopulation"], 0);
    assert_eq!(output.findings[0].value().validate(), Ok(()));

    let scheduler = Scheduler::new(
        std::sync::Arc::clone(&session.store),
        vec![std::sync::Arc::new(HistoryProvider::default())],
        SchedulerConfig::default(),
        std::sync::Arc::new(SystemClock),
    )
    .unwrap();
    let handles = scheduler.begin_snapshot(persisted_input.clone()).unwrap();
    let report = handles.into_iter().next().unwrap().wait().await;
    assert_eq!(report.stored_status, RunStatus::Partial, "{report:?}");
    let first_finding_id = {
        let store = session.store.lock().unwrap();
        let findings = store.list_findings(&session.pr_id, true).unwrap();
        assert_eq!(findings.len(), 2);
        let corpus = findings
            .iter()
            .find(|f| f.finding.value().title == "Pinned baseline history corpus")
            .unwrap();
        assert_eq!(
            store
                .list_finding_evidence(&corpus.finding.value().id)
                .unwrap()[0]
                .value()
                .value
                .as_ref()
                .unwrap()["walked"],
            6
        );
        let file = findings
            .iter()
            .find(|f| f.finding.value().title != "Pinned baseline history corpus")
            .unwrap();
        let evidence = store
            .list_finding_evidence(&file.finding.value().id)
            .unwrap();
        assert_eq!(evidence.len(), 1);
        assert_eq!(
            evidence[0].value().value.as_ref().unwrap()["touchingCommits"],
            6
        );
        file.finding.value().id.clone()
    };

    // A new PR head has a new snapshot/run but the same pinned baseline.
    // Pruning the old snapshot must not delete the evidence still referenced
    // by the current observation.
    std::fs::write(root.join(target), "next PR revision").unwrap();
    git(&["commit", "-qam", "second PR-only touch"]);
    let next_head = git(&["rev-parse", "HEAD"]);
    let mut next = demo::snapshot(1);
    next.pr.base_sha = persisted_input.base_sha.clone();
    next.pr.head_sha = next_head.clone();
    next.files.retain(|f| f.path == target);
    next.files[0].previous_path = Some(target.into());
    next.files[0].path = "internal/identity/renamed-store.go".into();
    let mut next_input = AnalysisInput::new(&session.pr_id, &session.repository.id, next);
    let mut next_context = persisted_input.history_context.clone().unwrap();
    next_context.head_sha = next_head;
    next_input.history_context = Some(next_context);
    next_input.history_objects = persisted_input.history_objects.clone();
    assert_ne!(next_input.snapshot_id, persisted_input.snapshot_id);
    let report = scheduler
        .begin_snapshot(next_input)
        .unwrap()
        .remove(0)
        .wait()
        .await;
    assert_eq!(report.stored_status, RunStatus::Partial, "{report:?}");
    {
        let mut store = session.store.lock().unwrap();
        let current = store.list_findings(&session.pr_id, false).unwrap();
        assert_eq!(current.len(), 2);
        let corpus = current
            .iter()
            .find(|f| f.finding.value().title == "Pinned baseline history corpus")
            .unwrap();
        assert_eq!(
            store
                .list_finding_evidence(&corpus.finding.value().id)
                .unwrap()[0]
                .value()
                .value
                .as_ref()
                .unwrap()["walked"],
            6
        );
        let current = current
            .iter()
            .find(|f| f.finding.value().title != "Pinned baseline history corpus")
            .unwrap();
        let current_evidence = store
            .list_finding_evidence(&current.finding.value().id)
            .unwrap();
        assert_eq!(current_evidence.len(), 1);
        assert_ne!(current.finding.value().id, first_finding_id);
        assert_eq!(
            current.finding.value().anchor.current_path.as_deref(),
            Some("internal/identity/renamed-store.go")
        );
        assert_eq!(
            current.finding.value().anchor.previous_path.as_deref(),
            Some(target)
        );
        assert_eq!(
            current.finding.value().evidence_ids,
            vec![current_evidence[0].value().id.clone()]
        );
        assert_eq!(
            current_evidence[0].value().value.as_ref().unwrap()["touchingCommits"],
            6
        );
        let report = store.prune_analysis(&session.pr_id, 0).unwrap();
        assert!(report.snapshots > 0, "{report:?}");
        let retained = store.list_findings(&session.pr_id, false).unwrap();
        assert_eq!(retained.len(), 2);
        let corpus = retained
            .iter()
            .find(|f| f.finding.value().title == "Pinned baseline history corpus")
            .unwrap();
        assert_eq!(
            store
                .list_finding_evidence(&corpus.finding.value().id)
                .unwrap()[0]
                .value()
                .value
                .as_ref()
                .unwrap()["walked"],
            6
        );
        let retained = retained
            .iter()
            .find(|f| f.finding.value().title != "Pinned baseline history corpus")
            .unwrap();
        let evidence = store
            .list_finding_evidence(&retained.finding.value().id)
            .unwrap();
        assert_eq!(evidence.len(), 1);
        assert_eq!(
            retained.finding.value().evidence_ids,
            vec![evidence[0].value().id.clone()]
        );
        assert_eq!(
            evidence[0].value().value.as_ref().unwrap()["touchingCommits"],
            6
        );
    }

    // A shallow boundary produces a persisted, structured incomplete report.
    std::fs::write(
        root.join(".git/shallow"),
        format!("{}\n", persisted_input.base_sha),
    )
    .unwrap();
    std::fs::write(root.join(target), "third PR revision").unwrap();
    git(&["commit", "-qam", "third PR-only touch"]);
    let third_head = git(&["rev-parse", "HEAD"]);
    let mut third = demo::snapshot(2);
    third.pr.base_sha = persisted_input.base_sha.clone();
    third.pr.head_sha = third_head.clone();
    third.files.retain(|f| f.path == target);
    let mut third_input = AnalysisInput::new(&session.pr_id, &session.repository.id, third);
    let mut third_context = persisted_input.history_context.clone().unwrap();
    third_context.head_sha = third_head;
    third_input.history_context = Some(third_context);
    third_input.history_objects = persisted_input.history_objects.clone();
    let report = scheduler
        .begin_snapshot(third_input)
        .unwrap()
        .remove(0)
        .wait()
        .await;
    assert_eq!(report.stored_status, RunStatus::Partial);
    let store = session.store.lock().unwrap();
    let run = store.get_analysis_run(&report.run_id).unwrap();
    let diagnostic = run
        .value()
        .warnings
        .iter()
        .find(|w| w.code == "shallow_boundary")
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&diagnostic.message).unwrap();
    assert_eq!(value["reason"], "shallow_boundary");
    assert_eq!(value["baseSha"], persisted_input.base_sha);
    assert_eq!(value["commitCap"], 1000);
    assert!(
        store
            .list_findings(&session.pr_id, false)
            .unwrap()
            .is_empty()
    );
}
