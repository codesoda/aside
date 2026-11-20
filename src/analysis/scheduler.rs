//! Snapshot-pinned, bounded, non-blocking scheduling of analysis providers
//! (#22, analyze.md §4 and §12.4).
//!
//! # Properties
//!
//! - **Never blocks review.** Providers run on their own tasks. The store
//!   lock is only taken for short, synchronous state writes, never across an
//!   await, so `Session::view` stays responsive while engines run.
//! - **Snapshot-pinned.** Every run is recorded against the snapshot it was
//!   started for. The store turns output for a snapshot that is no longer
//!   current into `stale` (keeping the provider's own status), so late
//!   results stay historical.
//! - **Isolated.** Each provider future runs on its own spawned task, so a
//!   panic is caught as a `JoinError`; errors, timeouts and over-budget
//!   output end only that run, as `failed` with a reason code.
//! - **Bounded.** Wall-clock time, global concurrency, input size and output
//!   size are limited ([`ProviderLimits`], [`SchedulerConfig`]). Every bound
//!   that trips is recorded as `skipped` or `failed`, never dropped.
//! - **All-or-nothing.** Results are written with one
//!   [`Store::commit_analysis_batch`]. Cancellation and batch commit are both
//!   serialised by the store, which rejects results for a cancelled run and
//!   rejects cancelling a finished run; there is no half-written state.
//! - **Scoped, idempotent rerun.** A rerun creates a new attempt for one
//!   provider on one snapshot; another provider's runs are never touched.
//!   Rerunning while that provider is already queued or running returns the
//!   in-flight run instead of starting a second one.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, anyhow, bail};
use tokio::{
    sync::{Semaphore, oneshot, watch},
    time::Instant,
};

use super::provider::{
    AnalysisInput, AnalysisProvider, ProviderContext, ProviderLimits, ProviderOutput,
};
use crate::{
    model::{
        AnalysisCoverage, AnalysisRun, AnalysisRunId, Completeness, RunKeyInput, RunStatus,
        RunWarning, SnapshotId,
    },
    store::{AnalysisBatch, Raw, Store},
    util::now_rfc3339,
};

/// Reason codes recorded in `coverage.reasonCodes` and `warnings[].code`.
pub mod reason {
    pub const INPUT_LIMIT_EXCEEDED: &str = "input_limit_exceeded";
    pub const SNAPSHOT_SUPERSEDED: &str = "snapshot_superseded";
    pub const TIME_LIMIT_EXCEEDED: &str = "time_limit_exceeded";
    pub const OUTPUT_LIMIT_EXCEEDED: &str = "output_limit_exceeded";
    pub const PROVIDER_PANICKED: &str = "provider_panicked";
    pub const PROVIDER_ERROR: &str = "provider_error";
    pub const INVALID_OUTPUT: &str = "invalid_output";
    pub const CANCELLED: &str = "cancelled";
    pub const INTERRUPTED: &str = "interrupted";
}

/// Source of wall-clock timestamps for run records. Durations and timeouts
/// use `tokio::time`, which tests can pause and advance.
pub trait Clock: Send + Sync + 'static {
    fn now_rfc3339(&self) -> String;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_rfc3339(&self) -> String {
        now_rfc3339()
    }
}

#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// Providers running at the same time, across all snapshots.
    pub max_concurrency: usize,
    pub default_limits: ProviderLimits,
    /// Per-provider overrides, keyed by provider ID. Win over the provider's
    /// own limits.
    pub overrides: HashMap<String, ProviderLimits>,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        let cpus = std::thread::available_parallelism().map_or(2, |n| n.get());
        Self {
            max_concurrency: cpus.clamp(1, 4),
            default_limits: ProviderLimits::default(),
            overrides: HashMap::new(),
        }
    }
}

