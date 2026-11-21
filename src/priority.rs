//! Explainable review queue and scope-aware counts (#30, analyze.md §10.8,
//! §11).
//!
//! Everything here is a pure function of stored facts: file review state
//! (acknowledgements), repository exclusion rules, stored findings, native
//! GitHub threads, local triage and per-source coverage. Nothing is written
//! back, so the result is presentation only:
//!
//! - **Exclusion edits only re-rank.** Rules are an input here and nowhere
//!   in the analysis key (`model::lifecycle::AnalysisKey`), so excluding and
//!   re-including a file recomputes this view without rerunning engines, and
//!   the round trip returns exactly the original queue.
//! - **Three axes stay separate.** Each [`QueueItem`] carries review
//!   acknowledgement ([`FileStatus`]), attention ([`AttentionLevel`]) and the
//!   per-finding [`SeverityRecord`] side by side. None is derived from another
//!   and they are never folded into one number.
//! - **No fake probabilities.** The queue is the lexicographic bucket order of
//!   analyze.md §11.2. Within a bucket, items are ordered by discrete,
//!   countable indicators and then path. Provider scores and confidence values
//!   are deliberately not read or exposed; every item carries the concrete
//!   [`Reason`]s (finding, evidence, thread, rule) that placed it.
//! - **Counts partition.** Every count is a [`Tally`] with
//!   `in_scope + ignored == total`, using `rules::evaluate` as the single
//!   scope decision.
//! - **Coverage is honest.** Counts and the queue carry the
//!   [`OverallCoverage`] from `model::lifecycle::overall`; an empty queue is
//!   only [`QueueVerdict::AllClear`] when coverage is complete.
//! - **Correlation keeps provenance.** Related findings and threads are
//!   grouped in [`TopicGroup`]s that list member IDs; source records are
//!   borrowed, never merged or rewritten.
//!
//! TODO(#33): the session view / findings API should call [`build_queue`] and
//! [`scope_counts`] over `Store::list_findings`, the snapshot threads and
//! `AnalysisSummary::sources`. No endpoint or wire format is defined here.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;
use ts_rs::TS;

use crate::{
    model::{
        AssertionKind, AttentionLevel, ExclusionRule, FileScope, FileState, FileStatus, FindingId,
        FindingRelation, FindingUserState, GithubThread, OverallCoverage, RelationKind,
        RelationTarget, ReviewFinding, SeverityRecord, SourceCoverage, SourceKind, UserDisposition,
        ValidationStatus, overall,
    },
    rules,
    store::{ObservationLabel, StoredFinding},
};

/// A stored finding with its derived current/historical label.
#[derive(Debug, Clone, PartialEq)]
pub struct ObservedFinding {
    pub finding: ReviewFinding,
    /// True when the finding applies to the PR's current snapshot.
    pub current: bool,
}

impl From<&StoredFinding> for ObservedFinding {
    /// Reads the decoded view of `Raw<ReviewFinding>`. The raw JSON stays in
    /// the store; this module never writes findings back, so lossy
    /// `#[serde(other)]` decoding cannot leak into storage from here.
    fn from(stored: &StoredFinding) -> Self {
        Self {
            finding: stored.finding.value().clone(),
            current: stored.label == ObservationLabel::Current,
        }
    }
}

/// Everything the queue is derived from. All borrowed and unmodified.
#[derive(Debug, Clone, Copy)]
pub struct QueueInput<'a> {
    /// One per changed file. Only `review_status` (acknowledgement) is read;
    /// scope is re-evaluated from `rules` so stale `scope` cannot leak in.
    pub files: &'a [FileState],
    pub rules: &'a [ExclusionRule],
    pub findings: &'a [ObservedFinding],
    pub relations: &'a [FindingRelation],
    /// Native GitHub review threads from the snapshot.
    pub threads: &'a [GithubThread],
    pub user_states: &'a [FindingUserState],
    /// Every source of the current snapshot (`AnalysisSummary::sources`).
    pub coverage: &'a [SourceCoverage],
}

