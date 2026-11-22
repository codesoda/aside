//! Snapshot-scoped presentation of pinned baseline history.
use crate::{
    analysis::{
        AnalysisProvider, ProviderContext, ProviderDescriptor, ProviderKind, ProviderLimits,
        ProviderOutput,
    },
    history::{complexity, engine, reader},
    model::{
        ANALYSIS_SCHEMA_VERSION, AnalysisCoverage, AnalysisMode, AnchorKind, Applicability,
        AssertionKind, AttentionAssessment, AttentionLevel, CodeAnchor, Completeness,
        CoverageEntry, CoverageScope, Evidence, EvidenceId, EvidenceKeyInput, EvidenceKind,
        FindingId, FindingKeyInput, FindingKind, FindingSource, KnownCount, ReviewFinding,
        RunStatus, RunWarning, ScopeOutcome, Severity, SeverityRecord, SourceKind, SourceStatus,
        ValidationRecord, ValidationStatus,
    },
    store::Raw,
    util::now_rfc3339,
};
use anyhow::Result;
use futures_util::{FutureExt, future::BoxFuture};
use serde_json::{Map, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub const ID: &str = "aside.git-history";
const VERSION: &str = "aside.git-history/1";
const METHOD: &str = "aside.git-history.baseline@1";

pub struct HistoryProvider {
    descriptor: ProviderDescriptor,
}

// A cancelled provider future must never synchronously join a worker on Tokio.
pub(super) struct Worker {
    pub(super) handle: Option<std::thread::JoinHandle<()>>,
    pub(super) cancel: Arc<AtomicBool>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            std::thread::spawn(move || {
                let _ = handle.join();
            });
        }
    }
}
impl Default for HistoryProvider {
    fn default() -> Self {
        Self {
            descriptor: ProviderDescriptor {
                id: ID.into(),
                kind: ProviderKind::History,
                engine_version: VERSION.into(),
                configuration_digest: crate::util::sha256_hex(
                    b"180d;1000commits;exact-path;rust-ts-0.27.2-0.24.2;v1",
                ),
                mode: AnalysisMode::Static,
                limits: Some(ProviderLimits {
                    timeout: Duration::from_secs(100),
                    ..ProviderLimits::default()
                }),
            },
        }
    }
}
impl AnalysisProvider for HistoryProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }
    fn run(&self, ctx: ProviderContext) -> BoxFuture<'static, Result<ProviderOutput>> {
        async move {
            // Cancellation requests stop the worker at cooperative checkpoints.
            // Reaping is off-runtime: a cancelled future never blocks a Tokio thread.
            let cancel = Arc::new(AtomicBool::new(false));
            let (send, receive) = tokio::sync::oneshot::channel();
            let worker_cancel = Arc::clone(&cancel);
            let worker = Worker {
                handle: Some(std::thread::spawn(move || {
                    let _ = send.send(scan(ctx, &worker_cancel));
                })),
                cancel,
            };
            let result = receive.await.map_err(anyhow::Error::from)?;
            drop(worker);
            result
        }
        .boxed()
    }
}
fn partial(ctx: &ProviderContext, reason: &str) -> ProviderOutput {
    ProviderOutput {
        status: RunStatus::Partial,
        coverage: AnalysisCoverage {
            completeness: Completeness::Partial,
            eligible_files: KnownCount::Unknown,
            analyzed_files: KnownCount::Unknown,
            eligible_entities: KnownCount::Unknown,
            analyzed_entities: KnownCount::Unknown,
            entries: ctx
                .input
                .snapshot
                .files
                .iter()
                .map(|file| CoverageEntry {
                    scope: CoverageScope::File {
                        file_id: file.id.clone(),
                        path: file.path.clone(),
                    },
                    rule_id: Some(ID.into()),
                    outcome: ScopeOutcome::Unknown {
                        reason: reason.into(),
                    },
                })
                .collect(),
            reason_codes: vec![reason.into()],
        },
        warnings: vec![RunWarning {
            code: reason.into(),
            message: format!(
                "{reason}; base={}; comparison_base={}; context_history_count={}; shallow={}; context_complete={}; context_reason={}",
                ctx.input.base_sha,
                ctx.input
                    .history_context
                    .as_ref()
                    .and_then(|c| c.comparison_base.as_deref())
                    .unwrap_or("unknown"),
                ctx.input
                    .history_context
                    .as_ref()
                    .map_or(0, |c| c.history_count),
                ctx.input
                    .history_context
                    .as_ref()
                    .is_some_and(|c| c.shallow),
                ctx.input
                    .history_context
                    .as_ref()
                    .is_some_and(|c| c.complete),
                ctx.input
                    .history_context
                    .as_ref()
                    .and_then(|c| c.reason.as_deref())
                    .unwrap_or("unknown")
            ),
        }],
        findings: vec![],
        evidence: vec![],
        relations: vec![],
    }
}
fn scan(ctx: ProviderContext, cancelled: &AtomicBool) -> Result<ProviderOutput> {
    let Some(context) = ctx.input.history_context.as_ref() else {
        return Ok(partial(&ctx, "history_context_unavailable"));
    };
    let Some(objects) = ctx.input.history_objects.as_ref() else {
        return Ok(partial(&ctx, "history_objects_unavailable"));
    };
    let Some(base) = context.comparison_base.as_deref() else {
        return Ok(partial(&ctx, "comparison_base_unavailable"));
    };
    if context.base_sha != ctx.input.base_sha || context.head_sha != ctx.input.head_sha {
        return Ok(partial(&ctx, "history_context_stale"));
    }
    let deadline = Instant::now() + Duration::from_secs(75);
    let history = match reader::read_diagnostic(&objects.dir, base, deadline, cancelled) {
        Ok(value) => value,
        Err(diagnostic) => {
            let mut output = partial(&ctx, "history_read_incomplete");
            output.coverage.reason_codes.push(diagnostic.reason.clone());
            output.warnings.push(RunWarning {
                code: diagnostic.reason.clone(),
                message: serde_json::to_string(&diagnostic)?,
            });
            return Ok(output);
        }
    };
    let rust: Vec<_> = history
        .files
        .iter()
        .filter(|(path, _)| path.ends_with(".rs") && history.touches.contains_key(*path))
        .collect();
    let mut parsed = BTreeMap::new();
    let mut failed = 0;
    if rust.len() <= 500 {
        for (path, sha) in &rust {
            if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
                return Ok(partial(&ctx, "history_cancelled_or_deadline"));
            }
            match reader::blob_with_cancel(&objects.dir, sha, deadline, cancelled)
                .map_err(|e| e.to_string())
                .and_then(|bytes| complexity::measure(&bytes, deadline).map(|v| v.max_cyclomatic))
            {
                Ok(measure) => {
                    parsed.insert((*path).clone(), measure);
                }
                Err(_) => failed += 1,
            }
            if Instant::now() >= deadline {
                return Ok(partial(&ctx, "history_deadline"));
            }
        }
    } else {
        failed = rust.len();
    }
    let mut rank_history = history.clone();
    if !context.complete {
        rank_history.ambiguous.insert("<incomplete-context>".into());
    }
    let measurements = match engine::evaluate_checked(&rank_history, &parsed, || {
        cancelled.load(Ordering::Acquire) || Instant::now() >= deadline
    }) {
        Some(values) => values,
        None => return Ok(partial(&ctx, "history_cancelled_or_deadline")),
    };
    let observed_at = now_rfc3339();
    let mut output = ProviderOutput {
        status: RunStatus::Succeeded,
        coverage: AnalysisCoverage {
            completeness: Completeness::Complete,
            eligible_files: KnownCount::Known {
                value: ctx.input.snapshot.files.len() as u32,
            },
            analyzed_files: KnownCount::Known { value: 0 },
            eligible_entities: KnownCount::Unknown,
            analyzed_entities: KnownCount::Unknown,
            entries: Vec::new(),
            reason_codes: Vec::new(),
        },
        warnings: Vec::new(),
        findings: Vec::new(),
        evidence: Vec::new(),
        relations: Vec::new(),
    };
    if !context.complete
        || !history.ambiguous.is_empty()
        || failed > 0
        || history.files.len() < 20
        || rust.len() > 500
    {
        output.status = RunStatus::Partial;
        output.coverage.completeness = Completeness::Partial;
        output
            .coverage
            .reason_codes
            .push("history_rank_incomplete".into());
        output.warnings.push(RunWarning { code: "history_rank_incomplete".into(), message: format!("History or parser rank incomplete: {} failed of {} Rust population files; {} ambiguous paths", failed, rust.len(), history.ambiguous.len()) });
    }
    let coverage = json!({"baseSha": history.base, "anchorUnix": history.anchor, "windowStartUnix": history.window_start,
        "walked": history.walked, "counted": history.counted, "mergeSkipped": history.merges_skipped,
        "capped": false, "historyComplete": context.complete, "renamePolicy": "path-exact; PR renames use previous path; no inferred following",
        "parser": "tree-sitter 0.27.2/tree-sitter-rust 0.24.2", "rustPopulation": rust.len(), "rustParsed": parsed.len(), "rustFailed": failed,
        "baselinePopulation": history.files.len() });
    if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
        return Ok(partial(&ctx, "history_cancelled_or_deadline"));
    }
    record_corpus(&ctx, base, &coverage, &observed_at, &mut output)?;
    let mut analyzed = 0;
    for file in &ctx.input.snapshot.files {
        if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Ok(partial(&ctx, "history_cancelled_or_deadline"));
        }
        let path = file.previous_path.as_deref().unwrap_or(&file.path);
        let Some(measure) = measurements.get(path) else {
            output.coverage.entries.push(CoverageEntry {
                scope: CoverageScope::File {
                    file_id: file.id.clone(),
                    path: file.path.clone(),
                },
                rule_id: Some(ID.into()),
                outcome: ScopeOutcome::Unknown {
                    reason: "not_a_baseline_regular_file".into(),
                },
            });
            output.status = RunStatus::Partial;
            output.coverage.completeness = Completeness::Partial;
            continue;
        };
        let mut anchor = CodeAnchor::new(AnchorKind::File);
        anchor.file_id = Some(file.id.clone());
        anchor.current_path = Some(file.path.clone());
        anchor.previous_path = file.previous_path.clone();
        anchor.diff_digest = Some(file.diff_digest.clone());
        let value = json!({ "baseline": coverage, "path": path, "touchingCommits": measure.commits,
            "daysSinceChange": measure.days_since_change,
            "churnRank": measure.churn_rank.map(|r| json!({"percent": r.percent(), "below": r.below, "population": r.population})),
            "maxCyclomatic": measure.complexity,
            "complexityRank": measure.complexity_rank.map(|r| json!({"percent": r.percent(), "below": r.below, "population": r.population})),
            "hotspotIndex": measure.hotspot_index });
        let value_json = serde_json::to_string(&value)?;
        let key = EvidenceKeyInput {
            kind: EvidenceKind::History,
            source_run_id: Some(&ctx.run_id),
            label: "Pinned baseline history",
            method: Some(METHOD),
            input_digest: Some(base),
            value_json: Some(&value_json),
        };
        let id = EvidenceId::derive(&key);
        output.evidence.push(Raw::new(Evidence {
            schema_version: ANALYSIS_SCHEMA_VERSION,
            id: id.clone(),
            kind: EvidenceKind::History,
            source_run_id: Some(ctx.run_id.clone()),
            source_entity_id: None,
            anchor: Some(anchor.clone()),
            label: "Pinned baseline history".into(),
            value: Some(value.clone()),
            unit: None,
            numerator: measure.churn_rank.map(|r| r.below as f64),
            denominator: measure.churn_rank.map(|r| r.population as f64),
            method: Some(METHOD.into()),
            input_digest: Some(base.into()),
            observed_at: observed_at.clone(),
            limitations: if output.status == RunStatus::Partial {
                vec!["rank or population incomplete".into()]
            } else {
                vec![]
            },
        })?);
        let attention = if measure.hotspot_attention {
            Some((FindingKind::Hotspot, "hotspot"))
        } else if measure.churn_attention {
            Some((FindingKind::Churn, "churn"))
        } else {
            None
        };
        if let Some((kind, rule)) =
            attention.filter(|_| context.complete && history.ambiguous.is_empty())
        {
            // A first-insert finding is immutable: changing a count, rank or
            // baseline must produce a distinct finding ID.
            let claim = format!(
                "{}:{}:{}:{}:{}",
                base, rule, ctx.run_id, file.path, value_json
            );
            let finding_id = FindingId::derive(&FindingKeyInput {
                canonical_repo_id: &ctx.input.canonical_repo_id,
                pr_number: ctx.input.pr_number,
                provider_id: ID,
                rule_id: rule,
                claim_identity: &claim,
                subject_identity: Some(path),
                code_fingerprint: None,
            });
            let mut extensions = Map::new();
            extensions.insert("measurement".into(), value);
            extensions.insert("ruleVersion".into(), json!("1"));
            output.findings.push(Raw::new(ReviewFinding { schema_version: ANALYSIS_SCHEMA_VERSION,
                id: finding_id, repository_id: ctx.input.canonical_repo_id.clone(), pr_number: ctx.input.pr_number,
                observed_snapshot_id: ctx.input.snapshot_id.clone(),
                source: FindingSource { kind: SourceKind::AsideStatic, provider_id: ID.into(), run_id: Some(ctx.run_id.clone()),
                    rule_id: Some(rule.into()), rule_version: Some("1".into()), model_id: None, model_revision: None,
                    prompt_version: None, native: None, github_actor: None },
                assertion_kind: AssertionKind::AttentionHint, kinds: vec![kind], title: format!("Historical {rule} attention: {path}"),
                body: format!("{path} has {} touching commits in the pinned 180-day baseline window.", measure.commits),
                rationale: "Historical frequency and baseline Rust complexity are review-priority signals, not defect claims.".into(),
                suggested_action: Some("Inspect this file's changed behavior and tests.".into()), anchor,
                related_anchors: Vec::new(), evidence_ids: vec![id],
                attention: AttentionAssessment { level: AttentionLevel::Investigate, score: None, score_version: None, reasons: vec![rule.into()] },
                severity: SeverityRecord { normalized: Severity::Info, original: None }, confidence: None,
                validation: ValidationRecord { status: ValidationStatus::NotRequired, method: Some(METHOD.into()), validated_at: None, note: None },
                applicability: Applicability::CurrentExact, source_status: SourceStatus::Open,
                first_seen_at: observed_at.clone(), last_seen_at: observed_at.clone(), external_url: None,
                parent_finding_id: None, correlation_group_id: None, extensions })?);
        }
        analyzed += 1;
        output.coverage.entries.push(CoverageEntry {
            scope: CoverageScope::File {
                file_id: file.id.clone(),
                path: file.path.clone(),
            },
            rule_id: Some("aside.git-history.churn".into()),
            outcome: if measure.churn_rank.is_none() {
                ScopeOutcome::Unknown {
                    reason: "ranking_unavailable".into(),
                }
            } else {
                ScopeOutcome::Analyzed {
                    findings: u32::from(attention.is_some()),
                }
            },
        });
        let complexity_outcome = if path.ends_with(".rs") {
            if measure.complexity_rank.is_some() {
                ScopeOutcome::Analyzed {
                    findings: u32::from(measure.hotspot_attention),
                }
            } else {
                ScopeOutcome::Unknown {
                    reason: "rust_complexity_rank_unavailable".into(),
                }
            }
        } else if [
            "txt", "md", "json", "toml", "yaml", "yml", "lock", "png", "jpg", "svg",
        ]
        .iter()
        .any(|extension| path.ends_with(&format!(".{extension}")))
        {
            ScopeOutcome::NotApplicable {
                reason: "non_source_file".into(),
            }
        } else {
            ScopeOutcome::Unknown {
                reason: "unsupported_or_unclassified_source".into(),
            }
        };
        if matches!(complexity_outcome, ScopeOutcome::Unknown { .. }) {
            output.status = RunStatus::Partial;
            output.coverage.completeness = Completeness::Partial;
            if !output
                .coverage
                .reason_codes
                .contains(&"complexity_unknown".to_string())
            {
                output
                    .coverage
                    .reason_codes
                    .push("complexity_unknown".into());
            }
        }
        output.coverage.entries.push(CoverageEntry {
            scope: CoverageScope::File {
                file_id: file.id.clone(),
                path: file.path.clone(),
            },
            rule_id: Some("aside.git-history.complexity".into()),
            outcome: complexity_outcome,
        });
    }
    output.coverage.analyzed_files = KnownCount::Known { value: analyzed };
    Ok(output)
}

