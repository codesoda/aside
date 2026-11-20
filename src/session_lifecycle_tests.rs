//! Run lifecycle, per-source completeness, invalidation and resync (#27).
//!
//! Providers are scripted doubles replaying the #17 offline fixture; slow
//! ones are held with semaphores, so nothing sleeps on a real timer.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use anyhow::Result;
use futures_util::{FutureExt, future::BoxFuture};
use serde_json::Value;
use tokio::sync::{Semaphore, broadcast};

use super::{Session, source};
use crate::{
    analysis::{
        AnalysisProvider, ProviderContext, ProviderDescriptor, ProviderKind, ProviderOutput,
        RunHandle, SchedulerConfig,
        test_doubles::{DIFF, fixture_output},
    },
    demo,
    diff::facts,
    model::{
        Actor, AnalysisCoverage, AnalysisMode, Completeness, EventEnvelope, FindingUserState,
        KnownCount, OverallCoverage, PrSnapshot, RuleAction, RuleKind, RunStatus, SessionView,
        SnapshotId, SourceCoverage, SourceState, UserDisposition,
    },
    store::{ObservationLabel, Store},
};

type Script = Arc<dyn Fn(&ProviderContext) -> Result<ProviderOutput> + Send + Sync>;

/// A provider whose output is a closure, optionally held behind a gate.
struct Scripted {
    descriptor: ProviderDescriptor,
    script: Script,
    gate: Option<Arc<Semaphore>>,
    calls: Arc<AtomicUsize>,
}

impl Scripted {
    fn new(script: Script, gate: Option<Arc<Semaphore>>) -> (Arc<Self>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(Self {
            descriptor: ProviderDescriptor {
                id: DIFF.into(),
                kind: ProviderKind::Diff,
                engine_version: "test-1".into(),
                configuration_digest: "cfg".into(),
                mode: AnalysisMode::Static,
                limits: None,
            },
            script,
            gate,
            calls: Arc::clone(&calls),
        });
        (provider, calls)
    }
}

impl AnalysisProvider for Scripted {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn run(&self, ctx: ProviderContext) -> BoxFuture<'static, Result<ProviderOutput>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let script = Arc::clone(&self.script);
        let gate = self.gate.clone();
        async move {
            if let Some(gate) = gate {
                gate.acquire().await?.forget();
            }
            script(&ctx)
        }
        .boxed()
    }
}

fn fixture() -> Script {
    Arc::new(|ctx| fixture_output(DIFF, ctx))
}

/// Fixture findings on demo snapshot 0 only; a clean, complete run elsewhere.
fn fixture_on_first_snapshot() -> Script {
    Arc::new(|ctx| {
        let mut out = fixture_output(DIFF, ctx)?;
        if ctx.input.head_sha != demo::snapshot(0).pr.head_sha {
            out.findings.clear();
            out.evidence.clear();
        }
        Ok(out)
    })
}

/// A run that reports `partial` and found nothing.
fn partial_with_nothing() -> Script {
    Arc::new(|_| {
        Ok(ProviderOutput {
            status: RunStatus::Partial,
            coverage: AnalysisCoverage {
                completeness: Completeness::Partial,
                reason_codes: vec!["language_unsupported".into()],
                ..AnalysisCoverage::not_run()
            },
            warnings: Vec::new(),
            findings: Vec::new(),
            evidence: Vec::new(),
            relations: Vec::new(),
        })
    })
}

fn session_with(snapshot: PrSnapshot, providers: Vec<Arc<dyn AnalysisProvider>>) -> Arc<Session> {
    Session::with_analysis(
        snapshot,
        Store::open_in_memory().unwrap(),
        None,
        None,
        providers,
        SchedulerConfig::default(),
    )
    .unwrap()
}

fn source_of<'a>(view: &'a SessionView, id: &str) -> &'a SourceCoverage {
    view.analysis
        .sources
        .iter()
        .find(|s| s.source_id == id)
        .unwrap_or_else(|| panic!("no source {id}"))
}