/// Lexicographic queue buckets (analyze.md §11.2), in queue order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Bucket {
    /// Changed since reviewed, with an unresolved current thread or a
    /// validated high-attention finding.
    ChangedSinceReviewedWithFeedback,
    /// Unreviewed, with an unresolved current thread or a confirmed failure.
    UnreviewedWithFeedback,
    /// Not reviewed at the current digest, with high deterministic attention.
    HighDeterministicAttention,
    /// Other in-scope files that still need review.
    NeedsReview,
    /// Reviewed at the current digest. Accessible, not "remaining".
    Reviewed,
    /// Excluded by repository rules. Discoverable, outside the normal queue.
    Ignored,
}

impl Bucket {
    /// True for buckets that make up the remaining review work.
    pub fn is_remaining(self) -> bool {
        matches!(
            self,
            Self::ChangedSinceReviewedWithFeedback
                | Self::UnreviewedWithFeedback
                | Self::HighDeterministicAttention
                | Self::NeedsReview
        )
    }
}

/// What kind of fact a [`Reason`] cites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReasonCode {
    /// Acknowledgement: no current review of this file.
    Unreviewed,
    /// Acknowledgement: reviewed, but the diff changed since.
    ChangedSinceReviewed,
    /// Acknowledgement: reviewed at the current digest.
    ReviewedCurrent,
    /// Scope: an exclusion rule decides this file is ignored.
    ExcludedByRule,
    /// An unresolved, not-outdated GitHub thread.
    UnresolvedThread,
    /// An unresolved thread GitHub marks outdated. Shown, never bucket-driving.
    OutdatedUnresolvedThread,
    /// A finding asserting a confirmed test or check failure.
    ConfirmedFailure,
    /// A high-attention finding that is validated or needs no validation.
    ValidatedHighAttention,
    /// A high-attention finding from a deterministic (static) provider.
    DeterministicHighAttention,
    /// Any other current finding at `investigate` or above.
    FindingAttention,
}

/// One concrete fact behind an item's position. At least one of the IDs is
/// set for every code except the acknowledgement codes, which cite the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Reason {
    pub code: ReasonCode,
    pub finding_id: Option<FindingId>,
    pub provider_id: Option<String>,
    /// Provider rule (`source.ruleId`) or exclusion rule ID.
    pub rule_id: Option<String>,
    pub thread_id: Option<String>,
    pub evidence_ids: Vec<String>,
}

impl Reason {
    fn file(code: ReasonCode) -> Self {
        Self {
            code,
            finding_id: None,
            provider_id: None,
            rule_id: None,
            thread_id: None,
            evidence_ids: Vec::new(),
        }
    }

    fn finding(code: ReasonCode, finding: &ReviewFinding) -> Self {
        Self {
            code,
            finding_id: Some(finding.id.clone()),
            provider_id: Some(finding.source.provider_id.clone()),
            rule_id: finding.source.rule_id.clone(),
            thread_id: finding.anchor.github_thread_id.clone(),
            evidence_ids: finding
                .evidence_ids
                .iter()
                .map(|e| e.as_str().to_string())
                .collect(),
        }
    }

    fn thread(code: ReasonCode, thread_id: &str) -> Self {
        Self {
            thread_id: Some(thread_id.to_string()),
            ..Self::file(code)
        }
    }
}

/// A finding attached to a file, with its three axes kept apart. Copies no
/// score or confidence from the finding on purpose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FindingEntry {
    pub id: FindingId,
    pub source_kind: SourceKind,
    pub provider_id: String,
    pub current: bool,
    /// Attention the producer assigned (presentation hint, discrete).
    pub attention: AttentionLevel,
    /// Impact if true, as the source stated it. `unknown` stays unknown.
    pub severity: SeverityRecord,
    pub validation: ValidationStatus,
    /// Local triage overlay; `untouched` when none is stored.
    pub disposition: UserDisposition,
}

/// Findings and threads about one topic, grouped for display. Members are
/// IDs of untouched source records; nothing is merged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TopicGroup {
    /// `correlationGroupId` when a member has one, else the smallest member ID.
    pub key: String,
    pub findings: Vec<FindingId>,
    pub threads: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct QueueItem {
    pub file_id: String,
    pub path: String,
    pub bucket: Bucket,
    /// Axis 1: human/agent acknowledgement, ignoring scope.
    pub acknowledgement: FileStatus,
    /// Axis 2: highest attention among current findings that are not
    /// dismissed or snoozed locally. Never derived from severity.
    pub attention: AttentionLevel,
    pub scope: FileScope,
    /// Why the item is in its bucket and position. Never empty.
    pub reasons: Vec<Reason>,
    /// Axis 3 lives per finding in here, alongside triage.
    pub findings: Vec<FindingEntry>,
    /// Native thread IDs on this path (any state).
    pub threads: Vec<String>,
    pub topics: Vec<TopicGroup>,
}

