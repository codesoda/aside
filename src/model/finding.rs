//! Finding, evidence, relation and local triage contracts (analyze.md §10).
//!
//! # Findings never set file-reviewed state
//!
//! Reviewed status comes only from `Acknowledgement` records (`review.rs`),
//! which humans and agents create explicitly. The types in this module are
//! separate on purpose:
//! - [`ReviewFinding`] has no field that refers to reviewed status,
//!   acknowledgements or `FileStatus`, and this module does not import them;
//! - [`UserDisposition`] has no "reviewed" variant, and [`FindingUserState`]
//!   rejects unknown fields, so `{"reviewed": true}` cannot sneak in;
//! - contract tests (`contract_tests.rs`) check all of the above.
//!
//! A finding can raise attention on a file; only an acknowledgement changes
//! whether it is reviewed.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::analysis::{ANALYSIS_SCHEMA_VERSION, AnalysisRunId, EvidenceId, FindingId, SnapshotId};
use super::imported::NativeSourceRef;
use super::snapshot::Side;

/// Source category, not a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum SourceKind {
    AsideStatic,
    InspectStatic,
    Llm,
    GithubReviewThread,
    GithubReviewSummary,
    GithubIssueComment,
    GithubCheckAnnotation,
    GithubCheckRun,
    GithubCommitStatus,
    GithubCodeScanning,
    AgentLocal,
    /// Also the fallback for kinds added by newer versions.
    #[serde(other)]
    OtherAdapter,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AsideStatic => "aside_static",
            Self::InspectStatic => "inspect_static",
            Self::Llm => "llm",
            Self::GithubReviewThread => "github_review_thread",
            Self::GithubReviewSummary => "github_review_summary",
            Self::GithubIssueComment => "github_issue_comment",
            Self::GithubCheckAnnotation => "github_check_annotation",
            Self::GithubCheckRun => "github_check_run",
            Self::GithubCommitStatus => "github_commit_status",
            Self::GithubCodeScanning => "github_code_scanning",
            Self::AgentLocal => "agent_local",
            Self::OtherAdapter => "other_adapter",
        }
    }

    /// True for records that came from GitHub and keep native state.
    pub fn is_imported(self) -> bool {
        self.as_str().starts_with("github_")
    }
}

/// What kind of claim a finding makes. External allegations stay
/// `external_feedback`; they never become `measured_fact`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AssertionKind {
    MeasuredFact,
    AttentionHint,
    DefectHypothesis,
    ConfirmedTestFailure,
    ExternalFeedback,
    SourceStatus,
    /// Also the fallback for kinds added by newer versions (least assertive).
    #[serde(other)]
    Informational,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum FindingKind {
    Complexity,
    Churn,
    Hotspot,
    TemporalCoupling,
    BlastRadius,
    PublicApi,
    Contract,
    Security,
    Concurrency,
    Correctness,
    Performance,
    TestCoverage,
    TestsChanged,
    Dependency,
    Migration,
    Suppression,
    Ownership,
    DuplicateCode,
    CiAnnotation,
    #[serde(other)]
    Other,
}

/// Aside presentation grouping. Not a defect severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AttentionLevel {
    None,
    Info,
    Investigate,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
    Info,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ValidationStatus {
    /// Bare measurements need no validation.
    NotRequired,
    Unverified,
    Supported,
    Contradicted,
    InsufficientEvidence,
    Failed,
}

/// Whether a finding applies to the *current* snapshot (analyze.md §10.6).
/// Derived per snapshot; independent of `source_status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Applicability {
    /// Same diff fingerprint; coordinates resolve exactly.
    CurrentExact,
    /// Provably remapped with unchanged meaning; shown distinctly.
    CurrentRelated,
    /// Code changed, source commit obsolete, or GitHub says outdated.
    HistoricalOutdated,
    /// Location cannot be resolved safely; keep the original citation.
    Unanchored,
    /// The inputs needed to decide are missing (for example, a truncated patch).
    Unavailable,
    /// Not yet determined for this snapshot. Also the fallback for values
    /// added by newer versions.
    #[serde(other)]
    Unknown,
}

impl Applicability {
    /// True when the finding may be shown as current.
    pub fn is_current(self) -> bool {
        matches!(self, Self::CurrentExact | Self::CurrentRelated)
    }
}

/// Native source state (GitHub thread resolution, check status, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum SourceStatus {
    Open,
    Resolved,
    Outdated,
    Dismissed,
    Pending,
    Completed,
    Neutral,
    #[serde(other)]
    Unknown,
}

