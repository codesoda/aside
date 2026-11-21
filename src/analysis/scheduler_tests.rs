//! Scheduler tests. Providers are doubles replaying the #17 fixture; slow
//! providers are held with semaphores, and timeouts use Tokio's paused clock,
//! so nothing here sleeps on a real timer.

use std::{
    sync::{Arc, Mutex, atomic::Ordering},
    time::Duration,
};

use super::*;
use crate::{
    analysis::{
        ProviderKind,
        test_doubles::{Behaviour, DIFF, Double, FEEDBACK, HISTORY},
    },
    demo,
    model::Actor,
    session::Session,
    store::ObservationLabel,
};

const CANONICAL_REPO: &str = "212613049";

struct FixedClock;

impl Clock for FixedClock {
    fn now_rfc3339(&self) -> String {
        "2026-11-20T12:00:00Z".into()
    }
}

fn session() -> Arc<Session> {
    Session::new(
        demo::snapshot(0),
        Store::open_in_memory().unwrap(),
        None,
        None,
    )
    .unwrap()
}

fn scheduler(
    session: &Session,
    providers: Vec<Arc<dyn AnalysisProvider>>,
    config: SchedulerConfig,
) -> Scheduler {
    Scheduler::new(
        Arc::clone(&session.store),
        providers,
        config,
        Arc::new(FixedClock),
    )
    .unwrap()
}

fn config(max_concurrency: usize) -> SchedulerConfig {
    SchedulerConfig {
        max_concurrency,
        ..SchedulerConfig::default()
    }
}

fn input(session: &Session, version: usize) -> AnalysisInput {
    AnalysisInput::new(&session.pr_id, CANONICAL_REPO, demo::snapshot(version))
}

fn handle_for<'a>(handles: &'a [RunHandle], provider: &str) -> &'a RunHandle {
    handles.iter().find(|h| h.provider_id == provider).unwrap()
}

fn stored(session: &Session, run: &AnalysisRunId) -> AnalysisRun {
    session
        .store
        .lock()
        .unwrap()
        .get_analysis_run(run)
        .unwrap()
        .into_value()
}

