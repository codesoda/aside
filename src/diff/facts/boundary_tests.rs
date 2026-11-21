//! Boundary golden fixtures for diff facts (#36, analyze.md §19.1, §19.2).
//!
//! Every `fixtures/analysis/*/pr.diff` directory is a synthetic PR:
//!
//! - `manifest.json`: [`BoundaryManifest`] (snapshot identity and expectations);
//! - `pr.diff`: the unified diff, parsed with the production parser;
//! - `expected-facts.json`: the golden, byte-exact [`DiffFacts`] serialisation;
//! - `expected-labels.json`: a hand annotation of every emitted label
//!   ([`LabelAnnotation`]) with an `appropriate`/`questionable` verdict.
//!
//! The goldens are regenerated with `ASIDE_UPDATE_GOLDEN=1 cargo test`;
//! review the diff before committing. Labels are never regenerated: they are
//! the human evaluation, and the test fails when emitted labels and
//! annotations disagree.
//!
//! Round-trip choice (carried-forward constraint, #15/#18/#23): goldens are
//! decoded through the *typed* contract structs, not `Raw<T>` or plain
//! strings. `#[serde(other)]` decodes unknown enum values lossily, so a value
//! the Rust types no longer know re-serialises differently and fails
//! [`assert_exact`] as drift instead of being silently preserved.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

use super::*;
use crate::{
    diff::{Limits, parse_unified},
    model::{
        FileScope, FileState, FileStatus, OverallCoverage, RunStatus, SourceCoverage, SourceState,
        classify, overall,
    },
    priority::{self, Bucket, ObservedFinding, QueueInput, QueueVerdict, ReasonCode},
};

const UPDATE_ENV: &str = "ASIDE_UPDATE_GOLDEN";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BoundaryManifest {
    schema_version: u32,
    name: String,
    /// Always true: nothing here came from a real PR.
    synthetic: bool,
    description: String,
    repository_id: String,
    pr_number: u64,
    base_sha: String,
    head_sha: String,
    /// `PrSnapshot.complete`: false simulates a capped/partial PR file list.
    snapshot_complete: bool,
    observed_at: String,
    scenarios: Vec<String>,
    /// Files whose line-level rules cannot run (binary, truncated,
    /// mode-only, unsupported language). Their outcomes must never read as
    /// "analysed, nothing found".
    unsupported_paths: Vec<String>,
    expected_completeness: Completeness,
}

