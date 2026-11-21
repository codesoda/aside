//! Contract and scenario tests over the synthetic demo intelligence fixture.
//!
//! Scenario numbers refer to analyze.md §19.5 unless marked §19.3/§19.4.

use std::collections::{BTreeSet, HashSet};

use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

use super::*;
use crate::{
    demo,
    model::{
        Actor, AnalysisMode, AnalysisRunId, Applicability, AssertionKind, AttentionLevel,
        CodeAnchor, Completeness, CoverageScope, EvidenceId, EvidenceKeyInput, EvidenceKind,
        FindingId, FindingKeyInput, KnownCount, NativeSourceKey, PrSnapshot, RelationKind,
        RelationTarget, RunStatus, ScopeOutcome, Side, SourceKind, SourceStatus, ThreadSource,
        UserDisposition, ValidationStatus,
    },
    rules,
    session::Session,
    store::Store,
};

fn base() -> DemoIntelligence {
    load(false)
}

fn full() -> DemoIntelligence {
    load(true)
}

fn finding<'a>(bundle: &'a DemoIntelligence, title_prefix: &str) -> &'a ReviewFinding {
    bundle
        .findings
        .iter()
        .find(|f| f.title.starts_with(title_prefix))
        .unwrap_or_else(|| panic!("no finding titled {title_prefix:?}"))
}

fn run<'a>(bundle: &'a DemoIntelligence, id: &AnalysisRunId) -> &'a AnalysisRun {
    bundle
        .runs
        .iter()
        .find(|r| &r.id == id)
        .expect("run exists")
}

fn thread<'a>(bundle: &'a DemoIntelligence, node_id: &str) -> &'a GithubThread {
    bundle
        .github_threads
        .iter()
        .find(|t| t.node_id == node_id)
        .expect("thread exists")
}

/// Applicability of `finding` to `snapshot`, from its anchor alone. This is a
/// test oracle for the fixture, not the view projection (TODO(#33)).
fn applies_to(finding: &ReviewFinding, snapshot: &PrSnapshot) -> bool {
    if !finding.applicability.is_current() {
        return false;
    }
    match (&finding.anchor.file_id, &finding.anchor.diff_digest) {
        (Some(file_id), Some(digest)) => snapshot
            .files
            .iter()
            .any(|f| &f.id == file_id && &f.diff_digest == digest),
        _ => true,
    }
}

/// "0 issues found" may only be said when every provider fully succeeded.
fn may_claim_no_issues(runs: &[AnalysisRun], findings: &[ReviewFinding]) -> bool {
    !runs.is_empty() && runs.iter().all(|r| r.status.can_assert_absence()) && findings.is_empty()
}

// --- Drift: every file is exactly its Rust contract shape ---------------------

fn assert_exact<T: Serialize + DeserializeOwned>(name: &str, json: &str) {
    let raw: Value = serde_json::from_str(json).unwrap_or_else(|e| panic!("{name}: {e}"));
    let typed: T = parse(name, json);
    let back = serde_json::to_value(&typed).unwrap();
    assert_eq!(
        back, raw,
        "{name} drifted from its Rust type: a field was added, renamed or removed. \
         Update the fixture to the serialised shape above."
    );
}

#[test]
fn every_fixture_file_round_trips_exactly_through_its_contract_type() {
    assert_exact::<FixtureManifest>("manifest.json", MANIFEST);
    assert_exact::<Vec<ExclusionRule>>("repository-rules.json", REPOSITORY_RULES);
    assert_exact::<Vec<GithubThread>>("github-review-threads.json", GITHUB_THREADS);
    assert_exact::<Vec<ImportedReview>>("github-reviews.json", GITHUB_REVIEWS);
    assert_exact::<Vec<IssueComment>>("github-issue-comments.json", GITHUB_ISSUE_COMMENTS);
    assert_exact::<Vec<ImportedCheckRun>>("github-check-runs.json", GITHUB_CHECK_RUNS);
    assert_exact::<Vec<ImportedCheckAnnotation>>("github-annotations.json", GITHUB_ANNOTATIONS);
    assert_exact::<Vec<ImportedCommitStatus>>(
        "github-commit-statuses.json",
        GITHUB_COMMIT_STATUSES,
    );
    assert_exact::<Vec<AnalysisRun>>("analysis-runs.json", ANALYSIS_RUNS);
    assert_exact::<Vec<Evidence>>("evidence.json", EVIDENCE);
    assert_exact::<Vec<ReviewFinding>>("expected-findings.json", FINDINGS);
    assert_exact::<Vec<FindingRelation>>("relations.json", RELATIONS);
    assert_exact::<Vec<FindingUserState>>("user-states.json", USER_STATES);
    assert_exact::<LlmHypothesisFixture>("llm-hypothesis.json", LLM_HYPOTHESIS);
}

