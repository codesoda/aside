//! Tests for the explainable review queue and scope-aware counts (#30), over
//! the synthetic demo intelligence fixture (#17).

use super::*;
use crate::{
    demo::{self, intelligence},
    model::{
        AnalysisMode, Completeness, KnownCount, RuleAction, RuleKind, Severity, SnapshotId,
        SourceState,
    },
};

const SSH: &str = "internal/identity/ssh_command.go";
const CONFIG: &str = "internal/config/config.go";
const SCRIPT: &str = "script/multiaccount-review/02-repository.sh";

struct Fixture {
    files: Vec<FileState>,
    rules: Vec<ExclusionRule>,
    findings: Vec<ObservedFinding>,
    relations: Vec<FindingRelation>,
    threads: Vec<GithubThread>,
    user_states: Vec<FindingUserState>,
    coverage: Vec<SourceCoverage>,
}

impl Fixture {
    fn load() -> Self {
        let b = intelligence::load(false);
        let files = demo::snapshot(0)
            .files
            .into_iter()
            .map(|f| FileState {
                file_id: f.id,
                path: f.path,
                status: FileStatus::Unreviewed,
                review_status: FileStatus::Unreviewed,
                scope: FileScope {
                    ignored: false,
                    effective_rule_id: None,
                    matching_rule_ids: Vec::new(),
                },
                last_reviewed_at: None,
            })
            .collect();
        let findings = b
            .findings
            .iter()
            .map(|f| ObservedFinding {
                finding: f.clone(),
                current: f.applicability.is_current(),
            })
            .collect();
        Self {
            files,
            rules: b.repository_rules,
            findings,
            relations: b.relations,
            threads: b.github_threads,
            user_states: b.user_states,
            coverage: vec![source(SourceState::HasResults)],
        }
    }

    fn input(&self) -> QueueInput<'_> {
        QueueInput {
            files: &self.files,
            rules: &self.rules,
            findings: &self.findings,
            relations: &self.relations,
            threads: &self.threads,
            user_states: &self.user_states,
            coverage: &self.coverage,
        }
    }

    fn queue(&self) -> ReviewQueue {
        build_queue(&self.input())
    }

    fn counts(&self) -> ScopeCounts {
        scope_counts(&self.input())
    }

    fn set_review(&mut self, path: &str, status: FileStatus) {
        let file = self.files.iter_mut().find(|f| f.path == path).unwrap();
        file.review_status = status;
    }
}

fn source(state: SourceState) -> SourceCoverage {
    SourceCoverage {
        source_id: "aside.static".into(),
        mode: AnalysisMode::Static,
        snapshot_id: SnapshotId::new("snap_test"),
        run_id: None,
        status: None,
        completeness: Completeness::Complete,
        items: KnownCount::Unknown,
        item_kind: "findings".into(),
        state,
        reason_codes: Vec::new(),
        limitations: Vec::new(),
    }
}

fn rule(
    id: &str,
    action: RuleAction,
    kind: RuleKind,
    expression: &str,
    order: i64,
) -> ExclusionRule {
    ExclusionRule {
        id: id.into(),
        action,
        kind,
        expression: expression.into(),
        enabled: true,
        order,
        created_at: "2026-11-01T00:00:00Z".into(),
        updated_at: "2026-11-01T00:00:00Z".into(),
    }
}

fn item<'a>(queue: &'a ReviewQueue, path: &str) -> &'a QueueItem {
    queue.items.iter().find(|i| i.path == path).unwrap()
}

#[test]
fn every_ranked_item_has_concrete_reasons_and_no_probability() {
    let fixture = Fixture::load();
    // The fixture findings carry provider scores and confidences; none may
    // surface in the queue.
    assert!(
        fixture
            .findings
            .iter()
            .any(|o| o.finding.attention.score.is_some() || o.finding.confidence.is_some())
    );
    let queue = fixture.queue();
    assert!(!queue.items.is_empty());
    for item in &queue.items {
        assert!(!item.reasons.is_empty(), "{} has no reason", item.path);
        for reason in &item.reasons {
            let cites_file = matches!(
                reason.code,
                ReasonCode::Unreviewed
                    | ReasonCode::ChangedSinceReviewed
                    | ReasonCode::ReviewedCurrent
            );
            assert!(
                cites_file
                    || reason.finding_id.is_some()
                    || reason.thread_id.is_some()
                    || reason.rule_id.is_some(),
                "{reason:?} on {} cites nothing concrete",
                item.path
            );
            if reason.finding_id.is_some() {
                assert!(reason.provider_id.is_some());
            }
        }
        // Every non-default bucket is backed by a specific reason.
        let has = |c: ReasonCode| item.reasons.iter().any(|r| r.code == c);
        match item.bucket {
            Bucket::Ignored => assert!(has(ReasonCode::ExcludedByRule)),
            Bucket::UnreviewedWithFeedback => {
                assert!(has(ReasonCode::UnresolvedThread) || has(ReasonCode::ConfirmedFailure))
            }
            Bucket::HighDeterministicAttention => {
                assert!(has(ReasonCode::DeterministicHighAttention))
            }
            _ => {}
        }
    }
    let rendered = format!("{queue:?} {:?}", fixture.counts()).to_lowercase();
    for banned in ["score", "confidence", "probability", "risk", "%"] {
        assert!(!rendered.contains(banned), "output exposes {banned:?}");
    }
}