/// Whether an empty remaining queue may be read as "nothing to review".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", content = "count", rename_all = "snake_case")]
#[ts(export)]
pub enum QueueVerdict {
    /// Remaining items exist.
    Remaining(#[ts(type = "number")] usize),
    /// Nothing remains and every source is complete.
    AllClear,
    /// Nothing remains so far, but some source is still running.
    EmptyPending,
    /// Nothing remains, but some source is incomplete, failed or never ran:
    /// this is *not* "nothing to review".
    EmptyIncomplete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ReviewQueue {
    /// Every changed file exactly once, in queue order.
    pub items: Vec<QueueItem>,
    /// Current findings with no file path (PR-level), never dropped.
    pub pr_level_findings: Vec<FindingEntry>,
    pub coverage: OverallCoverage,
    pub verdict: QueueVerdict,
}

impl ReviewQueue {
    pub fn remaining(&self) -> impl Iterator<Item = &QueueItem> {
        self.items.iter().filter(|i| i.bucket.is_remaining())
    }
}

/// A count split by scope. `in_scope + ignored == total` always holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Tally {
    pub total: u32,
    pub in_scope: u32,
    pub ignored: u32,
}

impl Tally {
    fn add(&mut self, ignored: bool) {
        self.total += 1;
        if ignored {
            self.ignored += 1;
        } else {
            self.in_scope += 1;
        }
    }

    pub fn partitions(&self) -> bool {
        self.in_scope + self.ignored == self.total
    }
}

/// Scope-aware summary counts (analyze.md §11.4). Labels denominators: file,
/// thread and finding tallies are separate and never summed together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScopeCounts {
    pub files: Tally,
    /// In-scope files only, by acknowledgement.
    pub reviewed_current: u32,
    pub changed_since_reviewed: u32,
    pub unreviewed: u32,
    /// Threads, not comments. Counted once per native thread ID.
    pub threads: Tally,
    /// In-scope threads by state.
    pub threads_unresolved_current: u32,
    pub threads_unresolved_outdated: u32,
    pub threads_resolved: u32,
    pub current_findings: Tally,
    pub historical_findings: Tally,
    /// Not authoritative unless `Complete`: a zero from an incomplete source
    /// proves nothing.
    pub coverage: OverallCoverage,
}

impl ScopeCounts {
    pub fn authoritative(&self) -> bool {
        self.coverage == OverallCoverage::Complete
    }
}

/// True when `path` is excluded. Paths missing means PR-level: in scope,
/// matching `Session::analysis_summary`.
fn ignored(rules_: &[ExclusionRule], path: Option<&str>) -> bool {
    path.is_some_and(|p| rules::evaluate(rules_, p).ignored)
}