#[test]
fn drift_detection_rejects_unknown_and_missing_fields() {
    // An extra field (for example a renamed Rust field) is caught by the
    // exact round trip, and a missing required field fails to parse.
    let mut findings: Value = serde_json::from_str(FINDINGS).unwrap();
    findings[0]["legacyTitle"] = Value::from("x");
    let typed: Vec<ReviewFinding> = serde_json::from_value(findings.clone()).unwrap();
    assert_ne!(serde_json::to_value(typed).unwrap(), findings);
    findings[0].as_object_mut().unwrap().remove("legacyTitle");
    findings[0].as_object_mut().unwrap().remove("assertionKind");
    assert!(serde_json::from_value::<Vec<ReviewFinding>>(findings).is_err());
    // Review state cannot be smuggled into local triage.
    let mut states: Value = serde_json::from_str(USER_STATES).unwrap();
    states[0]["reviewed"] = Value::Bool(true);
    assert!(serde_json::from_value::<Vec<FindingUserState>>(states).is_err());
}

#[test]
fn manifest_is_synthetic_and_pins_the_demo_snapshot() {
    let m = base().manifest;
    let snap = demo::snapshot(0);
    assert!(m.synthetic);
    assert!(!m.llm_enabled_by_default);
    assert_eq!(m.schema_version, crate::model::ANALYSIS_SCHEMA_VERSION);
    assert_eq!(
        m.repository_id,
        snap.provider_repository_id.clone().unwrap()
    );
    assert_eq!(m.pr_number, snap.pr.number);
    assert_eq!(m.base_sha, snap.pr.base_sha);
    assert_eq!(m.head_sha, snap.pr.head_sha);
    assert_eq!(
        m.snapshot_id,
        SnapshotId::derive(&m.repository_id, m.pr_number, &m.base_sha, &m.head_sha)
    );
    assert_ne!(m.previous_head_sha, m.head_sha);
}

#[test]
fn synthetic_content_is_marked_and_uses_reserved_ids() {
    let b = full();
    for t in &b.github_threads {
        for c in &t.comments {
            assert!(c.body.contains("synthetic fixture"), "{}", c.node_id);
            assert!(c.database_id.is_none_or(|id| id >= 9_000_000_000));
        }
    }
    for r in b.reviews.iter().filter(|r| !r.body.is_empty()) {
        assert!(r.body.contains("synthetic fixture"));
    }
    for c in &b.issue_comments {
        assert!(c.body.contains("synthetic fixture"));
    }
    for c in &b.check_runs {
        assert!(c.github_check_run_id >= 9_000_000_000);
    }
}

// --- IDs, references and contract invariants ----------------------------------

#[test]
fn every_id_is_derived_from_its_inputs() {
    let b = full();
    let repo = &b.manifest.repository_id;
    for r in &b.runs {
        assert!(
            r.id_is_consistent(),
            "run {} id is not derived",
            r.provider_id
        );
    }
    for e in &b.evidence {
        let value_json = e.value.as_ref().map(|v| serde_json::to_string(v).unwrap());
        let expected = EvidenceId::derive(&EvidenceKeyInput {
            kind: e.kind,
            source_run_id: e.source_run_id.as_ref(),
            label: &e.label,
            method: e.method.as_deref(),
            input_digest: e.input_digest.as_deref(),
            value_json: value_json.as_deref(),
        });
        assert_eq!(e.id, expected, "evidence {:?}", e.label);
    }
    for f in &b.findings {
        let expected = if let Some(native) = &f.source.native {
            FindingId::for_imported(repo, f.pr_number, native)
        } else {
            let key = &f.extensions["idKey"];
            let s = |k: &str| key[k].as_str();
            FindingId::derive(&FindingKeyInput {
                canonical_repo_id: repo,
                pr_number: f.pr_number,
                provider_id: &f.source.provider_id,
                rule_id: f.source.rule_id.as_deref().unwrap(),
                claim_identity: s("claimIdentity").unwrap(),
                subject_identity: s("subjectIdentity"),
                code_fingerprint: s("codeFingerprint"),
            })
        };
        assert_eq!(f.id, expected, "finding {:?}", f.title);
    }
    for (ordinal, a) in b.check_annotations.iter().enumerate() {
        assert_eq!(
            a.key,
            ImportedCheckAnnotation::content_fingerprint(
                a.check_run_id,
                &a.path,
                a.start_line,
                a.end_line,
                &a.message,
                ordinal as u32,
            )
        );
    }
}

