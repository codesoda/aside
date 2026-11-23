//! Snapshot-scoped structural inventory from retained immutable Git objects.
use super::history::Worker;
use crate::{
    analysis::{
        AnalysisProvider, ProviderContext, ProviderDescriptor, ProviderKind, ProviderLimits,
        ProviderOutput,
    },
    history::reader,
    model::{
        ANALYSIS_SCHEMA_VERSION, AnalysisCoverage, AnalysisMode, AnchorKind, Applicability,
        AssertionKind, AttentionAssessment, AttentionLevel, CodeAnchor, Completeness,
        CoverageEntry, CoverageScope, Evidence, EvidenceId, EvidenceKeyInput, EvidenceKind,
        FindingId, FindingKeyInput, FindingKind, FindingRelation, FindingSource, KnownCount,
        RelationKind, RelationTarget, ReviewFinding, RunStatus, RunWarning, ScopeOutcome, Severity,
        SeverityRecord, Side, SourceKind, SourceStatus, ValidationRecord, ValidationStatus,
        structure::{Blob, StructureInput, StructureLimits, StructureOutput},
    },
    store::Raw,
    util::{now_rfc3339, sha256_hex},
};
use anyhow::Result;
use futures_util::{FutureExt, future::BoxFuture};
use serde_json::{Map, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub const ID: &str = "aside.structure";
const VERSION: &str = "aside.structure/1";
const METHOD: &str = "aside.structure.pinned@1";
pub struct StructureProvider {
    descriptor: ProviderDescriptor,
    #[cfg(test)]
    checkpoint: Option<Arc<dyn Fn() + Send + Sync>>,
}
#[cfg(test)]
impl StructureProvider {
    fn with_test_checkpoint(checkpoint: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            checkpoint: Some(checkpoint),
            ..Self::default()
        }
    }
}
impl Default for StructureProvider {
    fn default() -> Self {
        Self {
            #[cfg(test)]
            checkpoint: None,
            descriptor: ProviderDescriptor {
                id: ID.into(),
                kind: ProviderKind::Structural,
                engine_version: VERSION.into(),
                configuration_digest: sha256_hex(
                    format!("{VERSION}:{:?}", StructureLimits::default()).as_bytes(),
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
impl AnalysisProvider for StructureProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }
    fn run(&self, ctx: ProviderContext) -> BoxFuture<'static, Result<ProviderOutput>> {
        #[cfg(test)]
        let checkpoint = self.checkpoint.clone();
        async move {
            let cancel = Arc::new(AtomicBool::new(false));
            let (send, receive) = tokio::sync::oneshot::channel();
            let worker_cancel = Arc::clone(&cancel);
            let worker = Worker {
                handle: Some(std::thread::spawn(move || {
                    #[cfg(test)]
                    let result = scan_with_checkpoint(ctx, &worker_cancel, checkpoint.as_deref());
                    #[cfg(not(test))]
                    let result = scan_with_checkpoint(ctx, &worker_cancel);
                    let _ = send.send(result);
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
                .map(|f| CoverageEntry {
                    scope: CoverageScope::File {
                        file_id: f.id.clone(),
                        path: f.path.clone(),
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
            message: format!("Structural analysis unavailable: {reason}"),
        }],
        findings: vec![],
        evidence: vec![],
        relations: vec![],
    }
}
#[cfg(test)]
fn scan(ctx: ProviderContext, cancelled: &AtomicBool) -> Result<ProviderOutput> {
    scan_with_checkpoint(ctx, cancelled, None)
}
fn scan_with_checkpoint(
    ctx: ProviderContext,
    cancelled: &AtomicBool,
    #[cfg(test)] checkpoint: Option<&(dyn Fn() + Send + Sync)>,
) -> Result<ProviderOutput> {
    let Some(context) = ctx.input.history_context.as_ref() else {
        return Ok(partial(&ctx, "structure_context_unavailable"));
    };
    let Some(objects) = ctx.input.history_objects.as_ref() else {
        return Ok(partial(&ctx, "structure_objects_unavailable"));
    };
    // HEAD inventory remains useful without a comparison base. Deletion anchors
    // and base-derived relationships stay unavailable in that case.
    let base = context.comparison_base.as_deref();
    if context.base_sha != ctx.input.base_sha || context.head_sha != ctx.input.head_sha {
        return Ok(partial(&ctx, "structure_context_stale"));
    }
    let deadline = Instant::now() + Duration::from_secs(75);
    let read = || cancelled.load(Ordering::Acquire) || Instant::now() >= deadline;
    let (head_tree, excluded_paths) =
        match reader::tree_with_exclusions(&objects.dir, &ctx.input.head_sha, deadline) {
            Ok(v) => v,
            Err(_) => return Ok(partial(&ctx, "structure_head_tree_unavailable")),
        };
    let base_tree = base.and_then(|sha| reader::tree(&objects.dir, sha, deadline).ok());
    let base_tree_unavailable = base.is_some() && base_tree.is_none();
    if read() {
        return Ok(partial(&ctx, "structure_cancelled_or_deadline"));
    }
    let mut limits = StructureLimits::default();
    limits.max_results = limits
        .max_results
        .min(ctx.limits.max_findings.saturating_sub(1));
    let changed: std::collections::BTreeSet<_> = ctx
        .input
        .snapshot
        .files
        .iter()
        .map(|f| f.path.as_str())
        .collect();
    let mut paths: Vec<_> = head_tree
        .iter()
        .filter(|(p, _)| p.ends_with(".rs"))
        .collect();
    paths.sort_by_key(|(p, _)| (!changed.contains(p.as_str()), (*p).clone()));
    let mut head_blobs = Vec::new();
    let mut unreadable = Vec::new();
    let mut object_read_failed = false;
    for (path, sha) in paths.into_iter().take(limits.max_files) {
        if read() {
            return Ok(partial(&ctx, "structure_cancelled_or_deadline"));
        }
        match reader::blob_with_cancel(&objects.dir, sha, deadline, cancelled) {
            Ok(bytes) => head_blobs.push(Blob {
                path: path.clone(),
                bytes,
            }),
            Err(_) => {
                object_read_failed = true;
                unreadable.push(path.clone());
            }
        }
    }
    // Manifest bytes are pinned alongside source. The engine uses them to
    // establish conventional Cargo target roots, not as Rust parser input.
    if let Some(sha) = head_tree.get("Cargo.toml") {
        match reader::blob_with_cancel(&objects.dir, sha, deadline, cancelled) {
            Ok(bytes) => head_blobs.push(Blob {
                path: "Cargo.toml".into(),
                bytes,
            }),
            Err(_) => {
                object_read_failed = true;
                unreadable.push("Cargo.toml".into());
            }
        }
    } else {
        unreadable.push("Cargo.toml: missing manifest".into());
    }
    let mut base_blobs = Vec::new();
    if let Some(sha) = base_tree.as_ref().and_then(|tree| tree.get("Cargo.toml")) {
        match reader::blob_with_cancel(&objects.dir, sha, deadline, cancelled) {
            Ok(bytes) => base_blobs.push(Blob {
                path: "Cargo.toml".into(),
                bytes,
            }),
            Err(_) => {
                object_read_failed = true;
                unreadable.push("Cargo.toml: base manifest unavailable".into());
            }
        }
    }
    // Supply only the base declarations needed to scope changed children.
    // The base tree is already bounded by reader; never traverse its entire
    // source corpus or inspect the checkout.
    let mut base_paths = std::collections::BTreeSet::new();
    for file in &ctx.input.snapshot.files {
        let path = file.previous_path.as_deref().unwrap_or(&file.path);
        if !path.ends_with(".rs") {
            continue;
        }
        base_paths.insert(path.to_owned());
        // A changed module may itself have a competing base representation.
        // Supply both forms so base discovery cannot validate one by omission.
        if let Some(dir) = path.strip_suffix("/mod.rs") {
            base_paths.insert(format!("{dir}.rs"));
        } else if let Some(dir) = path.strip_suffix(".rs") {
            base_paths.insert(format!("{dir}/mod.rs"));
        }
        base_paths.insert("src/lib.rs".to_owned());
        base_paths.insert("src/main.rs".to_owned());
        let mut parent = path.rsplit_once('/').map(|(parent, _)| parent);
        while let Some(dir) = parent {
            if dir == "src" || dir == "src/bin" || !dir.starts_with("src/") {
                break;
            }
            base_paths.insert(format!("{dir}.rs"));
            base_paths.insert(format!("{dir}/mod.rs"));
            parent = dir.rsplit_once('/').map(|(parent, _)| parent);
        }
    }
    if base_paths.len() > limits.max_files {
        object_read_failed = true;
        unreadable.push("base layout: path limit reached".into());
    }
    for path in base_paths.into_iter().take(limits.max_files) {
        if read() {
            return Ok(partial(&ctx, "structure_cancelled_or_deadline"));
        }
        if let Some(sha) = base_tree.as_ref().and_then(|tree| tree.get(&path)) {
            match reader::blob_with_cancel(&objects.dir, sha, deadline, cancelled) {
                Ok(bytes) => base_blobs.push(Blob { path, bytes }),
                Err(_) => {
                    object_read_failed = true;
                    unreadable.push(path);
                }
            }
        }
    }
    if read() {
        return Ok(partial(&ctx, "structure_cancelled_or_deadline"));
    }
    #[cfg(test)]
    if let Some(checkpoint) = checkpoint {
        checkpoint();
    }
    if read() {
        return Ok(partial(&ctx, "structure_cancelled_or_deadline"));
    }
    let tree_paths = head_tree.keys().cloned().collect::<Vec<_>>();
    let mut result = crate::structure::analyze_with_corpus(
        &StructureInput {
            repository: ctx.input.canonical_repo_id.clone(),
            base_sha: base.unwrap_or(&ctx.input.base_sha).into(),
            head_sha: ctx.input.head_sha.clone(),
            head_blobs,
            base_blobs,
            changed_files: ctx.input.snapshot.files.clone(),
            limits,
        },
        &tree_paths,
        &read,
    );
    if read() {
        return Ok(partial(&ctx, "structure_cancelled_or_deadline"));
    }
    result.coverage.skipped_files.extend(unreadable);
    result.coverage.skipped_files.extend(excluded_paths);
    let rust_count = head_tree.keys().filter(|p| p.ends_with(".rs")).count();
    result.coverage.omitted_files = Some(rust_count.saturating_sub(limits.max_files));
    if base_tree.is_none() {
        result.coverage.limitations.push(
            if base_tree_unavailable {
                "structure_base_tree_unavailable"
            } else {
                "comparison_base_unavailable"
            }
            .into(),
        );
    }
    if object_read_failed {
        result
            .coverage
            .limitations
            .push("structure_object_read_failed".into());
    }
    if !ctx.input.snapshot.complete {
        result
            .coverage
            .limitations
            .push("structure_changed_file_enumeration_incomplete".into());
    }
    if !result.coverage.skipped_files.is_empty()
        || result.coverage.omitted_files != Some(0)
        || !context.complete
        || !ctx.input.snapshot.complete
        || base_tree.is_none()
    {
        result.coverage.complete = false;
        // A result cannot assert its reverse graph is complete if any pinned
        // corpus object was omitted. Preserve resolved paths, but not totals.
        for item in &mut result.results {
            item.complete = false;
            item.resolved_dependants = None;
            item.warnings.push("pinned_corpus_incomplete".into());
        }
    }
    present(&ctx, result)
}
fn anchor(
    ctx: &ProviderContext,
    path: &str,
    entity_id: Option<&str>,
    site: Option<&crate::model::structure::Site>,
) -> CodeAnchor {
    let file = ctx
        .input
        .snapshot
        .files
        .iter()
        .find(|f| f.path == path || f.previous_path.as_deref() == Some(path));
    let is_base = site.is_some_and(|s| {
        ctx.input
            .history_context
            .as_ref()
            .and_then(|c| c.comparison_base.as_deref())
            == Some(s.snapshot_sha.as_str())
    });
    let proven_site = site.filter(|s| is_base || s.snapshot_sha == ctx.input.head_sha);
    let mut a = CodeAnchor::new(if proven_site.is_some() && file.is_some() {
        AnchorKind::DiffLine
    } else if file.is_some() {
        AnchorKind::Entity
    } else {
        AnchorKind::RelatedContext
    });
    a.entity_id = entity_id.map(str::to_owned);
    a.current_path = Some(file.map_or(path, |f| f.path.as_str()).into());
    a.previous_path = file.and_then(|f| f.previous_path.clone());
    a.file_id = file.map(|f| f.id.clone());
    a.diff_digest = file.map(|f| f.diff_digest.clone());
    if let Some(s) = proven_site {
        a.side = Some(if is_base { Side::Left } else { Side::Right });
        a.line = Some(s.line);
        a.commit_sha = Some(s.snapshot_sha.clone());
    }
    a
}
fn present(ctx: &ProviderContext, result: StructureOutput) -> Result<ProviderOutput> {
    let at = now_rfc3339();
    let complete = result.coverage.complete;
    let mut coverage_value = serde_json::to_value(&result.coverage)?;
    if !ctx.input.snapshot.complete
        || ctx
            .input
            .snapshot
            .files
            .iter()
            .any(|file| !file.patch_complete)
    {
        // The engine can count omissions only among enumerated changed rows.
        // A truncated file listing or patch cannot establish a zero for missing entities.
        coverage_value["omitted_results"] = serde_json::Value::Null;
    }
    let mut output = ProviderOutput {
        status: if complete {
            RunStatus::Succeeded
        } else {
            RunStatus::Partial
        },
        coverage: AnalysisCoverage {
            completeness: if complete {
                Completeness::Complete
            } else {
                Completeness::Partial
            },
            eligible_files: KnownCount::Unknown,
            analyzed_files: KnownCount::Known {
                value: result.coverage.analyzed_files as u32,
            },
            eligible_entities: KnownCount::Unknown,
            analyzed_entities: KnownCount::Known {
                value: result.coverage.analyzed_entities as u32,
            },
            entries: vec![],
            reason_codes: if complete {
                vec![]
            } else {
                let mut reasons = vec!["structure_corpus_incomplete".into()];
                reasons.extend(
                    result
                        .coverage
                        .limitations
                        .iter()
                        .filter(|reason| {
                            matches!(
                                reason.as_str(),
                                "comparison_base_unavailable"
                                    | "structure_base_tree_unavailable"
                                    | "structure_object_read_failed"
                                    | "structure_changed_file_enumeration_incomplete"
                            )
                        })
                        .cloned(),
                );
                reasons
            },
        },
        warnings: vec![],
        findings: vec![],
        evidence: vec![],
        relations: vec![],
    };
    if !complete {
        output.warnings.push(RunWarning {
            code: "structure_corpus_incomplete".into(),
            message: serde_json::to_string(&coverage_value)?,
        });
    }
    for file in &ctx.input.snapshot.files {
        let analyzed = file.path.ends_with(".rs")
            && !result
                .coverage
                .skipped_files
                .iter()
                .any(|s| s.starts_with(&file.path));
        output.coverage.entries.push(CoverageEntry {
            scope: CoverageScope::File {
                file_id: file.id.clone(),
                path: file.path.clone(),
            },
            rule_id: Some(ID.into()),
            outcome: if analyzed {
                ScopeOutcome::Analyzed {
                    findings: result
                        .results
                        .iter()
                        .filter(|r| r.entity.path == file.path)
                        .count() as u32,
                }
            } else if !file.path.ends_with(".rs")
                && [
                    ".md", ".txt", ".json", ".toml", ".yaml", ".yml", ".lock", ".png", ".jpg",
                    ".svg", ".pdf",
                ]
                .iter()
                .any(|ext| file.path.ends_with(ext))
            {
                ScopeOutcome::NotApplicable {
                    reason: "non_source_file".into(),
                }
            } else {
                ScopeOutcome::Unknown {
                    reason: if file.path.ends_with(".rs") {
                        "source_unavailable"
                    } else {
                        "language_unsupported"
                    }
                    .into(),
                }
            },
        });
    }
    let corpus = json!({"coverage": coverage_value, "limitsDigest": result.limits_digest, "baseSha":result.base_sha, "headSha":result.head_sha});
    emit(
        ctx,
        &at,
        "corpus",
        "Pinned structural corpus",
        None,
        CodeAnchor::new(AnchorKind::Pr),
        corpus,
        vec![],
        &mut output,
    )?;
    for item in result.results {
        let a = anchor(
            ctx,
            &item.entity.path,
            Some(&item.entity.id),
            item.anchor.as_ref(),
        );
        let related = item
            .dependant_paths
            .iter()
            .flat_map(|p| p.entities.iter().skip(1))
            .map(|e| {
                let mut a = CodeAnchor::new(AnchorKind::RelatedContext);
                a.entity_id = Some(e.id.clone());
                a.current_path = Some(e.path.clone());
                a.commit_sha = Some(e.snapshot_sha.clone());
                a
            })
            .collect();
        let value = serde_json::to_value(&item)?;
        let id = emit(
            ctx,
            &at,
            "aside.structure.blast_radius",
            &format!("Structural relationships: {}", item.entity.qualified_name),
            Some(&item.entity.id),
            a,
            value,
            related,
            &mut output,
        )?;
        output.coverage.entries.push(CoverageEntry {
            scope: CoverageScope::Entity {
                entity_id: item.entity.id.clone(),
            },
            rule_id: Some(ID.into()),
            outcome: if item.complete {
                ScopeOutcome::Analyzed { findings: 1 }
            } else {
                ScopeOutcome::Unknown {
                    reason: item
                        .relationships_unavailable
                        .clone()
                        .unwrap_or_else(|| "relationships_incomplete".into()),
                }
            },
        });
        let direct: std::collections::BTreeSet<_> = item
            .dependant_paths
            .iter()
            .filter_map(|p| p.edges.first())
            .filter(|edge| edge.to == item.entity.id)
            .map(|edge| edge.from.clone())
            .collect();
        for dependent in direct {
            output.relations.push(FindingRelation {
                source_finding_id: id.clone(),
                kind: RelationKind::RelatedCode,
                target: RelationTarget::Entity(dependent),
                method: METHOD.into(),
                method_version: "1".into(),
                score: None,
                validated: true,
                created_at: at.clone(),
            });
        }
    }
    Ok(output)
}
#[allow(clippy::too_many_arguments)] // One immutable finding/evidence batch with shared snapshot provenance.
fn emit(
    ctx: &ProviderContext,
    at: &str,
    rule: &str,
    title: &str,
    entity: Option<&str>,
    anchor: CodeAnchor,
    value: serde_json::Value,
    related: Vec<CodeAnchor>,
    output: &mut ProviderOutput,
) -> Result<FindingId> {
    let text = serde_json::to_string(&value)?;
    let evidence_id = EvidenceId::derive(&EvidenceKeyInput {
        kind: EvidenceKind::CodeReference,
        source_run_id: Some(&ctx.run_id),
        label: title,
        method: Some(METHOD),
        input_digest: Some(&ctx.input.head_sha),
        value_json: Some(&text),
    });
    output.evidence.push(Raw::new(Evidence {
        schema_version: ANALYSIS_SCHEMA_VERSION,
        id: evidence_id.clone(),
        kind: EvidenceKind::CodeReference,
        source_run_id: Some(ctx.run_id.clone()),
        source_entity_id: entity.map(str::to_owned),
        anchor: Some(anchor.clone()),
        label: title.into(),
        value: Some(value),
        unit: None,
        numerator: None,
        denominator: None,
        method: Some(METHOD.into()),
        input_digest: Some(ctx.input.head_sha.clone()),
        observed_at: at.into(),
        limitations: vec![],
    })?);
    let id = FindingId::derive(&FindingKeyInput {
        canonical_repo_id: &ctx.input.canonical_repo_id,
        pr_number: ctx.input.pr_number,
        provider_id: ID,
        rule_id: rule,
        claim_identity: &format!("{}:{}", rule, text),
        subject_identity: entity,
        code_fingerprint: None,
    });
    output.findings.push(Raw::new(ReviewFinding {
        schema_version: ANALYSIS_SCHEMA_VERSION,
        id: id.clone(),
        repository_id: ctx.input.canonical_repo_id.clone(),
        pr_number: ctx.input.pr_number,
        observed_snapshot_id: ctx.input.snapshot_id.clone(),
        source: FindingSource {
            kind: SourceKind::AsideStatic,
            provider_id: ID.into(),
            run_id: Some(ctx.run_id.clone()),
            rule_id: Some(rule.into()),
            rule_version: Some("1".into()),
            model_id: None,
            model_revision: None,
            prompt_version: None,
            native: None,
            github_actor: None,
        },
        assertion_kind: if entity.is_some() {
            AssertionKind::MeasuredFact
        } else {
            AssertionKind::Informational
        },
        kinds: vec![if entity.is_some() {
            FindingKind::BlastRadius
        } else {
            FindingKind::Other
        }],
        title: title.into(),
        body: "Pinned Rust entity inventory and conservative in-corpus relationships.".into(),
        rationale: "Unresolved or out-of-corpus relationships are not inferred.".into(),
        suggested_action: None,
        anchor,
        related_anchors: related,
        evidence_ids: vec![evidence_id],
        attention: AttentionAssessment {
            level: AttentionLevel::None,
            score: None,
            score_version: None,
            reasons: vec![],
        },
        severity: SeverityRecord {
            normalized: Severity::Info,
            original: None,
        },
        confidence: None,
        validation: ValidationRecord {
            status: ValidationStatus::NotRequired,
            method: Some(METHOD.into()),
            validated_at: None,
            note: None,
        },
        applicability: Applicability::CurrentExact,
        source_status: SourceStatus::Open,
        first_seen_at: at.into(),
        last_seen_at: at.into(),
        external_url: None,
        parent_finding_id: None,
        correlation_group_id: None,
        extensions: Map::new(),
    })?);
    Ok(id)
}
#[cfg(test)]
#[path = "structure_cancellation_tests.rs"]
mod cancellation_tests;
#[cfg(test)]
#[path = "structure_tests.rs"]
mod tests;
