//! Synthetic, offline review-intelligence fixture for `aside demo` (#17).
//!
//! Layout follows analyze.md §19.1 under `fixtures/analysis/demo-cli-14921/`.
//! Every file deserialises into the #15 contract types (or the existing
//! snapshot/rules types) and is compiled into the binary with `include_str!`,
//! so loading it needs no network, no `gh`, no persistence (#18) and no
//! scheduler (#22).
//!
//! The optional LLM hypothesis lives in its own file and is only merged when
//! the caller opts in; the default bundle has no LLM run or finding
//! (analyze.md §8, §19.5 scenario 11).
//!
//! TODO(#33): serve this bundle through the analysis API in demo sessions.
//! TODO(#38): render it on the Review screen (`app/src/mock/intelligence.ts`
//! already exposes it with generated types).

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use ts_rs::TS;

use crate::model::{
    AnalysisRun, Evidence, ExclusionRule, FindingRelation, FindingUserState, GithubThread,
    ImportedCheckAnnotation, ImportedCheckRun, ImportedCommitStatus, ImportedReview, IssueComment,
    ReviewFinding, SnapshotId,
};

macro_rules! fixture {
    ($file:literal) => {
        include_str!(concat!("../../fixtures/analysis/demo-cli-14921/", $file))
    };
}

pub(crate) const MANIFEST: &str = fixture!("manifest.json");
pub(crate) const REPOSITORY_RULES: &str = fixture!("repository-rules.json");
pub(crate) const GITHUB_THREADS: &str = fixture!("github-review-threads.json");
pub(crate) const GITHUB_REVIEWS: &str = fixture!("github-reviews.json");
pub(crate) const GITHUB_ISSUE_COMMENTS: &str = fixture!("github-issue-comments.json");
pub(crate) const GITHUB_CHECK_RUNS: &str = fixture!("github-check-runs.json");
pub(crate) const GITHUB_ANNOTATIONS: &str = fixture!("github-annotations.json");
pub(crate) const GITHUB_COMMIT_STATUSES: &str = fixture!("github-commit-statuses.json");
pub(crate) const ANALYSIS_RUNS: &str = fixture!("analysis-runs.json");
pub(crate) const EVIDENCE: &str = fixture!("evidence.json");
pub(crate) const FINDINGS: &str = fixture!("expected-findings.json");
pub(crate) const RELATIONS: &str = fixture!("relations.json");
pub(crate) const USER_STATES: &str = fixture!("user-states.json");
pub(crate) const LLM_HYPOTHESIS: &str = fixture!("llm-hypothesis.json");
/// Raw REST `pulls/comments` responses. Source-like, not a contract type:
/// only used to test GraphQL+REST duplicate handling (§19.5 scenario 5).
#[cfg(test)]
pub(crate) const REST_REVIEW_COMMENTS: &str =
    fixture!("source-responses/github-rest-review-comments.json");

/// What the fixture is and which scenarios it covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct FixtureManifest {
    pub schema_version: u32,
    pub name: String,
    /// Always true: no record in the fixture came from GitHub.
    pub synthetic: bool,
    pub description: String,
    pub repository_id: String,
    #[ts(type = "number")]
    pub pr_number: u64,
    pub base_sha: String,
    pub head_sha: String,
    pub snapshot_id: SnapshotId,
    /// Synthetic earlier head that stale records were observed on.
    pub previous_head_sha: String,
    pub observed_at: String,
    pub seed: u32,
    pub llm_enabled_by_default: bool,
    /// analyze.md §19.5 scenario numbers exercised by tests over this fixture.
    pub scenarios: Vec<String>,
}

/// Opt-in LLM output (`llm-hypothesis.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct LlmHypothesisFixture {
    pub runs: Vec<AnalysisRun>,
    pub evidence: Vec<Evidence>,
    pub findings: Vec<ReviewFinding>,
    pub relations: Vec<FindingRelation>,
    pub user_states: Vec<FindingUserState>,
}

/// Everything the demo knows about review intelligence for its snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct DemoIntelligence {
    pub manifest: FixtureManifest,
    /// True only when the caller opted into the LLM hypothesis.
    pub llm_included: bool,
    pub repository_rules: Vec<ExclusionRule>,
    pub github_threads: Vec<GithubThread>,
    pub reviews: Vec<ImportedReview>,
    pub issue_comments: Vec<IssueComment>,
    pub check_runs: Vec<ImportedCheckRun>,
    pub check_annotations: Vec<ImportedCheckAnnotation>,
    pub commit_statuses: Vec<ImportedCommitStatus>,
    pub runs: Vec<AnalysisRun>,
    pub evidence: Vec<Evidence>,
    pub findings: Vec<ReviewFinding>,
    pub relations: Vec<FindingRelation>,
    pub user_states: Vec<FindingUserState>,
}

pub(crate) fn parse<T: DeserializeOwned>(name: &str, json: &str) -> T {
    serde_json::from_str(json)
        .unwrap_or_else(|error| panic!("bundled fixture {name} does not match its type: {error}"))
}

pub fn github_threads() -> Vec<GithubThread> {
    parse("github-review-threads.json", GITHUB_THREADS)
}

pub fn reviews() -> Vec<ImportedReview> {
    parse("github-reviews.json", GITHUB_REVIEWS)
}

pub fn issue_comments() -> Vec<IssueComment> {
    parse("github-issue-comments.json", GITHUB_ISSUE_COMMENTS)
}

/// Load the bundled fixture. `include_llm` merges the opt-in hypothesis;
/// the demo default is `false`.
pub fn load(include_llm: bool) -> DemoIntelligence {
    let mut bundle = DemoIntelligence {
        manifest: parse("manifest.json", MANIFEST),
        llm_included: false,
        repository_rules: parse("repository-rules.json", REPOSITORY_RULES),
        github_threads: github_threads(),
        reviews: reviews(),
        issue_comments: issue_comments(),
        check_runs: parse("github-check-runs.json", GITHUB_CHECK_RUNS),
        check_annotations: parse("github-annotations.json", GITHUB_ANNOTATIONS),
        commit_statuses: parse("github-commit-statuses.json", GITHUB_COMMIT_STATUSES),
        runs: parse("analysis-runs.json", ANALYSIS_RUNS),
        evidence: parse("evidence.json", EVIDENCE),
        findings: parse("expected-findings.json", FINDINGS),
        relations: parse("relations.json", RELATIONS),
        user_states: parse("user-states.json", USER_STATES),
    };
    if include_llm {
        let llm: LlmHypothesisFixture = parse("llm-hypothesis.json", LLM_HYPOTHESIS);
        bundle.llm_included = true;
        bundle.runs.extend(llm.runs);
        bundle.evidence.extend(llm.evidence);
        bundle.findings.extend(llm.findings);
        bundle.relations.extend(llm.relations);
        bundle.user_states.extend(llm.user_states);
    }
    bundle
}

#[cfg(test)]
#[path = "intelligence_tests.rs"]
mod tests;