/// Local triage. Never writes to GitHub and never changes file review
/// progress (see module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum UserDisposition {
    Untouched,
    Investigating,
    AcceptedForReview,
    DismissedLocal,
    Snoozed,
    PromotedToDraft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AnchorKind {
    DiffLine,
    DiffRange,
    File,
    Entity,
    Pr,
    RelatedContext,
    ExternalUrl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum EvidenceKind {
    Measurement,
    CodeReference,
    History,
    External,
    Test,
}

impl EvidenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Measurement => "measurement",
            Self::CodeReference => "code_reference",
            Self::History => "history",
            Self::External => "external",
            Self::Test => "test",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RelationKind {
    DuplicateTopicOf,
    Supports,
    Contradicts,
    ReferencesThread,
    RelatedCode,
    Supersedes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FindingSource {
    pub kind: SourceKind,
    pub provider_id: String,
    /// Absent for imported records not tied to an Aside run.
    pub run_id: Option<AnalysisRunId>,
    pub rule_id: Option<String>,
    pub rule_version: Option<String>,
    pub model_id: Option<String>,
    pub model_revision: Option<String>,
    pub prompt_version: Option<String>,
    /// Native record this finding projects, for imported sources.
    pub native: Option<NativeSourceRef>,
    pub github_actor: Option<String>,
}

/// A location. Line numbers are display coordinates, never identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CodeAnchor {
    pub kind: AnchorKind,
    pub file_id: Option<String>,
    pub current_path: Option<String>,
    pub previous_path: Option<String>,
    pub entity_id: Option<String>,
    pub side: Option<Side>,
    pub line: Option<u32>,
    pub start_line: Option<u32>,
    pub original_line: Option<u32>,
    pub original_start_line: Option<u32>,
    pub commit_sha: Option<String>,
    pub original_commit_sha: Option<String>,
    pub diff_digest: Option<String>,
    pub github_thread_id: Option<String>,
    pub external_url: Option<String>,
}

impl CodeAnchor {
    /// An anchor of `kind` with every optional coordinate empty.
    pub fn new(kind: AnchorKind) -> Self {
        Self {
            kind,
            file_id: None,
            current_path: None,
            previous_path: None,
            entity_id: None,
            side: None,
            line: None,
            start_line: None,
            original_line: None,
            original_start_line: None,
            commit_sha: None,
            original_commit_sha: None,
            diff_digest: None,
            github_thread_id: None,
            external_url: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AttentionAssessment {
    pub level: AttentionLevel,
    /// Versioned UI heuristic in 0..=100, not a bug probability.
    pub score: Option<f64>,
    pub score_version: Option<String>,
    pub reasons: Vec<String>,
}

/// Normalised severity plus the source's raw value, kept separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SeverityRecord {
    pub normalized: Severity,
    pub original: Option<String>,
}

/// Confidence with its meaning. `value` is not a calibrated defect probability
/// unless `calibration_version` says so.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Confidence {
    /// For example `measured_association`, `model_self_reported`.
    pub kind: String,
    pub value: f64,
    pub denominator: Option<u32>,
    pub calibration_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ValidationRecord {
    pub status: ValidationStatus,
    pub method: Option<String>,
    pub validated_at: Option<String>,
    pub note: Option<String>,
}

/// One logical claim observed on a snapshot. Immutable once produced: a rerun
/// that sees the same claim records a new occurrence ([`FindingOccurrence`]);
/// changed code yields a new [`FindingId`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ReviewFinding {
    pub schema_version: u32,
    pub id: FindingId,
    pub repository_id: String,
    #[ts(type = "number")]
    pub pr_number: u64,
    /// Snapshot the producing run read. Fixed for the life of the finding.
    pub observed_snapshot_id: SnapshotId,
    pub source: FindingSource,
    pub assertion_kind: AssertionKind,
    pub kinds: Vec<FindingKind>,
    pub title: String,
    pub body: String,
    pub rationale: String,
    pub suggested_action: Option<String>,
    pub anchor: CodeAnchor,
    pub related_anchors: Vec<CodeAnchor>,
    pub evidence_ids: Vec<EvidenceId>,
    pub attention: AttentionAssessment,
    pub severity: SeverityRecord,
    pub confidence: Option<Confidence>,
    pub validation: ValidationRecord,
    /// Applicability to `observed_snapshot_id` when produced. Applicability to
    /// a later snapshot is derived by the view (TODO(#33)), not written here.
    pub applicability: Applicability,
    pub source_status: SourceStatus,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub external_url: Option<String>,
    pub parent_finding_id: Option<FindingId>,
    pub correlation_group_id: Option<String>,
    /// Provider-specific metadata; must not carry review state.
    #[ts(type = "Record<string, unknown>")]
    pub extensions: serde_json::Map<String, serde_json::Value>,
}

/// Why a finding breaks the contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindingContractError {
    UnsupportedSchemaVersion(u32),
    EmptyTitle,
    /// High-attention findings need at least one evidence reference (§10.1 rule 5).
    HighAttentionWithoutEvidence,
    /// External allegations must not be presented as measured facts.
    ImportedAsMeasuredFact,
    /// `extensions` used a key reserved for review state.
    ReservedExtensionKey(String),
}

/// Extension keys that would smuggle review state into a finding.
pub const RESERVED_EXTENSION_KEYS: &[&str] = &[
    "reviewed",
    "fileStatus",
    "reviewStatus",
    "acknowledgement",
    "acknowledgements",
];

impl ReviewFinding {
    /// Checks the invariants downstream producers must uphold.
    pub fn validate(&self) -> Result<(), Vec<FindingContractError>> {
        let mut errors = Vec::new();
        if self.schema_version != ANALYSIS_SCHEMA_VERSION {
            errors.push(FindingContractError::UnsupportedSchemaVersion(
                self.schema_version,
            ));
        }
        if self.title.trim().is_empty() {
            errors.push(FindingContractError::EmptyTitle);
        }
        if self.attention.level == AttentionLevel::High && self.evidence_ids.is_empty() {
            errors.push(FindingContractError::HighAttentionWithoutEvidence);
        }
        if self.source.kind.is_imported() && self.assertion_kind == AssertionKind::MeasuredFact {
            errors.push(FindingContractError::ImportedAsMeasuredFact);
        }
        for key in self.extensions.keys() {
            if RESERVED_EXTENSION_KEYS
                .iter()
                .any(|r| r.eq_ignore_ascii_case(key))
            {
                errors.push(FindingContractError::ReservedExtensionKey(key.clone()));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// One observation of a finding by a run (analyze.md §12.1
/// `finding_occurrences`). Unique per `(finding_id, run_id)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FindingOccurrence {
    pub finding_id: FindingId,
    pub run_id: AnalysisRunId,
    pub snapshot_id: SnapshotId,
    pub observed_at: String,
    pub schema_version: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Evidence {
    pub schema_version: u32,
    pub id: EvidenceId,
    pub kind: EvidenceKind,
    pub source_run_id: Option<AnalysisRunId>,
    pub source_entity_id: Option<String>,
    pub anchor: Option<CodeAnchor>,
    pub label: String,
    #[ts(type = "unknown")]
    pub value: Option<serde_json::Value>,
    pub unit: Option<String>,
    pub numerator: Option<f64>,
    pub denominator: Option<f64>,
    pub method: Option<String>,
    pub input_digest: Option<String>,
    pub observed_at: String,
    pub limitations: Vec<String>,
}

/// Typed edge between a finding and another finding, thread or entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FindingRelation {
    pub source_finding_id: FindingId,
    pub kind: RelationKind,
    pub target: RelationTarget,
    pub method: String,
    pub method_version: String,
    pub score: Option<f64>,
    pub validated: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
#[ts(export)]
pub enum RelationTarget {
    Finding(FindingId),
    Thread(String),
    Entity(String),
}

/// Local, per-session triage overlay for one finding. Separate from the
/// finding (which is immutable) and from review acknowledgements.
///
/// `deny_unknown_fields` makes payloads like `{"reviewed": true}` fail to parse.
/// Updates use optimistic `revision` (CAS); TODO(#18) persists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct FindingUserState {
    pub finding_id: FindingId,
    pub session_id: String,
    pub disposition: UserDisposition,
    pub snoozed_until: Option<String>,
    pub note: Option<String>,
    pub promoted_draft_id: Option<String>,
    pub updated_at: String,
    #[ts(type = "number")]
    pub revision: u64,
}

impl FindingUserState {
    pub fn untouched(finding_id: FindingId, session_id: impl Into<String>, now: &str) -> Self {
        Self {
            finding_id,
            session_id: session_id.into(),
            disposition: UserDisposition::Untouched,
            snoozed_until: None,
            note: None,
            promoted_draft_id: None,
            updated_at: now.to_string(),
            revision: 0,
        }
    }
}