async fn wait_all(handles: Vec<RunHandle>) {
    for handle in handles {
        handle.wait().await;
    }
}

fn drain(rx: &mut broadcast::Receiver<EventEnvelope>) -> Vec<EventEnvelope> {
    let mut out = Vec::new();
    while let Ok(event) = rx.try_recv() {
        out.push(event);
    }
    out
}

fn types(events: &[EventEnvelope]) -> Vec<&str> {
    events.iter().map(|e| e.event_type.as_str()).collect()
}

fn complete_snapshot_without_feedback() -> PrSnapshot {
    let mut snapshot = demo::snapshot(0);
    snapshot.github_threads.clear();
    snapshot
}

// ---------------------------------------------------------------------------
// No partial import is displayed as clean
// ---------------------------------------------------------------------------

#[test]
fn complete_thread_import_with_no_threads_is_clean() {
    let session = session_with(complete_snapshot_without_feedback(), vec![]);
    let view = session.view(Actor::Human).unwrap();
    let threads = source_of(&view, source::REVIEW_THREADS);
    assert_eq!(threads.items, KnownCount::Known { value: 0 });
    assert_eq!(threads.state, SourceState::Clean);
}

#[test]
fn partial_thread_import_with_no_threads_is_never_clean() {
    let mut snapshot = complete_snapshot_without_feedback();
    snapshot.github_threads_completeness = Completeness::Partial;
    snapshot.github_threads_problems = vec!["review threads exceed 50 pages; stopped early".into()];
    let session = session_with(snapshot, vec![]);
    let view = session.view(Actor::Human).unwrap();
    let threads = source_of(&view, source::REVIEW_THREADS);
    assert_eq!(threads.items, KnownCount::Known { value: 0 });
    assert_eq!(threads.state, SourceState::Incomplete);
    assert_eq!(threads.completeness, Completeness::Partial);
    assert!(
        threads
            .reason_codes
            .iter()
            .any(|c| c == "github_source_partial")
    );
    assert!(
        threads
            .reason_codes
            .iter()
            .any(|c| c == "page_limit_reached")
    );
    assert_eq!(threads.limitations.len(), 1);
    assert_eq!(view.analysis.overall, OverallCoverage::Incomplete);
}

#[test]
fn failed_or_denied_thread_import_is_failed_with_unknown_count() {
    let mut snapshot = complete_snapshot_without_feedback();
    snapshot.github_threads_completeness = Completeness::Failed;
    snapshot.github_threads_problems =
        vec!["review threads: FORBIDDEN: Resource not accessible by integration".into()];
    let session = session_with(snapshot, vec![]);
    let view = session.view(Actor::Human).unwrap();
    let threads = source_of(&view, source::REVIEW_THREADS);
    assert_eq!(threads.state, SourceState::Failed);
    // A failed import never reports "0 threads".
    assert_eq!(threads.items, KnownCount::Unknown);
    assert!(
        threads
            .reason_codes
            .iter()
            .any(|c| c == "github_permission_denied")
    );
    assert_eq!(view.analysis.overall, OverallCoverage::Incomplete);
}

#[test]
fn snapshot_stored_before_completeness_existed_is_not_clean() {
    let mut snapshot = complete_snapshot_without_feedback();
    snapshot.github_threads_completeness = Completeness::Unknown;
    let view = session_with(snapshot, vec![]).view(Actor::Human).unwrap();
    let threads = source_of(&view, source::REVIEW_THREADS);
    assert_eq!(threads.state, SourceState::Incomplete);
    assert_eq!(threads.completeness, Completeness::Unknown);
}