#[test]
fn every_finding_meets_the_contract_and_references_resolve() {
    let b = full();
    let ids: HashSet<_> = b.findings.iter().map(|f| &f.id).collect();
    assert_eq!(ids.len(), b.findings.len(), "finding ids are unique");
    let evidence: HashSet<_> = b.evidence.iter().map(|e| &e.id).collect();
    let threads: HashSet<_> = b
        .github_threads
        .iter()
        .map(|t| t.node_id.as_str())
        .collect();
    for f in &b.findings {
        assert_eq!(f.validate(), Ok(()), "{}", f.title);
        assert!(f.evidence_ids.iter().all(|e| evidence.contains(e)));
        let r = run(&b, f.source.run_id.as_ref().unwrap());
        assert_eq!(r.snapshot_id, f.observed_snapshot_id, "{}", f.title);
        if let Some(thread) = &f.anchor.github_thread_id {
            assert!(threads.contains(thread.as_str()));
        }
        assert!(!f.extensions.contains_key("reviewed"));
    }
    for e in &b.evidence {
        assert!(
            b.runs
                .iter()
                .any(|r| Some(&r.id) == e.source_run_id.as_ref())
        );
    }
    for rel in &b.relations {
        assert!(ids.contains(&rel.source_finding_id));
        match &rel.target {
            RelationTarget::Finding(id) => assert!(ids.contains(id)),
            RelationTarget::Thread(id) => assert!(threads.contains(id.as_str())),
            RelationTarget::Entity(_) => {}
        }
    }
    for s in &b.user_states {
        assert!(ids.contains(&s.finding_id));
    }
}

#[test]
fn current_anchors_resolve_against_the_demo_diff() {
    let snap = demo::snapshot(0);
    for f in base()
        .findings
        .iter()
        .filter(|f| f.applicability.is_current())
    {
        let a = &f.anchor;
        let Some(path) = &a.current_path else {
            continue;
        };
        let file = snap.files.iter().find(|x| &x.path == path).expect(path);
        assert_eq!(a.file_id.as_deref(), Some(file.id.as_str()), "{}", f.title);
        assert_eq!(
            a.diff_digest.as_deref(),
            Some(file.diff_digest.as_str()),
            "{}",
            f.title
        );
        if let (Some(line), Some(Side::Right)) = (a.line, a.side) {
            let start = a.start_line.unwrap_or(line);
            for n in start..=line {
                assert!(
                    file.hunks
                        .iter()
                        .flat_map(|h| &h.lines)
                        .any(|l| l.new_line == Some(n)),
                    "{}: line {n} is not in the diff",
                    f.title
                );
            }
            assert_eq!(a.commit_sha.as_deref(), Some(snap.pr.head_sha.as_str()));
        }
    }
}

#[test]
fn bundle_is_byte_stable_across_loads() {
    let a = serde_json::to_string(&full()).unwrap();
    let b = serde_json::to_string(&full()).unwrap();
    assert_eq!(a, b);
}

// --- §19.3 import coverage ------------------------------------------------------

#[test]
fn reviews_cover_every_verdict_bot_and_deleted_authors() {
    let b = base();
    let states: BTreeSet<_> = b.reviews.iter().map(|r| r.state.as_str()).collect();
    for s in [
        "APPROVED",
        "CHANGES_REQUESTED",
        "COMMENTED",
        "DISMISSED",
        "PENDING",
    ] {
        assert!(states.contains(s), "missing review state {s}");
    }
    assert!(b.reviews.iter().any(|r| r.body.is_empty()));
    assert!(b.reviews.iter().any(|r| r.author.ends_with("[bot]")));
    assert!(b.reviews.iter().any(|r| r.author == "ghost"));
    // A review with no inline comment still projects to a PR-level finding.
    let verdict = finding(&b, "Changes requested");
    assert_eq!(verdict.source.kind, SourceKind::GithubReviewSummary);
    assert_eq!(verdict.anchor.kind, crate::model::AnchorKind::Pr);
    assert_eq!(
        verdict.severity.original.as_deref(),
        Some("CHANGES_REQUESTED")
    );
}