#[test]
fn fixture_lands_in_explained_buckets() {
    let queue = Fixture::load().queue();
    let ssh = item(&queue, SSH);
    assert_eq!(ssh.bucket, Bucket::UnreviewedWithFeedback);
    assert!(
        ssh.reasons
            .iter()
            .any(|r| r.code == ReasonCode::UnresolvedThread
                && r.thread_id.as_deref() == Some("PRRT_synthBot0001"))
    );
    let static_reason = ssh
        .reasons
        .iter()
        .find(|r| r.code == ReasonCode::DeterministicHighAttention)
        .unwrap();
    assert!(!static_reason.evidence_ids.is_empty());
    assert!(static_reason.rule_id.is_some());

    assert_eq!(
        item(&queue, CONFIG).bucket,
        Bucket::HighDeterministicAttention
    );

    let script = item(&queue, SCRIPT);
    assert_eq!(script.bucket, Bucket::Ignored);
    assert!(
        script
            .reasons
            .iter()
            .any(|r| r.code == ReasonCode::ExcludedByRule
                && r.rule_id.as_deref() == Some("rule_demo_scripts"))
    );
    // Ignored files are discoverable but never become in-scope by priority,
    // even with an unresolved thread.
    assert!(script.threads.contains(&"PRRT_synthHuman0003".to_string()));
    assert!(!queue.remaining().any(|i| i.path == SCRIPT));

    // A resolved, outdated thread never drives urgency.
    let client = item(&queue, "git/client.go");
    assert_ne!(client.bucket, Bucket::UnreviewedWithFeedback);
    assert!(
        !client
            .reasons
            .iter()
            .any(|r| r.code == ReasonCode::UnresolvedThread)
    );

    // Bucket order is lexicographic.
    assert!(queue.items.windows(2).all(|w| w[0].bucket <= w[1].bucket));
    assert_eq!(queue.items[0].path, SSH);
    assert_eq!(queue.items.last().unwrap().bucket, Bucket::Ignored);
    // PR-level findings are kept, not dropped.
    assert!(!queue.pr_level_findings.is_empty());
}

#[test]
fn acknowledgement_changes_only_acknowledgement() {
    let mut fixture = Fixture::load();
    let before = fixture.queue();
    fixture.set_review(SSH, FileStatus::ReviewedCurrent);
    let after = fixture.queue();
    let (b, a) = (item(&before, SSH), item(&after, SSH));
    assert_eq!(b.acknowledgement, FileStatus::Unreviewed);
    assert_eq!(a.acknowledgement, FileStatus::ReviewedCurrent);
    assert_eq!(a.bucket, Bucket::Reviewed);
    // Attention and severity are untouched.
    assert_eq!(a.attention, b.attention);
    assert_eq!(a.attention, AttentionLevel::High);
    assert_eq!(a.findings, b.findings);
    assert!(
        a.findings
            .iter()
            .all(|f| f.severity.normalized == Severity::Unknown)
    );
    assert_eq!(a.topics, b.topics);
    // The other files did not move between buckets.
    for other in before.items.iter().filter(|i| i.path != SSH) {
        assert_eq!(item(&after, &other.path), other);
    }

    // Changed since reviewed with a current unresolved thread is most urgent.
    fixture.set_review(SSH, FileStatus::ChangedSinceReviewed);
    let changed = fixture.queue();
    assert_eq!(changed.items[0].path, SSH);
    assert_eq!(
        changed.items[0].bucket,
        Bucket::ChangedSinceReviewedWithFeedback
    );
    assert_eq!(changed.items[0].attention, b.attention);
}