/// Typed mirror of [`DiffFacts`] for the drift guard. `DiffFacts` is
/// serialise-only, so this struct must list the same fields.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GoldenFacts {
    provider_id: String,
    engine_version: String,
    configuration_digest: String,
    input_digest: String,
    coverage: AnalysisCoverage,
    findings: Vec<ReviewFinding>,
    evidence: Vec<Evidence>,
    warnings: Vec<RunWarning>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Verdict {
    /// The label is what a reviewer would want for this change.
    Appropriate,
    /// Safe (never a defect claim) but noisy, misleading or imprecise.
    /// Reported for calibration (#44); thresholds are not tuned here.
    Questionable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LabelAnnotation {
    rule_id: String,
    /// `null` for PR-level findings.
    path: Option<String>,
    assertion_kind: AssertionKind,
    attention: AttentionLevel,
    verdict: Verdict,
    note: String,
}

struct Fixture {
    dir: PathBuf,
    manifest: BoundaryManifest,
    diff: String,
}

impl Fixture {
    fn name(&self) -> &str {
        &self.manifest.name
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn snapshot(&self) -> PrSnapshot {
        let m = &self.manifest;
        let mut snap = crate::demo::snapshot(0);
        snap.pr.number = m.pr_number;
        snap.pr.base_sha = m.base_sha.clone();
        snap.pr.head_sha = m.head_sha.clone();
        let parsed = parse_unified(&self.diff, &m.head_sha, &Limits::default());
        snap.files = parsed.files;
        snap.warnings = parsed.warnings;
        snap.complete = m.snapshot_complete;
        snap.github_threads.clear();
        snap.reviews.clear();
        snap.issue_comments.clear();
        snap
    }

    fn snapshot_id(&self) -> SnapshotId {
        let m = &self.manifest;
        SnapshotId::derive(&m.repository_id, m.pr_number, &m.base_sha, &m.head_sha)
    }

    fn scan(&self, snapshot: &PrSnapshot) -> DiffFacts {
        let run_id = AnalysisRunId::new("run_boundary");
        scan(
            &DiffFactsInput {
                snapshot,
                snapshot_id: &self.snapshot_id(),
                canonical_repo_id: &self.manifest.repository_id,
                run_id: Some(&run_id),
                observed_at: &self.manifest.observed_at,
            },
            &DiffFactsConfig::default(),
        )
    }

    fn facts(&self) -> DiffFacts {
        self.scan(&self.snapshot())
    }
}

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/analysis")
}

/// Every fixture directory with a `pr.diff`, sorted by name.
fn fixtures() -> Vec<Fixture> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(fixtures_root())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.join("pr.diff").is_file())
        .collect();
    dirs.sort();
    let out: Vec<Fixture> = dirs
        .into_iter()
        .map(|dir| {
            let manifest: BoundaryManifest = parse_file(&dir.join("manifest.json"));
            let diff = fs::read_to_string(dir.join("pr.diff")).unwrap();
            Fixture {
                dir,
                manifest,
                diff,
            }
        })
        .collect();
    assert!(out.len() >= 8, "boundary fixtures went missing");
    out
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn parse_file<T: DeserializeOwned>(path: &Path) -> T {
    serde_json::from_str(&read(path))
        .unwrap_or_else(|e| panic!("{} does not match its type: {e}", path.display()))
}

fn assert_exact<T: Serialize + DeserializeOwned>(path: &Path) {
    let text = read(path);
    let raw: Value = serde_json::from_str(&text).unwrap();
    let typed: T = parse_file(path);
    assert_eq!(
        serde_json::to_value(&typed).unwrap(),
        raw,
        "{} drifted from its Rust type: a field or enum value was added, renamed \
         or removed. Regenerate with {UPDATE_ENV}=1 and review the diff.",
        path.display()
    );
}

fn canonical(facts: &DiffFacts) -> String {
    serde_json::to_string_pretty(facts).unwrap() + "\n"
}

fn rule_path(f: &ReviewFinding) -> Option<String> {
    f.anchor.current_path.clone()
}

const CONTENT_RULES: [Rule; 3] = [
    rules::TEST_ASSERTIONS_ADDED,
    rules::TESTS_REMOVED,
    rules::PUBLIC_API,
];

fn file_outcomes<'a>(facts: &'a DiffFacts, path: &str) -> Vec<(&'a str, &'a ScopeOutcome)> {
    facts
        .coverage
        .entries
        .iter()
        .filter(|e| matches!(&e.scope, CoverageScope::File { path: p, .. } if p == path))
        .map(|e| (e.rule_id.as_deref().unwrap_or(""), &e.outcome))
        .collect()
}

/// How the diff provider (#22) and session (#27) present a scan.
fn source_coverage(facts: &DiffFacts, snapshot_id: &SnapshotId) -> SourceCoverage {
    let status = match facts.coverage.completeness {
        Completeness::Complete => RunStatus::Succeeded,
        _ => RunStatus::Partial,
    };
    let items = KnownCount::Known {
        value: facts.findings.len() as u32,
    };
    SourceCoverage {
        source_id: PROVIDER_ID.into(),
        mode: crate::model::AnalysisMode::Static,
        snapshot_id: snapshot_id.clone(),
        run_id: None,
        status: Some(status),
        completeness: facts.coverage.completeness,
        items,
        item_kind: "findings".into(),
        state: classify(Some(status), facts.coverage.completeness, items),
        reason_codes: facts.coverage.reason_codes.clone(),
        limitations: Vec::new(),
    }
}