#[test]
fn issue_comment_with_path_text_stays_pr_level() {
    let b = base();
    let comment = b
        .issue_comments
        .iter()
        .find(|c| c.body.contains("store.go:42"))
        .unwrap();
    assert!(!comment.author.is_empty());
    assert!(
        b.findings
            .iter()
            .filter(|f| f.source.kind == SourceKind::GithubIssueComment)
            .all(|f| f.anchor.current_path.is_none())
    );
}

#[test]
fn checks_cover_conclusions_shas_and_annotation_counts() {
    let b = base();
    let head = &b.manifest.head_sha;
    let conclusions: BTreeSet<_> = b
        .check_runs
        .iter()
        .map(|c| c.conclusion.as_deref())
        .collect();
    for c in [Some("failure"), Some("success"), Some("neutral"), None] {
        assert!(conclusions.contains(&c), "missing conclusion {c:?}");
    }
    let lint: Vec<_> = b.check_runs.iter().filter(|c| c.name == "lint").collect();
    assert_eq!(lint.len(), 2, "two runs of the same tool");
    assert!(
        lint.iter().any(|c| &c.head_sha != head),
        "one on an old SHA"
    );
    assert!(
        b.check_runs
            .iter()
            .any(|c| c.annotations_count == 0 && c.conclusion.as_deref() == Some("success"))
    );
    for c in &b.check_runs {
        let n = b
            .check_annotations
            .iter()
            .filter(|a| a.check_run_id == c.github_check_run_id)
            .count();
        assert_eq!(n as u32, c.annotations_count, "{}", c.name);
    }
    // Legacy status: same context on two SHAs; only one is current.
    let current: Vec<_> = b
        .commit_statuses
        .iter()
        .filter(|s| s.context == "ci/legacy" && &s.sha == head)
        .collect();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].state, "success");
}

// --- §19.5 scenarios ---------------------------------------------------------

/// 1: the Review screen needs no analysis: imported feedback is visible as soon
/// as the demo session opens, with no provider having run.
#[test]
fn s01_demo_session_shows_feedback_without_any_analysis() {
    let session = Session::new(
        demo::snapshot(0),
        Store::open_in_memory().unwrap(),
        None,
        None,
    )
    .unwrap();
    let view = session.view(Actor::Human).unwrap();
    assert!(view.demo && view.complete);
    assert_eq!(view.files.len(), 33);
    assert_eq!(view.reviews, reviews());
    assert_eq!(view.issue_comments, issue_comments());
    let imported: Vec<_> = view
        .threads
        .iter()
        .filter(|t| t.source == ThreadSource::Github)
        .collect();
    assert_eq!(imported.len(), github_threads().len());
    let bot = imported
        .iter()
        .find(|t| t.github_thread_id.as_deref() == Some("PRRT_synthBot0001"))
        .unwrap();
    assert_eq!(bot.comments.len(), 3);
    assert!(!bot.resolved);
    assert_eq!(
        view.progress.reviewed, 0,
        "feedback never marks files reviewed"
    );
}

/// 3: churn shows real counts, the history window and its caveat.
#[test]
fn s03_churn_finding_carries_counts_window_and_caveat() {
    let b = base();
    let f = finding(&b, "High churn");
    assert!(f.kinds.contains(&crate::model::FindingKind::Churn));
    let e = b
        .evidence
        .iter()
        .find(|e| f.evidence_ids.contains(&e.id))
        .unwrap();
    assert_eq!(e.kind, EvidenceKind::History);
    assert_eq!(e.numerator, Some(14.0));
    let v = e.value.as_ref().unwrap();
    assert_eq!(v["commits"], 14);
    assert_eq!(v["windowDays"], 180);
    assert!(e.limitations.iter().any(|l| l.contains("shallow")));
    let r = run(&b, f.source.run_id.as_ref().unwrap());
    assert_eq!(r.status, RunStatus::Partial);
    assert!(r.warnings.iter().any(|w| w.code == "history_truncated"));
}