#[test]
fn incomplete_diff_ingestion_is_incomplete() {
    let mut snapshot = demo::snapshot(0);
    snapshot.complete = false;
    snapshot.files[0].patch_complete = false;
    let view = session_with(snapshot.clone(), vec![])
        .view(Actor::Human)
        .unwrap();
    let diff = source_of(&view, source::PR_DIFF);
    assert_eq!(diff.state, SourceState::Incomplete);
    assert!(diff.reason_codes.iter().any(|c| c == "pr_diff_incomplete"));
    assert!(diff.limitations[0].contains(&snapshot.files[0].path));
}

#[test]
fn providers_that_never_ran_are_not_run_not_clean() {
    let (provider, _) = Scripted::new(fixture(), None);
    let view = session_with(demo::snapshot(0), vec![provider])
        .view(Actor::Human)
        .unwrap();
    let run = source_of(&view, DIFF);
    assert_eq!(run.state, SourceState::NotRun);
    assert_eq!(run.items, KnownCount::Unknown);
    assert_eq!(view.analysis.overall, OverallCoverage::Incomplete);
}

#[tokio::test]
async fn partial_provider_run_with_zero_findings_is_not_clean() {
    let (provider, _) = Scripted::new(partial_with_nothing(), None);
    let session = session_with(complete_snapshot_without_feedback(), vec![provider]);
    wait_all(session.start_analysis().unwrap()).await;
    let view = session.view(Actor::Human).unwrap();
    let run = source_of(&view, DIFF);
    assert_eq!(run.status, Some(RunStatus::Partial));
    assert_eq!(run.items, KnownCount::Known { value: 0 });
    assert_eq!(run.state, SourceState::Incomplete);
    assert!(run.reason_codes.iter().any(|c| c == "language_unsupported"));
    assert_eq!(view.analysis.current_findings, 0);
    assert_ne!(view.analysis.overall, OverallCoverage::Complete);
}

#[tokio::test]
async fn real_diff_provider_partial_status_is_incomplete() {
    let mut snapshot = demo::snapshot(0);
    snapshot.complete = false;
    let session = Session::new(snapshot, Store::open_in_memory().unwrap(), None, None).unwrap();
    wait_all(session.start_analysis().unwrap()).await;
    let view = session.view(Actor::Human).unwrap();
    let diff = source_of(&view, facts::PROVIDER_ID);
    assert_eq!(diff.status, Some(RunStatus::Partial));
    assert_eq!(diff.state, SourceState::Incomplete);
    assert_eq!(view.analysis.overall, OverallCoverage::Incomplete);
}

#[tokio::test]
async fn complete_sources_are_persisted_per_snapshot_with_their_limitations() {
    let mut snapshot = demo::snapshot(0);
    snapshot.github_threads_completeness = Completeness::Partial;
    snapshot.github_threads_problems = vec!["thread T: cursor did not advance".into()];
    let session = session_with(snapshot, vec![]);
    session.start_analysis().unwrap();
    // Recording the same import again is idempotent.
    session.start_analysis().unwrap();
    let runs = session
        .store
        .lock()
        .unwrap()
        .list_analysis_runs(&session.pr_id)
        .unwrap();
    let threads: Vec<_> = runs
        .iter()
        .map(|r| r.value())
        .filter(|r| r.provider_id == source::REVIEW_THREADS)
        .collect();
    assert_eq!(threads.len(), 1);
    let run = threads[0];
    assert_eq!(run.mode, AnalysisMode::GithubImport);
    assert_eq!(run.status, RunStatus::Partial);
    assert_eq!(run.coverage.completeness, Completeness::Partial);
    assert!(
        run.coverage
            .reason_codes
            .iter()
            .any(|c| c == "github_source_partial")
    );
    assert_eq!(run.warnings[0].message, "thread T: cursor did not advance");
    assert!(
        runs.iter()
            .any(|r| r.value().provider_id == source::PR_DIFF)
    );
}