fn file_states(snapshot: &PrSnapshot, status: FileStatus) -> Vec<FileState> {
    snapshot
        .files
        .iter()
        .map(|f| FileState {
            file_id: f.id.clone(),
            path: f.path.clone(),
            status,
            review_status: status,
            scope: FileScope {
                ignored: false,
                effective_rule_id: None,
                matching_rule_ids: Vec::new(),
            },
            last_reviewed_at: None,
        })
        .collect()
}

fn observed(facts: &DiffFacts) -> Vec<ObservedFinding> {
    facts
        .findings
        .iter()
        .map(|f| ObservedFinding {
            finding: f.clone(),
            current: f.applicability.is_current(),
        })
        .collect()
}

/// Deterministic Fisher-Yates with xorshift; no RNG dependency.
fn shuffled<T: Clone>(items: &[T], mut seed: u64) -> Vec<T> {
    let mut out = items.to_vec();
    for i in (1..out.len()).rev() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        out.swap(i, (seed % (i as u64 + 1)) as usize);
    }
    out
}

#[test]
fn manifests_are_synthetic_and_round_trip_exactly() {
    for fx in fixtures() {
        let m = &fx.manifest;
        assert_exact::<BoundaryManifest>(&fx.file("manifest.json"));
        assert!(m.synthetic, "{}", fx.name());
        assert_eq!(
            fx.dir.file_name().unwrap().to_str(),
            Some(m.name.as_str()),
            "manifest name matches its directory"
        );
        assert_eq!(m.repository_id, "synthetic/boundary");
        assert!(m.description.starts_with("Synthetic"));
        assert!(!m.scenarios.is_empty());
        let paths: BTreeSet<String> = fx.snapshot().files.into_iter().map(|f| f.path).collect();
        for p in &m.unsupported_paths {
            assert!(paths.contains(p), "{}: unknown path {p}", fx.name());
        }
    }
}

#[test]
fn golden_facts_are_byte_identical_and_match_the_rust_types() {
    let update = std::env::var_os(UPDATE_ENV).is_some();
    for fx in fixtures() {
        let path = fx.file("expected-facts.json");
        let actual = canonical(&fx.facts());
        if update {
            fs::write(&path, &actual).unwrap();
        }
        assert_exact::<GoldenFacts>(&path);
        assert_eq!(
            read(&path),
            actual,
            "{}: diff facts changed. If intended, regenerate with {UPDATE_ENV}=1, \
             bump the rule/engine version and review the diff.",
            fx.name()
        );
    }
}

#[test]
fn golden_drift_guard_rejects_renamed_fields_and_unknown_values() {
    let fx = fixtures().into_iter().next().unwrap();
    let text = read(&fx.file("expected-facts.json"));
    let mut golden: Value = serde_json::from_str(&text).unwrap();
    golden["findings"][0]["legacyTitle"] = Value::from("x");
    assert!(serde_json::from_value::<GoldenFacts>(golden.clone()).is_ok());
    let typed: GoldenFacts = serde_json::from_value(golden.clone()).unwrap();
    assert_ne!(serde_json::to_value(typed).unwrap(), golden);

    // `#[serde(other)]` is lossy: an unknown assertion kind decodes, but the
    // round trip no longer matches, so it is reported as drift.
    let mut golden: Value = serde_json::from_str(&text).unwrap();
    golden["findings"][0]["assertionKind"] = Value::from("proven_bug");
    let typed: GoldenFacts = serde_json::from_value(golden.clone()).unwrap();
    assert_eq!(
        typed.findings[0].assertion_kind,
        AssertionKind::Informational
    );
    assert_ne!(serde_json::to_value(typed).unwrap(), golden);

    let mut golden: Value = serde_json::from_str(&text).unwrap();
    golden["unexpected"] = Value::Bool(true);
    assert!(serde_json::from_value::<GoldenFacts>(golden).is_err());
}

