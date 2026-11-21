//! Analysis contracts: schema version, stable IDs, analysis runs and coverage.
//!
//! Design: `docs/plans/analyze.md` §§10 and 12. These records are produced by
//! analysis providers and importers. They are modelled separately from the
//! review state in `review.rs` (threads and acknowledgements): nothing here can
//! express "this file is reviewed". See `finding.rs` for the enforcement notes.
//!
//! Persistence is owned by #18, scheduling by #22 and the HTTP API by #33.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::util::sha256_hex;

/// Version of the analysis/finding JSON contract. Bump on any breaking change
/// to the shape of the types in `analysis.rs`, `finding.rs` or `imported.rs`.
///
/// Independent of [`super::SCHEMA_VERSION`], which versions the session view.
pub const ANALYSIS_SCHEMA_VERSION: u32 = 1;

/// Deterministic digest over an ordered list of optional fields.
///
/// Encoding, fed to SHA-256 in order:
/// 1. the domain tag bytes (for example `aside-finding/v1`), then a `0x00` byte;
/// 2. for each field: `0x00` when absent, or `0x01` followed by the field's
///    UTF-8 byte length as a big-endian `u64` and then the bytes.
///
/// Length prefixes make the encoding injective: `("ab", "c")` and `("a", "bc")`
/// differ, and an absent field differs from an empty string. The output is the
/// lowercase hex digest, so the same input gives the same ID in every process,
/// platform and run. Never include timestamps, random IDs or display line
/// numbers in the fields.
pub fn stable_digest(domain: &str, fields: &[Option<&str>]) -> String {
    let mut buf = Vec::with_capacity(64);
    buf.extend_from_slice(domain.as_bytes());
    buf.push(0);
    for field in fields {
        match field {
            None => buf.push(0),
            Some(s) => {
                buf.push(1);
                buf.extend_from_slice(&(s.len() as u64).to_be_bytes());
                buf.extend_from_slice(s.as_bytes());
            }
        }
    }
    sha256_hex(&buf)
}

macro_rules! id_newtype {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
        #[ts(export)]
        pub struct $name(String);

        impl $name {
            /// Prefix carried by IDs derived by Aside, so IDs are not confusable.
            pub const PREFIX: &'static str = $prefix;

            /// Wraps an ID read from storage or the wire. No validation: stored
            /// IDs from older derivations must still load.
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            fn from_digest(digest: &str) -> Self {
                Self(format!("{}{}", $prefix, digest))
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_newtype!(
    /// Identifies one immutable PR snapshot (base/head pair) an analysis read.
    SnapshotId,
    "snap_"
);
id_newtype!(
    /// Identifies one analysis execution attempt.
    AnalysisRunId,
    "run_"
);
id_newtype!(
    /// Identifies one logical finding claim; survives reruns and line moves.
    FindingId,
    "find_"
);
id_newtype!(
    /// Identifies one piece of evidence.
    EvidenceId,
    "ev_"
);

/// Domain tags. Changing a derivation means bumping the `/vN` suffix.
pub const SNAPSHOT_ID_DOMAIN: &str = "aside-snapshot/v1";
pub const RUN_ID_DOMAIN: &str = "aside-run/v1";
pub const FINDING_ID_DOMAIN: &str = "aside-finding/v1";
pub const EVIDENCE_ID_DOMAIN: &str = "aside-evidence/v1";

impl SnapshotId {
    /// `snap_` + [`stable_digest`]`("aside-snapshot/v1", [repo, pr, base, head])`.
    ///
    /// `canonical_repo_id` should be the provider's immutable repository ID
    /// when known, else `host/owner/repo` lowercased.
    pub fn derive(canonical_repo_id: &str, pr_number: u64, base_sha: &str, head_sha: &str) -> Self {
        let pr = pr_number.to_string();
        Self::from_digest(&stable_digest(
            SNAPSHOT_ID_DOMAIN,
            &[
                Some(canonical_repo_id),
                Some(&pr),
                Some(base_sha),
                Some(head_sha),
            ],
        ))
    }
}

/// Inputs to [`AnalysisRunId::derive`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunKeyInput<'a> {
    pub snapshot_id: &'a SnapshotId,
    pub provider_id: &'a str,
    pub engine_version: &'a str,
    pub configuration_digest: &'a str,
    pub input_digest: &'a str,
    /// 0 for the first attempt with these inputs; retries increment it so each
    /// attempt is its own immutable run.
    pub attempt: u32,
}

impl AnalysisRunId {
    /// `run_` + [`stable_digest`]`("aside-run/v1", [snapshot, provider, engine,
    /// config digest, input digest, attempt])`.
    pub fn derive(input: &RunKeyInput<'_>) -> Self {
        let attempt = input.attempt.to_string();
        Self::from_digest(&stable_digest(
            RUN_ID_DOMAIN,
            &[
                Some(input.snapshot_id.as_str()),
                Some(input.provider_id),
                Some(input.engine_version),
                Some(input.configuration_digest),
                Some(input.input_digest),
                Some(&attempt),
            ],
        ))
    }
}

/// Inputs to [`FindingId::derive`] (analyze.md §10.5).
///
/// Deliberately excludes the snapshot, run ID, timestamps and hunk offsets so
/// the same claim on the same code keeps its ID across reruns and refreshes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingKeyInput<'a> {
    pub canonical_repo_id: &'a str,
    pub pr_number: u64,
    pub provider_id: &'a str,
    pub rule_id: &'a str,
    /// Normalised text naming the claim, for example `coupled-with:src/token.rs`.
    pub claim_identity: &'a str,
    /// File lineage or entity identity (not a display line number).
    pub subject_identity: Option<&'a str>,
    /// Digest of the relevant code. A new fingerprint means a new finding.
    pub code_fingerprint: Option<&'a str>,
}

impl FindingId {
    /// `find_` + [`stable_digest`]`("aside-finding/v1", [repo, pr, provider,
    /// rule, claim, subject?, code fingerprint?])`.
    ///
    /// Imported records should instead use [`FindingId::for_imported`], which
    /// keys on the exact upstream identity.
    pub fn derive(input: &FindingKeyInput<'_>) -> Self {
        let pr = input.pr_number.to_string();
        Self::from_digest(&stable_digest(
            FINDING_ID_DOMAIN,
            &[
                Some(input.canonical_repo_id),
                Some(&pr),
                Some(input.provider_id),
                Some(input.rule_id),
                Some(input.claim_identity),
                input.subject_identity,
                input.code_fingerprint,
            ],
        ))
    }

    /// Finding (attention projection) of an imported native record.
    /// `find_` + [`stable_digest`]`("aside-finding/v1", [repo, pr, "imported",
    /// source kind, key kind, key value])`.
    pub fn for_imported(
        canonical_repo_id: &str,
        pr_number: u64,
        source: &super::imported::NativeSourceRef,
    ) -> Self {
        let pr = pr_number.to_string();
        let kind = source.source_kind.as_str();
        let (key_kind, key_value) = source.key.parts();
        Self::from_digest(&stable_digest(
            FINDING_ID_DOMAIN,
            &[
                Some(canonical_repo_id),
                Some(&pr),
                Some("imported"),
                Some(kind),
                Some(key_kind),
                Some(key_value),
            ],
        ))
    }
}

/// Inputs to [`EvidenceId::derive`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceKeyInput<'a> {
    pub kind: super::finding::EvidenceKind,
    /// Run that produced the evidence, when any. Evidence is immutable per run.
    pub source_run_id: Option<&'a AnalysisRunId>,
    pub label: &'a str,
    pub method: Option<&'a str>,
    pub input_digest: Option<&'a str>,
    /// Canonical value JSON (`serde_json::to_string` of the value; serde_json
    /// keeps object keys sorted by default, so this is deterministic).
    pub value_json: Option<&'a str>,
}

