//! Runtime session: durable PR snapshots plus derived scope and review state.

use std::sync::{
    Arc, Mutex, RwLock,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::sync::{Mutex as AsyncMutex, broadcast};

use crate::{
    analysis::{
        self, AnalysisInput, AnalysisProvider, RunEvent, RunHandle, Scheduler, SchedulerConfig,
        SystemClock,
    },
    demo,
    diff::{self, Limits},
    github::{self, GhRunner, PrRef},
    model::{
        Acknowledgement, Actor, AnalysisCoverage, AnalysisKey, AnalysisMode, AnalysisRun,
        AnalysisSummary, AnchorState, ChangedFile, Completeness, CoverageEntry, CoverageScope,
        EventEnvelope, ExclusionRule, FINDINGS_INVALIDATED, FINDINGS_UPSERTED, FileState,
        FileStatus, Invalidation, KnownCount, LIFECYCLE_EVENT_VERSION, PrSnapshot, Progress,
        Repository, ReviewThread, RunKeyInput, RunStatus, RunWarning, SCHEMA_VERSION, ScopeOutcome,
        SessionView, SnapshotId, SourceCoverage, ThreadSource, classify, overall, run_event_type,
        stable_digest,
    },
    rules,
    store::{ObservationLabel, Raw, Store},
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
        store.sync_github_threads(&pr_id, &snapshot.pr.head_sha, &snapshot.github_threads)?;
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
        }))
    }

    pub fn view(&self, actor: Actor) -> Result<SessionView> {
        let snapshot = self
            .snapshot
            .read()
            .expect("snapshot lock poisoned")
            .clone();
        let store = self.store.lock().expect("store lock poisoned");
        let rules = store.list_rules(&self.repository.id)?;
        let acknowledgements = store.list_acknowledgements(&self.pr_id)?;
        let mut threads = store.list_threads(&self.pr_id)?;
        attach_files(&mut threads, &snapshot.files);
        let file_states = snapshot
            .files
            .iter()
            .map(|file| {
                let scope = rules::evaluate(&rules, &file.path);
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
            .collect::<Vec<_>>();
        let progress = progress(&file_states);
        let analysis = self.analysis_summary(&store, &snapshot, &rules)?;
        Ok(SessionView {
            schema_version: SCHEMA_VERSION,
            session_id: self.id.clone(),
            demo: self.demo,
            sequence: self.sequence.load(Ordering::SeqCst),
            actor,
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
        })
    }

    fn analysis_input(&self, snapshot: PrSnapshot) -> AnalysisInput {
        let canonical = snapshot
            .provider_repository_id
            .clone()
            .unwrap_or_else(|| self.repository.id.clone());
        AnalysisInput::new(&self.pr_id, &canonical, snapshot)
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
                    limitations: Vec::new(),
                },
                Some(run) => {
                    let items = if run.status.is_terminal() && run.status != RunStatus::Failed {
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
                        limitations: run.warnings.iter().map(|w| w.message.clone()).collect(),
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
        let next = AnalysisKey {
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
        let handles = self.analysis.begin_snapshot(input)?;
        {
            let mut store = self.store.lock().expect("store lock poisoned");
            record_import_sources(&mut store, &self.pr_id, &next.snapshot_id, &snapshot)?;
        }
        let invalidation = previous.and_then(|previous| previous.invalidation(&next));
        *self.analysis_key.lock().expect("analysis key lock") = Some(next);
        Ok((handles, invalidation))
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

    /// Fetch outside SQLite locks, then persist before replacing the in-memory snapshot.
    /// On failure the previous snapshot stays available and no refresh event is emitted.
    /// Demo mode alternates deterministic versions 0 and 1.
    pub async fn refresh(&self) -> Result<SessionView> {
        let _refresh = self.refresh_lock.lock().await;
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
            store.sync_github_threads(
                &self.pr_id,
                &snapshot.pr.head_sha,
                &snapshot.github_threads,
            )?;
            revalidate_anchors(&mut store, &self.pr_id, &snapshot)?;
            if previous_head != snapshot.pr.head_sha {
                store.reopen_review(&self.pr_id)?;
            }
        }
        *self.snapshot.write().expect("snapshot lock poisoned") = snapshot;
        if let Some(next) = next_demo_version {
            self.demo_version.store(next, Ordering::SeqCst);
        }
        let invalidation = match self.begin_analysis() {
            Ok((_, invalidation)) => invalidation,
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
pub mod source {
    pub const PR_DIFF: &str = "github.pr_diff";
    pub const REVIEW_THREADS: &str = "github.review_threads";
    pub const REVIEWS_AND_COMMENTS: &str = "github.reviews_and_comments";
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
    let lower: Vec<String> = problems.iter().map(|p| p.to_ascii_lowercase()).collect();
    if lower.iter().any(|p| {
        p.contains("forbidden") || p.contains("permission") || p.contains("not accessible")
    }) {
        codes.push(source_reason::GITHUB_PERMISSION_DENIED.into());
    }
    if lower.iter().any(|p| p.contains("pages; stopped early")) {
        codes.push(source_reason::PAGE_LIMIT_REACHED.into());
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
        limitations: problems.clone(),
    });

    // `gh pr view --json reviews,comments` does not report whether it saw
    // every page, so this source can never claim completeness yet.
    // TODO(#24): paginated review/comment import with real completeness.
    out.push(ImportSource {
        source_id: source::REVIEWS_AND_COMMENTS,
        status: RunStatus::Partial,
        completeness: Completeness::Unknown,
        items: KnownCount::Known {
            value: (snapshot.reviews.len() + snapshot.issue_comments.len()) as u32,
        },
        item_kind: "reviews_and_comments",
        reason_codes: vec![source_reason::PAGINATION_UNVERIFIED.into()],
        limitations: vec!["review verdicts and PR comments are not verified as complete".into()],
    });
    out
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
