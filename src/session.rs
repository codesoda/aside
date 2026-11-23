//! Runtime session: durable PR snapshots plus derived scope and review state.

use std::sync::{
    Arc, Mutex, RwLock,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::sync::{Mutex as AsyncMutex, broadcast};

use crate::github::{LegacyKind, ThreadImportProblem};
use crate::{
    analysis::{
        self, AnalysisInput, AnalysisProvider, RunEvent, RunHandle, Scheduler, SchedulerConfig,
        SystemClock,
    },
    config::{self, AnalysisConfig},
    demo,
    diff::{self, Limits},
    github::{self, GhRunner, PrRef},
    model::{
        Acknowledgement, Actor, AnalysisCoverage, AnalysisKey, AnalysisMode, AnalysisRun,
        AnalysisSummary, AnchorState, ChangedFile, Completeness, CoverageEntry, CoverageScope,
        EventEnvelope, ExclusionRule, FINDINGS_INVALIDATED, FINDINGS_UPSERTED, FileState,
        FileStatus, GithubThread, Invalidation, KnownCount, LIFECYCLE_EVENT_VERSION, PrSnapshot,
        Progress, Repository, ReviewThread, RunKeyInput, RunStatus, RunWarning, SCHEMA_VERSION,
        ScopeOutcome, SessionView, SnapshotId, SourceCoverage, ThreadSource, classify, overall,
        run_event_type, stable_digest,
    },
    model::{NativeSourceKey, NativeSourceRef, SourceKind},
    rules,
    store::{ImportBatch, ImportedSourceWrite, ObservationLabel, Raw, Store},
    util::{new_id, now_rfc3339},
};

pub struct Session {
    pub id: String,
    pub demo: bool,
    pub pr_ref: Option<PrRef>,
    pub gh: Option<Arc<dyn GhRunner>>,
    pub store: Arc<Mutex<Store>>,
    /// Background analysis. Never holds the store lock across an await.
    pub analysis: Scheduler,
    pub snapshot: RwLock<PrSnapshot>,
    pub repository: Repository,
    pub pr_id: String,
    /// Shared with the analysis observer so run events use the same sequence.
    pub sequence: Arc<AtomicU64>,
    pub events: broadcast::Sender<EventEnvelope>,
    refresh_lock: AsyncMutex<()>,
    demo_version: AtomicUsize,
    /// What the current analysis results were computed for (#27).
    analysis_key: Mutex<Option<AnalysisKey>>,
    analysis_config_path: RwLock<Option<std::path::PathBuf>>,
    analysis_settings: RwLock<AnalysisConfig>,
    analysis_context: RwLock<Option<github::context::AnalysisContext>>,
    history_objects: RwLock<Option<github::context::HistoryObjects>>,
    history_generation: AtomicU64,
    history_config: RwLock<Option<(Option<std::path::PathBuf>, bool, std::path::PathBuf)>>,
}

/// Emits envelopes on the session's single SSE channel. Cloned into the
/// analysis observer; there is no second event transport.
#[derive(Clone)]
struct EventBus {
    session_id: String,
    sequence: Arc<AtomicU64>,
    events: broadcast::Sender<EventEnvelope>,
}

impl EventBus {
    fn notify(&self, event_type: &str, payload: Value) -> EventEnvelope {
        let event = EventEnvelope {
            schema_version: SCHEMA_VERSION,
            session_id: self.session_id.clone(),
            sequence: self.sequence.fetch_add(1, Ordering::SeqCst) + 1,
            event_type: event_type.to_owned(),
            timestamp: now_rfc3339(),
            payload,
        };
        let _ = self.events.send(event.clone());
        event
    }

    /// Small, versioned run delta: IDs and status only, never the snapshot.
    fn run_event(&self, event: RunEvent) {
        let label = if event.current {
            "current"
        } else {
            "historical"
        };
        if let Some(count) = event.findings {
            self.notify(
                FINDINGS_UPSERTED,
                serde_json::json!({
                    "eventVersion": LIFECYCLE_EVENT_VERSION,
                    "runId": event.run_id,
                    "providerId": event.provider_id,
                    "snapshotId": event.snapshot_id,
                    "count": count,
                    "label": label,
                }),
            );
            return;
        }
        self.notify(
            run_event_type(event.status),
            serde_json::json!({
                "eventVersion": LIFECYCLE_EVENT_VERSION,
                "runId": event.run_id,
                "providerId": event.provider_id,
                "snapshotId": event.snapshot_id,
                "status": event.status,
                "reportedStatus": event.reported_status,
                "reason": event.reason,
                "label": label,
            }),
        );
    }
}

impl Session {
    /// Persist the initial snapshot and imported threads before exposing the session.
    /// Demo mode is selected by passing neither `gh` nor `pr_ref`.
    pub fn new(
        snapshot: PrSnapshot,
        store: Store,
        gh: Option<Arc<dyn GhRunner>>,
        pr_ref: Option<PrRef>,
    ) -> Result<Arc<Self>> {
        Self::with_analysis(
            snapshot,
            store,
            gh,
            pr_ref,
            analysis::default_providers(),
            SchedulerConfig::default(),
        )
    }

    /// [`Self::new`] with explicit analysis providers.
    pub(crate) fn with_analysis(
        snapshot: PrSnapshot,
        mut store: Store,
        gh: Option<Arc<dyn GhRunner>>,
        pr_ref: Option<PrRef>,
        providers: Vec<Arc<dyn AnalysisProvider>>,
        config: SchedulerConfig,
    ) -> Result<Arc<Self>> {
        if gh.is_some() != pr_ref.is_some() {
            bail!("GitHub runner and PR reference must be provided together");
        }
        if let Some(reference) = &pr_ref
            && (reference.owner != snapshot.pr.owner
                || reference.repo != snapshot.pr.repo
                || reference.number != snapshot.pr.number)
        {
            bail!("PR reference does not match the snapshot");
        }
        let repository = store.resolve_repository(
            "github.com",
            &snapshot.pr.owner,
            &snapshot.pr.repo,
            snapshot.provider_repository_id.as_deref(),
        )?;
        let pr_id = store.upsert_pull_request(
            &repository.id,
            snapshot.pr.number,
            &snapshot.pr.title,
            &snapshot.pr.url,
        )?;
        store.save_snapshot(&pr_id, &snapshot)?;
        persist_checks(&mut store, &pr_id, &repository.id, &snapshot)?;
        store.sync_github_threads(
            &pr_id,
            &snapshot.pr.head_sha,
            &snapshot.github_threads,
            snapshot.github_threads_complete,
        )?;
        revalidate_anchors(&mut store, &pr_id, &snapshot)?;
        let demo_version =
            if pr_ref.is_none() && snapshot.pr.head_sha == demo::snapshot(1).pr.head_sha {
                1
            } else {
                0
            };
        let (events, _) = broadcast::channel(256);
        let store = Arc::new(Mutex::new(store));
        let analysis =
            Scheduler::new(Arc::clone(&store), providers, config, Arc::new(SystemClock))?;
        let bus = EventBus {
            session_id: new_id("session"),
            sequence: Arc::new(AtomicU64::new(0)),
            events,
        };
        let observer = bus.clone();
        analysis.observe(Arc::new(move |event| observer.run_event(event)));
        Ok(Arc::new(Self {
            id: bus.session_id,
            demo: pr_ref.is_none(),
            pr_ref,
            gh,
            store,
            analysis,
            snapshot: RwLock::new(snapshot),
            repository,
            pr_id,
            sequence: bus.sequence,
            events: bus.events,
            refresh_lock: AsyncMutex::new(()),
            demo_version: AtomicUsize::new(demo_version),
            analysis_key: Mutex::new(None),
            analysis_config_path: RwLock::new(None),
            analysis_settings: RwLock::new(AnalysisConfig::default()),
            analysis_context: RwLock::new(None),
            history_objects: RwLock::new(None),
            history_generation: AtomicU64::new(0),
            history_config: RwLock::new(None),
        }))
    }

    pub fn view(&self, actor: Actor) -> Result<SessionView> {
        let (snapshot, analysis_context) = {
            let snapshot = self.snapshot.read().expect("snapshot lock poisoned");
            let context = self.analysis_context.read().expect("context lock poisoned");
            (snapshot.clone(), context.clone())
        };
        let store = self.store.lock().expect("store lock poisoned");
        let rules = store.effective_rules(&self.repository.id, &self.pr_id)?;
        let mut threads = store.list_threads(&self.pr_id)?;
        attach_files(&mut threads, &snapshot.files);
        crate::publish::attach_draft_targets(&snapshot, &mut threads)?;
        let file_states = self.file_states(&store, &snapshot, &rules)?;
        let progress = progress(&file_states);
        let analysis = self.analysis_summary(&store, &snapshot, &rules)?;
        Ok(SessionView {
            schema_version: SCHEMA_VERSION,
            session_id: self.id.clone(),
            demo: self.demo,
            sequence: self.sequence.load(Ordering::SeqCst),
            actor,
            agent_presence: crate::model::AgentPresence::default(),
            repository: self.repository.clone(),
            pr: snapshot.pr,
            files: snapshot.files,
            file_states,
            progress,
            rules,
            threads,
            reviews: snapshot.reviews,
            issue_comments: snapshot.issue_comments,
            review: store.review_state(&self.pr_id)?,
            warnings: snapshot.warnings,
            complete: snapshot.complete,
            fetched_at: snapshot.fetched_at,
            analysis,
            analysis_context,
        })
    }

    /// Derived per-file review and scope state. Reads acknowledgements only;
    /// never writes them.
    fn file_states(
        &self,
        store: &Store,
        snapshot: &PrSnapshot,
        rules: &[ExclusionRule],
    ) -> Result<Vec<FileState>> {
        let acknowledgements = store.list_acknowledgements(&self.pr_id)?;
        Ok(snapshot
            .files
            .iter()
            .map(|file| {
                let scope = rules::evaluate(rules, &file.path);
                let (review_status, last_reviewed_at) = review_status(file, &acknowledgements);
                FileState {
                    file_id: file.id.clone(),
                    path: file.path.clone(),
                    status: if scope.ignored {
                        FileStatus::Ignored
                    } else {
                        review_status
                    },
                    review_status,
                    scope,
                    last_reviewed_at,
                }
            })
            .collect())
    }

    /// One consistent read of analysis state for the read-only API (#33): a
    /// single snapshot clone and a single store lock, so the items a page
    /// returns and the coverage it reports describe the same state.
    pub(crate) fn read_analysis<T>(
        &self,
        read: impl FnOnce(AnalysisRead<'_>) -> Result<T>,
    ) -> Result<T> {
        let snapshot = self
            .snapshot
            .read()
            .expect("snapshot lock poisoned")
            .clone();
        let store = self.store.lock().expect("store lock poisoned");
        let sequence = self.sequence.load(Ordering::SeqCst);
        let rules = store.effective_rules(&self.repository.id, &self.pr_id)?;
        let summary = self.analysis_summary(&store, &snapshot, &rules)?;
        let mut threads = store.list_threads(&self.pr_id)?;
        attach_files(&mut threads, &snapshot.files);
        let file_states = self.file_states(&store, &snapshot, &rules)?;
        read(AnalysisRead {
            store: &store,
            pr_id: &self.pr_id,
            session_id: &self.id,
            sequence,
            rules,
            summary,
            threads,
            file_states,
            github_threads: snapshot.github_threads,
            reviews: snapshot.reviews,
            issue_comments: snapshot.issue_comments,
            reviews_completeness: snapshot.reviews_completeness,
            issue_comments_completeness: snapshot.issue_comments_completeness,
            head_sha: snapshot.pr.head_sha,
            annotation_coverage: snapshot.checks.annotation_coverage,
        })
    }

    fn analysis_input(&self, snapshot: PrSnapshot) -> AnalysisInput {
        let canonical = snapshot
            .provider_repository_id
            .clone()
            .unwrap_or_else(|| self.repository.id.clone());
        let mut input = AnalysisInput::new(&self.pr_id, &canonical, snapshot);
        input.history_context = self.analysis_context.read().expect("context lock").clone();
        input.history_objects = self.history_objects.read().expect("history lock").clone();
        input.history_generation = self.history_generation.load(Ordering::SeqCst);
        input
    }

    /// Per-source completeness and finding counts for the canonical snapshot.
    /// Derived on every read from durable state, so it is what a reconnecting
    /// client converges to. Exclusion rules only affect presentation here.
    fn analysis_summary(
        &self,
        store: &Store,
        snapshot: &PrSnapshot,
        rules: &[ExclusionRule],
    ) -> Result<AnalysisSummary> {
        let snapshot_id = self.analysis_input(snapshot.clone()).snapshot_id;
        let mut sources: Vec<SourceCoverage> = import_sources(snapshot)
            .into_iter()
            .map(|source| source.coverage(&snapshot_id))
            .collect();
        let runs = store.list_analysis_runs(&self.pr_id)?;
        for provider in self.analysis.provider_ids() {
            let notice = self.analysis.capability_notice(&provider);
            // Latest attempt on the current snapshot; earlier attempts and
            // other snapshots never stand in for it.
            let latest = runs
                .iter()
                .map(Raw::value)
                .filter(|r| r.snapshot_id == snapshot_id && r.provider_id == provider)
                .max_by_key(|r| r.attempt);
            sources.push(match latest {
                None => SourceCoverage {
                    source_id: provider,
                    mode: AnalysisMode::Static,
                    snapshot_id: snapshot_id.clone(),
                    run_id: None,
                    status: None,
                    completeness: Completeness::NotRun,
                    items: KnownCount::Unknown,
                    item_kind: "findings".into(),
                    state: classify(None, Completeness::NotRun, KnownCount::Unknown),
                    reason_codes: vec!["not_run".into()],
                    limitations: vec![notice],
                },
                Some(run) => {
                    let items = if matches!(run.status, RunStatus::Succeeded | RunStatus::Partial) {
                        KnownCount::Known {
                            value: store.count_run_findings(&run.id)?,
                        }
                    } else {
                        KnownCount::Unknown
                    };
                    SourceCoverage {
                        source_id: provider,
                        mode: run.mode,
                        snapshot_id: snapshot_id.clone(),
                        run_id: Some(run.id.clone()),
                        status: Some(run.status),
                        completeness: run.coverage.completeness,
                        items,
                        item_kind: "findings".into(),
                        state: classify(Some(run.status), run.coverage.completeness, items),
                        reason_codes: run.coverage.reason_codes.clone(),
                        limitations: std::iter::once(notice)
                            .chain(run.warnings.iter().map(|w| w.message.clone()))
                            .collect(),
                    }
                }
            });
        }
        let mut summary = AnalysisSummary {
            event_version: LIFECYCLE_EVENT_VERSION,
            snapshot_id,
            overall: overall(&sources),
            sources,
            current_findings: 0,
            historical_findings: 0,
            current_findings_excluded: 0,
        };
        for finding in store.list_findings(&self.pr_id, true)? {
            match finding.label {
                ObservationLabel::Historical => summary.historical_findings += 1,
                ObservationLabel::Current => {
                    summary.current_findings += 1;
                    let path = finding.finding.value().anchor.current_path.as_deref();
                    if path.is_some_and(|p| rules::evaluate(rules, p).ignored) {
                        summary.current_findings_excluded += 1;
                    }
                }
            }
        }
        Ok(summary)
    }

    /// Called before startup analysis/history resolution. Configuration stays
    /// outside PR data and is reloaded only during explicit session refresh.
    pub fn configure_analysis(&self, path: std::path::PathBuf) -> Result<()> {
        if self
            .analysis_key
            .lock()
            .expect("analysis key lock")
            .is_some()
        {
            bail!("configure analysis before starting the session; use refresh to reload");
        }
        let settings = config::load_from(&path)?.analysis_for(&self.repository_config_key())?;
        *self.analysis_settings.write().expect("settings lock") = settings;
        *self.analysis_config_path.write().expect("config path lock") = Some(path);
        Ok(())
    }

    fn repository_config_key(&self) -> String {
        format!(
            "{}/{}/{}",
            self.repository.host, self.repository.owner, self.repository.name
        )
    }

    fn reload_analysis_settings(&self) -> Result<AnalysisConfig> {
        match self
            .analysis_config_path
            .read()
            .expect("config path lock")
            .as_ref()
        {
            Some(path) => config::load_from(path)?.analysis_for(&self.repository_config_key()),
            None => Ok(self
                .analysis_settings
                .read()
                .expect("settings lock")
                .clone()),
        }
    }

    /// Pins analysis to the current PR snapshot and starts providers in the
    /// background. Returns at once; the Review screen never waits for them.
    /// Must be called inside a Tokio runtime.
    ///
    /// Emits `analysis.findings.invalidated` when the head, base or analysis
    /// configuration changed since the results last made current.
    pub fn start_analysis(&self) -> Result<Vec<RunHandle>> {
        let (handles, invalidation) = self.begin_analysis()?;
        if let Some(invalidation) = invalidation {
            self.notify(FINDINGS_INVALIDATED, invalidation_payload(&invalidation));
        }
        Ok(handles)
    }

    /// Records the canonical snapshot's import sources, flips the current
    /// snapshot pointer and starts providers that have not run on this exact
    /// input. Returns the invalidation (if any) for the caller to announce.
    ///
    /// Head, base and provider configuration are the only invalidation keys;
    /// exclusion rules are not, so editing them never reruns engines.
    fn begin_analysis(&self) -> Result<(Vec<RunHandle>, Option<Invalidation>)> {
        let snapshot = self
            .snapshot
            .read()
            .expect("snapshot lock poisoned")
            .clone();
        let input = self.analysis_input(snapshot.clone());
        let mut next = AnalysisKey {
            snapshot_id: input.snapshot_id.clone(),
            base_sha: input.base_sha.clone(),
            head_sha: input.head_sha.clone(),
            config_digest: Some(self.analysis.config_digest()),
        };
        let previous = match self.analysis_key.lock().expect("analysis key lock").clone() {
            Some(key) => Some(key),
            // First start in this process: compare with what a previous
            // process left current. Its configuration is not known.
            None => {
                let store = self.store.lock().expect("store lock poisoned");
                match store.current_analysis_snapshot(&self.pr_id)? {
                    Some(id) => store
                        .analysis_snapshot_shas(&id)?
                        .map(|(base_sha, head_sha)| AnalysisKey {
                            snapshot_id: id,
                            base_sha,
                            head_sha,
                            config_digest: None,
                        }),
                    None => None,
                }
            }
        };
        let policies = self
            .analysis_settings
            .read()
            .expect("settings lock")
            .policies();
        let handles = self
            .analysis
            .begin_snapshot_configured(input, Some(policies))?;
        next.config_digest = Some(self.analysis.config_digest());
        {
            let mut store = self.store.lock().expect("store lock poisoned");
            record_import_sources(&mut store, &self.pr_id, &next.snapshot_id, &snapshot)?;
        }
        let invalidation = previous.and_then(|previous| previous.invalidation(&next));
        *self.analysis_key.lock().expect("analysis key lock") = Some(next);
        Ok((handles, invalidation))
    }

    #[cfg(test)]
    pub fn set_analysis_context(&self, context: github::context::AnalysisContext) {
        *self
            .analysis_context
            .write()
            .expect("context lock poisoned") = Some(context);
        *self.history_objects.write().expect("history lock") = None;
        self.history_generation.fetch_add(1, Ordering::SeqCst);
    }

    /// Resolve and retain immutable objects for this session. The settings
    /// also permit a revision refresh to resolve its new pinned commits.
    pub fn configure_history(
        &self,
        checkout: Option<std::path::PathBuf>,
        fetch: bool,
        cache: std::path::PathBuf,
    ) {
        *self.history_config.write().expect("history config lock") = Some((checkout, fetch, cache));
        self.resolve_history();
    }

    fn resolve_history(&self) {
        let settings = self.analysis_settings.read().expect("settings lock");
        if !settings.history.enabled && !settings.structure.enabled {
            return;
        }
        let Some((checkout, fetch, cache)) = self
            .history_config
            .read()
            .expect("history config lock")
            .clone()
        else {
            return;
        };
        let (Some(gh), Some(reference)) = (&self.gh, &self.pr_ref) else {
            return;
        };
        let snapshot = self.snapshot.read().expect("snapshot lock").clone();
        let (context, objects) = github::context::resolve_owned(
            gh.as_ref(),
            reference,
            snapshot.provider_repository_id.as_deref(),
            &snapshot.pr.base_sha,
            &snapshot.pr.head_sha,
            checkout.as_deref(),
            fetch,
            &cache,
        );
        *self.analysis_context.write().expect("context lock") = Some(context);
        *self.history_objects.write().expect("history lock") = objects;
        self.history_generation.fetch_add(1, Ordering::SeqCst);
    }

    /// Replace context for the current revision without rerunning diff facts.
    /// A terminal unavailable/read-failed history run is explicitly retried
    /// even when the recovered context has the same metadata digest.
    #[cfg(test)]
    async fn replace_analysis_context(
        &self,
        context: github::context::AnalysisContext,
        objects: Option<github::context::HistoryObjects>,
    ) -> Result<Vec<RunHandle>> {
        let snapshot = self.snapshot.read().expect("snapshot lock").clone();
        if context.base_sha != snapshot.pr.base_sha || context.head_sha != snapshot.pr.head_sha {
            bail!("history context does not match the captured revision");
        }
        let id = self.analysis_input(snapshot).snapshot_id;
        self.invalidate_context_providers_for_snapshot(&id)?;
        *self.analysis_context.write().expect("context lock") = Some(context);
        *self.history_objects.write().expect("history lock") = objects;
        self.history_generation.fetch_add(1, Ordering::SeqCst);
        let (mut handles, _) = self.begin_analysis()?;
        if !handles.iter().any(|h| h.provider_id == "aside.git-history") {
            handles.push(self.analysis.rerun(&id, "aside.git-history")?);
        }
        Ok(handles)
    }

    fn invalidate_context_providers_for_snapshot(&self, id: &SnapshotId) -> Result<()> {
        // Active attempts are superseded by the scheduler at the new digest;
        // do not wait for old work before starting the replacement.
        let mut store = self.store.lock().expect("store lock");
        for raw in store.list_analysis_runs(&self.pr_id)? {
            let run = raw.value();
            if &run.snapshot_id == id
                && matches!(
                    run.provider_id.as_str(),
                    "aside.git-history" | "aside.structure"
                )
                && matches!(run.status, RunStatus::Succeeded | RunStatus::Partial)
            {
                let mut historical = run.clone();
                historical.status = RunStatus::Stale;
                store.record_analysis_run(&self.pr_id, &Raw::new(historical)?)?;
            }
        }
        Ok(())
    }

    /// Emit an event only after the corresponding mutation has committed.
    pub fn notify(&self, event_type: &str, payload: Value) -> EventEnvelope {
        EventBus {
            session_id: self.id.clone(),
            sequence: Arc::clone(&self.sequence),
            events: self.events.clone(),
        }
        .notify(event_type, payload)
    }

    fn context_retry_needed(&self, snapshot: &PrSnapshot, provider_id: &str) -> Result<bool> {
        let id = self.analysis_input(snapshot.clone()).snapshot_id;
        let store = self.store.lock().expect("store lock");
        let runs = store.list_analysis_runs(&self.pr_id)?;
        let latest = runs
            .iter()
            .map(Raw::value)
            .filter(|r| r.snapshot_id == id && r.provider_id == provider_id)
            .max_by_key(|r| r.attempt);
        Ok(latest.is_some_and(|run| {
            (run.status == RunStatus::Failed
                && run.coverage.reason_codes.iter().any(|code| {
                    matches!(
                        code.as_str(),
                        "provider_error" | "time_limit_exceeded" | "interrupted"
                    )
                }))
                || (run.status == RunStatus::Partial
                    && run.coverage.reason_codes.iter().any(|code| {
                        matches!(
                            code.as_str(),
                            "history_read_incomplete"
                                | "history_objects_unavailable"
                                | "history_context_unavailable"
                                | "comparison_base_unavailable"
                                | "history_context_stale"
                                | "history_deadline"
                                | "history_cancelled_or_deadline"
                                | "structure_context_unavailable"
                                | "structure_objects_unavailable"
                                | "structure_base_unavailable"
                                | "structure_context_stale"
                                | "structure_head_tree_unavailable"
                                | "structure_base_tree_unavailable"
                                | "structure_object_read_failed"
                                | "structure_deadline"
                                | "structure_cancelled_or_deadline"
                        )
                    }))
        }))
    }

    /// Fetch outside SQLite locks, then persist before replacing the in-memory snapshot.
    /// On failure the previous snapshot stays available and no refresh event is emitted.
    /// Demo mode alternates deterministic versions 0 and 1.
    pub async fn refresh(&self) -> Result<SessionView> {
        let _refresh = self.refresh_lock.lock().await;
        // Fail before network calls or state changes on malformed local config.
        let settings = self.reload_analysis_settings()?;
        let (snapshot, next_demo_version) = if self.demo {
            let next = (self.demo_version.load(Ordering::SeqCst) + 1) % 2;
            (demo::snapshot(next), Some(next))
        } else {
            let gh = Arc::clone(self.gh.as_ref().context("GitHub runner unavailable")?);
            let reference = self.pr_ref.clone().context("PR reference unavailable")?;
            let snapshot = tokio::task::spawn_blocking(move || {
                github::fetch_snapshot(gh.as_ref(), &reference, &Limits::default())
            })
            .await
            .context("GitHub refresh task failed")??;
            (snapshot, None)
        };
        let prior_context = self
            .analysis_context
            .read()
            .expect("context lock poisoned")
            .clone();
        let changed_revision = prior_context.as_ref().is_some_and(|context| {
            context.base_sha != snapshot.pr.base_sha || context.head_sha != snapshot.pr.head_sha
        });
        let context_enabled = settings.history.enabled || settings.structure.enabled;
        let needs_resolution = context_enabled
            && self
                .history_config
                .read()
                .expect("history config lock")
                .is_some()
            && (changed_revision
                || self.history_objects.read().expect("history lock").is_none()
                || (settings.history.enabled
                    && self.context_retry_needed(&snapshot, "aside.git-history")?)
                || (settings.structure.enabled
                    && self.context_retry_needed(&snapshot, "aside.structure")?));
        let resolved = if needs_resolution {
            let config = self
                .history_config
                .read()
                .expect("history config lock")
                .clone();
            if let (Some((checkout, fetch, cache)), Some(gh), Some(reference)) =
                (config, self.gh.clone(), self.pr_ref.clone())
            {
                let snapshot_for_context = snapshot.clone();
                Some(
                    tokio::task::spawn_blocking(move || {
                        github::context::resolve_owned(
                            gh.as_ref(),
                            &reference,
                            snapshot_for_context.provider_repository_id.as_deref(),
                            &snapshot_for_context.pr.base_sha,
                            &snapshot_for_context.pr.head_sha,
                            checkout.as_deref(),
                            fetch,
                            &cache,
                        )
                    })
                    .await
                    .context("history context resolution task failed")?,
                )
            } else {
                None
            }
        } else {
            None
        };
        let (replacement, replacement_objects) = if !context_enabled {
            (None, prior_context.is_some().then_some(None))
        } else {
            match resolved {
                Some((context, objects)) => (Some(context), Some(objects)),
                None => (
                    prior_context.map(|context| {
                        if changed_revision {
                            github::context::AnalysisContext::unavailable(
                                &snapshot.pr.base_sha,
                                &snapshot.pr.head_sha,
                                "PR revision changed; analysis context requires resolution again",
                            )
                        } else {
                            context
                        }
                    }),
                    changed_revision.then_some(None),
                ),
            }
        };
        let previous_head = self
            .snapshot
            .read()
            .expect("snapshot lock poisoned")
            .pr
            .head_sha
            .clone();
        {
            let mut store = self.store.lock().expect("store lock poisoned");
            store.upsert_pull_request(
                &self.repository.id,
                snapshot.pr.number,
                &snapshot.pr.title,
                &snapshot.pr.url,
            )?;
            store.save_snapshot(&self.pr_id, &snapshot)?;
            persist_checks(&mut store, &self.pr_id, &self.repository.id, &snapshot)?;
            store.sync_github_threads(
                &self.pr_id,
                &snapshot.pr.head_sha,
                &snapshot.github_threads,
                snapshot.github_threads_complete,
            )?;
            revalidate_anchors(&mut store, &self.pr_id, &snapshot)?;
            if previous_head != snapshot.pr.head_sha {
                store.reopen_review(&self.pr_id)?;
            }
        }
        // Publish both views together only after all persistent writes succeed.
        // Readers acquire snapshot before context (see view), so hold that lock
        // while replacing the paired context.
        {
            let mut current = self.snapshot.write().expect("snapshot lock poisoned");
            *self
                .analysis_context
                .write()
                .expect("context lock poisoned") = replacement;
            if let Some(objects) = replacement_objects {
                *self.history_objects.write().expect("history lock") = objects;
                self.history_generation.fetch_add(1, Ordering::SeqCst);
            }
            *current = snapshot;
        }
        if let Some(next) = next_demo_version {
            self.demo_version.store(next, Ordering::SeqCst);
        }
        if needs_resolution && !changed_revision {
            let id = self
                .analysis_input(self.snapshot.read().expect("snapshot lock").clone())
                .snapshot_id;
            self.invalidate_context_providers_for_snapshot(&id)?;
        }
        *self.analysis_settings.write().expect("settings lock") = settings.clone();
        let invalidation = match self.begin_analysis() {
            Ok((handles, invalidation)) => {
                if needs_resolution {
                    let id = self
                        .analysis_input(self.snapshot.read().expect("snapshot lock").clone())
                        .snapshot_id;
                    for (enabled, provider_id) in [
                        (settings.history.enabled, "aside.git-history"),
                        (settings.structure.enabled, "aside.structure"),
                    ] {
                        if enabled
                            && self
                                .analysis
                                .provider_ids()
                                .iter()
                                .any(|id| id == provider_id)
                            && !handles.iter().any(|h| h.provider_id == provider_id)
                            && let Err(error) = self.analysis.rerun(&id, provider_id)
                        {
                            eprintln!("could not retry {provider_id} analysis: {error:#}");
                        }
                    }
                }
                invalidation
            }
            Err(error) => {
                // Analysis is advisory; a failure here must not fail the refresh.
                eprintln!("could not start analysis: {error:#}");
                None
            }
        };
        // One small event: what changed and which sources are now incomplete.
        // Clients resync the authoritative view rather than patching state.
        let (head_sha, sources) = {
            let snapshot = self.snapshot.read().expect("snapshot lock poisoned");
            let sources: Vec<Value> = import_sources(&snapshot)
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "sourceId": s.source_id,
                        "completeness": s.completeness,
                    })
                })
                .collect();
            (snapshot.pr.head_sha.clone(), sources)
        };
        self.notify(
            "pr.refreshed",
            serde_json::json!({
                "eventVersion": LIFECYCLE_EVENT_VERSION,
                "headSha": head_sha,
                "invalidated": invalidation.as_ref().map(invalidation_payload),
                "sources": sources,
            }),
        );
        self.view(Actor::Human)
    }
}