/// 4: an unresolved current CodeRabbit thread is a native GitHub thread, not an
/// Aside discovery.
#[test]
fn s04_bot_thread_stays_a_native_github_thread() {
    let b = base();
    let t = thread(&b, "PRRT_synthBot0001");
    assert!(!t.is_resolved && !t.is_outdated);
    assert_eq!(t.comments.len(), 3);
    assert_eq!(t.comments[0].author, "coderabbitai[bot]");
    assert!(t.comments.iter().any(|c| !c.author.ends_with("[bot]")));
    let f = b
        .findings
        .iter()
        .find(|f| f.anchor.github_thread_id.as_deref() == Some("PRRT_synthBot0001"))
        .unwrap();
    assert_eq!(f.source.kind, SourceKind::GithubReviewThread);
    assert_eq!(f.assertion_kind, AssertionKind::ExternalFeedback);
    assert_eq!(f.source_status, SourceStatus::Open);
    assert!(f.applicability.is_current());
    assert_eq!(f.source.github_actor.as_deref(), Some("coderabbitai[bot]"));
    let native = f.source.native.as_ref().unwrap();
    assert_eq!(native.key, NativeSourceKey::Upstream(t.node_id.clone()));
    assert!(
        f.external_url
            .as_deref()
            .unwrap()
            .starts_with("https://github.com/")
    );
}

/// 5: GraphQL and REST copies of the same comments merge to one record each.
#[test]
fn s05_graphql_and_rest_copies_merge_without_duplicates() {
    let rest: Vec<Value> = serde_json::from_str(REST_REVIEW_COMMENTS).unwrap();
    let b = base();
    let t = thread(&b, "PRRT_synthBot0001");
    let mut merged: Vec<(i64, String)> = t
        .comments
        .iter()
        .map(|c| (c.database_id.unwrap(), c.node_id.clone()))
        .collect();
    for r in &rest {
        let key = (
            r["id"].as_i64().unwrap(),
            r["node_id"].as_str().unwrap().to_string(),
        );
        let matches = merged.iter().filter(|m| **m == key).count();
        assert_eq!(
            matches, 1,
            "REST comment {key:?} matches one GraphQL comment"
        );
        if !merged.contains(&key) {
            merged.push(key);
        }
        if let Some(parent) = r["in_reply_to_id"].as_i64() {
            assert_eq!(parent, t.comments[0].database_id.unwrap());
        }
    }
    assert_eq!(merged.len(), t.comments.len(), "all replies preserved once");
}

/// 6: a resolved, outdated thread is historical, not a current defect.
#[test]
fn s06_resolved_outdated_thread_is_historical_only() {
    let b = base();
    let t = thread(&b, "PRRT_synthHuman0002");
    assert!(t.is_resolved && t.is_outdated);
    assert!(t.line.is_none() && t.original_line.is_some());
    let current_unresolved: Vec<_> = b
        .github_threads
        .iter()
        .filter(|t| !t.is_resolved && !t.is_outdated)
        .map(|t| t.node_id.as_str())
        .collect();
    assert!(!current_unresolved.contains(&"PRRT_synthHuman0002"));
    let historical: Vec<_> = b
        .findings
        .iter()
        .filter(|f| f.applicability == Applicability::HistoricalOutdated)
        .collect();
    let f = historical
        .iter()
        .find(|f| f.anchor.github_thread_id.as_deref() == Some("PRRT_synthHuman0002"))
        .expect("findable under a historical filter");
    assert_eq!(f.source_status, SourceStatus::Resolved);
    assert!(f.anchor.line.is_none() && f.anchor.original_line == Some(190));
    assert!(f.attention.level < AttentionLevel::Investigate);
}

