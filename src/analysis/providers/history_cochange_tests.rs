use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

use super::{history::HistoryProvider, history_cochange};
use crate::{
    analysis::{
        AnalysisInput, ProviderContext, ProviderLimits, ProviderOutput, Scheduler, SchedulerConfig,
        SystemClock,
    },
    demo,
    github::context::{AnalysisContext, HistoryObjects},
    history::reader::{History, Transaction},
    model::{AnalysisCoverage, AnalysisRunId, AssertionKind, FindingKind, RunStatus, ScopeOutcome},
    session::Session,
    store::Store,
};

const TARGET: &str = "internal/identity/store.go";

fn context() -> ProviderContext {
    let mut snapshot = demo::snapshot(0);
    snapshot.files.retain(|f| f.path == TARGET);
    let mut input = AnalysisInput::new("pr", "repo", snapshot);
    input.history_context = Some(AnalysisContext {
        source: "fixture".into(),
        base_sha: input.base_sha.clone(),
        head_sha: input.head_sha.clone(),
        comparison_base: Some(input.base_sha.clone()),
        history_count: 5,
        shallow: false,
        complete: true,
        reason: None,
    });
    ProviderContext {
        run_id: AnalysisRunId::new("run"),
        attempt: 0,
        limits: ProviderLimits::default(),
        input: Arc::new(input),
    }
}

fn history() -> History {
    let paths: BTreeSet<String> = [TARGET, "partner-a.go", "partner-b.go"]
        .into_iter()
        .map(String::from)
        .collect();
    History {
        base: "base".into(),
        anchor: 1,
        window_start: 0,
        walked: 5,
        counted: 5,
        merges_skipped: 0,
        broad_skipped: 0,
        files: paths.iter().map(|p| (p.clone(), "blob".into())).collect(),
        touches: BTreeMap::new(),
        ambiguous: BTreeSet::new(),
        transactions: (0..5)
            .map(|n| Transaction {
                id: format!("{n:040}"),
                timestamp: 1,
                paths: paths.clone(),
            })
            .collect(),
    }
}

fn output(ctx: &ProviderContext, history: &History, cancelled: bool) -> ProviderOutput {
    let mut out = ProviderOutput {
        status: RunStatus::Partial,
        coverage: AnalysisCoverage::not_run(),
        warnings: vec![],
        findings: vec![],
        evidence: vec![],
        relations: vec![],
    };
    history_cochange::append(
        ctx,
        history,
        Instant::now() + Duration::from_secs(30),
        &AtomicBool::new(cancelled),
        "2026-11-20T12:00:00Z",
        &mut out,
    )
    .unwrap();
    out
}

#[test]
fn associations_have_independent_coverage_and_non_defect_contracts() {
    let ctx = context();
    let out = output(&ctx, &history(), false);
    assert_eq!(out.findings.len(), 2);
    for raw in &out.findings {
        let f = raw.value();
        assert_eq!(f.validate(), Ok(()));
        assert_eq!(f.assertion_kind, AssertionKind::AttentionHint);
        assert_eq!(f.kinds, vec![FindingKind::TemporalCoupling]);
        assert_eq!(f.confidence.as_ref().unwrap().kind, "measured_association");
        assert_eq!(f.confidence.as_ref().unwrap().denominator, Some(5));
        assert!(f.related_anchors[0].file_id.is_none());
        assert!(f.rationale.contains("not a defect probability"));
    }
    assert!(matches!(
        out.coverage.entries[0].outcome,
        ScopeOutcome::Analyzed { findings: 2 }
    ));
    let value = out.evidence[0].value().value.as_ref().unwrap();
    assert_eq!(value["baseline"]["eligibleTransactions"], 5);
    assert_eq!(value["baseline"]["sampled"], false);
    assert_eq!(value["confidenceAtoB"], 1.0);
    assert_eq!(value["confidenceBtoA"], 1.0);
}

#[test]
fn renames_use_baseline_source_and_exclude_both_partner_identities() {
    let mut ctx = context();
    let input = Arc::make_mut(&mut ctx.input);
    let snapshot = Arc::make_mut(&mut input.snapshot);
    snapshot.files[0].previous_path = Some(TARGET.into());
    snapshot.files[0].path = "renamed-source.go".into();
    let mut partner = snapshot.files[0].clone();
    partner.id = "partner-id".into();
    partner.path = "renamed-partner.go".into();
    partner.previous_path = Some("partner-a.go".into());
    snapshot.files.push(partner);
    let out = output(&ctx, &history(), false);
    // Both changed sources can suggest B, but neither may suggest A under
    // its previous name (or its new PR name).
    assert_eq!(out.findings.len(), 2);
    assert!(
        out.findings
            .iter()
            .all(|f| f.value().related_anchors[0].current_path.as_deref() == Some("partner-b.go"))
    );
    assert!(
        out.findings
            .iter()
            .any(|f| f.value().anchor.current_path.as_deref() == Some("renamed-source.go"))
    );
}

