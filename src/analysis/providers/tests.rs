//! The registered diff engine, end to end: real `scan`, real scheduler,
//! real (in-memory) store, on the #17 demo fixture snapshot.

use std::{collections::BTreeSet, sync::Arc};

use super::DiffFactsProvider;
use crate::{
    analysis::{
        AnalysisInput, AnalysisProvider, ProviderKind, RunHandle, Scheduler, SchedulerConfig,
        SystemClock, default_providers,
        test_doubles::{Double, HISTORY},
    },
    demo,
    diff::facts::{self, DiffFactsConfig, DiffFactsInput},
    model::{Actor, Completeness, RunStatus},
    session::Session,
    store::{ObservationLabel, Store},
};

fn session() -> Arc<Session> {
    Session::new(
        demo::snapshot(0),
        Store::open_in_memory().unwrap(),
        None,
        None,
    )
    .unwrap()
}

fn diff_handle(handles: &[RunHandle]) -> RunHandle {
    handles
        .iter()
        .find(|h| h.provider_id == facts::PROVIDER_ID)
        .expect("diff provider was scheduled")
        .clone()
}

/// What `scan` produces for `input`, computed directly for comparison.
fn expected(input: &AnalysisInput) -> facts::DiffFacts {
    facts::scan(
        &DiffFactsInput {
            snapshot: &input.snapshot,
            snapshot_id: &input.snapshot_id,
            canonical_repo_id: &input.canonical_repo_id,
            run_id: None,
            observed_at: "2026-01-01T00:00:00Z",
        },
        &DiffFactsConfig::default(),
    )
}

fn expected_ids(input: &AnalysisInput) -> BTreeSet<String> {
    expected(input)
        .findings
        .iter()
        .map(|f| f.id.to_string())
        .collect()
}

/// The run status the contract requires for a scan's coverage.
fn status_for(facts: &facts::DiffFacts) -> RunStatus {
    if facts.coverage.completeness == Completeness::Complete {
        RunStatus::Succeeded
    } else {
        RunStatus::Partial
    }
}

fn stored(session: &Session) -> Vec<(String, String, ObservationLabel)> {
    session
        .store
        .lock()
        .unwrap()
        .list_findings(&session.pr_id, true)
        .unwrap()
        .into_iter()
        .map(|f| {
            let v = f.finding.value();
            (v.id.to_string(), v.source.provider_id.clone(), f.label)
        })
        .collect()
}

fn session_input(session: &Session) -> AnalysisInput {
    let snapshot = session.snapshot.read().unwrap().clone();
    let canonical = snapshot
        .provider_repository_id
        .clone()
        .unwrap_or_else(|| session.repository.id.clone());
    AnalysisInput::new(&session.pr_id, &canonical, snapshot)
}

#[test]
fn default_providers_register_the_diff_engine() {
    let providers = default_providers();
    assert!(!providers.is_empty(), "no analysis engines registered");
    let diff = providers
        .iter()
        .find(|p| p.descriptor().kind == ProviderKind::Diff)
        .expect("diff engine registered");
    let d = diff.descriptor();
    assert_eq!(d.id, facts::PROVIDER_ID);
    assert_eq!(d.engine_version, facts::ENGINE_VERSION);
    assert_eq!(d.configuration_digest, DiffFactsConfig::default().digest());
}

#[tokio::test]
async fn demo_findings_flow_from_scan_to_the_store() {
    let session = session();
    let direct = expected(&session_input(&session));
    let expected = expected_ids(&session_input(&session));
    assert!(!expected.is_empty(), "fixture should yield diff facts");

    let report = diff_handle(&session.start_analysis().unwrap()).wait().await;
    assert_eq!(report.write_error, None);
    assert_eq!(report.reason, None);
    assert_eq!(report.stored_status, status_for(&direct));

    let rows = stored(&session);
    let got: BTreeSet<String> = rows.iter().map(|(id, _, _)| id.clone()).collect();
    assert_eq!(got, expected);
    assert!(
        rows.iter()
            .all(|(_, p, l)| p == facts::PROVIDER_ID && *l == ObservationLabel::Current)
    );

    // Evidence and run metadata are persisted alongside.
    let store = session.store.lock().unwrap();
    let finding = store
        .list_findings(&session.pr_id, false)
        .unwrap()
        .into_iter()
        .find(|f| !f.finding.value().evidence_ids.is_empty())
        .expect("a finding with evidence");
    let ev = store
        .list_finding_evidence(&finding.finding.value().id)
        .unwrap();
    assert_eq!(ev.len(), finding.finding.value().evidence_ids.len());
    let run = store.get_analysis_run(&report.run_id).unwrap().into_value();
    assert_eq!(run.coverage, direct.coverage);
    assert_eq!(run.warnings, direct.warnings);
}