/// Final state of one run as the scheduler saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReport {
    pub run_id: AnalysisRunId,
    pub provider_id: String,
    pub snapshot_id: SnapshotId,
    /// Status the store holds (`stale` for late output on an old snapshot).
    pub stored_status: RunStatus,
    /// Status the run reached before any stale relabelling.
    pub reported_status: RunStatus,
    pub reason: Option<String>,
    /// Set when the final state could not be written. The run keeps whatever
    /// state was last stored.
    pub write_error: Option<String>,
}

/// A queued, running or finished run.
#[derive(Debug, Clone)]
pub struct RunHandle {
    pub run_id: AnalysisRunId,
    pub provider_id: String,
    pub snapshot_id: SnapshotId,
    done: watch::Receiver<Option<RunReport>>,
}

impl RunHandle {
    #[allow(dead_code)] // TODO(#33): polled by the analysis HTTP API.
    pub fn report(&self) -> Option<RunReport> {
        self.done.borrow().clone()
    }

    pub async fn wait(mut self) -> RunReport {
        if let Ok(report) = self.done.wait_for(Option::is_some).await {
            return report.clone().expect("checked by wait_for");
        }
        RunReport {
            run_id: self.run_id,
            provider_id: self.provider_id,
            snapshot_id: self.snapshot_id,
            stored_status: RunStatus::Failed,
            reported_status: RunStatus::Failed,
            reason: None,
            write_error: Some("scheduler task ended without a report".into()),
        }
    }
}

type RunKey = (SnapshotId, String);

struct Active {
    handle: RunHandle,
    cancel: Option<oneshot::Sender<()>>,
}

struct Inner {
    store: Arc<Mutex<Store>>,
    providers: Vec<Arc<dyn AnalysisProvider>>,
    config: SchedulerConfig,
    clock: Arc<dyn Clock>,
    slots: Arc<Semaphore>,
    /// Queued or running runs. Lock order: `active` before `store`.
    active: Mutex<HashMap<RunKey, Active>>,
    inputs: Mutex<HashMap<SnapshotId, Arc<AnalysisInput>>>,
}

#[derive(Clone)]
pub struct Scheduler {
    inner: Arc<Inner>,
}

impl Scheduler {
    pub fn new(
        store: Arc<Mutex<Store>>,
        providers: Vec<Arc<dyn AnalysisProvider>>,
        config: SchedulerConfig,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        let mut ids: Vec<&str> = providers
            .iter()
            .map(|p| p.descriptor().id.as_str())
            .collect();
        ids.sort_unstable();
        if ids.windows(2).any(|w| w[0] == w[1]) {
            bail!("provider IDs must be unique");
        }
        let slots = Arc::new(Semaphore::new(config.max_concurrency.max(1)));
        Ok(Self {
            inner: Arc::new(Inner {
                store,
                providers,
                config,
                clock,
                slots,
                active: Mutex::new(HashMap::new()),
                inputs: Mutex::new(HashMap::new()),
            }),
        })
    }