/// 7: a check annotation keeps its check run, exact range and SHA, and is not a
/// replyable thread.
#[test]
fn s07_check_annotation_is_ci_information_not_a_thread() {
    let b = base();
    let f = b
        .findings
        .iter()
        .find(|f| f.source.kind == SourceKind::GithubCheckAnnotation)
        .unwrap();
    let native = f.source.native.as_ref().unwrap();
    assert!(!native.key.is_upstream(), "fingerprint, not a GitHub ID");
    let a = b
        .check_annotations
        .iter()
        .find(|a| a.key == native.key)
        .unwrap();
    let check = b
        .check_runs
        .iter()
        .find(|c| c.github_check_run_id == a.check_run_id)
        .unwrap();
    assert_eq!(check.name, "lint");
    assert_eq!(f.anchor.current_path.as_deref(), Some(a.path.as_str()));
    assert_eq!(
        (f.anchor.start_line, f.anchor.line),
        (Some(a.start_line), Some(a.end_line))
    );
    assert_eq!(f.anchor.commit_sha.as_deref(), Some(a.head_sha.as_str()));
    assert_eq!(a.head_sha, check.head_sha);
    assert!(f.anchor.github_thread_id.is_none());
    assert!(!b.github_threads.iter().any(|t| t.path == a.path
        && t.line == Some(a.end_line)
        && t.comments.iter().any(|c| c.body == a.message)));
}

/// 8: an ignored file with unresolved threads is counted, and its findings are
/// preserved rather than dropped.
#[test]
fn s08_ignored_file_with_unresolved_threads_is_counted() {
    let b = base();
    let ignored_unresolved: Vec<_> = b
        .github_threads
        .iter()
        .filter(|t| !t.is_resolved && rules::evaluate(&b.repository_rules, &t.path).ignored)
        .collect();
    assert_eq!(ignored_unresolved.len(), 1);
    let path = &ignored_unresolved[0].path;
    assert!(
        b.findings
            .iter()
            .any(|f| f.anchor.current_path.as_ref() == Some(path))
    );
    let static_run = b
        .runs
        .iter()
        .find(|r| r.provider_id == "aside.static" && r.status == RunStatus::Succeeded)
        .unwrap();
    let file_id = demo::snapshot(0)
        .files
        .into_iter()
        .find(|f| &f.path == path)
        .unwrap()
        .id;
    assert_eq!(
        static_run.coverage.file_outcome(&file_id),
        ScopeOutcome::NotRun {
            reason: "skipped_ignored_file".into()
        }
    );
}

/// 10: a refresh that changes one file's digest only stales that file's
/// findings; earlier stale findings stay historical.
#[test]
fn s10_refresh_stales_only_the_changed_files_findings() {
    let b = base();
    let before = demo::snapshot(0);
    let after = demo::snapshot(1);
    let current_before: Vec<_> = b
        .findings
        .iter()
        .filter(|f| applies_to(f, &before))
        .collect();
    let flipped: Vec<_> = current_before
        .iter()
        .filter(|f| !applies_to(f, &after))
        .map(|f| f.anchor.current_path.as_deref().unwrap())
        .collect();
    assert_eq!(flipped, ["git/client.go"]);
    assert!(current_before.len() > 1);
    let stale = finding(&b, "Token variable list duplicated");
    assert_eq!(stale.applicability, Applicability::HistoricalOutdated);
    assert_ne!(stale.observed_snapshot_id, b.manifest.snapshot_id);
    assert!(!applies_to(stale, &before) && !applies_to(stale, &after));
    assert_eq!(
        run(&b, stale.source.run_id.as_ref().unwrap()).status,
        RunStatus::Stale
    );
}

/// 11: LLM is off by default.
#[test]
fn s11_llm_is_off_by_default() {
    let b = base();
    assert!(!b.llm_included && !b.manifest.llm_enabled_by_default);
    assert!(b.runs.iter().all(|r| r.mode != AnalysisMode::Llm));
    assert!(b.findings.iter().all(|f| f.source.kind != SourceKind::Llm));
    assert!(
        b.findings
            .iter()
            .all(|f| f.assertion_kind != AssertionKind::DefectHypothesis)
    );
}