impl EvidenceId {
    /// `ev_` + [`stable_digest`]`("aside-evidence/v1", [kind, run?, label,
    /// method?, input digest?, value json?])`.
    pub fn derive(input: &EvidenceKeyInput<'_>) -> Self {
        Self::from_digest(&stable_digest(
            EVIDENCE_ID_DOMAIN,
            &[
                Some(input.kind.as_str()),
                input.source_run_id.map(AnalysisRunId::as_str),
                Some(input.label),
                input.method,
                input.input_digest,
                input.value_json,
            ],
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AnalysisMode {
    Static,
    Llm,
    GithubImport,
}

/// Provider-level run status (analyze.md §10.2). Not a global verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RunStatus {
    Queued,
    Running,
    Succeeded,
    Partial,
    Skipped,
    Cancelled,
    Failed,
    /// The session snapshot advanced; output is historical only.
    Stale,
}

impl RunStatus {
    /// True when the run will not change any more.
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }

    /// True when "zero findings" from this run may be reported as "no findings".
    /// Only a fully successful run can claim an absence.
    pub fn can_assert_absence(self) -> bool {
        matches!(self, Self::Succeeded)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RunWarning {
    /// Machine-readable reason, for example `history_truncated`.
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RunMetrics {
    #[ts(type = "number | null")]
    pub duration_ms: Option<u64>,
    #[ts(type = "number | null")]
    pub input_tokens: Option<u64>,
    #[ts(type = "number | null")]
    pub output_tokens: Option<u64>,
    pub estimated_cost_usd: Option<f64>,
}

/// One immutable analysis execution attempt, pinned to a single snapshot.
///
/// A run never moves to another snapshot. Status changes are new versions of
/// the same record (#18 persists them); a retry is a new run with a new
/// `attempt` in its [`RunKeyInput`]. Late output for an old snapshot is stored
/// with `status: stale` and is never shown as current (analyze.md §12.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AnalysisRun {
    pub schema_version: u32,
    pub id: AnalysisRunId,
    pub snapshot_id: SnapshotId,
    /// For example `aside.git-history`, `inspect`, `github.import`.
    pub provider_id: String,
    pub engine_version: String,
    pub configuration_digest: String,
    pub input_digest: String,
    pub attempt: u32,
    pub mode: AnalysisMode,
    pub status: RunStatus,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub coverage: AnalysisCoverage,
    pub warnings: Vec<RunWarning>,
    pub metrics: RunMetrics,
}

impl AnalysisRun {
    /// Builds a queued run whose ID is derived from `key`.
    pub fn queued(key: &RunKeyInput<'_>, mode: AnalysisMode) -> Self {
        Self {
            schema_version: ANALYSIS_SCHEMA_VERSION,
            id: AnalysisRunId::derive(key),
            snapshot_id: key.snapshot_id.clone(),
            provider_id: key.provider_id.to_string(),
            engine_version: key.engine_version.to_string(),
            configuration_digest: key.configuration_digest.to_string(),
            input_digest: key.input_digest.to_string(),
            attempt: key.attempt,
            mode,
            status: RunStatus::Queued,
            started_at: None,
            finished_at: None,
            coverage: AnalysisCoverage::not_run(),
            warnings: Vec::new(),
            metrics: RunMetrics::default(),
        }
    }

    /// True when the stored ID matches the derivation from the stored inputs.
    pub fn id_is_consistent(&self) -> bool {
        self.id
            == AnalysisRunId::derive(&RunKeyInput {
                snapshot_id: &self.snapshot_id,
                provider_id: &self.provider_id,
                engine_version: &self.engine_version,
                configuration_digest: &self.configuration_digest,
                input_digest: &self.input_digest,
                attempt: self.attempt,
            })
    }
}

/// A count that may be unknown. Never encode "unknown" as `0` (analyze.md
/// §10.1 rule 9, §11.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[ts(export)]
pub enum KnownCount {
    Known { value: u32 },
    Unknown,
}

/// How much of the intended scope a run covered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Completeness {
    Complete,
    Partial,
    NotRun,
    Skipped,
    Failed,
    Unknown,
}

/// Outcome of one provider/rule for one scope (PR, file or entity).
///
/// The three cases callers must never confuse are separate variants:
/// - `analyzed` with `findings: 0` → it ran and found nothing ("no finding");
/// - `not_applicable` → the check does not apply here (for example, a
///   complexity rule on a binary file), so there is nothing to find;
/// - `unknown` / `not_run` → we cannot say; never displayed as "0 issues".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case")]
#[ts(export)]
pub enum ScopeOutcome {
    Analyzed {
        findings: u32,
    },
    NotApplicable {
        reason: String,
    },
    Unknown {
        reason: String,
    },
    NotRun {
        /// For example `skipped_ignored_file`, `budget_exhausted`, `not_scheduled`.
        reason: String,
    },
}

impl ScopeOutcome {
    /// True only for a completed analysis that produced no finding.
    pub fn is_no_finding(&self) -> bool {
        matches!(self, Self::Analyzed { findings: 0 })
    }

    /// Finding count when it is actually known. Not-applicable is `Some(0)`
    /// only through [`Self::is_not_applicable`]; this returns `None` for it.
    pub fn known_findings(&self) -> Option<u32> {
        match self {
            Self::Analyzed { findings } => Some(*findings),
            _ => None,
        }
    }

    pub fn is_not_applicable(&self) -> bool {
        matches!(self, Self::NotApplicable { .. })
    }
}

/// Which scope a coverage entry describes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum CoverageScope {
    Pr,
    File {
        #[serde(rename = "fileId")]
        #[ts(rename = "fileId")]
        file_id: String,
        path: String,
    },
    Entity {
        #[serde(rename = "entityId")]
        #[ts(rename = "entityId")]
        entity_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CoverageEntry {
    pub scope: CoverageScope,
    /// Rule within the provider, when the outcome is rule-specific.
    pub rule_id: Option<String>,
    pub outcome: ScopeOutcome,
}

/// Run-level coverage (analyze.md §12.1 `analysis_run_coverage`, §11.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AnalysisCoverage {
    pub completeness: Completeness,
    pub eligible_files: KnownCount,
    pub analyzed_files: KnownCount,
    pub eligible_entities: KnownCount,
    pub analyzed_entities: KnownCount,
    /// Per-scope outcomes. A scope with no entry is unknown, not clean.
    pub entries: Vec<CoverageEntry>,
    /// Machine-readable reasons for reduced coverage.
    pub reason_codes: Vec<String>,
}

impl AnalysisCoverage {
    pub fn not_run() -> Self {
        Self {
            completeness: Completeness::NotRun,
            eligible_files: KnownCount::Unknown,
            analyzed_files: KnownCount::Unknown,
            eligible_entities: KnownCount::Unknown,
            analyzed_entities: KnownCount::Unknown,
            entries: Vec::new(),
            reason_codes: Vec::new(),
        }
    }

    /// Outcome for a file. Missing entries are `Unknown`, never "no finding".
    pub fn file_outcome(&self, file_id: &str) -> ScopeOutcome {
        self.entries
            .iter()
            .find(|e| matches!(&e.scope, CoverageScope::File { file_id: f, .. } if f == file_id))
            .map(|e| e.outcome.clone())
            .unwrap_or(ScopeOutcome::Unknown {
                reason: "no_coverage_entry".into(),
            })
    }
}
