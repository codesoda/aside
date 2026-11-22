//! Typed domain model shared by the API, persistence and the frontend.
//!
//! Every type here derives `ts_rs::TS`; `cargo test` writes matching
//! TypeScript declarations into `app/src/generated/`.

// Analysis contracts (#15). Consumers land in #17/#18/#19/#22/#33; until
// then parts of the API are unused in this binary.
#[allow(dead_code)]
mod analysis;
#[allow(dead_code)]
mod finding;
#[allow(dead_code)]
mod imported;
mod lifecycle;
mod review;
mod rules;
mod snapshot;

pub use review::{
    Acknowledgement, Actor, AnchorState, Comment, EventEnvelope, FileScope, FileState, FileStatus,
    Progress, PublishState, ReviewState, ReviewThread, SessionView, ThreadSource,
    UnresolvedPublication, Verdict,
};
pub use rules::{ExclusionRule, Repository, RuleAction, RuleKind};
pub use snapshot::{
    ChangeKind, ChangedFile, DiffHunk, DiffLine, GithubComment, GithubThread, ImportedReview,
    IssueComment, LineKind, PrSnapshot, PullRequest, Side,
};

#[allow(unused_imports)]
pub use analysis::{
    ANALYSIS_SCHEMA_VERSION, AnalysisCoverage, AnalysisMode, AnalysisRun, AnalysisRunId,
    Completeness, CoverageEntry, CoverageScope, EVIDENCE_ID_DOMAIN, EvidenceId, EvidenceKeyInput,
    FINDING_ID_DOMAIN, FindingId, FindingKeyInput, KnownCount, RUN_ID_DOMAIN, RunKeyInput,
    RunMetrics, RunStatus, RunWarning, SNAPSHOT_ID_DOMAIN, ScopeOutcome, SnapshotId, stable_digest,
};
#[allow(unused_imports)]
pub use finding::{
    AnchorKind, Applicability, AssertionKind, AttentionAssessment, AttentionLevel, CodeAnchor,
    Confidence, Evidence, EvidenceKind, FindingContractError, FindingKind, FindingOccurrence,
    FindingRelation, FindingSource, FindingUserState, RESERVED_EXTENSION_KEYS, RelationKind,
    RelationTarget, ReviewFinding, Severity, SeverityRecord, SourceKind, SourceStatus,
    UserDisposition, ValidationRecord, ValidationStatus,
};
#[allow(unused_imports)]
pub use imported::{
    AnnotationLevel, ImportedCheckAnnotation, ImportedCheckRun, ImportedCommitStatus,
    NativeSourceKey, NativeSourceRef,
};

#[allow(unused_imports)]
pub use lifecycle::{
    AnalysisKey, AnalysisSummary, FINDINGS_INVALIDATED, FINDINGS_UPSERTED, Invalidation,
    InvalidationReason, LIFECYCLE_EVENT_VERSION, OverallCoverage, SourceCoverage, SourceState,
    classify, overall, run_event_type,
};

#[cfg(test)]
mod contract_tests;

/// Version of the JSON contract served to the browser and agents.
pub const SCHEMA_VERSION: u32 = 1;
