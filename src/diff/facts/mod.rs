//! Versioned, deterministic diff facts over a canonical PR snapshot (#19).
//!
//! [`scan`] is a plain, synchronous, side-effect-free function: the same
//! snapshot, input context and [`DiffFactsConfig`] always produce
//! byte-identical [`DiffFacts`] (stable IDs, stable ordering, no clock, no
//! randomness, no environment). The scheduler (TODO(#22)) owns runs,
//! persistence (#18) and API wiring (TODO(#33)); this module only computes.
//!
//! Design: `docs/plans/analyze.md` §6.1, §6.2, §6.11 and §10.5/§10.6.
//!
//! # Guarantees
//! - Every finding and evidence record names the rule and rule version that
//!   produced it (`source.ruleVersion`, `extensions.ruleVersion`,
//!   `evidence.method = "<rule>@<version>"`).
//! - Findings are only [`AssertionKind::MeasuredFact`] or
//!   [`AssertionKind::AttentionHint`]. Nothing here claims a defect.
//! - Line anchors are only emitted for real addition/deletion lines of the
//!   snapshot's hunks (RIGHT = new line of an addition, LEFT = old line of a
//!   deletion). Anything that cannot be located is file/PR anchored, or marked
//!   [`Applicability::Unavailable`] when the patch is known to be incomplete.
//! - Incomplete patches (`patch_complete == false`), binaries, generated
//!   files, oversized files and unsupported languages each get an explicit
//!   coverage outcome instead of a silent skip or a guessed fact.

#[cfg(test)]
mod boundary_tests;
mod classify;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Map, Value, json};

pub use classify::Language;
use classify::{ConfigKind, DependencyKind, TestLine};

use crate::model::{
    ANALYSIS_SCHEMA_VERSION, AnalysisCoverage, AnalysisRunId, AnchorKind, Applicability,
    AssertionKind, AttentionAssessment, AttentionLevel, ChangeKind, ChangedFile, CodeAnchor,
    Completeness, CoverageEntry, CoverageScope, Evidence, EvidenceId, EvidenceKeyInput,
    EvidenceKind, FindingId, FindingKeyInput, FindingKind, FindingSource, KnownCount, LineKind,
    PrSnapshot, ReviewFinding, RunWarning, ScopeOutcome, Severity, SeverityRecord, Side,
    SnapshotId, SourceKind, SourceStatus, ValidationRecord, ValidationStatus, stable_digest,
};
use crate::util::sha256_hex;

/// Provider ID for runs of this scan.
pub const PROVIDER_ID: &str = "aside.diff";
/// Engine version. Bump when any rule version or the output shape changes.
pub const ENGINE_VERSION: &str = "aside.diff-facts/1";

/// One deterministic rule and its immutable version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rule {
    pub id: &'static str,
    pub version: &'static str,
}

impl Rule {
    pub fn method(self) -> String {
        format!("{}@{}", self.id, self.version)
    }
}

pub mod rules {
    use super::Rule;

    pub const PR_SUMMARY: Rule = Rule {
        id: "aside.diff.pr_summary",
        version: "1",
    };
    pub const FILE_CHANGE: Rule = Rule {
        id: "aside.diff.file_change",
        version: "1",
    };
    pub const LARGE_CHANGE: Rule = Rule {
        id: "aside.diff.large_change",
        version: "1",
    };
    pub const GENERATED_FILE: Rule = Rule {
        id: "aside.diff.generated_file",
        version: "1",
    };
    pub const TESTS_TOUCHED: Rule = Rule {
        id: "aside.diff.tests_touched",
        version: "1",
    };
    pub const TEST_ASSERTIONS_ADDED: Rule = Rule {
        id: "aside.diff.test_assertions_added",
        version: "1",
    };
    pub const TESTS_REMOVED: Rule = Rule {
        id: "aside.diff.tests_removed",
        version: "1",
    };
    pub const PUBLIC_API: Rule = Rule {
        id: "aside.diff.schema_contract",
        version: "1",
    };
    pub const DEPENDENCY_MANIFEST: Rule = Rule {
        id: "aside.diff.dependency_manifest",
        version: "1",
    };
    pub const CONFIG_CHANGE: Rule = Rule {
        id: "aside.config.changed",
        version: "1",
    };

    /// Every rule, in output order.
    pub const ALL: &[Rule] = &[
        PR_SUMMARY,
        FILE_CHANGE,
        LARGE_CHANGE,
        GENERATED_FILE,
        TESTS_TOUCHED,
        TEST_ASSERTIONS_ADDED,
        TESTS_REMOVED,
        PUBLIC_API,
        DEPENDENCY_MANIFEST,
        CONFIG_CHANGE,
    ];
}

/// Tunable thresholds. Defaults are practical, not calibrated (§5.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffFactsConfig {
    /// Added + deleted lines in one file at or above which `large_change` fires.
    pub large_change_lines: u32,
    /// Files with more diff lines than this skip line-level rules (`not_run`).
    pub max_scan_lines_per_file: u32,
    /// Line evidence kept per finding; the full count is still measured.
    pub max_evidence_per_finding: u32,
    /// Run line-level rules on files with generated-file hints.
    pub scan_generated_files: bool,
}

