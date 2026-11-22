//! Presentation of independently measured, bounded co-change associations.
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

use anyhow::Result;
use serde_json::{Map, json};

use super::history::ID;
use crate::{
    analysis::{ProviderContext, ProviderOutput},
    history::{
        cochange::{self, Incomplete},
        reader::History,
    },
    model::{
        ANALYSIS_SCHEMA_VERSION, AnchorKind, Applicability, AssertionKind, AttentionAssessment,
        AttentionLevel, CodeAnchor, Completeness, Confidence, CoverageEntry, CoverageScope,
        Evidence, EvidenceId, EvidenceKeyInput, EvidenceKind, FindingId, FindingKeyInput,
        FindingKind, FindingSource, ReviewFinding, RunStatus, RunWarning, ScopeOutcome, Severity,
        SeverityRecord, SourceKind, SourceStatus, ValidationRecord, ValidationStatus,
    },
    store::Raw,
};

pub(super) const RULE: &str = "aside.git-history.cochange";
const LIMITATION: &str = "Exact paths only; historical renames are not followed. Measured association is not a defect probability or proof that this PR is missing code.";

pub(super) fn corpus(history: &History) -> serde_json::Value {
    json!({
        "method": cochange::METHOD, "comparisonBase": history.base,
        "windowStartUnix": history.window_start, "windowEndUnix": history.anchor,
        "windowInclusive": true, "walkedCommits": history.walked,
        "windowNonMergeCommits": history.counted, "mergeExcluded": history.merges_skipped,
        "eligibleTransactions": history.transactions.len(), "bulkExcluded": history.broad_skipped,
        "maxCommitPaths": cochange::MAX_COMMIT_PATHS,
        "minSupport": cochange::MIN_SUPPORT, "minDirectionalConfidence": 0.6,
        "pairLimit": cochange::MAX_PAIRS, "hintLimit": cochange::MAX_HINTS,
        "sampled": false, "renamePolicy": "exact-path; ambiguous identities excluded",
    })
}

fn unknown(ctx: &ProviderContext, output: &mut ProviderOutput, reason: &str) {
    output.status = RunStatus::Partial;
    output.coverage.completeness = Completeness::Partial;
    output.coverage.reason_codes.push(reason.into());
    output.warnings.push(RunWarning {
        code: reason.into(),
        message: format!(
            "Co-change associations unavailable: {reason}; no relationship hints emitted."
        ),
    });
    output
        .coverage
        .entries
        .extend(ctx.input.snapshot.files.iter().map(|file| CoverageEntry {
            scope: CoverageScope::File {
                file_id: file.id.clone(),
                path: file.path.clone(),
            },
            rule_id: Some(RULE.into()),
            outcome: ScopeOutcome::Unknown {
                reason: reason.into(),
            },
        }));
}