    /// Makes `input` the PR's current analysis snapshot and starts every
    /// provider that has not already run against this exact input.
    /// Returns immediately; runs continue in the background. Runs still in
    /// flight for older snapshots keep going and land as `stale`.
    ///
    /// Must be called inside a Tokio runtime.
    pub fn begin_snapshot(&self, input: AnalysisInput) -> Result<Vec<RunHandle>> {
        let input = Arc::new(input);
        let mut active = lock(&self.inner.active)?;
        let existing = {
            let mut store = lock(&self.inner.store)?;
            store.register_analysis_snapshot(
                &input.pr_id,
                &input.snapshot_id,
                &input.base_sha,
                &input.head_sha,
                None,
            )?;
            store.set_current_analysis_snapshot(&input.pr_id, &input.snapshot_id)?;
            let runs = store.list_analysis_runs(&input.pr_id)?;
            // Queued/running rows nobody is driving were left by an earlier
            // process; close them so they neither look live nor pin retention.
            for raw in &runs {
                let run = raw.value();
                let key = (run.snapshot_id.clone(), run.provider_id.clone());
                if !run.status.is_terminal() && !active.contains_key(&key) {
                    let closed = terminal(
                        run,
                        RunStatus::Failed,
                        reason::INTERRUPTED,
                        "the process driving this run stopped before it finished",
                        self.inner.clock.now_rfc3339(),
                        None,
                    );
                    store.record_analysis_run(&input.pr_id, &Raw::new(closed)?)?;
                }
            }
            runs
        };
        {
            let mut inputs = lock(&self.inner.inputs)?;
            inputs.retain(|id, _| active.keys().any(|(s, _)| s == id));
            inputs.insert(input.snapshot_id.clone(), Arc::clone(&input));
        }
        let digest = input.input_digest();
        let mut handles = Vec::new();
        for provider in &self.inner.providers {
            let d = provider.descriptor();
            let key = (input.snapshot_id.clone(), d.id.clone());
            if let Some(run) = active.get(&key) {
                handles.push(run.handle.clone());
                continue;
            }
            let already_ran = existing.iter().any(|raw| {
                let r = raw.value();
                r.snapshot_id == input.snapshot_id
                    && r.provider_id == d.id
                    && r.engine_version == d.engine_version
                    && r.configuration_digest == d.configuration_digest
                    && r.input_digest == digest
                    // Unfinished rows were just closed as interrupted; retry them.
                    && r.status.is_terminal()
            });
            if already_ran {
                continue;
            }
            let attempt = next_attempt(&existing, &input.snapshot_id, &d.id);
            handles.push(self.launch(&mut active, &input, provider, attempt)?);
        }
        Ok(handles)
    }

    /// Starts a new attempt of one provider on one snapshot. Idempotent: if
    /// that provider is already queued or running there, returns that run.
    /// Never touches other providers' runs or results.
    #[allow(dead_code)] // TODO(#33): exposed through the analysis HTTP API.
    pub fn rerun(&self, snapshot_id: &SnapshotId, provider_id: &str) -> Result<RunHandle> {
        let provider = self
            .inner
            .providers
            .iter()
            .find(|p| p.descriptor().id == provider_id)
            .ok_or_else(|| anyhow!("unknown analysis provider {provider_id}"))?;
        let mut active = lock(&self.inner.active)?;
        let key = (snapshot_id.clone(), provider_id.to_owned());
        if let Some(run) = active.get(&key) {
            return Ok(run.handle.clone());
        }
        let input = lock(&self.inner.inputs)?
            .get(snapshot_id)
            .cloned()
            .with_context(|| format!("snapshot {snapshot_id} is not loaded for analysis"))?;
        let runs = lock(&self.inner.store)?.list_analysis_runs(&input.pr_id)?;
        let attempt = next_attempt(&runs, snapshot_id, provider_id);
        self.launch(&mut active, &input, provider, attempt)
    }

    /// Cancels one provider's queued or running run on a snapshot and waits
    /// for its final state. Returns `None` when nothing was in flight. If the
    /// run's results were already being committed, the report shows that
    /// committed status instead of `cancelled`.
    #[allow(dead_code)] // TODO(#33): exposed through the analysis HTTP API.
    pub async fn cancel(
        &self,
        snapshot_id: &SnapshotId,
        provider_id: &str,
    ) -> Result<Option<RunReport>> {
        let handle = {
            let mut active = lock(&self.inner.active)?;
            let Some(run) = active.get_mut(&(snapshot_id.clone(), provider_id.to_owned())) else {
                return Ok(None);
            };
            if let Some(tx) = run.cancel.take() {
                let _ = tx.send(());
            }
            run.handle.clone()
        };
        Ok(Some(handle.wait().await))
    }

    /// Runs currently queued or running.
    #[allow(dead_code)] // TODO(#33): exposed through the analysis HTTP API.
    pub fn in_flight(&self) -> Vec<RunHandle> {
        lock(&self.inner.active)
            .map(|a| a.values().map(|r| r.handle.clone()).collect())
            .unwrap_or_default()
    }