impl Default for DiffFactsConfig {
    fn default() -> Self {
        Self {
            large_change_lines: 400,
            max_scan_lines_per_file: 10_000,
            max_evidence_per_finding: 20,
            scan_generated_files: false,
        }
    }
}

impl DiffFactsConfig {
    /// Digest of the canonical config JSON, for `AnalysisRun.configuration_digest`.
    pub fn digest(&self) -> String {
        let json = serde_json::to_string(self).expect("config serialises");
        sha256_hex(format!("{ENGINE_VERSION}\n{json}").as_bytes())
    }
}

/// Inputs the scheduler supplies with the snapshot. None of these are read
/// from the environment; `observed_at` is copied verbatim into timestamp
/// fields and never feeds an ID.
#[derive(Debug, Clone, Copy)]
pub struct DiffFactsInput<'a> {
    pub snapshot: &'a PrSnapshot,
    pub snapshot_id: &'a SnapshotId,
    pub canonical_repo_id: &'a str,
    /// Run the evidence belongs to, when the scheduler has created one.
    pub run_id: Option<&'a AnalysisRunId>,
    /// Timestamp for `observedAt`/`firstSeenAt`/`lastSeenAt` (run start).
    pub observed_at: &'a str,
}

/// Scan output: findings with their evidence and run coverage.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffFacts {
    pub provider_id: String,
    pub engine_version: String,
    pub configuration_digest: String,
    /// Digest of the snapshot content the scan read (file IDs + diff digests).
    pub input_digest: String,
    /// `analyzed`/`partial` coverage. Rule outcomes per PR and file.
    pub coverage: AnalysisCoverage,
    /// Sorted by rule order, then path, then ID.
    pub findings: Vec<ReviewFinding>,
    /// Sorted by ID; each is referenced by at least one finding.
    pub evidence: Vec<Evidence>,
    pub warnings: Vec<RunWarning>,
}

/// Digest of what the scan reads from the snapshot.
pub fn input_digest(snapshot: &PrSnapshot) -> String {
    let mut files: Vec<(&str, &str)> = snapshot
        .files
        .iter()
        .map(|f| (f.id.as_str(), f.diff_digest.as_str()))
        .collect();
    files.sort_unstable();
    let complete = if snapshot.complete { "1" } else { "0" };
    let mut fields = vec![
        Some(snapshot.pr.base_sha.as_str()),
        Some(snapshot.pr.head_sha.as_str()),
        Some(complete),
    ];
    for (id, digest) in &files {
        fields.push(Some(id));
        fields.push(Some(digest));
    }
    stable_digest("aside-diff-facts-input/v1", &fields)
}

/// One matched line of the diff, already resolved to a real anchor.
struct LineHit<'a> {
    file: &'a ChangedFile,
    side: Side,
    line: u32,
    kind: LineKind,
    text: &'a str,
    label: &'static str,
}

struct Emitter<'a> {
    input: DiffFactsInput<'a>,
    config: &'a DiffFactsConfig,
    findings: BTreeMap<(usize, String, String), ReviewFinding>,
    evidence: BTreeMap<String, Evidence>,
}

struct FindingSpec<'s> {
    rule: Rule,
    assertion: AssertionKind,
    kinds: Vec<FindingKind>,
    level: AttentionLevel,
    title: String,
    body: String,
    rationale: String,
    suggested_action: Option<String>,
    anchor: CodeAnchor,
    applicability: Applicability,
    claim: String,
    subject: Option<&'s str>,
    code_fingerprint: Option<String>,
    measurement: Value,
    limitations: Vec<String>,
    evidence_ids: Vec<EvidenceId>,
}

const MAX_SNIPPET_CHARS: usize = 200;

fn snippet(text: &str) -> String {
    let t = text.trim();
    match t.char_indices().nth(MAX_SNIPPET_CHARS) {
        Some((i, _)) => format!("{}…", &t[..i]),
        None => t.to_string(),
    }
}

fn file_anchor(file: &ChangedFile) -> CodeAnchor {
    let mut a = CodeAnchor::new(AnchorKind::File);
    a.file_id = Some(file.id.clone());
    a.current_path = Some(file.path.clone());
    a.previous_path = file.previous_path.clone();
    a.diff_digest = Some(file.diff_digest.clone());
    a
}

impl<'a> Emitter<'a> {
    fn rule_index(rule: Rule) -> usize {
        rules::ALL
            .iter()
            .position(|r| *r == rule)
            .unwrap_or(usize::MAX)
    }

    fn line_anchor(&self, hit: &LineHit<'_>) -> CodeAnchor {
        let mut a = file_anchor(hit.file);
        a.kind = AnchorKind::DiffLine;
        a.side = Some(hit.side);
        a.line = Some(hit.line);
        let pr = &self.input.snapshot.pr;
        a.commit_sha = Some(match hit.side {
            Side::Right => pr.head_sha.clone(),
            Side::Left => pr.base_sha.clone(),
        });
        a
    }