/// `(provider, label)` for every finding, current and historical.
fn findings(session: &Session) -> Vec<(String, ObservationLabel)> {
    let mut out: Vec<_> = session
        .store
        .lock()
        .unwrap()
        .list_findings(&session.pr_id, true)
        .unwrap()
        .into_iter()
        .map(|f| (f.finding.value().source.provider_id.clone(), f.label))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn current_providers(session: &Session) -> Vec<String> {
    let mut out: Vec<String> = findings(session)
        .into_iter()
        .filter(|(_, l)| *l == ObservationLabel::Current)
        .map(|(p, _)| p)
        .collect();
    out.dedup();
    out
}

fn run_count(session: &Session) -> usize {
    session
        .store
        .lock()
        .unwrap()
        .list_analysis_runs(&session.pr_id)
        .unwrap()
        .len()
}

#[test]
fn demo_input_pins_the_fixture_snapshot() {
    let session = session();
    let bundle = demo::intelligence::load(false);
    assert_eq!(input(&session, 0).snapshot_id, bundle.manifest.snapshot_id);
    assert_ne!(input(&session, 1).snapshot_id, bundle.manifest.snapshot_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_is_usable_while_a_slow_provider_is_in_flight() {
    let session = session();
    let before = session.view(Actor::Human).unwrap();
    let (slow, gate, started) = Double::gated(HISTORY, ProviderKind::History);
    let fast = Double::new(DIFF, ProviderKind::Diff, Behaviour::Fixture);
    let analysis = scheduler(&session, vec![slow, fast], config(4));

    let handles = analysis.begin_snapshot(input(&session, 0)).unwrap();
    started.notified().await;
    let fast_report = handle_for(&handles, DIFF).clone().wait().await;
    assert_eq!(fast_report.stored_status, RunStatus::Succeeded);

    // The slow provider is provably still running...
    let slow_handle = handle_for(&handles, HISTORY).clone();
    assert!(slow_handle.report().is_none());
    assert_eq!(
        stored(&session, &slow_handle.run_id).status,
        RunStatus::Running
    );
    // ...and the Review screen's data is complete and unchanged.
    let view = session.view(Actor::Human).unwrap();
    assert_eq!(
        view.files.len(),
        session.snapshot.read().unwrap().files.len()
    );
    assert_eq!(view.files, before.files);
    assert_eq!(view.progress, before.progress);
    assert_eq!(view.threads.len(), before.threads.len());
    // The fast engine's results are already current; the slow one has none.
    assert_eq!(current_providers(&session), vec![DIFF.to_owned()]);

    gate.add_permits(1);
    let slow_report = slow_handle.wait().await;
    assert_eq!(slow_report.stored_status, RunStatus::Succeeded);
    assert_eq!(
        current_providers(&session),
        vec![HISTORY.to_owned(), DIFF.to_owned()]
    );
    assert!(analysis.in_flight().is_empty());
}

#[tokio::test]
async fn session_starts_analysis_without_blocking() {
    let session = session();
    let handles = session.start_analysis().unwrap();
    // The registered diff engine is scheduled; this call returned without
    // waiting for it.
    assert!(
        handles
            .iter()
            .any(|h| h.provider_id == crate::diff::facts::PROVIDER_ID)
    );
    let current = session
        .store
        .lock()
        .unwrap()
        .current_analysis_snapshot(&session.pr_id)
        .unwrap();
    assert_eq!(current, Some(input(&session, 0).snapshot_id));
    // Refresh moves the pointer to the new snapshot.
    session.refresh().await.unwrap();
    let current = session
        .store
        .lock()
        .unwrap()
        .current_analysis_snapshot(&session.pr_id)
        .unwrap();
    assert_eq!(current, Some(input(&session, 1).snapshot_id));
}

#[tokio::test]
async fn late_result_for_an_old_snapshot_stays_historical() {
    let session = session();
    let (slow, gate, started) = Double::gated(DIFF, ProviderKind::Diff);
    let analysis = scheduler(&session, vec![slow], config(4));

    let old = analysis.begin_snapshot(input(&session, 0)).unwrap();
    started.notified().await;
    let new = analysis.begin_snapshot(input(&session, 1)).unwrap();
    assert_ne!(old[0].run_id, new[0].run_id);

    // The old run finishes only after the PR moved on.
    gate.add_permits(2);
    let new_report = new[0].clone().wait().await;
    assert_eq!(new_report.stored_status, RunStatus::Succeeded);
    let late = old[0].clone().wait().await;
    assert_eq!(late.stored_status, RunStatus::Stale);
    assert_eq!(late.reported_status, RunStatus::Succeeded);
    assert_eq!(stored(&session, &late.run_id).status, RunStatus::Stale);

    // Old observations are historical; the new run's are current.
    let current: Vec<_> = session
        .store
        .lock()
        .unwrap()
        .list_findings(&session.pr_id, false)
        .unwrap()
        .into_iter()
        .map(|f| f.latest_snapshot_id)
        .collect();
    assert!(!current.is_empty());
    assert!(current.iter().all(|s| *s == input(&session, 1).snapshot_id));
}

#[tokio::test]
async fn queued_run_for_a_superseded_snapshot_is_skipped_not_run() {
    let session = session();
    let (blocker, gate, started) = Double::gated(HISTORY, ProviderKind::History);
    let waiting = Double::new(DIFF, ProviderKind::Diff, Behaviour::Fixture);
    let calls = Arc::clone(&waiting.calls);
    let analysis = scheduler(&session, vec![blocker, waiting], config(1));

    let old = analysis.begin_snapshot(input(&session, 0)).unwrap();
    started.notified().await;
    analysis.begin_snapshot(input(&session, 1)).unwrap();
    gate.add_permits(2);
    let report = handle_for(&old, DIFF).clone().wait().await;
    assert_eq!(report.stored_status, RunStatus::Skipped);
    assert_eq!(report.reason.as_deref(), Some(reason::SNAPSHOT_SUPERSEDED));
    let run = stored(&session, &report.run_id);
    assert_eq!(run.coverage.reason_codes, vec![reason::SNAPSHOT_SUPERSEDED]);
    // Only the new snapshot's diff run actually executed.
    for h in analysis.in_flight() {
        h.wait().await;
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Runs `bad` next to a healthy provider and checks that only `bad` is hurt.
async fn isolated(bad: Arc<Double>, config: SchedulerConfig) -> (Arc<Session>, RunReport) {
    let session = session();
    let healthy = Double::new(DIFF, ProviderKind::Diff, Behaviour::Fixture);
    let analysis = scheduler(&session, vec![bad, healthy], config);
    let handles = analysis.begin_snapshot(input(&session, 0)).unwrap();
    let bad_report = handle_for(&handles, HISTORY).clone().wait().await;
    let good = handle_for(&handles, DIFF).clone().wait().await;
    assert_eq!(good.stored_status, RunStatus::Succeeded);
    assert_eq!(current_providers(&session), vec![DIFF.to_owned()]);
    assert!(session.view(Actor::Human).is_ok());

    let run = stored(&session, &bad_report.run_id);
    assert_eq!(run.status, bad_report.stored_status);
    let code = bad_report.reason.clone().expect("a reason is recorded");
    assert_eq!(run.coverage.reason_codes, vec![code.clone()]);
    assert_eq!(run.warnings.last().unwrap().code, code);
    assert!(run.finished_at.is_some());
    (session, bad_report)
}

#[tokio::test]
async fn panicking_provider_is_contained() {
    let bad = Double::new(HISTORY, ProviderKind::History, Behaviour::Panic);
    let (session, report) = isolated(bad, config(4)).await;
    assert_eq!(report.stored_status, RunStatus::Failed);
    assert_eq!(report.reason.as_deref(), Some(reason::PROVIDER_PANICKED));
    let run = stored(&session, &report.run_id);
    assert!(
        run.warnings[0]
            .message
            .contains("deliberate provider panic")
    );
    // The store lock was never poisoned.
    assert!(session.store.lock().is_ok());
}

#[tokio::test(start_paused = true)]
async fn hanging_provider_times_out() {
    let limits = ProviderLimits {
        timeout: Duration::from_secs(5),
        ..ProviderLimits::default()
    };
    let bad = Double::with_limits(
        HISTORY,
        ProviderKind::History,
        Behaviour::Hang,
        Some(limits),
    );
    let (session, report) = isolated(bad, config(4)).await;
    assert_eq!(report.stored_status, RunStatus::Failed);
    assert_eq!(report.reason.as_deref(), Some(reason::TIME_LIMIT_EXCEEDED));
    // Duration comes from the paused Tokio clock: exactly the budget.
    let run = stored(&session, &report.run_id);
    assert_eq!(run.metrics.duration_ms, Some(5_000));
}

#[tokio::test]
async fn erroring_provider_is_contained() {
    let bad = Double::new(HISTORY, ProviderKind::History, Behaviour::Error);
    let (session, report) = isolated(bad, config(4)).await;
    assert_eq!(report.stored_status, RunStatus::Failed);
    assert_eq!(report.reason.as_deref(), Some(reason::PROVIDER_ERROR));
    let run = stored(&session, &report.run_id);
    assert!(
        run.warnings[0]
            .message
            .contains("deliberate provider error")
    );
}

#[tokio::test]
async fn provider_over_its_input_budget_is_skipped_without_running() {
    let limits = ProviderLimits {
        max_input_files: 1,
        ..ProviderLimits::default()
    };
    let bad = Double::with_limits(
        HISTORY,
        ProviderKind::History,
        Behaviour::Fixture,
        Some(limits),
    );
    let calls = Arc::clone(&bad.calls);
    let (session, report) = isolated(bad, config(4)).await;
    assert_eq!(report.stored_status, RunStatus::Skipped);
    assert_eq!(report.reason.as_deref(), Some(reason::INPUT_LIMIT_EXCEEDED));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let run = stored(&session, &report.run_id);
    assert_eq!(run.coverage.completeness, Completeness::Skipped);
}

#[tokio::test]
async fn provider_over_its_output_budget_fails_and_writes_nothing() {
    let limits = ProviderLimits {
        max_findings: 3,
        ..ProviderLimits::default()
    };
    let bad = Double::with_limits(
        HISTORY,
        ProviderKind::History,
        Behaviour::Flood(10),
        Some(limits),
    );
    let (session, report) = isolated(bad, config(4)).await;
    assert_eq!(report.stored_status, RunStatus::Failed);
    assert_eq!(
        report.reason.as_deref(),
        Some(reason::OUTPUT_LIMIT_EXCEEDED)
    );
    assert!(findings(&session).iter().all(|(p, _)| p == DIFF));
}

#[tokio::test]
async fn scheduler_overrides_win_over_provider_limits() {
    let mut cfg = config(4);
    cfg.overrides.insert(
        HISTORY.into(),
        ProviderLimits {
            max_input_bytes: 1,
            ..ProviderLimits::default()
        },
    );
    let lenient = ProviderLimits {
        max_input_bytes: u64::MAX,
        ..ProviderLimits::default()
    };
    let bad = Double::with_limits(
        HISTORY,
        ProviderKind::History,
        Behaviour::Fixture,
        Some(lenient),
    );
    let (_, report) = isolated(bad, cfg).await;
    assert_eq!(report.reason.as_deref(), Some(reason::INPUT_LIMIT_EXCEEDED));
}

#[tokio::test]
async fn concurrency_limit_queues_the_rest() {
    let session = session();
    let (a, gate_a, started_a) = Double::gated(DIFF, ProviderKind::Diff);
    let (b, gate_b, _) = Double::gated(HISTORY, ProviderKind::History);
    let (a_calls, b_calls) = (Arc::clone(&a.calls), Arc::clone(&b.calls));
    let analysis = scheduler(&session, vec![a, b], config(1));
    let handles = analysis.begin_snapshot(input(&session, 0)).unwrap();
    started_a.notified().await;
    tokio::task::yield_now().await;
    assert_eq!(a_calls.load(Ordering::SeqCst), 1);
    assert_eq!(b_calls.load(Ordering::SeqCst), 0);
    let b_run = handle_for(&handles, HISTORY).run_id.clone();
    assert_eq!(stored(&session, &b_run).status, RunStatus::Queued);

    gate_a.add_permits(1);
    gate_b.add_permits(1);
    for h in handles {
        assert_eq!(h.wait().await.stored_status, RunStatus::Succeeded);
    }
}

#[tokio::test]
async fn cancel_running_provider_leaves_no_partial_state() {
    let session = session();
    let (slow, _gate, started) = Double::gated(HISTORY, ProviderKind::History);
    let fast = Double::new(DIFF, ProviderKind::Diff, Behaviour::Fixture);
    let analysis = scheduler(&session, vec![slow, fast], config(4));
    let handles = analysis.begin_snapshot(input(&session, 0)).unwrap();
    started.notified().await;
    handle_for(&handles, DIFF).clone().wait().await;
    let snapshot = input(&session, 0).snapshot_id;

    let report = analysis.cancel(&snapshot, HISTORY).await.unwrap().unwrap();
    assert_eq!(report.stored_status, RunStatus::Cancelled);
    assert_eq!(report.write_error, None);
    let run = stored(&session, &report.run_id);
    assert_eq!(run.status, RunStatus::Cancelled);
    assert_eq!(run.coverage.reason_codes, vec![reason::CANCELLED]);
    // Nothing from the cancelled run; the other provider is untouched.
    assert_eq!(current_providers(&session), vec![DIFF.to_owned()]);
    assert!(analysis.in_flight().is_empty());
    // Nothing left to cancel.
    assert!(analysis.cancel(&snapshot, HISTORY).await.unwrap().is_none());
    // A late batch for the cancelled run is refused by the store as a whole.
    let ctx = ProviderContext {
        run_id: run.id.clone(),
        attempt: run.attempt,
        limits: ProviderLimits::default(),
        input: Arc::new(input(&session, 0)),
    };
    let out = crate::analysis::test_doubles::fixture_output(HISTORY, &ctx).unwrap();
    let mut done = run.clone();
    done.status = RunStatus::Succeeded;
    let batch = AnalysisBatch {
        run: Raw::new(done).unwrap(),
        findings: out.findings,
        evidence: out.evidence,
        relations: out.relations,
    };
    assert!(
        session
            .store
            .lock()
            .unwrap()
            .commit_analysis_batch(&batch)
            .is_err()
    );
    assert_eq!(current_providers(&session), vec![DIFF.to_owned()]);
}

#[tokio::test]
async fn cancel_queued_provider_never_runs_it() {
    let session = session();
    let (blocker, gate, started) = Double::gated(DIFF, ProviderKind::Diff);
    let queued = Double::new(HISTORY, ProviderKind::History, Behaviour::Fixture);
    let calls = Arc::clone(&queued.calls);
    let analysis = scheduler(&session, vec![blocker, queued], config(1));
    analysis.begin_snapshot(input(&session, 0)).unwrap();
    started.notified().await;
    let snapshot = input(&session, 0).snapshot_id;
    let report = analysis.cancel(&snapshot, HISTORY).await.unwrap().unwrap();
    assert_eq!(report.stored_status, RunStatus::Cancelled);
    gate.add_permits(1);
    for h in analysis.in_flight() {
        h.wait().await;
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(current_providers(&session), vec![DIFF.to_owned()]);
}

#[tokio::test]
async fn rerun_is_idempotent_and_scoped_to_one_provider() {
    let session = session();
    let (diff, gate, started) = Double::gated(DIFF, ProviderKind::Diff);
    let history = Double::new(HISTORY, ProviderKind::History, Behaviour::Fixture);
    let feedback = Double::new(FEEDBACK, ProviderKind::Feedback, Behaviour::Fixture);
    let history_calls = Arc::clone(&history.calls);
    let analysis = scheduler(&session, vec![diff, history, feedback], config(4));
    let snapshot = input(&session, 0).snapshot_id;

    gate.add_permits(1);
    for h in analysis.begin_snapshot(input(&session, 0)).unwrap() {
        assert_eq!(h.wait().await.stored_status, RunStatus::Succeeded);
    }
    let all = vec![HISTORY.to_owned(), DIFF.to_owned(), FEEDBACK.to_owned()];
    assert_eq!(current_providers(&session), all);
    assert_eq!(run_count(&session), 3);

    // Beginning the same snapshot again starts nothing.
    assert!(
        analysis
            .begin_snapshot(input(&session, 0))
            .unwrap()
            .is_empty()
    );
    assert_eq!(run_count(&session), 3);

    // Two reruns while the first is in flight are one run.
    let first = analysis.rerun(&snapshot, DIFF).unwrap();
    started.notified().await;
    let second = analysis.rerun(&snapshot, DIFF).unwrap();
    assert_eq!(first.run_id, second.run_id);
    assert_eq!(run_count(&session), 4);
    assert_eq!(stored(&session, &first.run_id).attempt, 2);
    // Other providers' results are still current mid-rerun.
    assert_eq!(current_providers(&session), all);

    gate.add_permits(1);
    assert_eq!(first.wait().await.stored_status, RunStatus::Succeeded);
    assert_eq!(current_providers(&session), all);
    assert_eq!(history_calls.load(Ordering::SeqCst), 1);
    // Both diff attempts are kept.
    let attempts: Vec<u32> = session
        .store
        .lock()
        .unwrap()
        .list_analysis_runs(&session.pr_id)
        .unwrap()
        .iter()
        .filter(|r| r.value().provider_id == DIFF)
        .map(|r| r.value().attempt)
        .collect();
    assert_eq!(attempts.len(), 2);
    assert!(attempts.contains(&1) && attempts.contains(&2));

    assert!(analysis.rerun(&snapshot, "nope").is_err());
}

#[tokio::test]
async fn orphaned_runs_from_an_earlier_process_are_closed_and_retried() {
    let session = session();
    let first = scheduler(
        &session,
        vec![Double::new(DIFF, ProviderKind::Diff, Behaviour::Hang)],
        config(4),
    );
    let handles = first.begin_snapshot(input(&session, 0)).unwrap();
    let orphan = handles[0].run_id.clone();
    // Simulate a crash: the old scheduler's tasks stop, rows stay "running".
    drop(first);
    let restarted = scheduler(
        &session,
        vec![Double::new(DIFF, ProviderKind::Diff, Behaviour::Fixture)],
        config(4),
    );
    let handles = restarted.begin_snapshot(input(&session, 0)).unwrap();
    let report = handles[0].clone().wait().await;
    assert_eq!(report.stored_status, RunStatus::Succeeded);
    let old = stored(&session, &orphan);
    assert_eq!(old.status, RunStatus::Failed);
    assert_eq!(old.coverage.reason_codes, vec![reason::INTERRUPTED]);
    assert_eq!(stored(&session, &report.run_id).attempt, 2);
}

#[test]
fn duplicate_provider_ids_are_rejected() {
    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let a = Double::new(DIFF, ProviderKind::Diff, Behaviour::Fixture);
    let b = Double::new(DIFF, ProviderKind::Diff, Behaviour::Fixture);
    assert!(
        Scheduler::new(
            store,
            vec![a, b],
            SchedulerConfig::default(),
            Arc::new(SystemClock)
        )
        .is_err()
    );
}

#[test]
fn default_limits_are_sane() {
    let limits = ProviderLimits::default();
    assert!(limits.timeout >= Duration::from_secs(10));
    assert!(limits.max_input_bytes > 0 && limits.max_input_files > 0);
    let cfg = SchedulerConfig::default();
    assert!((1..=4).contains(&cfg.max_concurrency));
}