    fn limits_for(&self, provider: &dyn AnalysisProvider) -> ProviderLimits {
        let d = provider.descriptor();
        self.inner
            .config
            .overrides
            .get(&d.id)
            .copied()
            .or(d.limits)
            .unwrap_or(self.inner.config.default_limits)
    }

    /// Records the run as queued (or skipped, if its input is over budget)
    /// and spawns its driver. Caller holds the `active` lock.
    fn launch(
        &self,
        active: &mut HashMap<RunKey, Active>,
        input: &Arc<AnalysisInput>,
        provider: &Arc<dyn AnalysisProvider>,
        attempt: u32,
    ) -> Result<RunHandle> {
        let d = provider.descriptor();
        let limits = self.limits_for(provider.as_ref());
        let digest = input.input_digest();
        let queued = AnalysisRun::queued(
            &RunKeyInput {
                snapshot_id: &input.snapshot_id,
                provider_id: &d.id,
                engine_version: &d.engine_version,
                configuration_digest: &d.configuration_digest,
                input_digest: &digest,
                attempt,
            },
            d.mode,
        );
        let (tx, rx) = watch::channel(None);
        let handle = RunHandle {
            run_id: queued.id.clone(),
            provider_id: d.id.clone(),
            snapshot_id: input.snapshot_id.clone(),
            done: rx,
        };

        let (bytes, files) = (input.input_bytes(), input.input_files());
        if bytes > limits.max_input_bytes || files > limits.max_input_files {
            let message = format!(
                "input of {files} files / {bytes} bytes exceeds the limit of {} files / {} bytes",
                limits.max_input_files, limits.max_input_bytes
            );
            let skipped = terminal(
                &queued,
                RunStatus::Skipped,
                reason::INPUT_LIMIT_EXCEEDED,
                &message,
                self.inner.clock.now_rfc3339(),
                None,
            );
            let report = self
                .inner
                .write(&input.pr_id, skipped, reason::INPUT_LIMIT_EXCEEDED);
            if let Some(error) = &report.write_error {
                bail!("could not record skipped run: {error}");
            }
            let _ = tx.send(Some(report));
            return Ok(handle);
        }

        lock(&self.inner.store)?.record_analysis_run(&input.pr_id, &Raw::new(queued.clone())?)?;
        let (cancel_tx, cancel_rx) = oneshot::channel();
        active.insert(
            (input.snapshot_id.clone(), d.id.clone()),
            Active {
                handle: handle.clone(),
                cancel: Some(cancel_tx),
            },
        );
        let inner = Arc::clone(&self.inner);
        let provider = Arc::clone(provider);
        let input = Arc::clone(input);
        tokio::spawn(async move {
            let key = (queued.snapshot_id.clone(), queued.provider_id.clone());
            let run_id = queued.id.clone();
            let report = inner
                .drive(provider, input, limits, queued, cancel_rx)
                .await;
            if let Ok(mut active) = inner.active.lock()
                && active.get(&key).is_some_and(|a| a.handle.run_id == run_id)
            {
                active.remove(&key);
            }
            let _ = tx.send(Some(report));
        });
        Ok(handle)
    }
}