    fn add_evidence(
        &mut self,
        rule: Rule,
        kind: EvidenceKind,
        label: String,
        anchor: Option<CodeAnchor>,
        value: Value,
        limitations: Vec<String>,
    ) -> EvidenceId {
        let method = rule.method();
        let value_json = serde_json::to_string(&value).expect("value serialises");
        let input_digest = anchor.as_ref().and_then(|a| a.diff_digest.clone());
        let id = EvidenceId::derive(&EvidenceKeyInput {
            kind,
            source_run_id: self.input.run_id,
            label: &label,
            method: Some(&method),
            input_digest: input_digest.as_deref(),
            value_json: Some(&value_json),
        });
        self.evidence
            .entry(id.as_str().to_string())
            .or_insert_with(|| Evidence {
                schema_version: ANALYSIS_SCHEMA_VERSION,
                id: id.clone(),
                kind,
                source_run_id: self.input.run_id.cloned(),
                source_entity_id: None,
                anchor,
                label,
                value: Some(value),
                unit: None,
                numerator: None,
                denominator: None,
                method: Some(method),
                input_digest,
                observed_at: self.input.observed_at.to_string(),
                limitations,
            });
        id
    }

    fn measurement_evidence(
        &mut self,
        rule: Rule,
        label: String,
        anchor: CodeAnchor,
        value: Value,
        limitations: Vec<String>,
    ) -> EvidenceId {
        self.add_evidence(
            rule,
            EvidenceKind::Measurement,
            label,
            Some(anchor),
            value,
            limitations,
        )
    }

    /// Line evidence for up to `max_evidence_per_finding` hits.
    fn line_evidence(&mut self, rule: Rule, hits: &[LineHit<'_>]) -> (Vec<EvidenceId>, bool) {
        let cap = self.config.max_evidence_per_finding as usize;
        let mut ids = Vec::new();
        for hit in hits.iter().take(cap) {
            let anchor = self.line_anchor(hit);
            let change = match hit.kind {
                LineKind::Addition => "added",
                _ => "removed",
            };
            let value = json!({
                "side": hit.side.as_str(),
                "line": hit.line,
                "change": change,
                "match": hit.label,
                "snippet": snippet(hit.text),
            });
            ids.push(self.add_evidence(
                rule,
                EvidenceKind::CodeReference,
                format!("{} line: {}", change, hit.label),
                Some(anchor),
                value,
                Vec::new(),
            ));
        }
        (ids, hits.len() > cap)
    }

    fn emit(&mut self, spec: FindingSpec<'_>) {
        let input = self.input;
        let id = FindingId::derive(&FindingKeyInput {
            canonical_repo_id: input.canonical_repo_id,
            pr_number: input.snapshot.pr.number,
            provider_id: PROVIDER_ID,
            rule_id: spec.rule.id,
            claim_identity: &spec.claim,
            subject_identity: spec.subject,
            code_fingerprint: spec.code_fingerprint.as_deref(),
        });
        let mut extensions = Map::new();
        extensions.insert("ruleVersion".into(), json!(spec.rule.version));
        extensions.insert("engineVersion".into(), json!(ENGINE_VERSION));
        extensions.insert("measurement".into(), spec.measurement);
        extensions.insert("limitations".into(), json!(spec.limitations));
        let mut evidence_ids = spec.evidence_ids;
        evidence_ids.sort();
        evidence_ids.dedup();
        let finding = ReviewFinding {
            schema_version: ANALYSIS_SCHEMA_VERSION,
            id: id.clone(),
            repository_id: input.canonical_repo_id.to_string(),
            pr_number: input.snapshot.pr.number,
            observed_snapshot_id: input.snapshot_id.clone(),
            source: FindingSource {
                kind: SourceKind::AsideStatic,
                provider_id: PROVIDER_ID.into(),
                run_id: input.run_id.cloned(),
                rule_id: Some(spec.rule.id.into()),
                rule_version: Some(spec.rule.version.into()),
                model_id: None,
                model_revision: None,
                prompt_version: None,
                native: None,
                github_actor: None,
            },
            assertion_kind: spec.assertion,
            kinds: spec.kinds,
            title: spec.title,
            body: spec.body,
            rationale: spec.rationale,
            suggested_action: spec.suggested_action,
            anchor: spec.anchor,
            related_anchors: Vec::new(),
            evidence_ids,
            attention: AttentionAssessment {
                level: spec.level,
                score: None,
                score_version: None,
                reasons: vec![spec.rule.id.to_string()],
            },
            severity: SeverityRecord {
                normalized: Severity::Info,
                original: None,
            },
            confidence: None,
            validation: ValidationRecord {
                status: ValidationStatus::NotRequired,
                method: Some(spec.rule.method()),
                validated_at: None,
                note: None,
            },
            applicability: spec.applicability,
            source_status: SourceStatus::Open,
            first_seen_at: input.observed_at.to_string(),
            last_seen_at: input.observed_at.to_string(),
            external_url: None,
            parent_finding_id: None,
            correlation_group_id: None,
            extensions,
        };
        let path = finding.anchor.current_path.clone().unwrap_or_default();
        self.findings
            .entry((Self::rule_index(spec.rule), path, id.as_str().to_string()))
            .or_insert(finding);
    }
}

/// How the file changed, using the §6.2 vocabulary.
pub fn operation(file: &ChangedFile) -> &'static str {
    let submodule = |m: &Option<String>| m.as_deref() == Some("160000");
    if submodule(&file.old_mode) || submodule(&file.new_mode) {
        return "submodule";
    }
    match file.change_kind {
        ChangeKind::Added => "added",
        ChangeKind::Removed => "deleted",
        ChangeKind::Renamed => "renamed",
        ChangeKind::Copied => "copied",
        ChangeKind::Modified if file.is_binary => "binary",
        ChangeKind::Modified
            if file.hunks.is_empty()
                && file.old_mode.is_some()
                && file.old_mode != file.new_mode =>
        {
            "mode-only"
        }
        ChangeKind::Modified => "modified",
    }
}