/// Counts partitioned by scope.
pub fn scope_counts(input: &QueueInput<'_>) -> ScopeCounts {
    let mut counts = ScopeCounts {
        files: Tally::default(),
        reviewed_current: 0,
        changed_since_reviewed: 0,
        unreviewed: 0,
        threads: Tally::default(),
        threads_unresolved_current: 0,
        threads_unresolved_outdated: 0,
        threads_resolved: 0,
        current_findings: Tally::default(),
        historical_findings: Tally::default(),
        coverage: overall(input.coverage),
    };
    for file in input.files {
        let is_ignored = ignored(input.rules, Some(&file.path));
        counts.files.add(is_ignored);
        if !is_ignored {
            match file.review_status {
                FileStatus::ReviewedCurrent => counts.reviewed_current += 1,
                FileStatus::ChangedSinceReviewed => counts.changed_since_reviewed += 1,
                // `Ignored` is never a review status; count it as unreviewed
                // rather than drop it.
                FileStatus::Unreviewed | FileStatus::Ignored => counts.unreviewed += 1,
            }
        }
    }
    let mut seen = BTreeSet::new();
    for thread in input.threads {
        if !seen.insert(thread.node_id.as_str()) {
            continue; // Same native thread delivered twice.
        }
        let is_ignored = ignored(input.rules, Some(&thread.path));
        counts.threads.add(is_ignored);
        if !is_ignored {
            match (thread.is_resolved, thread.is_outdated) {
                (true, _) => counts.threads_resolved += 1,
                (false, false) => counts.threads_unresolved_current += 1,
                (false, true) => counts.threads_unresolved_outdated += 1,
            }
        }
    }
    let mut seen = BTreeSet::new();
    for observed in input.findings {
        if !seen.insert(observed.finding.id.as_str()) {
            continue;
        }
        let is_ignored = ignored(input.rules, observed.finding.anchor.current_path.as_deref());
        if observed.current {
            counts.current_findings.add(is_ignored);
        } else {
            counts.historical_findings.add(is_ignored);
        }
    }
    counts
}

fn suppressed(disposition: UserDisposition) -> bool {
    matches!(
        disposition,
        UserDisposition::DismissedLocal | UserDisposition::Snoozed
    )
}

fn deterministic(kind: SourceKind) -> bool {
    matches!(kind, SourceKind::AsideStatic | SourceKind::InspectStatic)
}

/// Builds the suggested review order. Pure: same input, same queue.
pub fn build_queue(input: &QueueInput<'_>) -> ReviewQueue {
    let dispositions: HashMap<&str, UserDisposition> = input
        .user_states
        .iter()
        .map(|s| (s.finding_id.as_str(), s.disposition))
        .collect();
    let disposition = |id: &FindingId| {
        dispositions
            .get(id.as_str())
            .copied()
            .unwrap_or(UserDisposition::Untouched)
    };
    let entry = |o: &ObservedFinding| FindingEntry {
        id: o.finding.id.clone(),
        source_kind: o.finding.source.kind,
        provider_id: o.finding.source.provider_id.clone(),
        current: o.current,
        attention: o.finding.attention.level,
        severity: o.finding.severity.clone(),
        validation: o.finding.validation.status,
        disposition: disposition(&o.finding.id),
    };

    let mut by_path: BTreeMap<&str, Vec<&ObservedFinding>> = BTreeMap::new();
    let mut pr_level = Vec::new();
    let mut seen = BTreeSet::new();
    for o in input.findings {
        if !seen.insert(o.finding.id.as_str()) {
            continue;
        }
        match o.finding.anchor.current_path.as_deref() {
            Some(path) => by_path.entry(path).or_default().push(o),
            None if o.current => pr_level.push(entry(o)),
            None => {}
        }
    }
    let mut threads_by_path: BTreeMap<&str, Vec<&GithubThread>> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for t in input.threads {
        if seen.insert(t.node_id.as_str()) {
            threads_by_path.entry(t.path.as_str()).or_default().push(t);
        }
    }

    let mut items: Vec<QueueItem> = input
        .files
        .iter()
        .map(|file| {
            let findings = by_path.get(file.path.as_str()).cloned().unwrap_or_default();
            let threads = threads_by_path
                .get(file.path.as_str())
                .cloned()
                .unwrap_or_default();
            build_item(input, file, &findings, &threads, &disposition, &entry)
        })
        .collect();
    items.sort_by(|a, b| {
        a.bucket
            .cmp(&b.bucket)
            .then_with(|| b.attention.cmp(&a.attention))
            .then_with(|| distinct_driving_topics(b).cmp(&distinct_driving_topics(a)))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.file_id.cmp(&b.file_id))
    });

    let coverage = overall(input.coverage);
    let remaining = items.iter().filter(|i| i.bucket.is_remaining()).count();
    let verdict = match (remaining, coverage) {
        (n, _) if n > 0 => QueueVerdict::Remaining(n),
        (_, OverallCoverage::Complete) => QueueVerdict::AllClear,
        (_, OverallCoverage::Pending) => QueueVerdict::EmptyPending,
        (_, OverallCoverage::Incomplete) => QueueVerdict::EmptyIncomplete,
    };
    ReviewQueue {
        items,
        pr_level_findings: pr_level,
        coverage,
        verdict,
    }
}