pub(super) fn append(
    ctx: &ProviderContext,
    history: &History,
    deadline: Instant,
    cancelled: &AtomicBool,
    observed_at: &str,
    output: &mut ProviderOutput,
) -> Result<()> {
    if !ctx
        .input
        .history_context
        .as_ref()
        .is_some_and(|c| c.complete)
    {
        unknown(ctx, output, "cochange_context_incomplete");
        return Ok(());
    }
    let pr_paths: BTreeSet<_> = ctx
        .input
        .snapshot
        .files
        .iter()
        .flat_map(|file| std::iter::once(file.path.clone()).chain(file.previous_path.clone()))
        .collect();
    let changed: BTreeSet<_> = ctx
        .input
        .snapshot
        .files
        .iter()
        .map(|f| f.previous_path.as_ref().unwrap_or(&f.path))
        .filter(|p| history.files.contains_key(*p) && !history.ambiguous.contains(*p))
        .cloned()
        .collect();
    let interrupted = || cancelled.load(Ordering::Acquire) || Instant::now() >= deadline;
    let available = ctx
        .limits
        .max_findings
        .saturating_sub(output.findings.len());
    let result = match cochange::evaluate(history, &changed, &pr_paths, available, interrupted) {
        Ok(result) => result,
        Err(reason) => {
            unknown(
                ctx,
                output,
                match reason {
                    Incomplete::Interrupted => "cochange_cancelled_or_deadline",
                    Incomplete::PairLimit => "cochange_pair_limit",
                },
            );
            return Ok(());
        }
    };
    let truncated = result.associations.len() < result.qualifying_pairs;
    let mut findings = Vec::new();
    let mut evidence = Vec::new();
    let mut counts = BTreeMap::<String, u32>::new();
    for association in &result.associations {
        if interrupted() {
            unknown(ctx, output, "cochange_cancelled_or_deadline");
            return Ok(());
        }
        let file = ctx
            .input
            .snapshot
            .files
            .iter()
            .find(|f| f.previous_path.as_ref().unwrap_or(&f.path) == &association.from)
            .expect("changed baseline path");
        let mut anchor = CodeAnchor::new(AnchorKind::File);
        anchor.file_id = Some(file.id.clone());
        anchor.current_path = Some(file.path.clone());
        anchor.previous_path = file.previous_path.clone();
        anchor.diff_digest = Some(file.diff_digest.clone());
        let mut related = CodeAnchor::new(AnchorKind::File);
        related.current_path = Some(association.to.clone());
        // The partner is baseline context, not a fabricated PR file/line.
        let value = json!({
            "baseline": corpus(history), "association": association,
            "confidenceAtoB": association.support_ab as f64 / association.support_a as f64,
            "confidenceBtoA": association.support_ab as f64 / association.support_b as f64,
            "qualifyingPairs": result.qualifying_pairs, "emittedHints": result.associations.len(),
            "outputTruncated": truncated, "exampleLimit": 5,
        });
        let value_json = serde_json::to_string(&value)?;
        let evidence_id = EvidenceId::derive(&EvidenceKeyInput {
            kind: EvidenceKind::History,
            source_run_id: Some(&ctx.run_id),
            label: "Pinned co-change association",
            method: Some(cochange::METHOD),
            input_digest: Some(&history.base),
            value_json: Some(&value_json),
        });
        evidence.push(Raw::new(Evidence {
            schema_version: ANALYSIS_SCHEMA_VERSION,
            id: evidence_id.clone(),
            kind: EvidenceKind::History,
            source_run_id: Some(ctx.run_id.clone()),
            source_entity_id: None,
            anchor: Some(anchor.clone()),
            label: "Pinned co-change association".into(),
            value: Some(value.clone()),
            unit: Some("eligible commits".into()),
            numerator: Some(association.support_ab as f64),
            denominator: Some(association.support_a as f64),
            method: Some(cochange::METHOD.into()),
            input_digest: Some(history.base.clone()),
            observed_at: observed_at.into(),
            limitations: vec![LIMITATION.into()],
        })?);
        let claim = format!("{}:{}:{}", cochange::METHOD, ctx.run_id, value_json);
        let id = FindingId::derive(&FindingKeyInput {
            canonical_repo_id: &ctx.input.canonical_repo_id,
            pr_number: ctx.input.pr_number,
            provider_id: ID,
            rule_id: RULE,
            claim_identity: &claim,
            subject_identity: Some(&association.from),
            code_fingerprint: None,
        });
        let mut extensions = Map::new();
        extensions.insert("measurement".into(), value);
        findings.push(Raw::new(ReviewFinding {
            schema_version: ANALYSIS_SCHEMA_VERSION, id,
            repository_id: ctx.input.canonical_repo_id.clone(), pr_number: ctx.input.pr_number,
            observed_snapshot_id: ctx.input.snapshot_id.clone(),
            source: FindingSource { kind: SourceKind::AsideStatic, provider_id: ID.into(),
                run_id: Some(ctx.run_id.clone()), rule_id: Some(RULE.into()), rule_version: Some("1".into()),
                model_id: None, model_revision: None, prompt_version: None, native: None, github_actor: None },
            assertion_kind: AssertionKind::AttentionHint, kinds: vec![FindingKind::TemporalCoupling],
            title: format!("Historical co-change: {} → {}", association.from, association.to),
            body: format!("{} and {} changed together in {} of {} eligible changes to {} (reverse: {} of {}). {} is not in this PR. Consider whether it needs updating.",
                association.from, association.to, association.support_ab, association.support_a, association.from,
                association.support_ab, association.support_b, association.to),
            rationale: LIMITATION.into(), suggested_action: Some("Inspect the related file if relevant; this is not a missing-code claim.".into()),
            anchor, related_anchors: vec![related], evidence_ids: vec![evidence_id],
            attention: AttentionAssessment { level: AttentionLevel::Investigate, score: None, score_version: None, reasons: vec![RULE.into()] },
            severity: SeverityRecord { normalized: Severity::Info, original: None },
            confidence: Some(Confidence { kind: "measured_association".into(),
                value: association.support_ab as f64 / association.support_a as f64,
                denominator: Some(association.support_a as u32), calibration_version: None }),
            validation: ValidationRecord { status: ValidationStatus::NotRequired, method: Some(cochange::METHOD.into()), validated_at: None, note: None },
            applicability: Applicability::CurrentExact, source_status: SourceStatus::Open,
            first_seen_at: observed_at.into(), last_seen_at: observed_at.into(), external_url: None,
            parent_finding_id: None, correlation_group_id: None, extensions,
        })?);
        *counts.entry(association.from.clone()).or_default() += 1;
    }
    if interrupted() {
        unknown(ctx, output, "cochange_cancelled_or_deadline");
        return Ok(());
    }
    output.findings.extend(findings);
    output.evidence.extend(evidence);
    if truncated {
        output.status = RunStatus::Partial;
        output.coverage.completeness = Completeness::Partial;
        output
            .coverage
            .reason_codes
            .push("cochange_output_truncated".into());
        output.warnings.push(RunWarning { code: "cochange_output_truncated".into(),
            message: format!("Emitted {} of {} qualifying co-change pairs; output truncated, not history sampled.", result.associations.len(), result.qualifying_pairs) });
    }
    for file in &ctx.input.snapshot.files {
        let path = file.previous_path.as_ref().unwrap_or(&file.path);
        let reason = if history.ambiguous.contains(path) {
            Some("cochange_ambiguous_path")
        } else if !history.files.contains_key(path) {
            Some("not_a_baseline_regular_file")
        } else if history.transactions.is_empty() {
            Some("no_eligible_cochange_history")
        } else if truncated {
            Some("cochange_output_truncated")
        } else {
            None
        };
        if let Some(reason) = reason {
            output.status = RunStatus::Partial;
            output.coverage.completeness = Completeness::Partial;
            if !output.coverage.reason_codes.iter().any(|r| r == reason) {
                output.coverage.reason_codes.push(reason.into());
            }
        }
        output.coverage.entries.push(CoverageEntry {
            scope: CoverageScope::File {
                file_id: file.id.clone(),
                path: file.path.clone(),
            },
            rule_id: Some(RULE.into()),
            outcome: match reason {
                Some(reason) => ScopeOutcome::Unknown {
                    reason: reason.into(),
                },
                None => ScopeOutcome::Analyzed {
                    findings: counts.get(path).copied().unwrap_or(0),
                },
            },
        });
    }
    Ok(())
}