// ---------------------------------------------------------------------------
// Invalidation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn head_change_invalidates_and_reruns() {
    let (provider, calls) = Scripted::new(fixture_on_first_snapshot(), None);
    let session = session_with(demo::snapshot(0), vec![provider]);
    wait_all(session.start_analysis().unwrap()).await;
    let before = session.view(Actor::Human).unwrap();
    assert!(before.analysis.current_findings > 0);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let mut rx = session.events.subscribe();
    let after = session.refresh().await.unwrap();
    assert_ne!(after.pr.head_sha, before.pr.head_sha);
    assert_ne!(after.analysis.snapshot_id, before.analysis.snapshot_id);
    let refreshed = drain(&mut rx)
        .into_iter()
        .find(|e| e.event_type == "pr.refreshed")
        .unwrap();
    let invalidated = &refreshed.payload["invalidated"];
    assert_eq!(invalidated["reasons"], serde_json::json!(["head_changed"]));
    assert_eq!(
        invalidated["previousSnapshotId"],
        before.analysis.snapshot_id.as_str()
    );
    assert_eq!(invalidated["eventVersion"], 1);

    // The engine reruns on the new snapshot.
    let handles: Vec<RunHandle> = session.analysis.in_flight();
    wait_all(handles).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let view = session.view(Actor::Human).unwrap();
    assert_eq!(view.analysis.current_findings, 0);
    assert_eq!(
        view.analysis.historical_findings,
        before.analysis.current_findings
    );
    assert_eq!(source_of(&view, DIFF).state, SourceState::Clean);
}

#[tokio::test]
async fn exclusion_edit_changes_presentation_without_rerunning() {
    let (provider, calls) = Scripted::new(fixture(), None);
    let session = session_with(demo::snapshot(0), vec![provider]);
    wait_all(session.start_analysis().unwrap()).await;
    let before = session.view(Actor::Human).unwrap();
    assert_eq!(before.analysis.current_findings_excluded, 0);
    let path = session
        .store
        .lock()
        .unwrap()
        .list_findings(&session.pr_id, false)
        .unwrap()
        .into_iter()
        .find_map(|f| f.finding.value().anchor.current_path.clone())
        .expect("a fixture finding with a path");
    let runs_before = session
        .store
        .lock()
        .unwrap()
        .list_analysis_runs(&session.pr_id)
        .unwrap()
        .len();

    let rule = session
        .store
        .lock()
        .unwrap()
        .create_rule(
            &session.repository.id,
            RuleAction::Exclude,
            RuleKind::File,
            &path,
            None,
            Actor::Human,
        )
        .unwrap();
    // Whatever triggers analysis afterwards, an exclusion is not a reason.
    assert!(session.start_analysis().unwrap().is_empty());
    let after = session.view(Actor::Human).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1, "no engine reran");
    assert_eq!(
        session
            .store
            .lock()
            .unwrap()
            .list_analysis_runs(&session.pr_id)
            .unwrap()
            .len(),
        runs_before
    );
    assert_eq!(after.analysis.snapshot_id, before.analysis.snapshot_id);
    assert_eq!(
        after.analysis.current_findings,
        before.analysis.current_findings
    );
    assert!(after.analysis.current_findings_excluded > 0);

    // Removing the rule restores the presentation, still without a rerun.
    session
        .store
        .lock()
        .unwrap()
        .delete_rule(&session.repository.id, &rule.id)
        .unwrap();
    assert_eq!(
        session.view(Actor::Human).unwrap().analysis,
        before.analysis
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn restart_reports_head_change_against_the_persisted_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("aside.db");
    let first = Session::with_analysis(
        demo::snapshot(0),
        Store::open(&path).unwrap(),
        None,
        None,
        vec![],
        SchedulerConfig::default(),
    )
    .unwrap();
    first.start_analysis().unwrap();
    drop(first);
    let second = Session::with_analysis(
        demo::snapshot(1),
        Store::open(&path).unwrap(),
        None,
        None,
        vec![],
        SchedulerConfig::default(),
    )
    .unwrap();
    let mut rx = second.events.subscribe();
    second.start_analysis().unwrap();
    let events = drain(&mut rx);
    assert_eq!(types(&events), ["analysis.findings.invalidated"]);
    assert_eq!(
        events[0].payload["reasons"],
        serde_json::json!(["head_changed"])
    );
}