#[test]
fn facts_are_byte_stable_under_reordered_and_shuffled_inputs() {
    for fx in fixtures() {
        let snapshot = fx.snapshot();
        let golden = canonical(&fx.facts());
        // Same input twice, and a fresh parse of the same diff.
        assert_eq!(canonical(&fx.scan(&snapshot)), golden, "{}", fx.name());
        assert_eq!(canonical(&fx.facts()), golden, "{}", fx.name());

        let mut orders = vec![snapshot.files.iter().rev().cloned().collect::<Vec<_>>()];
        for k in 1..snapshot.files.len() {
            let mut rotated = snapshot.files.clone();
            rotated.rotate_left(k);
            orders.push(rotated);
        }
        for seed in 1..=16u64 {
            orders.push(shuffled(
                &snapshot.files,
                seed.wrapping_mul(0x9E37_79B9_7F4A_7C15),
            ));
        }
        for files in orders {
            let mut reordered = snapshot.clone();
            reordered.files = files;
            reordered.warnings.reverse();
            assert_eq!(canonical(&fx.scan(&reordered)), golden, "{}", fx.name());
        }
    }
}

#[test]
fn stable_ids_do_not_collide_across_fixtures() {
    let mut ids = BTreeSet::new();
    for fx in fixtures() {
        for f in fx.facts().findings {
            assert!(ids.insert(f.id.as_str().to_string()), "{}", fx.name());
        }
    }
}

/// Layer 1 (#19): every unsupported file has an explicit, non-zero outcome
/// for every content rule, and the run reports reduced completeness when
/// the manifest says it must.
#[test]
fn unsupported_cases_are_unknown_or_not_applicable_never_analyzed_zero() {
    for fx in fixtures() {
        let snapshot = fx.snapshot();
        let facts = fx.facts();
        assert_eq!(
            facts.coverage.completeness,
            fx.manifest.expected_completeness,
            "{}: {:?}",
            fx.name(),
            facts.coverage.reason_codes
        );
        if fx.manifest.expected_completeness != Completeness::Complete {
            assert!(!facts.coverage.reason_codes.is_empty(), "{}", fx.name());
        }
        // Every changed file has an outcome for every content rule: a scope
        // with no entry would be silently "unknown".
        for file in &snapshot.files {
            let outcomes = file_outcomes(&facts, &file.path);
            for rule in CONTENT_RULES {
                assert!(
                    outcomes.iter().any(|(r, _)| *r == rule.id),
                    "{}: {} has no {} outcome",
                    fx.name(),
                    file.path,
                    rule.id
                );
            }
        }
        for path in &fx.manifest.unsupported_paths {
            let file = snapshot.files.iter().find(|f| &f.path == path).unwrap();
            for (rule, outcome) in file_outcomes(&facts, path) {
                assert!(
                    !outcome.is_no_finding() && outcome.known_findings().is_none(),
                    "{}: {path} {rule} reads as clean: {outcome:?}",
                    fx.name()
                );
                assert!(matches!(
                    outcome,
                    ScopeOutcome::Unknown { .. }
                        | ScopeOutcome::NotApplicable { .. }
                        | ScopeOutcome::NotRun { .. }
                ));
            }
            // No content was read, so nothing may be line-anchored or carry a
            // line-level finding.
            for f in facts.findings.iter().filter(|f| {
                f.anchor.current_path.as_deref() == Some(path.as_str())
                    || f.anchor.file_id.as_ref() == Some(&file.id)
            }) {
                let rule = f.source.rule_id.as_deref().unwrap();
                assert!(
                    !CONTENT_RULES.iter().any(|r| r.id == rule),
                    "{}: {path} has {rule}",
                    fx.name()
                );
            }
            for ev in &facts.evidence {
                if let Some(a) = &ev.anchor
                    && a.file_id.as_ref() == Some(&file.id)
                {
                    assert!(a.line.is_none() && a.side.is_none(), "{path}: {ev:?}");
                }
            }
            // Counts that were not measured stay null, never 0.
            let fact = facts
                .findings
                .iter()
                .find(|f| {
                    f.source.rule_id.as_deref() == Some(rules::FILE_CHANGE.id)
                        && f.anchor.file_id.as_ref() == Some(&file.id)
                })
                .unwrap();
            let m = &fact.extensions["measurement"];
            if file.is_binary || !file.patch_complete {
                assert_eq!(m["additions"], Value::Null, "{path}");
                assert_eq!(m["deletions"], Value::Null, "{path}");
                assert!(fact.body.contains("unknown"), "{path}: {}", fact.body);
            }
        }
        // PR totals are unknown, not undercounted, when any file is.
        let summary = &facts
            .findings
            .iter()
            .find(|f| f.source.rule_id.as_deref() == Some(rules::PR_SUMMARY.id))
            .unwrap();
        if !snapshot.complete
            || snapshot
                .files
                .iter()
                .any(|f| f.is_binary || !f.patch_complete)
        {
            assert_eq!(summary.extensions["measurement"]["additions"], Value::Null);
            assert!(summary.body.contains("unknown"), "{}", summary.body);
        }
        if !snapshot.complete {
            assert_eq!(
                summary.extensions["measurement"]["changedFiles"],
                Value::Null
            );
            let tests = facts
                .coverage
                .entries
                .iter()
                .find(|e| e.rule_id.as_deref() == Some(rules::TESTS_TOUCHED.id))
                .unwrap();
            if !facts
                .findings
                .iter()
                .any(|f| f.source.rule_id.as_deref() == Some(rules::TESTS_TOUCHED.id))
            {
                assert!(matches!(tests.outcome, ScopeOutcome::Unknown { .. }));
            }
        }
    }
}