fn invalidation_payload(invalidation: &Invalidation) -> Value {
    serde_json::json!({
        "eventVersion": LIFECYCLE_EVENT_VERSION,
        "previousSnapshotId": invalidation.previous_snapshot_id,
        "snapshotId": invalidation.snapshot_id,
        "reasons": invalidation.reasons,
    })
}

/// Import-source IDs. PR ingestion and GitHub feedback are sources alongside
/// analysis providers, so one view says what was and was not covered.
/// State handed to [`Session::read_analysis`]. Everything is scoped to the
/// session's PR; nothing here reaches another PR's records.
pub(crate) struct AnalysisRead<'a> {
    pub store: &'a Store,
    pub pr_id: &'a str,
    pub sequence: u64,
    pub rules: Vec<ExclusionRule>,
    pub summary: AnalysisSummary,
    /// Every thread of the PR with current file IDs attached.
    pub threads: Vec<ReviewThread>,
    pub session_id: &'a str,
    /// Derived review/scope state of every changed file (read-only).
    pub file_states: Vec<FileState>,
    /// Native imported GitHub threads of the canonical snapshot.
    pub github_threads: Vec<GithubThread>,
    pub reviews: Vec<crate::model::ImportedReview>,
    pub issue_comments: Vec<crate::model::IssueComment>,
    pub reviews_completeness: crate::model::Completeness,
    pub issue_comments_completeness: crate::model::Completeness,
    pub head_sha: String,
    pub annotation_coverage: Vec<(u64, crate::github::checks::Coverage)>,
}