#[test]
fn counts_partition_by_scope_including_overlapping_rules() {
    let mut fixture = Fixture::load();
    let rule_sets = vec![
        Vec::new(),
        fixture.rules.clone(),
        vec![
            // Several rules match the same paths; the last enabled match wins.
            rule("a", RuleAction::Exclude, RuleKind::Directory, "internal", 1),
            rule("b", RuleAction::Include, RuleKind::File, SSH, 2),
            rule("c", RuleAction::Exclude, RuleKind::Glob, "**/*.go", 3),
            rule(
                "d",
                RuleAction::Include,
                RuleKind::Glob,
                "internal/config/**",
                4,
            ),
            rule("e", RuleAction::Exclude, RuleKind::Glob, "script/**", 5),
            rule("f", RuleAction::Exclude, RuleKind::Directory, "script", 6),
        ],
        vec![rule("all", RuleAction::Exclude, RuleKind::Glob, "**", 1)],
    ];
    for rules_ in rule_sets {
        fixture.rules = rules_;
        let counts = fixture.counts();
        for tally in [
            counts.files,
            counts.threads,
            counts.current_findings,
            counts.historical_findings,
        ] {
            assert!(tally.partitions(), "{tally:?}");
        }
        let ignored =
            |p: Option<&str>| p.is_some_and(|p| rules::evaluate(&fixture.rules, p).ignored);
        // No silent drops: totals equal the inputs.
        assert_eq!(counts.files.total as usize, fixture.files.len());
        assert_eq!(
            counts.files.ignored as usize,
            fixture
                .files
                .iter()
                .filter(|f| ignored(Some(&f.path)))
                .count()
        );
        assert_eq!(counts.threads.total as usize, fixture.threads.len());
        assert_eq!(
            (counts.current_findings.total + counts.historical_findings.total) as usize,
            fixture.findings.len()
        );
        let current_ignored = fixture
            .findings
            .iter()
            .filter(|o| o.current && ignored(o.finding.anchor.current_path.as_deref()))
            .count();
        assert_eq!(counts.current_findings.ignored as usize, current_ignored);
        assert_eq!(
            counts.reviewed_current + counts.changed_since_reviewed + counts.unreviewed,
            counts.files.in_scope
        );
        assert_eq!(
            counts.threads_resolved
                + counts.threads_unresolved_current
                + counts.threads_unresolved_outdated,
            counts.threads.in_scope
        );
        // The queue agrees: every file exactly once, ignored iff excluded.
        let queue = fixture.queue();
        assert_eq!(queue.items.len(), fixture.files.len());
        let ignored_items = queue
            .items
            .iter()
            .filter(|i| i.bucket == Bucket::Ignored)
            .count();
        assert_eq!(ignored_items as u32, counts.files.ignored);
    }
}

#[test]
fn duplicate_source_records_are_counted_once() {
    let mut fixture = Fixture::load();
    let before = fixture.counts();
    let thread = fixture.threads[0].clone();
    let finding = fixture.findings[0].clone();
    fixture.threads.push(thread);
    fixture.findings.push(finding);
    assert_eq!(fixture.counts(), before);
    // Findings that project a native thread don't add a second thread.
    assert!(
        fixture
            .findings
            .iter()
            .any(|o| o.finding.anchor.github_thread_id.is_some())
    );
    assert_eq!(before.threads.total, 3);
}

#[test]
fn exclude_then_reinclude_round_trips_queue_and_triage() {
    let mut fixture = Fixture::load();
    let findings_before = fixture.findings.clone();
    let states_before = fixture.user_states.clone();
    let queue = fixture.queue();
    let counts = fixture.counts();

    fixture
        .rules
        .push(rule("x", RuleAction::Exclude, RuleKind::File, SSH, 10));
    let excluded = fixture.queue();
    assert_eq!(item(&excluded, SSH).bucket, Bucket::Ignored);
    assert_ne!(fixture.counts(), counts);
    // Triage is still visible on the ignored file.
    assert!(
        item(&excluded, SSH)
            .findings
            .iter()
            .any(|f| f.disposition == UserDisposition::Investigating)
    );

    // Re-include with a later rule, then by removing the exclusion.
    fixture
        .rules
        .push(rule("y", RuleAction::Include, RuleKind::File, SSH, 11));
    let reincluded = fixture.queue();
    let ssh = item(&reincluded, SSH);
    assert_eq!(ssh.bucket, item(&queue, SSH).bucket);
    assert_eq!(ssh.findings, item(&queue, SSH).findings);
    fixture.rules.truncate(fixture.rules.len() - 2);
    assert_eq!(fixture.queue(), queue);
    assert_eq!(fixture.counts(), counts);
    // Inputs (findings and triage) were never rewritten.
    assert_eq!(fixture.findings, findings_before);
    assert_eq!(fixture.user_states, states_before);
}

