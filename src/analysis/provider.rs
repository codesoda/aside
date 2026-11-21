//! The provider contract: what an analysis engine implements to be scheduled.
//!
//! Engines themselves live elsewhere — diff facts (#19), GitHub feedback
//! imports (#23/#24/#25), history (#28) and structural analysis (#31/#32).
//! This module only defines how the scheduler talks to them.

use std::{sync::Arc, time::Duration};

use anyhow::Result;
use futures_util::future::BoxFuture;

use crate::{
    model::{
        AnalysisCoverage, AnalysisMode, AnalysisRunId, Evidence, FindingRelation, PrSnapshot,
        ReviewFinding, RunStatus, RunWarning, SnapshotId, stable_digest,
    },
    store::Raw,
};

/// Domain separator for [`AnalysisInput::input_digest`].
pub const INPUT_DIGEST_DOMAIN: &str = "aside-analysis-input/v1";

/// The engine families named in analyze.md §4.1 step 5. Informational: the
/// scheduler treats every kind the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(dead_code)] // TODO(#23, #28, #31): remaining variants are used as engines register.
pub enum ProviderKind {
    Feedback,
    Diff,
    History,
    Structural,
}

/// Per-provider bounds. Exceeding one ends the run as `skipped` (input) or
/// `failed` (time, output) with a recorded reason code.
///
/// CPU is bounded indirectly: the scheduler caps how many providers run at
/// once and how long each may take. Engines that do heavy synchronous work
/// should run it on `spawn_blocking` so they do not stall the async runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderLimits {
    /// Wall-clock budget from the moment the provider starts running.
    pub timeout: Duration,
    /// Total diff text the provider may be handed.
    pub max_input_bytes: u64,
    /// Changed files the provider may be handed.
    pub max_input_files: usize,
    /// Findings a single run may publish.
    pub max_findings: usize,
}

impl Default for ProviderLimits {
    /// Engineering defaults (analyze.md §5.3), not calibrated values.
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(120),
            max_input_bytes: 32 * 1024 * 1024,
            max_input_files: 3_000,
            max_findings: 2_000,
        }
    }
}

/// Static identity of a provider. Everything here feeds the run ID, so a
/// version or configuration change produces a new run rather than silently
/// reinterpreting old output (analyze.md §4.3).
#[allow(dead_code)] // TODO(#23, #28, #31): remaining fields read by engines.
#[derive(Debug, Clone)]
pub struct ProviderDescriptor {
    pub id: String,
    pub kind: ProviderKind,
    pub engine_version: String,
    pub configuration_digest: String,
    pub mode: AnalysisMode,
    /// Provider-specific limits. Scheduler overrides take precedence, then
    /// these, then the scheduler defaults.
    pub limits: Option<ProviderLimits>,
}

/// The immutable snapshot a run is pinned to.
#[allow(dead_code)] // TODO(#23, #28, #31): remaining fields read by engines.
#[derive(Debug, Clone)]
pub struct AnalysisInput {
    pub pr_id: String,
    pub canonical_repo_id: String,
    pub pr_number: u64,
    pub snapshot_id: SnapshotId,
    pub base_sha: String,
    pub head_sha: String,
    pub snapshot: Arc<PrSnapshot>,
}

impl AnalysisInput {
    pub fn new(pr_id: &str, canonical_repo_id: &str, snapshot: PrSnapshot) -> Self {
        let snapshot_id = SnapshotId::derive(
            canonical_repo_id,
            snapshot.pr.number,
            &snapshot.pr.base_sha,
            &snapshot.pr.head_sha,
        );
        Self {
            pr_id: pr_id.to_owned(),
            canonical_repo_id: canonical_repo_id.to_owned(),
            pr_number: snapshot.pr.number,
            snapshot_id,
            base_sha: snapshot.pr.base_sha.clone(),
            head_sha: snapshot.pr.head_sha.clone(),
            snapshot: Arc::new(snapshot),
        }
    }

    /// Bytes of diff text handed to providers.
    pub fn input_bytes(&self) -> u64 {
        self.snapshot
            .files
            .iter()
            .flat_map(|f| &f.hunks)
            .flat_map(|h| &h.lines)
            .map(|l| l.text.len() as u64 + 1)
            .sum()
    }

    pub fn input_files(&self) -> usize {
        self.snapshot.files.len()
    }

    /// Identity of what providers see: the snapshot plus every file's
    /// fingerprint and completeness.
    pub fn input_digest(&self) -> String {
        let mut fields: Vec<Option<&str>> = vec![Some(self.snapshot_id.as_str())];
        for file in &self.snapshot.files {
            fields.push(Some(&file.id));
            fields.push(Some(&file.diff_digest));
            fields.push(Some(if file.patch_complete {
                "complete"
            } else {
                "partial"
            }));
        }
        stable_digest(INPUT_DIGEST_DOMAIN, &fields)
    }
}

/// Everything a provider gets for one run.
#[allow(dead_code)] // TODO(#23, #28, #31): remaining fields read by engines.
#[derive(Debug, Clone)]
pub struct ProviderContext {
    /// Findings must name this run and the input's snapshot.
    pub run_id: AnalysisRunId,
    pub attempt: u32,
    pub limits: ProviderLimits,
    pub input: Arc<AnalysisInput>,
}

/// One immutable result batch. Written atomically or not at all.
#[derive(Debug, Clone)]
pub struct ProviderOutput {
    /// `Succeeded` only when the whole defined scope was enumerated,
    /// otherwise `Partial` (analyze.md §4.4). Any other value fails the run.
    pub status: RunStatus,
    pub coverage: AnalysisCoverage,
    pub warnings: Vec<RunWarning>,
    /// [`Raw`] so importers can keep the exact source JSON.
    pub findings: Vec<Raw<ReviewFinding>>,
    pub evidence: Vec<Raw<Evidence>>,
    pub relations: Vec<FindingRelation>,
}

/// A pluggable analysis engine.
///
/// `run` is called on a dedicated task. It may fail, panic or never finish;
/// the scheduler contains each of those to this provider's run. The returned
/// future is dropped on cancel or timeout, so engines should be cancel-safe
/// at await points and must not write to the store themselves.
pub trait AnalysisProvider: Send + Sync + 'static {
    fn descriptor(&self) -> &ProviderDescriptor;
    fn run(&self, ctx: ProviderContext) -> BoxFuture<'static, Result<ProviderOutput>>;
}