/// Layer 2 (#27, #30): with every file reviewed, a partial scan must not
/// let the presentation say "all clear" or make zero counts authoritative.
#[test]
fn presentation_never_turns_partial_scans_into_all_clear() {
    for fx in fixtures() {
        let snapshot = fx.snapshot();
        let facts = fx.facts();
        let source = source_coverage(&facts, &fx.snapshot_id());
        assert_ne!(source.state, SourceState::Clean, "{}", fx.name());
        let sources = [source];
        let complete = fx.manifest.expected_completeness == Completeness::Complete;
        assert_eq!(
            overall(&sources),
            if complete {
                OverallCoverage::Complete
            } else {
                OverallCoverage::Incomplete
            },
            "{}",
            fx.name()
        );

        let findings = observed(&facts);
        let reviewed = file_states(&snapshot, FileStatus::ReviewedCurrent);
        let input = QueueInput {
            files: &reviewed,
            rules: &[],
            findings: &findings,
            relations: &[],
            threads: &[],
            user_states: &[],
            coverage: &sources,
        };
        let counts = priority::scope_counts(&input);
        let queue = priority::build_queue(&input);
        assert_eq!(counts.authoritative(), complete, "{}", fx.name());
        assert_eq!(counts.files.total as usize, snapshot.files.len());
        assert!(counts.current_findings.partitions());
        assert_eq!(queue.remaining().count(), 0);
        assert_eq!(
            queue.verdict,
            if complete {
                QueueVerdict::AllClear
            } else {
                QueueVerdict::EmptyIncomplete
            },
            "{}",
            fx.name()
        );
        // Unsupported files stay in the queue, never dropped as "nothing".
        let unreviewed = file_states(&snapshot, FileStatus::Unreviewed);
        let queue = priority::build_queue(&QueueInput {
            files: &unreviewed,
            ..input
        });
        for path in &fx.manifest.unsupported_paths {
            let item = queue.items.iter().find(|i| &i.path == path).unwrap();
            assert_eq!(item.bucket, Bucket::NeedsReview, "{path}");
        }
        assert_eq!(queue.items.len(), snapshot.files.len());
    }
}

/// Phrases that would turn an absent test change into a proven defect.
const DEFECT_PHRASES: &[&str] = &[
    "bug",
    "defect",
    "untested",
    "not tested",
    "missing test",
    "no test",
    "without test",
    "coverage gap",
    "uncovered",
    "lost coverage",
    "coverage was lost",
    "broken",
];