fn record_corpus(
    ctx: &ProviderContext,
    base: &str,
    corpus: &serde_json::Value,
    observed_at: &str,
    output: &mut ProviderOutput,
) -> Result<()> {
    let value_json = serde_json::to_string(corpus)?;
    let evidence_id = EvidenceId::derive(&EvidenceKeyInput {
        kind: EvidenceKind::History,
        source_run_id: Some(&ctx.run_id),
        label: "Pinned baseline corpus",
        method: Some(METHOD),
        input_digest: Some(base),
        value_json: Some(&value_json),
    });
    let anchor = CodeAnchor::new(AnchorKind::Pr);
    output.evidence.push(Raw::new(Evidence {
        schema_version: ANALYSIS_SCHEMA_VERSION,
        id: evidence_id.clone(),
        kind: EvidenceKind::History,
        source_run_id: Some(ctx.run_id.clone()),
        source_entity_id: None,
        anchor: Some(anchor.clone()),
        label: "Pinned baseline corpus".into(),
        value: Some(corpus.clone()),
        unit: None,
        numerator: None,
        denominator: None,
        method: Some(METHOD.into()),
        input_digest: Some(base.into()),
        observed_at: observed_at.into(),
        limitations: if output.status == RunStatus::Partial {
            vec!["rank or population incomplete".into()]
        } else {
            vec![]
        },
    })?);
    let claim = format!("{}:{}:{}", base, ctx.run_id, value_json);
    let finding_id = FindingId::derive(&FindingKeyInput {
        canonical_repo_id: &ctx.input.canonical_repo_id,
        pr_number: ctx.input.pr_number,
        provider_id: ID,
        rule_id: "corpus",
        claim_identity: &claim,
        subject_identity: None,
        code_fingerprint: None,
    });
    output.findings.push(Raw::new(ReviewFinding {
        schema_version: ANALYSIS_SCHEMA_VERSION, id: finding_id,
        repository_id: ctx.input.canonical_repo_id.clone(), pr_number: ctx.input.pr_number,
        observed_snapshot_id: ctx.input.snapshot_id.clone(),
        source: FindingSource { kind: SourceKind::AsideStatic, provider_id: ID.into(),
            run_id: Some(ctx.run_id.clone()), rule_id: Some("corpus".into()), rule_version: Some("1".into()),
            model_id: None, model_revision: None, prompt_version: None, native: None, github_actor: None },
        assertion_kind: AssertionKind::Informational, kinds: vec![FindingKind::Other],
        title: "Pinned baseline history corpus".into(),
        body: "Baseline history population and parser coverage for this review.".into(),
        rationale: "Corpus metadata describes the evidence available for history ranks; it is not a defect claim.".into(),
        suggested_action: None, anchor, related_anchors: Vec::new(), evidence_ids: vec![evidence_id],
        attention: AttentionAssessment { level: AttentionLevel::None, score: None, score_version: None, reasons: vec![] },
        severity: SeverityRecord { normalized: Severity::Info, original: None }, confidence: None,
        validation: ValidationRecord { status: ValidationStatus::NotRequired, method: Some(METHOD.into()), validated_at: None, note: None },
        applicability: Applicability::CurrentExact, source_status: SourceStatus::Open,
        first_seen_at: observed_at.into(), last_seen_at: observed_at.into(), external_url: None,
        parent_finding_id: None, correlation_group_id: None, extensions: Map::new(),
    })?);
    Ok(())
}