/// Topic groups that contribute a non-acknowledgement reason. Counting groups
/// rather than findings keeps one verbose source from dominating (§11.3).
fn distinct_driving_topics(item: &QueueItem) -> usize {
    item.topics
        .iter()
        .filter(|g| {
            item.reasons.iter().any(|r| {
                r.finding_id
                    .as_ref()
                    .is_some_and(|f| g.findings.contains(f))
                    || r.thread_id.as_ref().is_some_and(|t| g.threads.contains(t))
            })
        })
        .count()
}

fn build_item(
    input: &QueueInput<'_>,
    file: &FileState,
    findings: &[&ObservedFinding],
    threads: &[&GithubThread],
    disposition: &dyn Fn(&FindingId) -> UserDisposition,
    entry: &dyn Fn(&ObservedFinding) -> FindingEntry,
) -> QueueItem {
    let scope = rules::evaluate(input.rules, &file.path);
    let mut reasons = Vec::new();
    let mut thread_driven = false;
    for t in threads.iter().filter(|t| !t.is_resolved) {
        if t.is_outdated {
            reasons.push(Reason::thread(
                ReasonCode::OutdatedUnresolvedThread,
                &t.node_id,
            ));
        } else {
            thread_driven = true;
            reasons.push(Reason::thread(ReasonCode::UnresolvedThread, &t.node_id));
        }
    }
    let thread_ids: BTreeSet<&str> = threads.iter().map(|t| t.node_id.as_str()).collect();

    let mut attention = AttentionLevel::None;
    let (mut validated_high, mut confirmed_failure, mut deterministic_high) = (false, false, false);
    for o in findings
        .iter()
        .filter(|o| o.current && !suppressed(disposition(&o.finding.id)))
    {
        let f = &o.finding;
        attention = attention.max(f.attention.level);
        // A projection of a native thread is already cited via the thread.
        if f.anchor
            .github_thread_id
            .as_deref()
            .is_some_and(|t| thread_ids.contains(t))
        {
            continue;
        }
        let high = f.attention.level == AttentionLevel::High;
        let validated = matches!(
            f.validation.status,
            ValidationStatus::Supported | ValidationStatus::NotRequired
        );
        let code = if f.assertion_kind == AssertionKind::ConfirmedTestFailure {
            confirmed_failure = true;
            ReasonCode::ConfirmedFailure
        } else if high && deterministic(f.source.kind) && validated {
            validated_high = true;
            deterministic_high = true;
            ReasonCode::DeterministicHighAttention
        } else if high && validated {
            validated_high = true;
            ReasonCode::ValidatedHighAttention
        } else if f.attention.level >= AttentionLevel::Investigate {
            ReasonCode::FindingAttention
        } else {
            continue;
        };
        reasons.push(Reason::finding(code, f));
    }

    let acknowledgement = file.review_status;
    let bucket = if scope.ignored {
        Bucket::Ignored
    } else {
        match acknowledgement {
            FileStatus::ReviewedCurrent => Bucket::Reviewed,
            FileStatus::ChangedSinceReviewed if thread_driven || validated_high => {
                Bucket::ChangedSinceReviewedWithFeedback
            }
            FileStatus::Unreviewed | FileStatus::Ignored if thread_driven || confirmed_failure => {
                Bucket::UnreviewedWithFeedback
            }
            _ if deterministic_high => Bucket::HighDeterministicAttention,
            _ => Bucket::NeedsReview,
        }
    };
    let mut leading = Vec::new();
    if scope.ignored {
        leading.push(Reason {
            rule_id: scope.effective_rule_id.clone(),
            ..Reason::file(ReasonCode::ExcludedByRule)
        });
    }
    leading.push(Reason::file(match acknowledgement {
        FileStatus::ReviewedCurrent => ReasonCode::ReviewedCurrent,
        FileStatus::ChangedSinceReviewed => ReasonCode::ChangedSinceReviewed,
        FileStatus::Unreviewed | FileStatus::Ignored => ReasonCode::Unreviewed,
    }));
    leading.append(&mut reasons);

    QueueItem {
        file_id: file.file_id.clone(),
        path: file.path.clone(),
        bucket,
        acknowledgement,
        attention,
        scope,
        reasons: leading,
        findings: findings.iter().map(|o| entry(o)).collect(),
        threads: threads.iter().map(|t| t.node_id.clone()).collect(),
        topics: correlate(findings, threads, input.relations),
    }
}