fn finding_text(f: &ReviewFinding) -> String {
    format!(
        "{}\n{}\n{}\n{}",
        f.title,
        f.body,
        f.rationale,
        f.suggested_action.as_deref().unwrap_or("")
    )
    .to_lowercase()
}

#[test]
fn absent_or_moved_tests_are_never_asserted_as_defects() {
    for fx in fixtures() {
        let snapshot = fx.snapshot();
        let facts = fx.facts();
        for f in &facts.findings {
            assert!(
                matches!(
                    f.assertion_kind,
                    AssertionKind::MeasuredFact | AssertionKind::AttentionHint
                ),
                "{}: {} is {:?}",
                fx.name(),
                f.title,
                f.assertion_kind
            );
            assert!(!f.kinds.contains(&FindingKind::TestCoverage), "{}", f.title);
            assert!(!f.kinds.contains(&FindingKind::Correctness), "{}", f.title);
            assert_ne!(f.attention.level, AttentionLevel::High, "{}", f.title);
            assert_eq!(f.severity.normalized, Severity::Info);
            let text = finding_text(f);
            for phrase in DEFECT_PHRASES {
                // "does not mean coverage was lost" is the disclaimer itself.
                let disclaimed = text.replace("does not mean coverage was lost", "");
                assert!(
                    !disclaimed.contains(phrase),
                    "{}: {:?} says {phrase:?}",
                    fx.name(),
                    f.title
                );
            }
            // A test-related hint always says why and what to check.
            if f.kinds.contains(&FindingKind::TestsChanged)
                && f.assertion_kind == AssertionKind::AttentionHint
            {
                assert!(!f.attention.reasons.is_empty());
                assert!(!f.evidence_ids.is_empty());
                assert!(f.suggested_action.is_some());
                assert!(f.rationale.contains("does not mean"), "{}", f.rationale);
                let lim = f.extensions["limitations"].to_string();
                assert!(lim.contains("not tracked"), "{lim}");
            }
        }
        // Presentation: nothing about tests can drive a feedback or
        // high-attention bucket, or cite a confirmed failure.
        let findings = observed(&facts);
        let files = file_states(&snapshot, FileStatus::Unreviewed);
        let sources = [source_coverage(&facts, &fx.snapshot_id())];
        let queue = priority::build_queue(&QueueInput {
            files: &files,
            rules: &[],
            findings: &findings,
            relations: &[],
            threads: &[],
            user_states: &[],
            coverage: &sources,
        });
        for item in &queue.items {
            assert_eq!(item.bucket, Bucket::NeedsReview, "{}", item.path);
            assert!(item.reasons.iter().all(|r| !matches!(
                r.code,
                ReasonCode::ConfirmedFailure
                    | ReasonCode::ValidatedHighAttention
                    | ReasonCode::DeterministicHighAttention
            )));
        }
    }
}

fn fixture(name: &str) -> Fixture {
    fixtures()
        .into_iter()
        .find(|f| f.name() == name)
        .unwrap_or_else(|| panic!("no fixture {name}"))
}

fn rule_ids(facts: &DiffFacts) -> BTreeSet<(String, Option<String>)> {
    facts
        .findings
        .iter()
        .map(|f| (f.source.rule_id.clone().unwrap(), rule_path(f)))
        .collect()
}

#[test]
fn code_without_test_changes_is_only_a_measured_zero() {
    for name in [
        "test-gap-tests-elsewhere",
        "test-gap-comment-only",
        "test-gap-docs-only",
    ] {
        let facts = fixture(name).facts();
        // No test rule fires, and nothing at all mentions tests.
        for f in &facts.findings {
            assert!(!f.kinds.contains(&FindingKind::TestsChanged), "{name}");
            assert!(!finding_text(f).contains("test"), "{name}: {}", f.title);
        }
        // The only statement about tests is the PR-level measured count.
        let tests = facts
            .coverage
            .entries
            .iter()
            .find(|e| e.rule_id.as_deref() == Some(rules::TESTS_TOUCHED.id))
            .unwrap();
        assert_eq!(tests.scope, CoverageScope::Pr);
        assert_eq!(tests.outcome, ScopeOutcome::Analyzed { findings: 0 });
    }
    // Comment-only: no public-API or test hint from comment lines.
    let facts = fixture("test-gap-comment-only").facts();
    assert_eq!(
        rule_ids(&facts),
        BTreeSet::from([
            (rules::PR_SUMMARY.id.into(), None),
            (rules::FILE_CHANGE.id.into(), Some("src/math.rs".into())),
        ])
    );
}

