//! Run lifecycle, per-source completeness and invalidation contracts (#27,
//! analyze.md §4.3, §4.4, §11.4 and §13.5).
//!
//! The safety property this module exists for: **a source that failed, was
//! denied, truncated or stopped at a page cap must never read as "found
//! nothing".** [`SourceState::Clean`] is the only state that may be shown as
//! "all clear", and [`classify`] grants it only to a successful, complete
//! source with a known zero item count.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::analysis::{
    AnalysisMode, AnalysisRunId, Completeness, KnownCount, RunStatus, SnapshotId,
};

/// Version of the payloads of `analysis.*` events, `pr.refreshed` and
/// [`AnalysisSummary`]. Bumped when a payload changes incompatibly. The
/// envelope keeps its own `schemaVersion`.
pub const LIFECYCLE_EVENT_VERSION: u32 = 1;

/// What a source's current result may be displayed as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum SourceState {
    /// Queued or running; nothing can be said yet.
    Pending,
    /// Ran over its whole scope and found nothing. The only "all clear".
    Clean,
    /// Ran over its whole scope and found something.
    HasResults,
    /// Ran but did not cover its whole scope (partial, truncated, page cap,
    /// permission-limited, unknown completeness). Any items are real; their
    /// absence proves nothing.
    Incomplete,
    Failed,
    /// Never ran on this snapshot (not started, skipped, cancelled).
    NotRun,
}

/// Overall verdict across every source of the current snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum OverallCoverage {
    /// Every source is `clean` or `has_results`.
    Complete,
    /// Some source is still running and none is known to be incomplete.
    Pending,
    /// Some source is incomplete, failed or never ran.
    Incomplete,
}

/// One source's completeness and limitations on the current snapshot. Unifies
/// PR ingestion, GitHub feedback imports and analysis providers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SourceCoverage {
    /// Provider ID (`aside.diff-facts`) or import source (`github.review_threads`).
    pub source_id: String,
    pub mode: AnalysisMode,
    pub snapshot_id: SnapshotId,
    /// Latest run of this source on the snapshot, if it has one.
    pub run_id: Option<AnalysisRunId>,
    pub status: Option<RunStatus>,
    pub completeness: Completeness,
    /// Findings, threads, files... as named by `itemKind`. `unknown` is never 0.
    pub items: KnownCount,
    pub item_kind: String,
    pub state: SourceState,
    /// Machine-readable reasons for reduced coverage.
    pub reason_codes: Vec<String>,
    /// Human-readable limitations (what was not covered, and why).
    pub limitations: Vec<String>,
}

/// Compact analysis status carried by the session view. Authoritative: a
/// client that missed events resyncs from this, never from replayed deltas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AnalysisSummary {
    pub event_version: u32,
    /// The canonical snapshot everything "current" refers to.
    pub snapshot_id: SnapshotId,
    pub overall: OverallCoverage,
    pub sources: Vec<SourceCoverage>,
    pub current_findings: u32,
    pub historical_findings: u32,
    /// Current findings whose file is excluded by repository rules. Exclusion
    /// edits change this without rerunning analysis.
    /// TODO(#30): full scope-aware counts and priority buckets.
    pub current_findings_excluded: u32,
}

/// Why a snapshot change invalidated current analysis results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
#[allow(clippy::enum_variant_names)] // Wire names: `head_changed`, ...
pub enum InvalidationReason {
    HeadChanged,
    BaseChanged,
    /// A provider's engine version or configuration changed.
    ConfigChanged,
    /// Same SHAs, different snapshot identity (for example repository ID).
    SnapshotChanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Invalidation {
    pub previous_snapshot_id: SnapshotId,
    pub snapshot_id: SnapshotId,
    pub reasons: Vec<InvalidationReason>,
}

/// What the analysis of a PR is keyed on. Exclusion rules are deliberately
/// absent: editing them recalculates presentation only (analyze.md §4.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisKey {
    pub snapshot_id: SnapshotId,
    pub base_sha: String,
    pub head_sha: String,
    /// `None` when unknown (for example, restored from a previous process).
    pub config_digest: Option<String>,
}

impl AnalysisKey {
    /// Reasons `next` invalidates results computed for `self`. Empty when
    /// nothing that analysis depends on changed.
    pub fn invalidation(&self, next: &AnalysisKey) -> Option<Invalidation> {
        let mut reasons = Vec::new();
        if self.head_sha != next.head_sha {
            reasons.push(InvalidationReason::HeadChanged);
        }
        if self.base_sha != next.base_sha {
            reasons.push(InvalidationReason::BaseChanged);
        }
        if let (Some(a), Some(b)) = (&self.config_digest, &next.config_digest)
            && a != b
        {
            reasons.push(InvalidationReason::ConfigChanged);
        }
        if reasons.is_empty() && self.snapshot_id != next.snapshot_id {
            reasons.push(InvalidationReason::SnapshotChanged);
        }
        (!reasons.is_empty()).then(|| Invalidation {
            previous_snapshot_id: self.snapshot_id.clone(),
            snapshot_id: next.snapshot_id.clone(),
            reasons,
        })
    }
}

/// The single rule deciding how a source's result may be displayed.
///
/// `Clean` requires all of: a `succeeded` run, `complete` coverage and a
/// *known* zero count. Partial, failed, skipped, unknown-completeness and
/// unknown-count sources can never read as clean.
pub fn classify(
    status: Option<RunStatus>,
    completeness: Completeness,
    items: KnownCount,
) -> SourceState {
    match status {
        None | Some(RunStatus::Skipped | RunStatus::Cancelled) => SourceState::NotRun,
        Some(RunStatus::Queued | RunStatus::Running) => SourceState::Pending,
        Some(RunStatus::Failed) => SourceState::Failed,
        // A stale run is never the current result; treat defensively.
        Some(RunStatus::Partial | RunStatus::Stale) => SourceState::Incomplete,
        Some(RunStatus::Succeeded) => match (completeness, items) {
            (Completeness::Complete, KnownCount::Known { value: 0 }) => SourceState::Clean,
            (Completeness::Complete, KnownCount::Known { .. }) => SourceState::HasResults,
            (Completeness::Failed, _) => SourceState::Failed,
            _ => SourceState::Incomplete,
        },
    }
}