enum Outcome {
    Cancelled,
    Output(ProviderOutput),
    Failed(&'static str, String),
}

impl Inner {
    async fn drive(
        &self,
        provider: Arc<dyn AnalysisProvider>,
        input: Arc<AnalysisInput>,
        limits: ProviderLimits,
        queued: AnalysisRun,
        mut cancel: oneshot::Receiver<()>,
    ) -> RunReport {
        let pr_id = input.pr_id.as_str();
        let permit = tokio::select! {
            biased;
            Ok(()) = &mut cancel => return self.cancelled(pr_id, &queued, None),
            permit = Arc::clone(&self.slots).acquire_owned() => permit,
        };
        let Ok(_permit) = permit else {
            return self.fail(
                pr_id,
                &queued,
                reason::PROVIDER_ERROR,
                "scheduler closed",
                None,
            );
        };

        // Don't spend a slot on a snapshot the PR has already moved past.
        let current = lock(&self.store).and_then(|s| s.current_analysis_snapshot(pr_id));
        if current.is_ok_and(|c| c.as_ref() != Some(&queued.snapshot_id)) {
            let skipped = terminal(
                &queued,
                RunStatus::Skipped,
                reason::SNAPSHOT_SUPERSEDED,
                "the PR moved to a newer snapshot before this run started",
                self.clock.now_rfc3339(),
                None,
            );
            return self.write(pr_id, skipped, reason::SNAPSHOT_SUPERSEDED);
        }

        let mut running = queued.clone();
        running.status = RunStatus::Running;
        running.started_at = Some(self.clock.now_rfc3339());
        let report = self.write(pr_id, running.clone(), "");
        if report.write_error.is_some() {
            return report;
        }

        let started = Instant::now();
        let ctx = ProviderContext {
            run_id: running.id.clone(),
            attempt: running.attempt,
            limits,
            input: Arc::clone(&input),
        };
        // `run` is called on the spawned task too, so a panic while building
        // the future is caught like one while polling it.
        let mut job = tokio::spawn(async move { provider.run(ctx).await });
        let outcome = tokio::select! {
            biased;
            Ok(()) = &mut cancel => Outcome::Cancelled,
            joined = tokio::time::timeout(limits.timeout, &mut job) => match joined {
                Err(_) => Outcome::Failed(
                    reason::TIME_LIMIT_EXCEEDED,
                    format!("did not finish within {:?}", limits.timeout),
                ),
                Ok(Err(error)) if error.is_panic() => {
                    Outcome::Failed(reason::PROVIDER_PANICKED, panic_message(error.into_panic()))
                }
                Ok(Err(error)) => Outcome::Failed(reason::PROVIDER_ERROR, error.to_string()),
                Ok(Ok(Err(error))) => Outcome::Failed(reason::PROVIDER_ERROR, format!("{error:#}")),
                Ok(Ok(Ok(output))) => Outcome::Output(output),
            },
        };
        job.abort();
        let elapsed = Some(started.elapsed().as_millis() as u64);

        match outcome {
            Outcome::Cancelled => self.cancelled(pr_id, &running, elapsed),
            Outcome::Failed(code, message) => self.fail(pr_id, &running, code, &message, elapsed),
            Outcome::Output(output) => self.commit(pr_id, &running, limits, output, elapsed),
        }
    }

    fn commit(
        &self,
        pr_id: &str,
        running: &AnalysisRun,
        limits: ProviderLimits,
        output: ProviderOutput,
        elapsed: Option<u64>,
    ) -> RunReport {
        if !matches!(output.status, RunStatus::Succeeded | RunStatus::Partial) {
            let message = format!(
                "provider reported {:?}; expected succeeded or partial",
                output.status
            );
            return self.fail(pr_id, running, reason::INVALID_OUTPUT, &message, elapsed);
        }
        if output.findings.len() > limits.max_findings {
            let message = format!(
                "{} findings exceed the limit of {}",
                output.findings.len(),
                limits.max_findings
            );
            return self.fail(
                pr_id,
                running,
                reason::OUTPUT_LIMIT_EXCEEDED,
                &message,
                elapsed,
            );
        }
        let mut done = running.clone();
        done.status = output.status;
        done.finished_at = Some(self.clock.now_rfc3339());
        done.coverage = output.coverage;
        done.warnings = output.warnings;
        done.metrics.duration_ms = elapsed;
        let result = Raw::new(done).and_then(|run| {
            let batch = AnalysisBatch {
                run,
                findings: output.findings,
                evidence: output.evidence,
                relations: output.relations,
            };
            lock(&self.store)?.commit_analysis_batch(&batch)
        });
        match result {
            Ok(outcome) => RunReport {
                run_id: running.id.clone(),
                provider_id: running.provider_id.clone(),
                snapshot_id: running.snapshot_id.clone(),
                stored_status: outcome.stored_status,
                reported_status: output.status,
                reason: None,
                write_error: None,
            },
            // The batch rolled back as a whole. Most likely the run was
            // cancelled in between (the store refuses its results) or the
            // output broke the contract.
            Err(error) => {
                let stored = lock(&self.store)
                    .and_then(|s| s.get_analysis_run(&running.id))
                    .map(|r| r.value().status);
                if let Ok(status) = stored
                    && status.is_terminal()
                {
                    return RunReport {
                        run_id: running.id.clone(),
                        provider_id: running.provider_id.clone(),
                        snapshot_id: running.snapshot_id.clone(),
                        stored_status: status,
                        reported_status: status,
                        reason: None,
                        write_error: Some(format!("{error:#}")),
                    };
                }
                self.fail(
                    pr_id,
                    running,
                    reason::INVALID_OUTPUT,
                    &format!("{error:#}"),
                    elapsed,
                )
            }
        }
    }