#[test]
fn moved_tests_are_a_hint_to_check_not_lost_coverage() {
    let facts = fixture("test-gap-moved-tests").facts();
    let removed: Vec<_> = facts
        .findings
        .iter()
        .filter(|f| f.source.rule_id.as_deref() == Some(rules::TESTS_REMOVED.id))
        .collect();
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].assertion_kind, AssertionKind::AttentionHint);
    assert_eq!(removed[0].anchor.kind, AnchorKind::File);
    assert_eq!(
        removed[0].anchor.current_path.as_deref(),
        Some("src/math.rs")
    );
    assert!(
        removed[0]
            .suggested_action
            .as_deref()
            .unwrap()
            .contains("moved")
    );
    // The destination is measured in the same PR.
    let ids = rule_ids(&facts);
    assert!(ids.contains(&(
        rules::TEST_ASSERTIONS_ADDED.id.into(),
        Some("tests/math.rs".into())
    )));
    assert!(ids.contains(&(rules::TESTS_TOUCHED.id.into(), None)));
}

#[test]
fn renames_keep_lineage_and_file_level_anchors() {
    let facts = fixture("boundary-renames").facts();
    let file = |path: &str| {
        facts
            .findings
            .iter()
            .find(|f| {
                f.source.rule_id.as_deref() == Some(rules::FILE_CHANGE.id)
                    && f.anchor.current_path.as_deref() == Some(path)
            })
            .unwrap()
    };
    for (path, previous) in [
        ("src/new_api.rs", "src/old_api.rs"),
        ("src/parse/mod.rs", "src/parser.rs"),
        ("tests/parse.rs", "tests/parser.rs"),
    ] {
        let f = file(path);
        assert_eq!(f.anchor.previous_path.as_deref(), Some(previous));
        assert_eq!(f.extensions["measurement"]["operation"], "renamed");
    }
    // Rename with edit: the signature change is measured on the new path.
    assert!(
        rule_ids(&facts).contains(&(rules::PUBLIC_API.id.into(), Some("src/parse/mod.rs".into())))
    );
    // Pure rename: no content facts at all.
    assert_eq!(
        facts
            .findings
            .iter()
            .filter(|f| f.anchor.current_path.as_deref() == Some("src/new_api.rs"))
            .count(),
        1
    );
}

#[test]
fn deleted_binary_and_mode_only_files_get_file_level_facts_only() {
    let facts = fixture("boundary-file-kinds").facts();
    let op = |path: &str| {
        facts
            .findings
            .iter()
            .find(|f| {
                f.source.rule_id.as_deref() == Some(rules::FILE_CHANGE.id)
                    && f.anchor.current_path.as_deref() == Some(path)
            })
            .unwrap()
            .extensions["measurement"]["operation"]
            .clone()
    };
    assert_eq!(op("src/legacy.rs"), "deleted");
    assert_eq!(op("assets/logo.png"), "binary");
    assert_eq!(op("scripts/gen.rs"), "mode-only");
    assert_eq!(op("scripts/release.sh"), "mode-only");
    // Mode-only files have no content to scan, in any language.
    for path in ["scripts/gen.rs", "scripts/release.sh"] {
        for (_, outcome) in file_outcomes(&facts, path) {
            assert_eq!(
                outcome,
                &ScopeOutcome::NotApplicable {
                    reason: "mode_only".into()
                },
                "{path}"
            );
        }
    }
    // Deleted file: removed public signature anchored on the old side only.
    let api = facts
        .findings
        .iter()
        .find(|f| f.source.rule_id.as_deref() == Some(rules::PUBLIC_API.id))
        .unwrap();
    assert_eq!(api.anchor.current_path.as_deref(), Some("src/legacy.rs"));
    for id in &api.evidence_ids {
        let ev = facts.evidence.iter().find(|e| &e.id == id).unwrap();
        if let Some(side) = ev.anchor.as_ref().and_then(|a| a.side) {
            assert_eq!(side, Side::Left);
        }
    }
}