/// 12 (and §19.4): an opted-in LLM finding is a labelled, unverified
/// hypothesis with self-reported confidence; promoting it makes a local draft.
#[test]
fn s12_llm_finding_is_a_labelled_hypothesis() {
    let b = full();
    assert!(b.llm_included);
    let f = b
        .findings
        .iter()
        .find(|f| f.source.kind == SourceKind::Llm)
        .unwrap();
    assert_eq!(f.assertion_kind, AssertionKind::DefectHypothesis);
    assert!(f.title.starts_with("Hypothesis:"));
    assert_eq!(f.extensions["hypothesis"], Value::Bool(true));
    assert!(f.source.model_id.is_some() && f.source.prompt_version.is_some());
    assert!(f.source.native.is_none());
    assert_eq!(f.validation.status, ValidationStatus::Unverified);
    let c = f.confidence.as_ref().unwrap();
    assert_eq!(c.kind, "model_self_reported");
    assert!(c.calibration_version.is_none());
    assert!(f.attention.level < AttentionLevel::High);
    assert_eq!(
        run(&b, f.source.run_id.as_ref().unwrap())
            .metrics
            .estimated_cost_usd,
        None,
        "unknown cost is not fabricated"
    );
    let state = b.user_states.iter().find(|s| s.finding_id == f.id).unwrap();
    assert_eq!(state.disposition, UserDisposition::PromotedToDraft);
    assert!(state.promoted_draft_id.is_some());
}

/// 13: output is typed, carries IDs and evidence, and says when it is partial.
#[test]
fn s13_output_is_typed_complete_or_partial_with_ids_and_evidence() {
    let b = base();
    assert!(
        b.findings
            .iter()
            .all(|f| f.id.as_str().starts_with("find_"))
    );
    assert!(
        b.findings
            .iter()
            .filter(|f| !f.source.kind.is_imported())
            .all(|f| !f.evidence_ids.is_empty())
    );
    let statuses: BTreeSet<_> = b.runs.iter().map(|r| format!("{:?}", r.status)).collect();
    assert!(statuses.contains("Partial") && statuses.contains("Failed"));
    let import = b
        .runs
        .iter()
        .find(|r| r.provider_id == "github.import")
        .unwrap();
    assert_eq!(import.coverage.completeness, Completeness::Partial);
    assert!(import.coverage.entries.iter().any(|e| matches!(
        &e.outcome,
        ScopeOutcome::Unknown { reason } if reason.contains("rate_limited")
    )));
}

/// Correlated sources (§19.4): several providers report one topic; each keeps
/// its provenance and they are linked, not merged.
#[test]
fn correlated_topic_keeps_each_providers_provenance() {
    for (b, expected_kinds) in [(base(), 3), (full(), 4)] {
        let group: Vec<_> = b
            .findings
            .iter()
            .filter(|f| f.correlation_group_id.as_deref() == Some("corr_ssh_variant_normalisation"))
            .collect();
        let kinds: HashSet<_> = group.iter().map(|f| f.source.kind).collect();
        assert_eq!(kinds.len(), expected_kinds);
        let ids: HashSet<_> = group.iter().map(|f| &f.id).collect();
        assert_eq!(ids.len(), group.len(), "no finding overwrote another");
        for f in group
            .iter()
            .filter(|f| f.source.kind != SourceKind::GithubReviewThread)
        {
            assert!(
                b.relations.iter().any(|r| r.source_finding_id == f.id
                    || matches!(&r.target, RelationTarget::Finding(id) if id == &f.id)),
                "{} is linked",
                f.title
            );
        }
        assert!(
            b.relations
                .iter()
                .any(|r| r.kind == RelationKind::DuplicateTopicOf && !r.validated)
        );
    }
}

/// 16: a failed provider is isolated; review and imported feedback still work,
/// and its coverage is unknown rather than zero.
#[test]
fn s16_failed_provider_is_isolated() {
    let b = base();
    let inspect = b.runs.iter().find(|r| r.provider_id == "inspect").unwrap();
    assert_eq!(inspect.status, RunStatus::Failed);
    assert_eq!(inspect.coverage.completeness, Completeness::Failed);
    assert_eq!(inspect.coverage.analyzed_files, KnownCount::Unknown);
    assert!(
        matches!(
            inspect.coverage.file_outcome("f_f0371c00cdf24354"),
            ScopeOutcome::Unknown { .. }
        ),
        "no entry is unknown, never clean"
    );
    assert!(
        b.findings
            .iter()
            .all(|f| f.source.run_id.as_ref() != Some(&inspect.id))
    );
    assert!(!b.github_threads.is_empty() && !b.findings.is_empty());
    assert!(demo::snapshot(0).complete);
}