    fn cancelled(&self, pr_id: &str, run: &AnalysisRun, elapsed: Option<u64>) -> RunReport {
        let run = terminal(
            run,
            RunStatus::Cancelled,
            reason::CANCELLED,
            "cancelled on request",
            self.clock.now_rfc3339(),
            elapsed,
        );
        self.write(pr_id, run, reason::CANCELLED)
    }

    fn fail(
        &self,
        pr_id: &str,
        run: &AnalysisRun,
        code: &'static str,
        message: &str,
        elapsed: Option<u64>,
    ) -> RunReport {
        let run = terminal(
            run,
            RunStatus::Failed,
            code,
            message,
            self.clock.now_rfc3339(),
            elapsed,
        );
        self.write(pr_id, run, code)
    }

    /// Records a run state and reports what the store kept.
    fn write(&self, pr_id: &str, run: AnalysisRun, code: &str) -> RunReport {
        let reported_status = run.status;
        let result = Raw::new(run.clone())
            .and_then(|raw| lock(&self.store)?.record_analysis_run(pr_id, &raw));
        let (stored_status, write_error) = match result {
            Ok(outcome) => (outcome.stored_status, None),
            Err(error) => (reported_status, Some(format!("{error:#}"))),
        };
        RunReport {
            run_id: run.id,
            provider_id: run.provider_id,
            snapshot_id: run.snapshot_id,
            stored_status,
            reported_status,
            reason: (!code.is_empty()).then(|| code.to_owned()),
            write_error,
        }
    }
}

/// A run ended without output: coverage says why, and so does a warning.
fn terminal(
    run: &AnalysisRun,
    status: RunStatus,
    code: &str,
    message: &str,
    now: String,
    elapsed: Option<u64>,
) -> AnalysisRun {
    let mut run = run.clone();
    run.status = status;
    run.finished_at = Some(now);
    run.coverage = AnalysisCoverage {
        completeness: match status {
            RunStatus::Skipped => Completeness::Skipped,
            RunStatus::Failed => Completeness::Failed,
            _ => Completeness::NotRun,
        },
        reason_codes: vec![code.to_owned()],
        ..AnalysisCoverage::not_run()
    };
    run.warnings.push(RunWarning {
        code: code.to_owned(),
        message: message.to_owned(),
    });
    if elapsed.is_some() {
        run.metrics.duration_ms = elapsed;
    }
    run
}

fn next_attempt(runs: &[Raw<AnalysisRun>], snapshot: &SnapshotId, provider: &str) -> u32 {
    runs.iter()
        .map(Raw::value)
        .filter(|r| &r.snapshot_id == snapshot && r.provider_id == provider)
        .map(|r| r.attempt)
        .max()
        .map_or(1, |a| a + 1)
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "provider panicked".into())
}

fn lock<T>(mutex: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>> {
    mutex.lock().map_err(|_| anyhow!("analysis lock poisoned"))
}

#[cfg(test)]
#[path = "scheduler_tests.rs"]
mod tests;