type Label = (String, Option<String>, AssertionKind, AttentionLevel);

/// Compares emitted labels with `path`'s hand annotation and returns the
/// questionable ones.
fn check_labels(name: &str, facts: &DiffFacts, path: &Path) -> Vec<String> {
    let mut emitted: Vec<Label> = facts
        .findings
        .iter()
        .map(|f| {
            (
                f.source.rule_id.clone().unwrap(),
                rule_path(f),
                f.assertion_kind,
                f.attention.level,
            )
        })
        .collect();
    emitted.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    if std::env::var_os(UPDATE_ENV).is_some() && !path.exists() {
        // Skeleton only: verdicts and notes must be written by hand.
        let skeleton: Vec<LabelAnnotation> = emitted
            .iter()
            .map(|(rule, path, kind, level)| LabelAnnotation {
                rule_id: rule.clone(),
                path: path.clone(),
                assertion_kind: *kind,
                attention: *level,
                verdict: Verdict::Questionable,
                note: "TODO".into(),
            })
            .collect();
        fs::write(
            path,
            serde_json::to_string_pretty(&skeleton).unwrap() + "\n",
        )
        .unwrap();
    }
    assert_exact::<Vec<LabelAnnotation>>(path);
    let labels: Vec<LabelAnnotation> = parse_file(path);
    let mut annotated: Vec<Label> = labels
        .iter()
        .map(|l| {
            (
                l.rule_id.clone(),
                l.path.clone(),
                l.assertion_kind,
                l.attention,
            )
        })
        .collect();
    annotated.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    assert_eq!(
        annotated,
        emitted,
        "{name}: emitted labels changed; re-evaluate {} by hand",
        path.display()
    );
    let mut questionable = Vec::new();
    for l in labels {
        assert!(
            !l.note.trim().is_empty() && l.note != "TODO",
            "{name}: unannotated label {}",
            l.rule_id
        );
        if l.verdict == Verdict::Questionable {
            questionable.push(format!("{name}: {} {:?}", l.rule_id, l.path));
        }
    }
    questionable
}

#[test]
fn emitted_labels_match_the_hand_annotated_evaluation() {
    let mut questionable = Vec::new();
    for fx in fixtures() {
        questionable.extend(check_labels(
            fx.name(),
            &fx.facts(),
            &fx.file("expected-labels.json"),
        ));
    }
    // The evaluation must stay honest: it records known-noisy labels rather
    // than pretending every rule is calibrated (#44 owns tuning).
    assert!(!questionable.is_empty());
}

/// The existing public demo capture (cli/cli#14921, #17) evaluated with the
/// same annotation format. Its PR content is the already-bundled public
/// diff; the annotations are ours.
#[test]
fn demo_capture_labels_match_the_hand_annotated_evaluation() {
    let snapshot = crate::demo::snapshot(0);
    let manifest = crate::demo::intelligence::load(false).manifest;
    let run_id = AnalysisRunId::new("run_boundary");
    let facts = scan(
        &DiffFactsInput {
            snapshot: &snapshot,
            snapshot_id: &manifest.snapshot_id,
            canonical_repo_id: &manifest.repository_id,
            run_id: Some(&run_id),
            observed_at: &manifest.observed_at,
        },
        &DiffFactsConfig::default(),
    );
    check_labels(
        "demo-cli-14921",
        &facts,
        &fixtures_root().join("demo-cli-14921/diff-facts-labels.json"),
    );
}