#[test]
fn truncation_is_disclosed_and_incomplete_context_emits_no_hints() {
    let mut ctx = context();
    ctx.limits.max_findings = 1;
    let out = output(&ctx, &history(), false);
    assert_eq!(out.findings.len(), 1);
    assert!(
        out.coverage
            .reason_codes
            .contains(&"cochange_output_truncated".into())
    );
    assert!(matches!(
        out.coverage.entries[0].outcome,
        ScopeOutcome::Unknown { .. }
    ));
    let value = out.evidence[0].value().value.as_ref().unwrap();
    assert_eq!(value["qualifyingPairs"], 2);
    assert_eq!(value["emittedHints"], 1);
    assert_eq!(value["outputTruncated"], true);
    assert!(output(&ctx, &history(), true).findings.is_empty());
    Arc::make_mut(&mut ctx.input)
        .history_context
        .as_mut()
        .unwrap()
        .complete = false;
    let incomplete = output(&ctx, &history(), false);
    assert!(incomplete.findings.is_empty());
    assert!(
        incomplete
            .coverage
            .reason_codes
            .contains(&"cochange_context_incomplete".into())
    );
}

#[tokio::test]
async fn pinned_cochange_evidence_persists_across_pr_revisions_and_pruning() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_owned();
    let git = |args: &[&str]| {
        let result = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(args)
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        String::from_utf8(result.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "-q"]);
    git(&["config", "user.name", "Fixture"]);
    git(&["config", "user.email", "fixture@example.com"]);
    std::fs::create_dir_all(root.join("internal/identity")).unwrap();
    for path in [TARGET, "partner.go"] {
        std::fs::write(root.join(path), "0").unwrap();
    }
    // Broad root contributes to churn, but never to association support.
    for n in 0..49 {
        std::fs::write(root.join(format!("zero-{n}.txt")), "0").unwrap();
    }
    git(&["add", "."]);
    git(&["commit", "-qm", "bulk baseline"]);
    for n in 1..=5 {
        for path in [TARGET, "partner.go"] {
            std::fs::write(root.join(path), n.to_string()).unwrap();
        }
        git(&["commit", "-qam", "cochange"]);
    }
    let base = git(&["rev-parse", "HEAD"]);
    std::fs::write(root.join(TARGET), "PR-only").unwrap();
    git(&["commit", "-qam", "PR-only"]);
    let head = git(&["rev-parse", "HEAD"]);
    let mut snapshot = demo::snapshot(0);
    snapshot.files.retain(|f| f.path == TARGET);
    snapshot.pr.base_sha = base.clone();
    snapshot.pr.head_sha = head.clone();
    let session = Session::new(
        snapshot.clone(),
        Store::open_in_memory().unwrap(),
        None,
        None,
    )
    .unwrap();
    let mut input = AnalysisInput::new(&session.pr_id, &session.repository.id, snapshot.clone());
    input.history_context = Some(AnalysisContext {
        source: "fixture".into(),
        base_sha: base.clone(),
        head_sha: head,
        comparison_base: Some(base),
        history_count: 6,
        shallow: false,
        complete: true,
        reason: None,
    });
    input.history_objects = Some(HistoryObjects::fixture(temp, root.join(".git")));
    let scheduler = Scheduler::new(
        Arc::clone(&session.store),
        vec![Arc::new(HistoryProvider::default())],
        SchedulerConfig::default(),
        Arc::new(SystemClock),
    )
    .unwrap();
    scheduler
        .begin_snapshot(input.clone())
        .unwrap()
        .remove(0)
        .wait()
        .await;
    let first_id = {
        let store = session.store.lock().unwrap();
        let current = store.list_findings(&session.pr_id, false).unwrap();
        let finding = current
            .iter()
            .find(|f| {
                f.finding
                    .value()
                    .kinds
                    .contains(&FindingKind::TemporalCoupling)
            })
            .expect("persisted association");
        let evidence = store
            .list_finding_evidence(&finding.finding.value().id)
            .unwrap();
        let value = evidence[0].value().value.as_ref().unwrap();
        assert_eq!(value["association"]["supportAb"], 5);
        assert_eq!(value["association"]["supportA"], 5);
        assert_eq!(value["baseline"]["eligibleTransactions"], 5);
        assert_eq!(value["baseline"]["bulkExcluded"], 1);
        assert_eq!(value["baseline"]["walkedCommits"], 6);
        finding.finding.value().id.clone()
    };
    std::fs::write(root.join(TARGET), "next PR-only").unwrap();
    git(&["commit", "-qam", "next PR-only"]);
    snapshot.pr.head_sha = git(&["rev-parse", "HEAD"]);
    snapshot.files[0].previous_path = Some(TARGET.into());
    snapshot.files[0].path = "renamed.go".into();
    let mut next = AnalysisInput::new(&session.pr_id, &session.repository.id, snapshot);
    next.history_context = input.history_context.clone();
    next.history_context.as_mut().unwrap().head_sha = next.head_sha.clone();
    next.history_objects = input.history_objects.clone();
    scheduler
        .begin_snapshot(next)
        .unwrap()
        .remove(0)
        .wait()
        .await;
    let mut store = session.store.lock().unwrap();
    store.prune_analysis(&session.pr_id, 0).unwrap();
    let current = store.list_findings(&session.pr_id, false).unwrap();
    let finding = current
        .iter()
        .find(|f| {
            f.finding
                .value()
                .kinds
                .contains(&FindingKind::TemporalCoupling)
        })
        .unwrap();
    assert_ne!(finding.finding.value().id, first_id);
    assert_eq!(
        finding.finding.value().anchor.current_path.as_deref(),
        Some("renamed.go")
    );
    let evidence = store
        .list_finding_evidence(&finding.finding.value().id)
        .unwrap();
    assert_eq!(
        evidence[0].value().value.as_ref().unwrap()["association"]["supportAb"],
        5
    );
}