pub mod source {
    pub const PR_DIFF: &str = "github.pr_diff";
    pub const REVIEW_THREADS: &str = "github.review_threads";
    pub const REVIEWS_AND_COMMENTS: &str = "github.reviews_and_comments";
    pub const CHECK_RUNS: &str = "github.check_runs";
    pub const CHECK_ANNOTATIONS: &str = "github.check_annotations";
    pub const COMMIT_STATUSES: &str = "github.commit_statuses";
}

/// Reason codes for import sources (analyze.md §13.4, lower-cased like the
/// scheduler's).
pub mod source_reason {
    pub const PR_DIFF_INCOMPLETE: &str = "pr_diff_incomplete";
    pub const GITHUB_SOURCE_PARTIAL: &str = "github_source_partial";
    pub const GITHUB_SOURCE_FAILED: &str = "github_source_failed";
    pub const GITHUB_PERMISSION_DENIED: &str = "github_permission_denied";
    pub const PAGE_LIMIT_REACHED: &str = "page_limit_reached";
    pub const COMPLETENESS_UNKNOWN: &str = "completeness_unknown";
    pub const PAGINATION_UNVERIFIED: &str = "pagination_unverified";
}

/// How completely one import source was fetched for a snapshot.
#[derive(Debug, Clone)]
struct ImportSource {
    source_id: &'static str,
    status: RunStatus,
    completeness: Completeness,
    items: KnownCount,
    item_kind: &'static str,
    reason_codes: Vec<String>,
    limitations: Vec<String>,
}