#[tokio::test]
async fn refresh_reruns_the_diff_engine_and_labels_old_findings_historical() {
    let session = session();
    let first = expected_ids(&session_input(&session));
    diff_handle(&session.start_analysis().unwrap()).wait().await;

    session.refresh().await.unwrap();
    let second = expected_ids(&session_input(&session));
    assert_ne!(first, second, "the refresh fixture changes a line");
    // refresh() starts analysis itself; wait for that run.
    let in_flight = session.analysis.in_flight();
    if let Some(h) = in_flight
        .iter()
        .find(|h| h.provider_id == facts::PROVIDER_ID)
    {
        h.clone().wait().await;
    }

    assert!(
        !second.is_empty(),
        "refresh fixture should yield diff facts"
    );
    assert!(
        first.difference(&second).next().is_some(),
        "refresh fixture should retire an old finding"
    );
    let rows = stored(&session);
    let got: BTreeSet<String> = rows.iter().map(|(id, _, _)| id.clone()).collect();
    assert_eq!(got, first.union(&second).cloned().collect());

    for (id, _, label) in rows {
        let want = if second.contains(&id) {
            ObservationLabel::Current
        } else {
            assert!(first.contains(&id));
            ObservationLabel::Historical
        };
        assert_eq!(label, want, "{id}");
    }
}

#[tokio::test]
async fn incomplete_snapshot_is_reported_partial_with_scan_warnings() {
    let session = session();
    let mut snapshot = demo::snapshot(0);
    snapshot.complete = false;
    let input = AnalysisInput::new(&session.pr_id, "212613049", snapshot);
    let analysis = Scheduler::new(
        Arc::clone(&session.store),
        vec![Arc::new(DiffFactsProvider::default())],
        SchedulerConfig::default(),
        Arc::new(SystemClock),
    )
    .unwrap();
    let report = diff_handle(&analysis.begin_snapshot(input).unwrap())
        .wait()
        .await;
    assert_eq!(report.reported_status, RunStatus::Partial);
    assert_eq!(report.stored_status, RunStatus::Partial);
    let run = session
        .store
        .lock()
        .unwrap()
        .get_analysis_run(&report.run_id)
        .unwrap()
        .into_value();
    assert_eq!(run.coverage.completeness, Completeness::Partial);
    assert!(run.warnings.iter().any(|w| w.code == "snapshot_incomplete"));
}

/// The non-blocking guarantee with the real engine registered: review data
/// is served while another provider is held in flight, and the diff engine's
/// findings are already current.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_stays_usable_with_the_real_diff_engine_registered() {
    let session = session();
    let before = session.view(Actor::Human).unwrap();
    let (slow, gate, started) = Double::gated(HISTORY, ProviderKind::History);
    let mut providers: Vec<Arc<dyn AnalysisProvider>> = default_providers();
    providers.push(slow);
    let analysis = Scheduler::new(
        Arc::clone(&session.store),
        providers,
        SchedulerConfig::default(),
        Arc::new(SystemClock),
    )
    .unwrap();

    let handles = analysis.begin_snapshot(session_input(&session)).unwrap();
    started.notified().await;
    let report = diff_handle(&handles).wait().await;
    assert_eq!(
        report.stored_status,
        status_for(&expected(&session_input(&session)))
    );

    let slow_handle = handles.iter().find(|h| h.provider_id == HISTORY).unwrap();
    assert!(
        slow_handle.report().is_none(),
        "slow provider still running"
    );
    let view = session.view(Actor::Human).unwrap();
    assert_eq!(view.files, before.files);
    assert_eq!(view.progress, before.progress);
    assert!(
        stored(&session)
            .iter()
            .any(|(_, p, l)| p == facts::PROVIDER_ID && *l == ObservationLabel::Current)
    );

    gate.add_permits(1);
    slow_handle.clone().wait().await;
}