/// Groups one file's findings and threads by topic: shared
/// `correlationGroupId`, `duplicate_topic_of`/`supports` relations, a
/// `references_thread` relation, or a finding projecting a thread. Union-find
/// over IDs only; the records themselves are untouched.
fn correlate(
    findings: &[&ObservedFinding],
    threads: &[&GithubThread],
    relations: &[FindingRelation],
) -> Vec<TopicGroup> {
    // Nodes: "f:<id>" and "t:<id>".
    let mut parent: BTreeMap<String, String> = BTreeMap::new();
    fn find(parent: &mut BTreeMap<String, String>, x: &str) -> String {
        let p = parent.get(x).cloned().unwrap_or_else(|| x.to_string());
        if p == x {
            return p;
        }
        let root = find(parent, &p);
        parent.insert(x.to_string(), root.clone());
        root
    }
    fn union(parent: &mut BTreeMap<String, String>, a: &str, b: &str) {
        let (ra, rb) = (find(parent, a), find(parent, b));
        if ra != rb {
            let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
            parent.insert(hi, lo);
        }
    }
    for o in findings {
        parent.insert(
            format!("f:{}", o.finding.id.as_str()),
            format!("f:{}", o.finding.id.as_str()),
        );
    }
    for t in threads {
        parent.insert(format!("t:{}", t.node_id), format!("t:{}", t.node_id));
    }
    let present = |k: &str, p: &BTreeMap<String, String>| p.contains_key(k);
    let mut by_group: BTreeMap<&str, String> = BTreeMap::new();
    for o in findings {
        let node = format!("f:{}", o.finding.id.as_str());
        if let Some(g) = o.finding.correlation_group_id.as_deref() {
            match by_group.get(g) {
                Some(first) => {
                    let first = first.clone();
                    union(&mut parent, &first, &node);
                }
                None => {
                    by_group.insert(g, node.clone());
                }
            }
        }
        if let Some(t) = o.finding.anchor.github_thread_id.as_deref() {
            let tn = format!("t:{t}");
            if present(&tn, &parent) {
                union(&mut parent, &node, &tn);
            }
        }
    }
    for r in relations {
        if !matches!(
            r.kind,
            RelationKind::DuplicateTopicOf
                | RelationKind::Supports
                | RelationKind::ReferencesThread
        ) {
            continue;
        }
        let a = format!("f:{}", r.source_finding_id.as_str());
        let b = match &r.target {
            RelationTarget::Finding(id) => format!("f:{}", id.as_str()),
            RelationTarget::Thread(id) => format!("t:{id}"),
            RelationTarget::Entity(_) => continue,
        };
        if present(&a, &parent) && present(&b, &parent) {
            union(&mut parent, &a, &b);
        }
    }
    let nodes: Vec<String> = parent.keys().cloned().collect();
    let mut groups: BTreeMap<String, TopicGroup> = BTreeMap::new();
    for node in nodes {
        let root = find(&mut parent, &node);
        let group = groups.entry(root.clone()).or_insert_with(|| TopicGroup {
            key: root[2..].to_string(),
            findings: Vec::new(),
            threads: Vec::new(),
        });
        match node.split_at(2) {
            ("f:", id) => group.findings.push(FindingId::new(id)),
            (_, id) => group.threads.push(id.to_string()),
        }
    }
    let correlation_ids: HashMap<&str, &str> = findings
        .iter()
        .filter_map(|o| {
            o.finding
                .correlation_group_id
                .as_deref()
                .map(|g| (o.finding.id.as_str(), g))
        })
        .collect();
    groups
        .into_values()
        .map(|mut g| {
            if let Some(cid) = g
                .findings
                .iter()
                .filter_map(|f| correlation_ids.get(f.as_str()))
                .min()
            {
                g.key = cid.to_string();
            }
            g
        })
        .collect()
}

#[cfg(test)]
#[path = "priority_tests.rs"]
mod tests;