impl ImportSource {
    fn coverage(&self, snapshot_id: &SnapshotId) -> SourceCoverage {
        SourceCoverage {
            source_id: self.source_id.into(),
            mode: AnalysisMode::GithubImport,
            snapshot_id: snapshot_id.clone(),
            run_id: None,
            status: Some(self.status),
            completeness: self.completeness,
            items: self.items,
            item_kind: self.item_kind.into(),
            state: classify(Some(self.status), self.completeness, self.items),
            reason_codes: self.reason_codes.clone(),
            limitations: self.limitations.clone(),
        }
    }
}

/// Completeness of every import source in a fetched snapshot.
fn import_sources(snapshot: &PrSnapshot) -> Vec<ImportSource> {
    let mut out = Vec::new();

    let incomplete: Vec<&str> = snapshot
        .files
        .iter()
        .filter(|f| !f.patch_complete)
        .map(|f| f.path.as_str())
        .collect();
    let diff_complete = snapshot.complete && incomplete.is_empty();
    let mut limitations: Vec<String> = incomplete
        .iter()
        .take(20)
        .map(|path| format!("patch for {path} is incomplete"))
        .collect();
    if incomplete.len() > 20 {
        limitations.push(format!("{} more incomplete patches", incomplete.len() - 20));
    }
    if !snapshot.complete && incomplete.is_empty() {
        limitations.push("some changed files could not be fetched".into());
    }
    out.push(ImportSource {
        source_id: source::PR_DIFF,
        status: if diff_complete {
            RunStatus::Succeeded
        } else {
            RunStatus::Partial
        },
        completeness: if diff_complete {
            Completeness::Complete
        } else {
            Completeness::Partial
        },
        items: KnownCount::Known {
            value: snapshot.files.len() as u32,
        },
        item_kind: "files",
        reason_codes: if diff_complete {
            Vec::new()
        } else {
            vec![source_reason::PR_DIFF_INCOMPLETE.into()]
        },
        limitations,
    });

    let threads = snapshot.github_threads.len() as u32;
    let problems = &snapshot.github_threads_problems;
    let (status, items, mut codes) = match snapshot.github_threads_completeness {
        Completeness::Complete => (
            RunStatus::Succeeded,
            KnownCount::Known { value: threads },
            vec![],
        ),
        Completeness::Partial => (
            RunStatus::Partial,
            KnownCount::Known { value: threads },
            vec![source_reason::GITHUB_SOURCE_PARTIAL.to_string()],
        ),
        Completeness::Failed => (
            RunStatus::Failed,
            KnownCount::Unknown,
            vec![source_reason::GITHUB_SOURCE_FAILED.to_string()],
        ),
        // Snapshots stored before #23 recorded completeness, or anything else.
        _ => (
            RunStatus::Partial,
            KnownCount::Known { value: threads },
            vec![source_reason::COMPLETENESS_UNKNOWN.to_string()],
        ),
    };
    // Each extra code is added at most once, permission before page limit.
    let extra: Vec<&str> = problems.iter().filter_map(thread_problem_reason).collect();
    for code in [
        source_reason::GITHUB_PERMISSION_DENIED,
        source_reason::PAGE_LIMIT_REACHED,
    ] {
        if extra.contains(&code) {
            codes.push(code.into());
        }
    }
    out.push(ImportSource {
        source_id: source::REVIEW_THREADS,
        status,
        completeness: match snapshot.github_threads_completeness {
            c @ (Completeness::Complete | Completeness::Partial | Completeness::Failed) => c,
            _ => Completeness::Unknown,
        },
        items,
        item_kind: "threads",
        reason_codes: codes,
        limitations: problems.iter().map(ToString::to_string).collect(),
    });

    // The combined source is complete only when both independent connections
    // are complete. A failed connection takes precedence over a partial one.
    let feedback_completeness = match (
        snapshot.reviews_completeness,
        snapshot.issue_comments_completeness,
    ) {
        (Completeness::Failed, _) | (_, Completeness::Failed) => Completeness::Failed,
        (Completeness::Complete, Completeness::Complete) => Completeness::Complete,
        (Completeness::Partial, _) | (_, Completeness::Partial) => Completeness::Partial,
        _ => Completeness::Unknown,
    };
    let feedback_status = match feedback_completeness {
        Completeness::Complete => RunStatus::Succeeded,
        Completeness::Failed => RunStatus::Failed,
        _ => RunStatus::Partial,
    };
    let mut feedback_codes = Vec::new();
    let mut feedback_limitations = Vec::new();
    for (name, completeness) in [
        ("reviews", snapshot.reviews_completeness),
        ("PR comments", snapshot.issue_comments_completeness),
    ] {
        if completeness != Completeness::Complete {
            feedback_limitations.push(format!(
                "{name} import is {completeness:?}; coverage is not complete"
            ));
        }
    }
    feedback_limitations.extend(
        snapshot
            .reviews_problems
            .iter()
            .map(|p| format!("reviews: {p}")),
    );
    feedback_limitations.extend(
        snapshot
            .issue_comments_problems
            .iter()
            .map(|p| format!("PR comments: {p}")),
    );
    match feedback_completeness {
        Completeness::Failed => feedback_codes.push(source_reason::GITHUB_SOURCE_FAILED.into()),
        Completeness::Partial => feedback_codes.push(source_reason::GITHUB_SOURCE_PARTIAL.into()),
        Completeness::Unknown => {
            feedback_codes.push(source_reason::COMPLETENESS_UNKNOWN.into());
            feedback_codes.push(source_reason::PAGINATION_UNVERIFIED.into());
        }
        _ => {}
    }
    out.push(ImportSource {
        source_id: source::REVIEWS_AND_COMMENTS,
        status: feedback_status,
        completeness: feedback_completeness,
        items: if feedback_completeness == Completeness::Failed {
            KnownCount::Unknown
        } else {
            KnownCount::Known {
                value: (snapshot.reviews.len() + snapshot.issue_comments.len()) as u32,
            }
        },
        item_kind: "reviews_and_comments",
        reason_codes: feedback_codes,
        limitations: feedback_limitations,
    });
    for (source_id, item_kind, count, coverage) in [
        (
            source::CHECK_RUNS,
            "check_runs",
            snapshot.checks.runs.len(),
            &snapshot.checks.runs_coverage,
        ),
        (
            source::COMMIT_STATUSES,
            "commit_statuses",
            snapshot.checks.statuses.len(),
            &snapshot.checks.statuses_coverage,
        ),
    ] {
        out.push(check_source(source_id, item_kind, count, coverage));
    }
    let mut annotation_coverage = snapshot.checks.runs_coverage.clone();
    for (id, coverage) in &snapshot.checks.annotation_coverage {
        if coverage.completeness != Completeness::Complete {
            annotation_coverage.completeness =
                match (annotation_coverage.completeness, coverage.completeness) {
                    (Completeness::Failed, _) | (_, Completeness::Failed) => Completeness::Failed,
                    _ => Completeness::Partial,
                };
        }
        annotation_coverage.reasons.extend(
            coverage
                .reasons
                .iter()
                .map(|reason| format!("run {id}: {reason}")),
        );
    }
    if annotation_coverage.completeness == Completeness::Failed
        && !snapshot.checks.annotations.is_empty()
    {
        annotation_coverage.completeness = Completeness::Partial;
    }
    out.push(check_source(
        source::CHECK_ANNOTATIONS,
        "check_annotations",
        snapshot.checks.annotations.len(),
        &annotation_coverage,
    ));
    out
}