#[test]
fn empty_queue_is_only_all_clear_with_complete_coverage() {
    let mut fixture = Fixture::load();
    let paths: Vec<String> = fixture.files.iter().map(|f| f.path.clone()).collect();
    for path in &paths {
        fixture.set_review(path, FileStatus::ReviewedCurrent);
    }
    assert_eq!(fixture.queue().verdict, QueueVerdict::AllClear);
    assert!(fixture.counts().authoritative());

    for (state, verdict) in [
        (SourceState::Incomplete, QueueVerdict::EmptyIncomplete),
        (SourceState::Failed, QueueVerdict::EmptyIncomplete),
        (SourceState::NotRun, QueueVerdict::EmptyIncomplete),
        (SourceState::Pending, QueueVerdict::EmptyPending),
    ] {
        fixture.coverage = vec![source(SourceState::Clean), source(state)];
        assert_eq!(fixture.queue().verdict, verdict, "{state:?}");
        assert!(!fixture.counts().authoritative());
    }

    fixture.set_review(SSH, FileStatus::Unreviewed);
    assert_eq!(fixture.queue().verdict, QueueVerdict::Remaining(1));
}

#[test]
fn correlated_sources_group_without_losing_provenance() {
    let fixture = Fixture::load();
    let original = fixture.findings.clone();
    let queue = fixture.queue();
    let ssh = item(&queue, SSH);
    // Three source records on one topic: a bot thread projection, a static
    // finding and a check annotation, plus the native thread.
    assert_eq!(ssh.findings.len(), 3);
    let group = ssh
        .topics
        .iter()
        .find(|g| g.key == "corr_ssh_variant_normalisation")
        .unwrap();
    assert_eq!(group.findings.len(), 3);
    assert_eq!(group.threads, vec!["PRRT_synthBot0001".to_string()]);
    // Each member is still addressable as its own record with its own source.
    let kinds: std::collections::HashSet<SourceKind> =
        ssh.findings.iter().map(|f| f.source_kind).collect();
    assert!(kinds.len() == 3);
    for id in &group.findings {
        assert!(fixture.findings.iter().any(|o| &o.finding.id == id));
    }
    assert_eq!(fixture.findings, original);
    // The thread is the reason; its projection doesn't cite it a second time.
    let thread_reasons = ssh
        .reasons
        .iter()
        .filter(|r| r.thread_id.as_deref() == Some("PRRT_synthBot0001"))
        .count();
    assert_eq!(thread_reasons, 1);
}

#[test]
fn locally_dismissed_findings_stay_listed_but_do_not_drive_rank() {
    let mut fixture = Fixture::load();
    let config_finding = fixture
        .findings
        .iter()
        .find(|o| o.finding.anchor.current_path.as_deref() == Some(CONFIG))
        .unwrap()
        .finding
        .id
        .clone();
    fixture.user_states.push(FindingUserState {
        disposition: UserDisposition::DismissedLocal,
        ..FindingUserState::untouched(config_finding.clone(), "s", "2026-11-20T00:00:00Z")
    });
    fixture.user_states.retain(|s| {
        s.finding_id != config_finding || s.disposition == UserDisposition::DismissedLocal
    });
    let queue = fixture.queue();
    let config = item(&queue, CONFIG);
    assert_eq!(config.bucket, Bucket::NeedsReview);
    assert!(
        config
            .findings
            .iter()
            .any(|f| f.id == config_finding && f.disposition == UserDisposition::DismissedLocal)
    );
    // Its severity and attention record are still there for the user.
    assert!(
        config
            .findings
            .iter()
            .any(|f| f.attention == AttentionLevel::High)
    );
}

#[test]
fn stored_findings_convert_with_their_label() {
    use crate::store::Raw;
    let finding = intelligence::load(false).findings.remove(0);
    let stored = StoredFinding {
        finding: Raw::new(finding.clone()).unwrap(),
        label: ObservationLabel::Historical,
        latest_snapshot_id: SnapshotId::new("snap"),
        latest_applicability: "historical_outdated".into(),
        first_seen_at: String::new(),
        last_seen_at: String::new(),
    };
    let observed = ObservedFinding::from(&stored);
    assert_eq!(observed.finding, finding);
    assert!(!observed.current);
}