// ---------------------------------------------------------------------------
// Refresh never loses triage or history
// ---------------------------------------------------------------------------

#[tokio::test]
async fn triage_survives_a_head_change_and_old_findings_become_historical() {
    let (provider, _) = Scripted::new(fixture_on_first_snapshot(), None);
    let session = session_with(demo::snapshot(0), vec![provider]);
    wait_all(session.start_analysis().unwrap()).await;
    let findings = session
        .store
        .lock()
        .unwrap()
        .list_findings(&session.pr_id, false)
        .unwrap();
    assert!(!findings.is_empty());
    let id = findings[0].finding.value().id.clone();
    let mut state = FindingUserState::untouched(id.clone(), "triage", "2026-01-01T00:00:00Z");
    state.disposition = UserDisposition::DismissedLocal;
    state.note = Some("not relevant".into());
    let saved = session
        .store
        .lock()
        .unwrap()
        .put_finding_user_state(&session.pr_id, &state, 0)
        .unwrap();

    session.refresh().await.unwrap();
    wait_all(session.analysis.in_flight()).await;

    let store = session.store.lock().unwrap();
    // Every old finding is still there, now historical.
    let all = store.list_findings(&session.pr_id, true).unwrap();
    for old in &findings {
        let now = all
            .iter()
            .find(|f| f.finding.value().id == old.finding.value().id)
            .expect("old finding kept");
        assert_eq!(now.label, ObservationLabel::Historical);
    }
    assert!(
        store
            .list_findings(&session.pr_id, false)
            .unwrap()
            .is_empty()
    );
    // Triage is untouched.
    assert_eq!(
        store.get_finding_user_state("triage", &id).unwrap(),
        Some(saved)
    );
}