fn check_source(
    source_id: &'static str,
    item_kind: &'static str,
    count: usize,
    coverage: &crate::github::checks::Coverage,
) -> ImportSource {
    ImportSource {
        source_id,
        item_kind,
        completeness: coverage.completeness,
        status: match coverage.completeness {
            Completeness::Complete => RunStatus::Succeeded,
            Completeness::Failed => RunStatus::Failed,
            _ => RunStatus::Partial,
        },
        items: if coverage.completeness == Completeness::Failed {
            KnownCount::Unknown
        } else {
            KnownCount::Known {
                value: count as u32,
            }
        },
        reason_codes: coverage.reasons.clone(),
        limitations: coverage.reasons.clone(),
    }
}

/// The source-specific reason code a thread import problem adds on top of the
/// completeness code, if any. Exhaustive on purpose: a new problem kind must
/// make a decision here.
fn thread_problem_reason(problem: &ThreadImportProblem) -> Option<&'static str> {
    use ThreadImportProblem as P;
    match problem {
        P::PermissionDenied { .. }
        | P::Legacy {
            legacy_kind: LegacyKind::PermissionDenied,
            ..
        } => Some(source_reason::GITHUB_PERMISSION_DENIED),
        P::PageLimitReached { .. }
        | P::Legacy {
            legacy_kind: LegacyKind::PageLimitReached,
            ..
        } => Some(source_reason::PAGE_LIMIT_REACHED),
        P::GraphqlError { .. }
        | P::PageFetchFailed { .. }
        | P::NoData { .. }
        | P::MissingThreadsConnection
        | P::MissingCommentsConnection { .. }
        | P::UnreadableThread
        | P::UnreadableReply { .. }
        | P::MissingPageInfo { .. }
        | P::MissingCursor { .. }
        | P::StalledCursor { .. }
        | P::Legacy {
            legacy_kind: LegacyKind::Other,
            ..
        } => None,
    }
}