fn diff_line_count(file: &ChangedFile) -> usize {
    file.hunks.iter().map(|h| h.lines.len()).sum()
}

/// Real addition/deletion lines with resolvable coordinates, in diff order.
fn changed_lines(file: &ChangedFile) -> impl Iterator<Item = (Side, u32, LineKind, &str)> {
    file.hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter_map(|l| match l.kind {
            LineKind::Addition => l
                .new_line
                .map(|n| (Side::Right, n, l.kind, l.text.as_str())),
            LineKind::Deletion => l.old_line.map(|n| (Side::Left, n, l.kind, l.text.as_str())),
            _ => None,
        })
}

fn code_fingerprint(hits: &[LineHit<'_>]) -> String {
    // Line numbers are excluded so a pure shift keeps the finding ID.
    let mut parts: Vec<String> = hits
        .iter()
        .map(|h| format!("{}|{}|{}", h.side.as_str(), h.label, h.text))
        .collect();
    parts.sort_unstable();
    sha256_hex(parts.join("\n").as_bytes())
}

/// Why line-level rules cannot run on a file, if they cannot.
fn content_gate(
    file: &ChangedFile,
    generated: bool,
    config: &DiffFactsConfig,
) -> Option<ScopeOutcome> {
    if file.is_binary {
        Some(ScopeOutcome::NotApplicable {
            reason: "binary_file".into(),
        })
    } else if !file.patch_complete {
        Some(ScopeOutcome::Unknown {
            reason: "patch_incomplete".into(),
        })
    } else if operation(file) == "mode-only" {
        // No content changed: line rules have nothing to read, which is not
        // the same as "read it and found nothing" (#36).
        Some(ScopeOutcome::NotApplicable {
            reason: "mode_only".into(),
        })
    } else if generated && !config.scan_generated_files {
        Some(ScopeOutcome::NotRun {
            reason: "generated_file".into(),
        })
    } else if diff_line_count(file) > config.max_scan_lines_per_file as usize {
        Some(ScopeOutcome::NotRun {
            reason: "file_too_large".into(),
        })
    } else {
        None
    }
}

fn file_scope(file: &ChangedFile) -> CoverageScope {
    CoverageScope::File {
        file_id: file.id.clone(),
        path: file.path.clone(),
    }
}

/// Scan `input.snapshot` and return versioned measured facts and attention
/// hints with evidence. Pure: no I/O, clock, randomness or global state.
pub fn scan(input: &DiffFactsInput<'_>, config: &DiffFactsConfig) -> DiffFacts {
    let snapshot = input.snapshot;
    let mut em = Emitter {
        input: *input,
        config,
        findings: BTreeMap::new(),
        evidence: BTreeMap::new(),
    };
    let mut entries: Vec<CoverageEntry> = Vec::new();
    let mut reason_codes: Vec<String> = Vec::new();
    let mut warnings: Vec<RunWarning> = Vec::new();

    // Canonical order, independent of how the snapshot listed files.
    let mut files: Vec<&ChangedFile> = snapshot.files.iter().collect();
    files.sort_by(|a, b| {
        (&a.path, &a.previous_path, &a.diff_digest).cmp(&(
            &b.path,
            &b.previous_path,
            &b.diff_digest,
        ))
    });

    if !snapshot.complete {
        reason_codes.push("snapshot_incomplete".into());
        warnings.push(RunWarning {
            code: "snapshot_incomplete".into(),
            message: "The snapshot is not verified complete; PR totals are unknown and \
                      incomplete files get no line-level facts."
                .into(),
        });
    }

    let mut test_files: Vec<&ChangedFile> = Vec::new();
    let mut analyzed_files = 0u32;

    for file in &files {
        let language = classify::language(&file.path);
        let is_test = classify::is_test_path(&file.path);
        let mut generated_hints: Vec<&str> = classify::generated_path_hints(&file.path);
        if file.patch_complete
            && file
                .hunks
                .first()
                .into_iter()
                .flat_map(|h| &h.lines)
                .take(5)
                .any(|l| l.kind != LineKind::Deletion && classify::is_generated_marker(&l.text))
        {
            generated_hints.push("generated_header");
        }
        let generated = !generated_hints.is_empty();
        if is_test {
            test_files.push(file);
        }

        let mut limitations = Vec::new();
        let counts_known = file.patch_complete && !file.is_binary;
        if file.is_binary {
            limitations.push("binary file: line counts and content are not available".to_string());
        } else if !file.patch_complete {
            limitations.push(
                "patch incomplete: line counts are not verified and content was not scanned"
                    .to_string(),
            );
        }
        if language.is_none() {
            limitations.push("language not supported by line-level rules".into());
        }
        let applicability = if file.patch_complete {
            Applicability::CurrentExact
        } else {
            Applicability::Unavailable
        };

        // ---- file_change (always) ----
        let measurement = json!({
            "operation": operation(file),
            "changeKind": file.change_kind.as_str(),
            "previousPath": file.previous_path,
            "oldMode": file.old_mode,
            "newMode": file.new_mode,
            "isBinary": file.is_binary,
            "patchComplete": file.patch_complete,
            "additions": counts_known.then_some(file.additions),
            "deletions": counts_known.then_some(file.deletions),
            "hunks": file.patch_complete.then_some(file.hunks.len()),
            "language": language.map(Language::as_str),
            "isTest": is_test,
            "generatedHints": generated_hints,
        });
        let ev = em.measurement_evidence(
            rules::FILE_CHANGE,
            "file change metadata".into(),
            file_anchor(file),
            measurement.clone(),
            limitations.clone(),
        );
        let counts = if counts_known {
            format!(
                "+{} −{} in {} hunk(s)",
                file.additions,
                file.deletions,
                file.hunks.len()
            )
        } else {
            "line counts unknown".to_string()
        };
        em.emit(FindingSpec {
            rule: rules::FILE_CHANGE,
            assertion: AssertionKind::MeasuredFact,
            kinds: vec![FindingKind::Other],
            level: AttentionLevel::None,
            title: format!("{} ({})", file.path, operation(file)),
            body: counts,
            rationale: "Measured from the snapshot diff.".into(),
            suggested_action: None,
            anchor: file_anchor(file),
            applicability,
            claim: "file_change".into(),
            subject: Some(&file.path),
            code_fingerprint: Some(file.diff_digest.clone()),
            measurement,
            limitations: limitations.clone(),
            evidence_ids: vec![ev],
        });

        // ---- large_change ----
        let changed = file.additions + file.deletions;
        if counts_known && changed >= config.large_change_lines {
            let measurement = json!({
                "changedLines": changed,
                "additions": file.additions,
                "deletions": file.deletions,
                "threshold": config.large_change_lines,
                "thresholdSource": "config.largeChangeLines",
            });
            let ev = em.measurement_evidence(
                rules::LARGE_CHANGE,
                "changed lines".into(),
                file_anchor(file),
                measurement.clone(),
                Vec::new(),
            );
            em.emit(FindingSpec {
                rule: rules::LARGE_CHANGE,
                assertion: AssertionKind::AttentionHint,
                kinds: vec![FindingKind::Other],
                level: AttentionLevel::Info,
                title: format!("Large change: {changed} lines"),
                body: format!(
                    "{} has {changed} changed lines (threshold {}).",
                    file.path, config.large_change_lines
                ),
                rationale: "Size alone is low-certainty; large changes are harder to review."
                    .into(),
                suggested_action: Some("Consider reviewing this file in smaller passes.".into()),
                anchor: file_anchor(file),
                applicability,
                claim: "large_change".into(),
                subject: Some(&file.path),
                code_fingerprint: Some(file.diff_digest.clone()),
                measurement,
                limitations: vec!["file size unknown: changed-line fraction not computed".into()],
                evidence_ids: vec![ev],
            });
        } else if !counts_known {
            entries.push(CoverageEntry {
                scope: file_scope(file),
                rule_id: Some(rules::LARGE_CHANGE.id.into()),
                outcome: ScopeOutcome::Unknown {
                    reason: if file.is_binary {
                        "binary_file".into()
                    } else {
                        "patch_incomplete".into()
                    },
                },
            });
        }

        // ---- generated_file ----
        if generated {
            let measurement = json!({ "hints": generated_hints });
            let ev = em.measurement_evidence(
                rules::GENERATED_FILE,
                "generated-file hints".into(),
                file_anchor(file),
                measurement.clone(),
                Vec::new(),
            );
            em.emit(FindingSpec {
                rule: rules::GENERATED_FILE,
                assertion: AssertionKind::MeasuredFact,
                kinds: vec![FindingKind::Other],
                level: AttentionLevel::Info,
                title: "Looks generated".into(),
                body: format!(
                    "{} matched generated-file hints: {}.",
                    file.path,
                    generated_hints.join(", ")
                ),
                rationale: "Hints are heuristics from the path or a generated-code header.".into(),
                suggested_action: Some("Check the generator input rather than the output.".into()),
                anchor: file_anchor(file),
                applicability,
                claim: "generated_file".into(),
                subject: Some(&file.path),
                code_fingerprint: None,
                measurement,
                limitations: Vec::new(),
                evidence_ids: vec![ev],
            });
        }

        // ---- dependency / config (path based, any language) ----
        if let Some(dep) = classify::dependency_kind(&file.path) {
            path_rule(&mut em, file, PathRule::Dependency(dep), applicability);
        }
        if let Some(cfg) = classify::config_kind(&file.path) {
            path_rule(&mut em, file, PathRule::Config(cfg), applicability);
        }

        // ---- line-level content rules ----
        let content_rules = [
            rules::TEST_ASSERTIONS_ADDED,
            rules::TESTS_REMOVED,
            rules::PUBLIC_API,
        ];
        let gate = content_gate(file, generated, config).or_else(|| {
            language.is_none().then(|| ScopeOutcome::Unknown {
                reason: "unsupported_language".into(),
            })
        });
        if let Some(outcome) = gate {
            if !file.patch_complete {
                reason_codes.push("patch_incomplete".into());
            }
            if matches!(&outcome, ScopeOutcome::Unknown { reason } if reason == "unsupported_language")
            {
                reason_codes.push("unsupported_language".into());
            }
            for rule in content_rules {
                entries.push(CoverageEntry {
                    scope: file_scope(file),
                    rule_id: Some(rule.id.into()),
                    outcome: outcome.clone(),
                });
            }
            continue;
        }
        let language = language.expect("gated above");
        analyzed_files += 1;

        let mut added_tests = Vec::new();
        let mut removed_tests = Vec::new();
        let mut api_hits = Vec::new();
        let scan_tests = classify::scans_test_lines(language, is_test);
        for (side, line, kind, text) in changed_lines(file) {
            if scan_tests && let Some(t) = classify::test_line(text, language) {
                let hit = LineHit {
                    file,
                    side,
                    line,
                    kind,
                    text,
                    label: t.as_str(),
                };
                if kind == LineKind::Addition {
                    added_tests.push(hit);
                } else {
                    removed_tests.push(hit);
                }
            }
            if !is_test && classify::public_signature(text, language) {
                api_hits.push(LineHit {
                    file,
                    side,
                    line,
                    kind,
                    text,
                    label: "public_signature",
                });
            }
        }

        let tests_n = test_rule(
            &mut em,
            file,
            language,
            is_test,
            rules::TEST_ASSERTIONS_ADDED,
            &added_tests,
            removed_tests.len(),
        );
        let removed_n = test_rule(
            &mut em,
            file,
            language,
            is_test,
            rules::TESTS_REMOVED,
            &removed_tests,
            added_tests.len(),
        );
        let api_n = api_rule(&mut em, file, language, &api_hits);
        for (rule, n, applicable) in [
            (rules::TEST_ASSERTIONS_ADDED, tests_n, scan_tests),
            (rules::TESTS_REMOVED, removed_n, scan_tests),
            (rules::PUBLIC_API, api_n, !is_test),
        ] {
            entries.push(CoverageEntry {
                scope: file_scope(file),
                rule_id: Some(rule.id.into()),
                outcome: if applicable {
                    ScopeOutcome::Analyzed { findings: n }
                } else {
                    ScopeOutcome::NotApplicable {
                        reason: if is_test {
                            "test_file".into()
                        } else {
                            "not_a_test_file".into()
                        },
                    }
                },
            });
        }
    }

    // ---- PR-level facts ----
    pr_summary(&mut em, &files);
    if !test_files.is_empty() {
        let paths: Vec<&str> = test_files.iter().map(|f| f.path.as_str()).collect();
        let measurement = json!({ "testFiles": paths.len(), "paths": paths });
        let ev = em.measurement_evidence(
            rules::TESTS_TOUCHED,
            "test files touched".into(),
            CodeAnchor::new(AnchorKind::Pr),
            measurement.clone(),
            Vec::new(),
        );
        em.emit(FindingSpec {
            rule: rules::TESTS_TOUCHED,
            assertion: AssertionKind::MeasuredFact,
            kinds: vec![FindingKind::TestsChanged],
            level: AttentionLevel::None,
            title: format!("{} test file(s) touched", paths.len()),
            body: paths.join("\n"),
            rationale: "Test files detected by path convention.".into(),
            suggested_action: None,
            anchor: CodeAnchor::new(AnchorKind::Pr),
            applicability: Applicability::CurrentExact,
            claim: "tests_touched".into(),
            subject: None,
            code_fingerprint: Some(sha256_hex(paths.join("\n").as_bytes())),
            measurement,
            limitations: vec!["path-convention detection only".into()],
            evidence_ids: vec![ev],
        });
    }
    entries.push(CoverageEntry {
        scope: CoverageScope::Pr,
        rule_id: Some(rules::TESTS_TOUCHED.id.into()),
        outcome: if snapshot.complete || !test_files.is_empty() {
            ScopeOutcome::Analyzed {
                findings: u32::from(!test_files.is_empty()),
            }
        } else {
            // An incomplete snapshot cannot prove no test file changed.
            ScopeOutcome::Unknown {
                reason: "snapshot_incomplete".into(),
            }
        },
    });

    reason_codes.sort();
    reason_codes.dedup();
    warnings.sort_by(|a, b| (&a.code, &a.message).cmp(&(&b.code, &b.message)));

    let completeness = if reason_codes.is_empty() {
        Completeness::Complete
    } else {
        Completeness::Partial
    };
    let findings: Vec<ReviewFinding> = em.findings.into_values().collect();
    DiffFacts {
        provider_id: PROVIDER_ID.into(),
        engine_version: ENGINE_VERSION.into(),
        configuration_digest: config.digest(),
        input_digest: input_digest(snapshot),
        coverage: AnalysisCoverage {
            completeness,
            eligible_files: KnownCount::Known {
                value: files.len() as u32,
            },
            analyzed_files: KnownCount::Known {
                value: analyzed_files,
            },
            eligible_entities: KnownCount::Unknown,
            analyzed_entities: KnownCount::Unknown,
            entries,
            reason_codes,
        },
        findings,
        evidence: em.evidence.into_values().collect(),
        warnings,
    }
}

fn pr_summary(em: &mut Emitter<'_>, files: &[&ChangedFile]) {
    let snapshot = em.input.snapshot;
    let totals_known = snapshot.complete && files.iter().all(|f| f.patch_complete && !f.is_binary);
    let sum =
        |f: fn(&ChangedFile) -> u32| totals_known.then(|| files.iter().map(|x| f(x)).sum::<u32>());
    let mut ops: BTreeMap<&str, u32> = BTreeMap::new();
    for f in files {
        *ops.entry(operation(f)).or_default() += 1;
    }
    let binary = files.iter().filter(|f| f.is_binary).count();
    let incomplete = files.iter().filter(|f| !f.patch_complete).count();
    let additions = sum(|f| f.additions);
    let deletions = sum(|f| f.deletions);
    let measurement = json!({
        "changedFiles": snapshot.complete.then_some(files.len()),
        "filesListed": files.len(),
        "additions": additions,
        "deletions": deletions,
        "hunks": totals_known.then(|| files.iter().map(|f| f.hunks.len()).sum::<usize>()),
        "operations": ops,
        "binaryFiles": binary,
        "incompleteFiles": incomplete,
        "snapshotComplete": snapshot.complete,
    });
    let mut limitations = Vec::new();
    if !snapshot.complete {
        limitations.push("snapshot incomplete: file count and totals are unknown".to_string());
    } else if !totals_known {
        limitations.push("binary or incomplete files: line totals are unknown".to_string());
    }
    let ev = em.measurement_evidence(
        rules::PR_SUMMARY,
        "PR diff totals".into(),
        CodeAnchor::new(AnchorKind::Pr),
        measurement.clone(),
        limitations.clone(),
    );
    let body = match (additions, deletions) {
        (Some(a), Some(d)) => format!("{} files, +{a} −{d}", files.len()),
        _ => format!("{} files listed; line totals unknown", files.len()),
    };
    em.emit(FindingSpec {
        rule: rules::PR_SUMMARY,
        assertion: AssertionKind::MeasuredFact,
        kinds: vec![FindingKind::Other],
        level: AttentionLevel::None,
        title: "Diff summary".into(),
        body,
        rationale: "Measured from the snapshot diff.".into(),
        suggested_action: None,
        anchor: CodeAnchor::new(AnchorKind::Pr),
        applicability: if snapshot.complete {
            Applicability::CurrentExact
        } else {
            Applicability::Unavailable
        },
        claim: "pr_summary".into(),
        subject: None,
        code_fingerprint: Some(input_digest(snapshot)),
        measurement,
        limitations,
        evidence_ids: vec![ev],
    });
}

enum PathRule {
    Dependency(DependencyKind),
    Config(ConfigKind),
}

fn path_rule(
    em: &mut Emitter<'_>,
    file: &ChangedFile,
    kind: PathRule,
    applicability: Applicability,
) {
    let (rule, category, level, finding_kind, title, rationale, action) = match kind {
        PathRule::Dependency(d) => (
            rules::DEPENDENCY_MANIFEST,
            d.as_str(),
            match d {
                DependencyKind::Manifest => AttentionLevel::Investigate,
                DependencyKind::Lockfile => AttentionLevel::Info,
            },
            FindingKind::Dependency,
            match d {
                DependencyKind::Manifest => "Dependency manifest changed",
                DependencyKind::Lockfile => "Lockfile changed",
            },
            "Dependency declarations changed; lockfile changes are often expected.",
            "Check added, removed or re-versioned dependencies.",
        ),
        PathRule::Config(c) => (
            rules::CONFIG_CHANGE,
            c.as_str(),
            match c {
                ConfigKind::Ci => AttentionLevel::Investigate,
                _ => AttentionLevel::Info,
            },
            match c {
                ConfigKind::Ci | ConfigKind::Environment => FindingKind::Security,
                _ => FindingKind::Contract,
            },
            match c {
                ConfigKind::Ci => "CI configuration changed",
                ConfigKind::Container => "Container configuration changed",
                ConfigKind::Environment => "Environment file changed",
                ConfigKind::Application => "Configuration changed",
            },
            "Configuration can change behaviour even with low churn.",
            "Check what the configuration change enables or disables.",
        ),
    };
    let mut evidence_ids = Vec::new();
    let mut limitations = Vec::new();
    let mut truncated = false;
    let mut changed = 0usize;
    let mut fp = Some(file.diff_digest.clone());
    // Environment files may hold secrets: never copy their lines into output.
    let redact = matches!(kind, PathRule::Config(ConfigKind::Environment));
    if file.patch_complete && !file.is_binary && !redact {
        let hits: Vec<LineHit<'_>> = changed_lines(file)
            .filter(|(_, _, _, t)| !t.trim().is_empty())
            .map(|(side, line, kind, text)| LineHit {
                file,
                side,
                line,
                kind,
                text,
                label: category,
            })
            .collect();
        changed = hits.len();
        let (ids, t) = em.line_evidence(rule, &hits);
        evidence_ids = ids;
        truncated = t;
        fp = Some(code_fingerprint(&hits));
    } else if redact {
        limitations.push("environment file: line content withheld".to_string());
    } else if file.is_binary {
        limitations.push("binary file: no line evidence".to_string());
    } else {
        limitations.push("patch incomplete: no line evidence".to_string());
    }
    if truncated {
        limitations.push("line evidence truncated to config.maxEvidencePerFinding".to_string());
    }
    let measurement = json!({
        "category": category,
        "operation": operation(file),
        "changedLines": (file.patch_complete && !file.is_binary && !redact).then_some(changed),
        "evidenceTruncated": truncated,
    });
    evidence_ids.push(em.measurement_evidence(
        rule,
        format!("{category} file changed"),
        file_anchor(file),
        measurement.clone(),
        limitations.clone(),
    ));
    em.emit(FindingSpec {
        rule,
        assertion: AssertionKind::AttentionHint,
        kinds: vec![finding_kind],
        level,
        title: title.into(),
        body: format!("{} ({category}, {}).", file.path, operation(file)),
        rationale: rationale.into(),
        suggested_action: Some(action.into()),
        anchor: file_anchor(file),
        applicability,
        claim: format!("{}:{category}", rule.id),
        subject: Some(&file.path),
        code_fingerprint: fp,
        measurement,
        limitations,
        evidence_ids,
    });
}

/// Emits `rule` for `hits` (if any) and returns the number of findings.
fn test_rule(
    em: &mut Emitter<'_>,
    file: &ChangedFile,
    language: Language,
    is_test: bool,
    rule: Rule,
    hits: &[LineHit<'_>],
    opposite: usize,
) -> u32 {
    if hits.is_empty() {
        return 0;
    }
    let count = |t: TestLine| hits.iter().filter(|h| h.label == t.as_str()).count();
    let assertions = count(TestLine::Assertion);
    let definitions = count(TestLine::TestDefinition);
    let removed = rule == rules::TESTS_REMOVED;
    let (ids, truncated) = em.line_evidence(rule, hits);
    let mut limitations = vec![format!(
        "syntactic {} matching, not an AST; replacements are not tracked",
        language.as_str()
    )];
    if truncated {
        limitations.push("line evidence truncated to config.maxEvidencePerFinding".into());
    }
    let mut measurement = json!({
        "language": language.as_str(),
        "isTestFile": is_test,
        "assertions": assertions,
        "testDefinitions": definitions,
        "evidenceTruncated": truncated,
    });
    let opposite_key = if removed {
        "addedInSameFile"
    } else {
        "removedInSameFile"
    };
    measurement[opposite_key] = json!(opposite);
    let (title, assertion, level, rationale, action) = if removed {
        (
            format!("Tests removed: {assertions} assertion(s), {definitions} test definition(s)"),
            AssertionKind::AttentionHint,
            AttentionLevel::Investigate,
            "Assertions or test definitions were deleted; this alone does not mean coverage was lost.",
            Some("Check whether the removed checks were replaced or moved.".to_string()),
        )
    } else {
        (
            format!("Tests added: {assertions} assertion(s), {definitions} test definition(s)"),
            AssertionKind::MeasuredFact,
            AttentionLevel::None,
            "Assertions or test definitions were added.",
            None,
        )
    };
    em.emit(FindingSpec {
        rule,
        assertion,
        kinds: vec![FindingKind::TestsChanged],
        level,
        title,
        body: format!(
            "{} in {}; {} line(s) in the opposite direction.",
            hits.len(),
            file.path,
            opposite
        ),
        rationale: rationale.into(),
        suggested_action: action,
        anchor: file_anchor(file),
        applicability: Applicability::CurrentExact,
        claim: rule.id.into(),
        subject: Some(&file.path),
        code_fingerprint: Some(code_fingerprint(hits)),
        measurement,
        limitations,
        evidence_ids: ids,
    });
    1
}

fn api_rule(
    em: &mut Emitter<'_>,
    file: &ChangedFile,
    language: Language,
    hits: &[LineHit<'_>],
) -> u32 {
    if hits.is_empty() {
        return 0;
    }
    let added = hits.iter().filter(|h| h.kind == LineKind::Addition).count();
    let removed = hits.len() - added;
    let (ids, truncated) = em.line_evidence(rules::PUBLIC_API, hits);
    let mut limitations = vec![format!(
        "syntactic {} matching: cannot tell crate/package-private exports or pure moves",
        language.as_str()
    )];
    if truncated {
        limitations.push("line evidence truncated to config.maxEvidencePerFinding".into());
    }
    em.emit(FindingSpec {
        rule: rules::PUBLIC_API,
        assertion: AssertionKind::AttentionHint,
        kinds: vec![FindingKind::PublicApi],
        level: AttentionLevel::Info,
        title: format!("Public signatures changed: +{added} −{removed}"),
        body: format!(
            "{} public-looking declaration line(s) changed in {}.",
            hits.len(),
            file.path
        ),
        rationale: "Signature-looking public declarations were added or removed.".into(),
        suggested_action: Some("Check callers and compatibility of changed signatures.".into()),
        anchor: file_anchor(file),
        applicability: Applicability::CurrentExact,
        claim: rules::PUBLIC_API.id.into(),
        subject: Some(&file.path),
        code_fingerprint: Some(code_fingerprint(hits)),
        measurement: json!({
            "language": language.as_str(),
            "added": added,
            "removed": removed,
            "evidenceTruncated": truncated,
        }),
        limitations,
        evidence_ids: ids,
    });
    1
}