// ---------------------------------------------------------------------------
// SSE lifecycle, reconnect and late results
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_lifecycle_events_are_small_versioned_and_ordered() {
    let (provider, _) = Scripted::new(fixture(), None);
    let session = session_with(demo::snapshot(0), vec![provider]);
    let mut rx = session.events.subscribe();
    wait_all(session.start_analysis().unwrap()).await;
    let events = drain(&mut rx);
    assert_eq!(
        types(&events),
        [
            "analysis.run.queued",
            "analysis.run.started",
            "analysis.findings.upserted",
            "analysis.run.succeeded",
        ]
    );
    let sequences: Vec<u64> = events.iter().map(|e| e.sequence).collect();
    assert!(sequences.windows(2).all(|w| w[1] == w[0] + 1));
    for event in &events {
        assert_eq!(event.payload["eventVersion"], 1);
        assert_eq!(event.payload["label"], "current");
        // Small deltas: no snapshot, files or finding bodies.
        assert!(serde_json::to_string(&event.payload).unwrap().len() < 512);
    }
    let count = events[2].payload["count"].as_u64().unwrap();
    assert_eq!(
        count,
        session
            .view(Actor::Human)
            .unwrap()
            .analysis
            .current_findings as u64
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_mid_run_converges_to_the_authoritative_view() {
    let gate = Arc::new(Semaphore::new(0));
    let (provider, _) = Scripted::new(fixture(), Some(Arc::clone(&gate)));
    let session = session_with(demo::snapshot(0), vec![provider]);

    // A client that stays connected throughout.
    let mut steady = session.events.subscribe();
    // A client that connects, sees the run start, then drops.
    let mut flaky = session.events.subscribe();
    let handles = session.start_analysis().unwrap();
    let first = flaky.recv().await.unwrap();
    assert_eq!(first.event_type, "analysis.run.queued");
    let mid = session.view(Actor::Human).unwrap();
    assert!(matches!(source_of(&mid, DIFF).state, SourceState::Pending));
    assert_eq!(mid.analysis.overall, OverallCoverage::Incomplete); // reviews unverified
    drop(flaky);

    gate.add_permits(1);
    wait_all(handles).await;

    // Reconnect: the client has missed events, so it reloads the view.
    let resynced = session.view(Actor::Human).unwrap();
    assert_eq!(resynced.sequence, session.sequence.load(Ordering::SeqCst));
    assert!(resynced.sequence > first.sequence);
    let diff = source_of(&resynced, DIFF);
    assert_eq!(diff.status, Some(RunStatus::Succeeded));
    assert_eq!(diff.state, SourceState::HasResults);

    // The steady client's last word on the run agrees with the resync.
    let seen = drain(&mut steady);
    let last = seen.last().unwrap();
    assert_eq!(last.event_type, "analysis.run.succeeded");
    assert_eq!(last.sequence, resynced.sequence);
    let upserted: u64 = seen
        .iter()
        .filter(|e| e.event_type == "analysis.findings.upserted")
        .map(|e| e.payload["count"].as_u64().unwrap())
        .sum();
    assert_eq!(upserted, resynced.analysis.current_findings as u64);
    assert_eq!(
        diff.items,
        KnownCount::Known {
            value: upserted as u32
        }
    );

    // Resyncing again is idempotent: no double counting.
    assert_eq!(
        session.view(Actor::Human).unwrap().analysis,
        resynced.analysis
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn late_results_from_a_superseded_snapshot_stay_historical() {
    let gate = Arc::new(Semaphore::new(0));
    let (provider, _) = Scripted::new(fixture_on_first_snapshot(), Some(Arc::clone(&gate)));
    let session = session_with(demo::snapshot(0), vec![provider]);
    let mut rx = session.events.subscribe();
    let old = session.start_analysis().unwrap();
    let old_snapshot: SnapshotId = old[0].snapshot_id.clone();
    // Make sure the old run is really running (not just queued) before the
    // head moves, so its output arrives late rather than being skipped.
    while rx.recv().await.unwrap().event_type != "analysis.run.started" {}

    let view = session.refresh().await.unwrap();
    let new = session
        .analysis
        .in_flight()
        .into_iter()
        .find(|h| h.snapshot_id != old_snapshot)
        .unwrap();
    gate.add_permits(2);
    wait_all(vec![new.clone(), old[0].clone()]).await;

    let old_report = old[0].clone().wait().await;
    assert_eq!(old_report.stored_status, RunStatus::Stale);

    let events = drain(&mut rx);
    let late: Vec<&EventEnvelope> = events
        .iter()
        .filter(|e| e.payload["snapshotId"] == old_snapshot.as_str())
        .collect();
    assert!(late.iter().any(|e| e.event_type == "analysis.run.stale"));
    assert!(late.iter().all(|e| e.payload["label"] == "historical"));

    // Current state belongs to the new snapshot only.
    let after = session.view(Actor::Human).unwrap();
    assert_eq!(after.analysis.snapshot_id, view.analysis.snapshot_id);
    assert_eq!(after.analysis.current_findings, 0);
    assert!(after.analysis.historical_findings > 0);
    let diff = source_of(&after, DIFF);
    assert_eq!(diff.run_id.as_ref(), Some(&new.run_id));
    assert_eq!(diff.state, SourceState::Clean);
}

#[test]
fn refresh_event_payload_is_versioned_and_small() {
    let session = session_with(demo::snapshot(0), vec![]);
    let mut rx = session.events.subscribe();
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(session.refresh())
        .unwrap();
    let event = rx.try_recv().unwrap();
    assert_eq!(event.event_type, "pr.refreshed");
    let payload: &Value = &event.payload;
    assert_eq!(payload["eventVersion"], 1);
    assert!(payload["sources"].as_array().unwrap().len() >= 3);
    assert!(payload.get("files").is_none());
}