fn persist_checks(
    store: &mut Store,
    pr_id: &str,
    repository_id: &str,
    snapshot: &PrSnapshot,
) -> Result<()> {
    let checks = &snapshot.checks;
    if checks.sha.is_empty() {
        return Ok(());
    }
    let native = |kind, key, url| {
        Raw::new(NativeSourceRef {
            host: "github.com".into(),
            repository_id: repository_id.into(),
            source_kind: kind,
            key,
            external_url: url,
        })
    };
    let runs = checks
        .runs
        .iter()
        .map(|r| {
            Ok(ImportedSourceWrite {
                native: native(
                    SourceKind::GithubCheckRun,
                    NativeSourceKey::Upstream(r.github_check_run_id.to_string()),
                    r.details_url.clone(),
                )?,
                raw_json: serde_json::to_string(r)?,
                aliases: vec![],
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let annotations = checks
        .annotations
        .iter()
        .map(|a| {
            Ok(ImportedSourceWrite {
                native: native(SourceKind::GithubCheckAnnotation, a.key.clone(), None)?,
                raw_json: serde_json::to_string(a)?,
                aliases: vec![],
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let statuses = checks
        .statuses
        .iter()
        .map(|s| {
            Ok(ImportedSourceWrite {
                native: native(
                    SourceKind::GithubCommitStatus,
                    NativeSourceKey::Upstream(s.github_status_id.to_string()),
                    s.target_url.clone(),
                )?,
                raw_json: serde_json::to_string(s)?,
                aliases: vec![],
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let annotations_complete = checks.runs_coverage.completeness == Completeness::Complete
        && checks
            .annotation_coverage
            .iter()
            .all(|(_, c)| c.completeness == Completeness::Complete);
    for (kind, complete, records) in [
        (
            "check_runs",
            checks.runs_coverage.completeness == Completeness::Complete,
            runs,
        ),
        ("check_annotations", annotations_complete, annotations),
        (
            "commit_statuses",
            checks.statuses_coverage.completeness == Completeness::Complete,
            statuses,
        ),
    ] {
        store.commit_import_batch(
            pr_id,
            &ImportBatch {
                resource_kind: format!("{kind}:{}", checks.sha),
                complete,
                observed_at: snapshot.fetched_at.clone(),
                records,
            },
        )?;
    }
    Ok(())
}

/// Persists each import source as a terminal `github_import` run on the
/// snapshot, so per-source completeness and limitations are durable next to
/// provider runs (stored verbatim through [`Raw`], #18). A re-import with the
/// same result is not recorded twice; a changed one is a new attempt.
fn record_import_sources(
    store: &mut Store,
    pr_id: &str,
    snapshot_id: &SnapshotId,
    snapshot: &PrSnapshot,
) -> Result<()> {
    let existing = store.list_analysis_runs(pr_id)?;
    for source in import_sources(snapshot) {
        let completeness = serde_json::to_string(&source.completeness)?;
        let items = serde_json::to_string(&source.items)?;
        let digest = stable_digest(
            "aside-import-source/v1",
            &[
                Some(snapshot_id.as_str()),
                Some(&snapshot.fetched_at),
                Some(&completeness),
                Some(&items),
            ],
        );
        let same =
            |r: &&AnalysisRun| r.snapshot_id == *snapshot_id && r.provider_id == source.source_id;
        if existing
            .iter()
            .map(Raw::value)
            .filter(same)
            .any(|r| r.input_digest == digest)
        {
            continue;
        }
        let attempt = existing
            .iter()
            .map(Raw::value)
            .filter(same)
            .map(|r| r.attempt)
            .max()
            .map_or(1, |a| a + 1);
        let mut run = AnalysisRun::queued(
            &RunKeyInput {
                snapshot_id,
                provider_id: source.source_id,
                engine_version: "1",
                configuration_digest: "",
                input_digest: &digest,
                attempt,
            },
            AnalysisMode::GithubImport,
        );
        run.status = source.status;
        run.started_at = Some(snapshot.fetched_at.clone());
        run.finished_at = Some(snapshot.fetched_at.clone());
        run.coverage = AnalysisCoverage {
            completeness: source.completeness,
            entries: match source.items {
                KnownCount::Known { value } => vec![CoverageEntry {
                    scope: CoverageScope::Pr,
                    rule_id: Some(source.item_kind.into()),
                    outcome: ScopeOutcome::Analyzed { findings: value },
                }],
                KnownCount::Unknown => Vec::new(),
            },
            reason_codes: source.reason_codes.clone(),
            ..AnalysisCoverage::not_run()
        };
        run.warnings = source
            .limitations
            .iter()
            .map(|message| RunWarning {
                code: source
                    .reason_codes
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "limitation".into()),
                message: message.clone(),
            })
            .collect();
        store.record_analysis_run(pr_id, &Raw::new(run)?)?;
    }
    Ok(())
}

fn review_status(file: &ChangedFile, history: &[Acknowledgement]) -> (FileStatus, Option<String>) {
    let latest = history.iter().rev().find(|ack| ack.path == file.path);
    let previous = file
        .previous_path
        .as_deref()
        .and_then(|path| history.iter().rev().find(|ack| ack.path == path));
    let last_reviewed_at = history
        .iter()
        .rev()
        .find(|ack| {
            ack.kind == "reviewed"
                && (ack.path == file.path
                    || Some(ack.path.as_str()) == file.previous_path.as_deref())
        })
        .map(|ack| ack.created_at.clone());
    if latest.is_some_and(|ack| ack.kind == "cleared") {
        return (FileStatus::Unreviewed, last_reviewed_at);
    }
    if latest.is_some_and(|ack| {
        file.patch_complete
            && ack.kind == "reviewed"
            && ack.fingerprint_version == file.fingerprint_version
            && ack.diff_digest == file.diff_digest
    }) {
        return (FileStatus::ReviewedCurrent, last_reviewed_at);
    }
    if latest.is_some_and(|ack| ack.kind == "reviewed")
        || previous.is_some_and(|ack| ack.kind == "reviewed")
    {
        (FileStatus::ChangedSinceReviewed, last_reviewed_at)
    } else {
        (FileStatus::Unreviewed, last_reviewed_at)
    }
}

fn progress(states: &[FileState]) -> Progress {
    let mut result = Progress {
        total: states.len() as u32,
        ..Default::default()
    };
    for state in states {
        match state.status {
            FileStatus::Ignored => result.ignored += 1,
            FileStatus::ReviewedCurrent => result.reviewed += 1,
            FileStatus::ChangedSinceReviewed => result.changed_since_reviewed += 1,
            FileStatus::Unreviewed => result.unreviewed += 1,
        }
    }
    result.in_scope = result.total - result.ignored;
    result
}

fn attach_files(threads: &mut [ReviewThread], files: &[ChangedFile]) {
    for thread in threads {
        thread.file_id = files
            .iter()
            .find(|file| file.path == thread.path)
            .map(|file| file.id.clone());
    }
}

fn revalidate_anchors(store: &mut Store, pr_id: &str, snapshot: &PrSnapshot) -> Result<()> {
    for thread in store.list_threads(pr_id)? {
        let file = snapshot.files.iter().find(|file| file.path == thread.path);
        let (state, reason) = match file {
            None => (AnchorState::Orphaned, Some("File is no longer in the PR")),
            Some(_) if thread.source == ThreadSource::Github => {
                let outdated = snapshot
                    .github_threads
                    .iter()
                    .find(|t| Some(t.node_id.as_str()) == thread.github_thread_id.as_deref())
                    .is_some_and(|t| t.is_outdated);
                if outdated {
                    (
                        AnchorState::Outdated,
                        Some("GitHub marks this thread as outdated"),
                    )
                } else if thread.anchor_state == AnchorState::Outdated {
                    (
                        AnchorState::Outdated,
                        Some("GitHub anchor cannot be verified locally"),
                    )
                } else {
                    (AnchorState::Current, None)
                }
            }
            Some(file) => match (thread.line, thread.line_text.as_deref()) {
                (Some(line), Some(text))
                    if diff::find_line(file, thread.side, line)
                        .is_some_and(|found| found.text == text) =>
                {
                    (AnchorState::Current, None)
                }
                _ => (
                    AnchorState::Outdated,
                    Some("Anchored line no longer matches the diff"),
                ),
            },
        };
        store.set_thread_anchor(&thread.id, state, reason)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "session_lifecycle_tests.rs"]
mod lifecycle_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::{RuleAction, RuleKind, Side, ThreadSource},
        store::NewThread,
        util::now_rfc3339,
    };

    #[test]
    fn session_view_exposes_context_independent_of_diff_completeness() {
        let session = session();
        let snapshot = session.snapshot.read().unwrap().clone();
        let context = github::context::AnalysisContext {
            source: "api-only".into(),
            base_sha: snapshot.pr.base_sha.clone(),
            head_sha: snapshot.pr.head_sha.clone(),
            comparison_base: None,
            history_count: 0,
            shallow: false,
            complete: false,
            reason: Some("fetch not consented".into()),
        };
        session.set_analysis_context(context);
        let view = session.view(Actor::Human).unwrap();
        assert_eq!(view.analysis_context.unwrap().source, "api-only");
        assert!(!view.analysis.sources.is_empty());
        assert_eq!(view.complete, snapshot.complete);
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

    fn acknowledge(session: &Session, path: &str, kind: &str) {
        let snapshot = session.snapshot.read().unwrap();
        let file = snapshot.files.iter().find(|f| f.path == path).unwrap();
        let ack = Acknowledgement {
            id: new_id("ack"),
            path: path.into(),
            kind: kind.into(),
            fingerprint_version: file.fingerprint_version,
            diff_digest: file.diff_digest.clone(),
            content_digest: file.content_digest.clone(),
            head_sha: snapshot.pr.head_sha.clone(),
            base_sha: snapshot.pr.base_sha.clone(),
            actor: Actor::Human,
            created_at: now_rfc3339(),
        };
        session
            .store
            .lock()
            .unwrap()
            .insert_acknowledgement(&session.pr_id, &ack)
            .unwrap();
    }

    #[test]
    fn scope_progress_and_cleared_acknowledgements() {
        let session = session();
        let initial = session.view(Actor::Agent).unwrap();
        assert_eq!(initial.actor, Actor::Agent);
        assert_eq!(initial.progress.unreviewed, initial.progress.total);
        let path = "git/client.go";
        acknowledge(&session, path, "reviewed");
        let mut store = session.store.lock().unwrap();
        store
            .create_rule(
                &session.repository.id,
                RuleAction::Exclude,
                RuleKind::File,
                path,
                None,
                Actor::Human,
            )
            .unwrap();
        drop(store);
        let ignored = session.view(Actor::Human).unwrap();
        let state = ignored.file_states.iter().find(|s| s.path == path).unwrap();
        assert_eq!(state.status, FileStatus::Ignored);
        assert_eq!(state.review_status, FileStatus::ReviewedCurrent);
        assert_eq!(ignored.progress.ignored, 1);
        assert_eq!(
            ignored.progress.in_scope + ignored.progress.ignored,
            ignored.progress.total
        );
        session
            .store
            .lock()
            .unwrap()
            .delete_rule(&session.repository.id, &ignored.rules[0].id)
            .unwrap();
        acknowledge(&session, path, "cleared");
        assert_eq!(
            session
                .view(Actor::Human)
                .unwrap()
                .file_states
                .iter()
                .find(|s| s.path == path)
                .unwrap()
                .status,
            FileStatus::Unreviewed
        );
    }

    #[test]
    fn incomplete_patch_never_derives_reviewed_current_from_stored_acknowledgement() {
        let session = session();
        let path = "git/client.go";
        acknowledge(&session, path, "reviewed");
        let current = session.view(Actor::Human).unwrap();
        assert_eq!(
            current
                .file_states
                .iter()
                .find(|s| s.path == path)
                .unwrap()
                .review_status,
            FileStatus::ReviewedCurrent
        );

        session
            .snapshot
            .write()
            .unwrap()
            .files
            .iter_mut()
            .find(|f| f.path == path)
            .unwrap()
            .patch_complete = false;
        let incomplete = session.view(Actor::Human).unwrap();
        let state = incomplete
            .file_states
            .iter()
            .find(|s| s.path == path)
            .unwrap();
        assert_eq!(state.review_status, FileStatus::ChangedSinceReviewed);
        assert_eq!(state.status, FileStatus::ChangedSinceReviewed);
        assert_eq!(incomplete.progress.reviewed, 0);
        assert_eq!(incomplete.progress.changed_since_reviewed, 1);
    }

    #[tokio::test]
    async fn refresh_keeps_unchanged_reviews_and_flags_changed_diffs() {
        let session = session();
        acknowledge(&session, "git/client.go", "reviewed");
        acknowledge(&session, "internal/identity/ssh.go", "reviewed");
        let mut receiver = session.events.subscribe();
        let view = session.refresh().await.unwrap();
        assert_eq!(
            view.file_states
                .iter()
                .find(|s| s.path == "git/client.go")
                .unwrap()
                .status,
            FileStatus::ChangedSinceReviewed
        );
        assert_eq!(
            view.file_states
                .iter()
                .find(|s| s.path == "internal/identity/ssh.go")
                .unwrap()
                .status,
            FileStatus::ReviewedCurrent
        );
        assert_eq!(view.sequence, 1);
        assert_eq!(receiver.try_recv().unwrap().sequence, 1);
        assert_eq!(session.refresh().await.unwrap().sequence, 2);
    }

    #[tokio::test]
    async fn same_revision_history_recovery_retries_without_rerunning_diff() {
        use std::process::Command;
        let session = session();
        let snapshot = session.snapshot.read().unwrap().clone();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_owned();
        let git = |args: &[&str]| {
            let result = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .output()
                .unwrap();
            assert!(result.status.success(), "{:?}", result);
            String::from_utf8(result.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Fixture"]);
        git(&["config", "user.email", "fixture@example.com"]);
        let path = &snapshot.files[0].path;
        std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
        std::fs::write(root.join(path), "fixture").unwrap();
        for n in 0..19 {
            std::fs::write(root.join(format!("zero-{n:02}.txt")), "fixture").unwrap();
        }
        git(&["add", "."]);
        git(&["commit", "-qm", "baseline"]);
        let base = git(&["rev-parse", "HEAD"]);
        let context = github::context::AnalysisContext {
            source: "fixture".into(),
            base_sha: snapshot.pr.base_sha.clone(),
            head_sha: snapshot.pr.head_sha.clone(),
            comparison_base: Some(base),
            history_count: 1,
            shallow: false,
            complete: true,
            reason: None,
        };
        let broken = tempfile::tempdir().unwrap();
        let broken_dir = broken.path().join("empty.git");
        let init = Command::new("git")
            .args(["init", "--bare", "-q"])
            .arg(&broken_dir)
            .status()
            .unwrap();
        assert!(init.success());
        *session.analysis_context.write().unwrap() = Some(context.clone());
        *session.history_objects.write().unwrap() =
            Some(github::context::HistoryObjects::fixture(broken, broken_dir));
        let initial = session.start_analysis().unwrap();
        for handle in &initial {
            handle.clone().wait().await;
        }
        let old_history = initial
            .iter()
            .find(|h| h.provider_id == "aside.git-history")
            .unwrap();
        let old_diff = initial
            .iter()
            .find(|h| h.provider_id == "aside.diff")
            .unwrap();
        let old_run = session
            .store
            .lock()
            .unwrap()
            .get_analysis_run(&old_history.run_id)
            .unwrap();
        assert_eq!(old_run.value().status, RunStatus::Partial);
        assert!(
            old_run
                .value()
                .coverage
                .reason_codes
                .contains(&"history_read_incomplete".into())
        );
        assert!(
            session
                .context_retry_needed(&snapshot, "aside.git-history")
                .unwrap()
        );
        let objects = github::context::HistoryObjects::fixture(temp, root.join(".git"));
        let recovered = session
            .replace_analysis_context(context, Some(objects))
            .await
            .unwrap();
        let new_history = recovered
            .iter()
            .find(|handle| handle.provider_id == "aside.git-history")
            .unwrap();
        assert!(recovered.iter().all(|handle| matches!(
            handle.provider_id.as_str(),
            "aside.git-history" | "aside.structure"
        )));
        assert_ne!(new_history.run_id, old_history.run_id);
        new_history.clone().wait().await;
        let store = session.store.lock().unwrap();
        assert_eq!(
            store
                .get_analysis_run(&old_history.run_id)
                .unwrap()
                .value()
                .status,
            RunStatus::Stale
        );
        assert_eq!(
            store
                .list_analysis_runs(&session.pr_id)
                .unwrap()
                .iter()
                .filter(|run| run.value().provider_id == "aside.diff")
                .count(),
            1
        );
        assert_eq!(
            store
                .get_analysis_run(&old_diff.run_id)
                .unwrap()
                .value()
                .status,
            RunStatus::Partial
        );
    }

    #[tokio::test]
    async fn structure_enabled_resolves_context_with_history_disabled_without_fetch() {
        let gh = Arc::new(github::fake::FakeGh::default());
        gh.on(&["pr", "view"], Ok(r#"{"number":7,"title":"test","body":"","url":"https://github.com/o/r/pull/7","state":"OPEN","isDraft":false,"author":{"login":"alice"},"baseRefName":"main","baseRefOid":"base1","headRefName":"feat","headRefOid":"head1","changedFiles":1}"#));
        gh.on(&["repo", "view"], Ok(r#"{"id":"R_1"}"#));
        gh.on(
            &["pr", "diff"],
            Ok("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n"),
        );
        gh.on(&["api", "--paginate"], Ok(r#"[[{"filename":"a.rs","status":"modified","patch":null,"additions":1,"deletions":1}]]"#));
        let reference = PrRef {
            owner: "o".into(),
            repo: "r".into(),
            number: 7,
        };
        let snapshot = github::fetch_snapshot(gh.as_ref(), &reference, &Limits::default()).unwrap();
        let session = Session::new(
            snapshot,
            Store::open_in_memory().unwrap(),
            Some(gh),
            Some(reference),
        )
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        let mut config = config::Config::default();
        config
            .set_scoped("analysis.history.enabled", "false", Some("github.com/o/r"))
            .unwrap();
        config::save_to(&path, &config).unwrap();
        session.configure_analysis(path).unwrap();
        let cache = root.path().join("context-cache");
        session.configure_history(None, false, cache);
        assert!(session.analysis_context.read().unwrap().is_some());
        assert!(!session.analysis_settings.read().unwrap().history.enabled);
        assert!(session.analysis_settings.read().unwrap().structure.enabled);
    }

    #[tokio::test]
    async fn repository_policy_refresh_is_scoped_preserves_review_and_never_fetches_when_disabled()
    {
        let gh = Arc::new(github::fake::FakeGh::default());
        gh.on(&["pr", "view"], Ok(r#"{"number":7,"title":"test","body":"","url":"https://github.com/o/r/pull/7","state":"OPEN","isDraft":false,"author":{"login":"alice"},"baseRefName":"main","baseRefOid":"base1","headRefName":"feat","headRefOid":"head1","changedFiles":1}"#));
        gh.on(&["repo", "view"], Ok(r#"{"id":"R_1"}"#));
        gh.on(
            &["pr", "diff"],
            Ok("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n"),
        );
        gh.on(&["api", "--paginate"], Ok(r#"[[{"filename":"a.rs","status":"modified","patch":null,"additions":1,"deletions":1}]]"#));
        let reference = PrRef {
            owner: "o".into(),
            repo: "r".into(),
            number: 7,
        };
        let snapshot = github::fetch_snapshot(gh.as_ref(), &reference, &Limits::default()).unwrap();
        let session = Session::new(
            snapshot,
            Store::open_in_memory().unwrap(),
            Some(gh.clone()),
            Some(reference),
        )
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        let cache = root.path().join("history-cache");
        let mut config = config::Config::default();
        config
            .set_scoped("analysis.history.enabled", "false", Some("github.com/o/r"))
            .unwrap();
        config
            .set_scoped(
                "analysis.structure.enabled",
                "false",
                Some("github.com/o/r"),
            )
            .unwrap();
        config::save_to(&path, &config).unwrap();
        session.configure_analysis(path.clone()).unwrap();
        let calls = gh.calls.lock().unwrap().len();
        session.configure_history(None, true, cache.clone());
        assert_eq!(gh.calls.lock().unwrap().len(), calls);
        assert!(
            !cache.exists(),
            "disabled history must not resolve or fetch"
        );
        acknowledge(&session, "a.rs", "reviewed");
        let initial = session.start_analysis().unwrap();
        for h in &initial {
            h.clone().wait().await;
        }
        let before = session.view(Actor::Human).unwrap();
        let history = before
            .analysis
            .sources
            .iter()
            .find(|s| s.source_id == "aside.git-history")
            .unwrap();
        assert_eq!(history.items, KnownCount::Unknown);
        assert_eq!(
            history.reason_codes,
            vec![analysis::reason::DISABLED_BY_CONFIGURATION]
        );
        assert!(
            history
                .limitations
                .iter()
                .any(|s| s.contains("no model execution"))
        );
        let old_history = history.run_id.clone();
        let old_diff = before
            .analysis
            .sources
            .iter()
            .find(|s| s.source_id == crate::diff::facts::PROVIDER_ID)
            .unwrap()
            .run_id
            .clone();
        config
            .set_scoped("analysis.diff.max_input_bytes", "1", Some("github.com/o/r"))
            .unwrap();
        config::save_to(&path, &config).unwrap();
        let after = session.refresh().await.unwrap();
        assert_eq!(before.file_states, after.file_states);
        assert_eq!(before.progress, after.progress);
        assert_eq!(
            after
                .analysis
                .sources
                .iter()
                .find(|s| s.source_id == "aside.git-history")
                .unwrap()
                .run_id,
            old_history
        );
        let diff = after
            .analysis
            .sources
            .iter()
            .find(|s| s.source_id == crate::diff::facts::PROVIDER_ID)
            .unwrap();
        assert_ne!(diff.run_id, old_diff);
        assert_eq!(
            diff.reason_codes,
            vec![analysis::reason::INPUT_LIMIT_EXCEEDED]
        );
        assert_eq!(diff.items, KnownCount::Unknown);
        assert!(!cache.exists());
        let calls = gh.calls.lock().unwrap().len();
        std::fs::write(
            &path,
            "[repositories.\"github.com/o/r\".analysis.history]\ntoken = 'credential-canary'\n",
        )
        .unwrap();
        let error = session.refresh().await.unwrap_err();
        assert!(!format!("{error:#}").contains("credential-canary"));
        assert_eq!(gh.calls.lock().unwrap().len(), calls);
        assert_eq!(session.view(Actor::Human).unwrap().analysis, after.analysis);
    }

    #[tokio::test]
    async fn live_refresh_retries_partial_history_deadline_on_unchanged_sha() {
        let gh = Arc::new(github::fake::FakeGh::default());
        gh.on(&["pr", "view"], Ok(r#"{"number":7,"title":"test","body":"","url":"https://github.com/o/r/pull/7","state":"OPEN","isDraft":false,"author":{"login":"alice"},"baseRefName":"main","baseRefOid":"base1","headRefName":"feat","headRefOid":"head1","changedFiles":1}"#));
        gh.on(&["repo", "view"], Ok(r#"{"id":"R_1"}"#));
        gh.on(
            &["pr", "diff"],
            Ok("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n"),
        );
        gh.on(&["api", "--paginate"], Ok(r#"[[{"filename":"a.rs","status":"modified","patch":null,"additions":1,"deletions":1}]]"#));
        let reference = PrRef {
            owner: "o".into(),
            repo: "r".into(),
            number: 7,
        };
        let snapshot = github::fetch_snapshot(gh.as_ref(), &reference, &Limits::default()).unwrap();
        let session = Session::new(
            snapshot.clone(),
            Store::open_in_memory().unwrap(),
            Some(gh),
            Some(reference),
        )
        .unwrap();
        let cache = tempfile::tempdir().unwrap();
        session.configure_history(None, false, cache.path().to_owned());
        // Keep an object source so the refresh must consult the latest run's reason.
        let broken = tempfile::tempdir().unwrap();
        let bare = broken.path().join("empty.git");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--bare", "-q"])
                .arg(&bare)
                .status()
                .unwrap()
                .success()
        );
        *session.history_objects.write().unwrap() =
            Some(github::context::HistoryObjects::fixture(broken, bare));
        let initial = session.start_analysis().unwrap();
        for handle in &initial {
            handle.clone().wait().await;
        }
        let old = initial
            .iter()
            .find(|h| h.provider_id == "aside.git-history")
            .unwrap();
        let mut deadline = session
            .store
            .lock()
            .unwrap()
            .get_analysis_run(&old.run_id)
            .unwrap()
            .value()
            .clone();
        assert_eq!(deadline.status, RunStatus::Partial);
        deadline.attempt += 1;
        deadline.id = crate::model::AnalysisRunId::derive(&crate::model::RunKeyInput {
            snapshot_id: &deadline.snapshot_id,
            provider_id: &deadline.provider_id,
            engine_version: &deadline.engine_version,
            configuration_digest: &deadline.configuration_digest,
            input_digest: &deadline.input_digest,
            attempt: deadline.attempt,
        });
        deadline.coverage.reason_codes = vec!["history_cancelled_or_deadline".into()];
        session
            .store
            .lock()
            .unwrap()
            .record_analysis_run(&session.pr_id, &Raw::new(deadline.clone()).unwrap())
            .unwrap();
        assert!(
            session
                .context_retry_needed(&snapshot, "aside.git-history")
                .unwrap(),
            "deadline must be retryable"
        );
        let refreshed = session.refresh().await.unwrap();
        assert_eq!(refreshed.pr.head_sha, snapshot.pr.head_sha);
        let runs = session
            .store
            .lock()
            .unwrap()
            .list_analysis_runs(&session.pr_id)
            .unwrap();
        assert!(
            runs.iter()
                .any(|r| r.value().provider_id == "aside.git-history"
                    && r.value().attempt > deadline.attempt),
            "unchanged revision must retry history"
        );
        assert_eq!(
            runs.iter()
                .filter(|r| r.value().provider_id == "aside.diff")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn history_disabled_structure_read_failure_and_missing_base_recover_on_unchanged_refresh()
    {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Fixture"]);
        git(&["config", "user.email", "fixture@example.com"]);
        git(&["remote", "add", "origin", "https://github.com/o/r.git"]);
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn example() {}\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        let base = git(&["rev-parse", "HEAD"]);
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn example() { helper(); }\nfn helper() {}\n",
        )
        .unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "head"]);
        let head = git(&["rev-parse", "HEAD"]);
        let blob = git(&["rev-parse", "HEAD:src/lib.rs"]);
        let gh = Arc::new(github::fake::FakeGh::default());
        gh.on(&["pr", "view"], Ok(&format!(r#"{{"number":7,"title":"test","body":"","url":"https://github.com/o/r/pull/7","state":"OPEN","isDraft":false,"author":{{"login":"alice"}},"baseRefName":"main","baseRefOid":"{base}","headRefName":"feat","headRefOid":"{head}","changedFiles":1}}"#)));
        gh.on(&["repo", "view"], Ok(r#"{"id":"R_1"}"#));
        gh.on(
            &["repo", "view", "o/r", "--json", "id"],
            Ok(r#"{"id":"R_1"}"#),
        );
        gh.on(&["pr", "diff"], Ok("diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1,2 @@\n-pub fn example() {}\n+pub fn example() { helper(); }\n+fn helper() {}\n"));
        gh.on(&["api", "--paginate"], Ok(r#"[[{"filename":"src/lib.rs","status":"modified","patch":null,"additions":2,"deletions":1}]]"#));
        let reference = PrRef {
            owner: "o".into(),
            repo: "r".into(),
            number: 7,
        };
        let snapshot = github::fetch_snapshot(gh.as_ref(), &reference, &Limits::default()).unwrap();
        let session = Session::new(
            snapshot,
            Store::open_in_memory().unwrap(),
            Some(gh),
            Some(reference),
        )
        .unwrap();
        let settings_dir = tempfile::tempdir().unwrap();
        let config_path = settings_dir.path().join("config.toml");
        let mut config = config::Config::default();
        config
            .set_scoped("analysis.history.enabled", "false", Some("github.com/o/r"))
            .unwrap();
        config
            .set_scoped("analysis.structure.enabled", "true", Some("github.com/o/r"))
            .unwrap();
        config::save_to(&config_path, &config).unwrap();
        session.configure_analysis(config_path).unwrap();
        session.configure_history(
            Some(root.to_owned()),
            false,
            settings_dir.path().join("cache"),
        );
        let objects = session.history_objects.read().unwrap().clone().unwrap();
        let object = objects
            .dir
            .join("objects")
            .join(&blob[..2])
            .join(&blob[2..]);
        assert!(object.exists(), "fixture must retain a loose source object");
        std::fs::remove_file(&object).unwrap();
        let initial = session.start_analysis().unwrap();
        for handle in &initial {
            handle.clone().wait().await;
        }
        let first = session
            .store
            .lock()
            .unwrap()
            .list_analysis_runs(&session.pr_id)
            .unwrap();
        let structure = first
            .iter()
            .filter(|r| r.value().provider_id == "aside.structure")
            .max_by_key(|r| r.value().attempt)
            .unwrap()
            .value();
        assert_eq!(structure.status, RunStatus::Partial);
        assert!(
            structure
                .coverage
                .reason_codes
                .contains(&"structure_object_read_failed".into())
        );
        assert!(
            session
                .context_retry_needed(&session.snapshot.read().unwrap(), "aside.structure")
                .unwrap()
        );
        let attempt = structure.attempt;
        let refreshed = session.refresh().await.unwrap();
        assert_eq!(refreshed.pr.head_sha, head);
        let handles = session.start_analysis().unwrap();
        for handle in &handles {
            handle.clone().wait().await;
        }
        let runs = session
            .store
            .lock()
            .unwrap()
            .list_analysis_runs(&session.pr_id)
            .unwrap();
        let recovered = runs
            .iter()
            .filter(|r| r.value().provider_id == "aside.structure")
            .max_by_key(|r| r.value().attempt)
            .unwrap()
            .value();
        assert!(recovered.attempt > attempt);
        assert!(
            !recovered
                .coverage
                .reason_codes
                .contains(&"structure_object_read_failed".into())
        );
        assert!(recovered.coverage.analyzed_entities != KnownCount::Known { value: 0 });
        {
            let store = session.store.lock().unwrap();
            let findings = store.list_findings(&session.pr_id, true).unwrap();
            assert!(
                findings.iter().any(|f| {
                    f.finding.value().source.run_id.as_ref() == Some(&recovered.id)
                        && !store
                            .list_finding_evidence(&f.finding.value().id)
                            .unwrap()
                            .is_empty()
                }),
                "refresh must persist recovered structural evidence"
            );
        }

        // Simulate a retained HEAD whose base resolution was temporarily
        // unavailable. History stays disabled; only structure can request the
        // unchanged-SHA context retry.
        let mut missing = session.analysis_context.read().unwrap().clone().unwrap();
        missing.comparison_base = None;
        let objects = session.history_objects.read().unwrap().clone();
        let handles = session
            .replace_analysis_context(missing, objects)
            .await
            .unwrap();
        for handle in &handles {
            handle.clone().wait().await;
        }
        let runs = session
            .store
            .lock()
            .unwrap()
            .list_analysis_runs(&session.pr_id)
            .unwrap();
        let incomplete = runs
            .iter()
            .filter(|r| r.value().provider_id == "aside.structure")
            .max_by_key(|r| r.value().attempt)
            .unwrap()
            .value();
        assert_eq!(incomplete.status, RunStatus::Partial);
        assert!(
            incomplete
                .coverage
                .reason_codes
                .contains(&"comparison_base_unavailable".into())
        );
        assert!(
            session
                .context_retry_needed(&session.snapshot.read().unwrap(), "aside.structure")
                .unwrap()
        );
        let attempt = incomplete.attempt;
        let refreshed = session.refresh().await.unwrap();
        assert_eq!(refreshed.pr.head_sha, head);
        let handles = session.start_analysis().unwrap();
        for handle in &handles {
            handle.clone().wait().await;
        }
        let runs = session
            .store
            .lock()
            .unwrap()
            .list_analysis_runs(&session.pr_id)
            .unwrap();
        let restored = runs
            .iter()
            .filter(|r| r.value().provider_id == "aside.structure")
            .max_by_key(|r| r.value().attempt)
            .unwrap()
            .value();
        assert!(restored.attempt > attempt);
        assert!(
            !restored
                .coverage
                .reason_codes
                .contains(&"comparison_base_unavailable".into())
        );
        assert_eq!(
            session
                .analysis_context
                .read()
                .unwrap()
                .as_ref()
                .unwrap()
                .comparison_base
                .as_deref(),
            Some(base.as_str())
        );
        assert!(restored.coverage.analyzed_entities != KnownCount::Known { value: 0 });
    }

    #[tokio::test]
    async fn refresh_invalidates_context_on_captured_revision_change() {
        let session = session();
        let initial = session.view(Actor::Human).unwrap();
        let context = github::context::AnalysisContext {
            source: "fetched-cache".into(),
            base_sha: initial.pr.base_sha.clone(),
            head_sha: initial.pr.head_sha.clone(),
            comparison_base: Some(initial.pr.base_sha.clone()),
            history_count: 2,
            shallow: false,
            complete: true,
            reason: None,
        };
        session.set_analysis_context(context);
        let changed = session.refresh().await.unwrap();
        let invalidated = changed.analysis_context.unwrap();
        assert_eq!(invalidated.source, "api-only");
        assert_eq!(invalidated.base_sha, changed.pr.base_sha);
        assert_eq!(invalidated.head_sha, changed.pr.head_sha);
        assert_eq!(
            invalidated.reason.as_deref(),
            Some("PR revision changed; analysis context requires resolution again")
        );
        assert_eq!(
            session
                .view(Actor::Human)
                .unwrap()
                .analysis_context
                .unwrap()
                .head_sha,
            changed.pr.head_sha
        );
        let unchanged = session.refresh().await.unwrap();
        assert_eq!(unchanged.analysis_context.unwrap().source, "api-only");
        assert_eq!(
            session
                .view(Actor::Human)
                .unwrap()
                .analysis_context
                .unwrap()
                .head_sha,
            unchanged.pr.head_sha
        );
    }

    #[tokio::test]
    async fn refresh_revalidates_local_anchors_without_moving_them() {
        let session = session();
        let initial = session.view(Actor::Human).unwrap();
        let file = initial
            .files
            .iter()
            .find(|f| f.path == "git/client.go")
            .unwrap();
        let line = file
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .find(|l| l.text.contains("GH_IDENTITY_SSH_VARIANT="))
            .unwrap();
        session
            .store
            .lock()
            .unwrap()
            .insert_thread(
                &session.pr_id,
                &NewThread {
                    id: "anchor".into(),
                    subject: crate::model::ThreadSubject::Line,
                    finding_id: None,
                    source: ThreadSource::Local,
                    github_thread_id: None,
                    path: file.path.clone(),
                    side: Side::Right,
                    line: line.new_line,
                    start_line: None,
                    line_text: Some(line.text.clone()),
                    diff_digest: Some(file.diff_digest.clone()),
                    head_sha: initial.pr.head_sha,
                    anchor_state: AnchorState::Current,
                    anchor_reason: None,
                    resolved: false,
                    created_at: None,
                },
            )
            .unwrap();
        let view = session.refresh().await.unwrap();
        let thread = view.threads.iter().find(|t| t.id == "anchor").unwrap();
        assert_eq!(thread.anchor_state, AnchorState::Outdated);
        assert_eq!(thread.line, line.new_line);
        assert_eq!(thread.path, file.path);
    }

    #[tokio::test]
    async fn failed_live_refresh_retains_snapshot_and_emits_no_event() {
        let snapshot = demo::snapshot(0);
        let reference = PrRef {
            owner: snapshot.pr.owner.clone(),
            repo: snapshot.pr.repo.clone(),
            number: snapshot.pr.number,
        };
        let gh = Arc::new(github::fake::FakeGh::default());
        let session = Session::new(
            snapshot,
            Store::open_in_memory().unwrap(),
            Some(gh.clone()),
            Some(reference),
        )
        .unwrap();
        let before = session.view(Actor::Agent).unwrap();
        let mut receiver = session.events.subscribe();
        assert!(session.refresh().await.is_err());
        assert_eq!(
            session.view(Actor::Agent).unwrap().pr.head_sha,
            before.pr.head_sha
        );
        assert_eq!(session.sequence.load(Ordering::SeqCst), 0);
        assert!(receiver.try_recv().is_err());
        assert_eq!(gh.calls_matching(&["pr", "view"]).len(), 1);
    }

    #[test]
    fn absent_file_orphans_anchor() {
        let session = session();
        let original = session.snapshot.read().unwrap().clone();
        session
            .store
            .lock()
            .unwrap()
            .insert_thread(
                &session.pr_id,
                &NewThread {
                    id: "missing".into(),
                    subject: crate::model::ThreadSubject::Line,
                    finding_id: None,
                    source: ThreadSource::Local,
                    github_thread_id: None,
                    path: "git/client.go".into(),
                    side: Side::Right,
                    line: Some(1),
                    start_line: None,
                    line_text: Some("old".into()),
                    diff_digest: None,
                    head_sha: original.pr.head_sha.clone(),
                    anchor_state: AnchorState::Current,
                    anchor_reason: None,
                    resolved: false,
                    created_at: None,
                },
            )
            .unwrap();
        let mut without_file = original;
        without_file.files.retain(|f| f.path != "git/client.go");
        revalidate_anchors(
            &mut session.store.lock().unwrap(),
            &session.pr_id,
            &without_file,
        )
        .unwrap();
        let thread = session
            .store
            .lock()
            .unwrap()
            .get_thread(&session.pr_id, "missing")
            .unwrap();
        assert_eq!(thread.anchor_state, AnchorState::Orphaned);
        assert_eq!(thread.path, "git/client.go");
    }

    #[test]
    fn mismatched_live_configuration_is_rejected() {
        assert!(
            Session::new(
                demo::snapshot(0),
                Store::open_in_memory().unwrap(),
                None,
                Some(PrRef {
                    owner: "a".into(),
                    repo: "b".into(),
                    number: 1
                })
            )
            .is_err()
        );
    }
}