/// 17: only failed/partial providers never yield "0 issues found".
#[test]
fn s17_failed_or_partial_runs_never_claim_zero_issues() {
    let b = base();
    let degraded: Vec<_> = b
        .runs
        .iter()
        .filter(|r| matches!(r.status, RunStatus::Failed | RunStatus::Partial))
        .cloned()
        .collect();
    assert!(degraded.len() >= 2);
    assert!(!may_claim_no_issues(&degraded, &[]));
    assert!(!may_claim_no_issues(&b.runs, &[]));
    let succeeded: Vec<_> = b
        .runs
        .iter()
        .filter(|r| r.status == RunStatus::Succeeded)
        .cloned()
        .collect();
    assert!(may_claim_no_issues(&succeeded, &[]));
}

/// 18: every current high-attention finding links to concrete code or a
/// declared PR-level context, with rationale and explicit input versions.
#[test]
fn s18_high_attention_findings_are_grounded() {
    let b = full();
    let high: Vec<_> = b
        .findings
        .iter()
        .filter(|f| f.attention.level == AttentionLevel::High && f.applicability.is_current())
        .collect();
    assert!(high.len() >= 2);
    for f in high {
        let a: &CodeAnchor = &f.anchor;
        let grounded = a.kind == crate::model::AnchorKind::Pr
            || (a.current_path.is_some() && a.file_id.is_some() && a.diff_digest.is_some());
        assert!(grounded, "{}", f.title);
        assert!(!f.rationale.trim().is_empty() && !f.evidence_ids.is_empty());
        let r = run(&b, f.source.run_id.as_ref().unwrap());
        assert!(!r.engine_version.is_empty() && !r.input_digest.is_empty());
        assert!(f.source.rule_version.is_some());
        for e in b.evidence.iter().filter(|e| f.evidence_ids.contains(&e.id)) {
            assert!(
                e.method.is_some() && e.input_digest.is_some(),
                "{}",
                e.label
            );
        }
    }
}

/// Local triage never touches native source state or review progress.
#[test]
fn local_dispositions_stay_local() {
    let b = base();
    let bot = b
        .findings
        .iter()
        .find(|f| f.anchor.github_thread_id.as_deref() == Some("PRRT_synthBot0001"))
        .unwrap();
    let state = b
        .user_states
        .iter()
        .find(|s| s.finding_id == bot.id)
        .unwrap();
    assert_eq!(state.disposition, UserDisposition::Investigating);
    assert_eq!(bot.source_status, SourceStatus::Open);
    assert!(!thread(&b, "PRRT_synthBot0001").is_resolved);
}

/// Coverage distinguishes analysed-clean, not-applicable and not-run files.
#[test]
fn coverage_separates_clean_not_applicable_and_not_run() {
    let b = base();
    let entries: Vec<_> = b.runs.iter().flat_map(|r| &r.coverage.entries).collect();
    assert!(entries.iter().any(|e| e.outcome.is_no_finding()));
    assert!(entries.iter().any(|e| e.outcome.is_not_applicable()));
    assert!(
        entries
            .iter()
            .any(|e| matches!(e.outcome, ScopeOutcome::NotRun { .. }))
    );
    assert!(entries.iter().any(|e| matches!(e.scope, CoverageScope::Pr)));
}

// --- TypeScript fixture for the mock UI ---------------------------------------

/// Writes `app/src/generated/fixtures/demoIntelligence.ts`, typed with the
/// ts-rs generated `DemoIntelligence`. `pnpm -C app check` then fails if the
/// fixture and the generated types disagree.
#[test]
fn export_typescript_fixture() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("app/src/generated/fixtures");
    std::fs::create_dir_all(&dir).unwrap();
    let render = |b: &DemoIntelligence| serde_json::to_string_pretty(b).unwrap();
    let text = format!(
        "// Generated by `cargo test` from fixtures/analysis/demo-cli-14921. Do not edit.\n\
         // Synthetic data only; see the fixture manifest.\n\
         import type {{ DemoIntelligence }} from \"../DemoIntelligence\";\n\n\
         export const demoIntelligence: DemoIntelligence = {};\n\n\
         export const demoIntelligenceWithLlm: DemoIntelligence = {};\n",
        render(&base()),
        render(&full()),
    );
    let path = dir.join("demoIntelligence.ts");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
        std::fs::write(&path, text).unwrap();
    }
}