/// `Complete` only when every source finished over its whole scope.
pub fn overall(sources: &[SourceCoverage]) -> OverallCoverage {
    let mut pending = false;
    for source in sources {
        match source.state {
            SourceState::Clean | SourceState::HasResults => {}
            SourceState::Pending => pending = true,
            SourceState::Incomplete | SourceState::Failed | SourceState::NotRun => {
                return OverallCoverage::Incomplete;
            }
        }
    }
    if pending {
        OverallCoverage::Pending
    } else {
        OverallCoverage::Complete
    }
}

/// SSE event type for a run reaching `status` (analyze.md §13.5).
pub fn run_event_type(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Queued => "analysis.run.queued",
        RunStatus::Running => "analysis.run.started",
        RunStatus::Succeeded => "analysis.run.succeeded",
        RunStatus::Partial => "analysis.run.partial",
        RunStatus::Skipped => "analysis.run.skipped",
        RunStatus::Cancelled => "analysis.run.cancelled",
        RunStatus::Failed => "analysis.run.failed",
        RunStatus::Stale => "analysis.run.stale",
    }
}

pub const FINDINGS_UPSERTED: &str = "analysis.findings.upserted";
pub const FINDINGS_INVALIDATED: &str = "analysis.findings.invalidated";

#[cfg(test)]
mod tests {
    use super::*;

    fn known(value: u32) -> KnownCount {
        KnownCount::Known { value }
    }

    #[test]
    fn only_complete_successful_known_zero_is_clean() {
        use Completeness as C;
        use RunStatus as S;
        assert_eq!(
            classify(Some(S::Succeeded), C::Complete, known(0)),
            SourceState::Clean
        );
        assert_eq!(
            classify(Some(S::Succeeded), C::Complete, known(3)),
            SourceState::HasResults
        );
        // Zero findings from anything short of complete is never clean.
        for (status, completeness, items) in [
            (Some(S::Partial), C::Partial, known(0)),
            (Some(S::Partial), C::Complete, known(0)),
            (Some(S::Succeeded), C::Partial, known(0)),
            (Some(S::Succeeded), C::Unknown, known(0)),
            (Some(S::Succeeded), C::Complete, KnownCount::Unknown),
            (Some(S::Failed), C::Failed, known(0)),
            (Some(S::Succeeded), C::Failed, known(0)),
            (Some(S::Skipped), C::Skipped, known(0)),
            (Some(S::Cancelled), C::NotRun, known(0)),
            (Some(S::Stale), C::Complete, known(0)),
            (Some(S::Queued), C::NotRun, known(0)),
            (Some(S::Running), C::NotRun, known(0)),
            (None, C::NotRun, known(0)),
        ] {
            assert_ne!(
                classify(status, completeness, items),
                SourceState::Clean,
                "{status:?} {completeness:?} {items:?}"
            );
        }
        assert_eq!(
            classify(Some(S::Failed), C::Failed, known(0)),
            SourceState::Failed
        );
        assert_eq!(
            classify(Some(S::Partial), C::Partial, known(0)),
            SourceState::Incomplete
        );
    }

    fn key(head: &str, base: &str, config: Option<&str>) -> AnalysisKey {
        AnalysisKey {
            snapshot_id: SnapshotId::derive("r", 1, base, head),
            base_sha: base.into(),
            head_sha: head.into(),
            config_digest: config.map(Into::into),
        }
    }

    #[test]
    fn head_base_and_config_changes_invalidate() {
        let a = key("h1", "b1", Some("c1"));
        assert_eq!(a.invalidation(&a.clone()), None);
        let reasons = |b: &AnalysisKey| a.invalidation(b).unwrap().reasons;
        assert_eq!(
            reasons(&key("h2", "b1", Some("c1"))),
            [InvalidationReason::HeadChanged]
        );
        assert_eq!(
            reasons(&key("h1", "b2", Some("c1"))),
            [InvalidationReason::BaseChanged]
        );
        assert_eq!(
            reasons(&key("h1", "b1", Some("c2"))),
            [InvalidationReason::ConfigChanged]
        );
        // An unknown previous config is not reported as a change.
        assert_eq!(a.invalidation(&key("h1", "b1", None)), None);
    }

    #[test]
    fn overall_is_complete_only_when_every_source_is() {
        let source = |state| SourceCoverage {
            source_id: "s".into(),
            mode: AnalysisMode::Static,
            snapshot_id: SnapshotId::derive("r", 1, "b", "h"),
            run_id: None,
            status: None,
            completeness: Completeness::Unknown,
            items: KnownCount::Unknown,
            item_kind: "findings".into(),
            state,
            reason_codes: vec![],
            limitations: vec![],
        };
        use SourceState::*;
        assert_eq!(
            overall(&[source(Clean), source(HasResults)]),
            OverallCoverage::Complete
        );
        assert_eq!(
            overall(&[source(Clean), source(Pending)]),
            OverallCoverage::Pending
        );
        for bad in [Incomplete, Failed, NotRun] {
            assert_eq!(
                overall(&[source(Clean), source(Pending), source(bad)]),
                OverallCoverage::Incomplete
            );
        }
    }
}
