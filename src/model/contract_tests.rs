//! Contract tests for the analysis/finding/evidence/imported-source types.

use serde_json::json;

use super::*;

const REPO: &str = "R_kgDOExample";

fn snapshot() -> SnapshotId {
    SnapshotId::derive(REPO, 42, "base0000", "head1111")
}

fn run_key(snapshot: &SnapshotId) -> RunKeyInput<'_> {
    RunKeyInput {
        snapshot_id: snapshot,
        provider_id: "aside.git-history",
        engine_version: "1.0.0",
        configuration_digest: "cfg",
        input_digest: "in",
        attempt: 0,
    }
}

fn finding_key() -> FindingKeyInput<'static> {
    FindingKeyInput {
        canonical_repo_id: REPO,
        pr_number: 42,
        provider_id: "aside.git-history",
        rule_id: "aside.history.temporal_coupling",
        claim_identity: "coupled-with:src/auth/token.rs",
        subject_identity: Some("file:src/auth/session.rs"),
        code_fingerprint: Some("sha256:abc"),
    }
}

fn sample_finding() -> ReviewFinding {
    let snap = snapshot();
    let run = AnalysisRunId::derive(&run_key(&snap));
    let mut anchor = CodeAnchor::new(AnchorKind::File);
    anchor.file_id = Some("file_session".into());
    anchor.current_path = Some("src/auth/session.rs".into());
    anchor.diff_digest = Some("sha256:digest".into());
    let mut related = CodeAnchor::new(AnchorKind::RelatedContext);
    related.current_path = Some("src/auth/token.rs".into());
    ReviewFinding {
        schema_version: ANALYSIS_SCHEMA_VERSION,
        id: FindingId::derive(&finding_key()),
        repository_id: REPO.into(),
        pr_number: 42,
        observed_snapshot_id: snap,
        source: FindingSource {
            kind: SourceKind::AsideStatic,
            provider_id: "aside.git-history".into(),
            run_id: Some(run),
            rule_id: Some("aside.history.temporal_coupling".into()),
            rule_version: Some("1.0.0".into()),
            model_id: None,
            model_revision: None,
            prompt_version: None,
            native: None,
            github_actor: None,
        },
        assertion_kind: AssertionKind::AttentionHint,
        kinds: vec![FindingKind::TemporalCoupling],
        title: "Historically related token-handling file is unchanged".into(),
        body: "Changes to session.rs frequently coincide with changes to token.rs.".into(),
        rationale: "17 of the last 21 eligible commits touching session.rs also touched token.rs."
            .into(),
        suggested_action: Some("Inspect token.rs.".into()),
        anchor,
        related_anchors: vec![related],
        evidence_ids: vec![EvidenceId::new("ev_support")],
        attention: AttentionAssessment {
            level: AttentionLevel::Investigate,
            score: Some(67.0),
            score_version: Some("aside-attention-v1".into()),
            reasons: vec!["high_coupling".into()],
        },
        severity: SeverityRecord {
            normalized: Severity::Unknown,
            original: None,
        },
        confidence: Some(Confidence {
            kind: "measured_association".into(),
            value: 0.81,
            denominator: Some(21),
            calibration_version: None,
        }),
        validation: ValidationRecord {
            status: ValidationStatus::NotRequired,
            method: None,
            validated_at: None,
            note: None,
        },
        applicability: Applicability::CurrentExact,
        source_status: SourceStatus::Unknown,
        first_seen_at: "2026-11-20T10:00:00Z".into(),
        last_seen_at: "2026-11-20T10:00:00Z".into(),
        external_url: None,
        parent_finding_id: None,
        correlation_group_id: None,
        extensions: serde_json::Map::new(),
    }
}

fn round_trip<T>(value: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    serde_json::from_str(&serde_json::to_string(value).unwrap()).unwrap()
}

// --- Schema versioning ---------------------------------------------------

#[test]
fn schema_version_is_explicit_and_serialized() {
    assert_eq!(ANALYSIS_SCHEMA_VERSION, 1);
    let v = serde_json::to_value(sample_finding()).unwrap();
    assert_eq!(v["schemaVersion"], 1);
    let run = AnalysisRun::queued(&run_key(&snapshot()), AnalysisMode::Static);
    assert_eq!(
        serde_json::to_value(&run).unwrap()["schemaVersion"],
        ANALYSIS_SCHEMA_VERSION
    );
}

#[test]
fn unsupported_schema_version_fails_validation() {
    let mut f = sample_finding();
    assert_eq!(f.validate(), Ok(()));
    f.schema_version = ANALYSIS_SCHEMA_VERSION + 1;
    assert_eq!(
        f.validate(),
        Err(vec![FindingContractError::UnsupportedSchemaVersion(2)])
    );
}

// --- Stable IDs ----------------------------------------------------------

#[test]
fn ids_are_deterministic_and_pinned() {
    // Pinned values guard against accidental changes to the derivation.
    // Changing them requires a new domain tag version.
    assert_eq!(
        stable_digest("d", &[Some("a"), None, Some("")]),
        stable_digest("d", &[Some("a"), None, Some("")])
    );
    let snap = snapshot();
    assert_eq!(snap, SnapshotId::derive(REPO, 42, "base0000", "head1111"));
    assert!(snap.as_str().starts_with("snap_"));
    assert_eq!(snap.as_str().len(), "snap_".len() + 64);

    let id = FindingId::derive(&finding_key());
    assert_eq!(id, FindingId::derive(&finding_key()));
    assert_eq!(id.as_str(), PINNED_FINDING_ID);
}

/// Independently reproduced with Python's hashlib using the documented
/// encoding, so it is not just "whatever the code returns today".
const PINNED_FINDING_ID: &str =
    "find_71454a8d7799ecc888963485eda90ec22d00ea8b36cd7f71a84811311cf9bd38";

#[test]
fn digest_encoding_is_injective() {
    let ab_c = stable_digest("d", &[Some("ab"), Some("c")]);
    let a_bc = stable_digest("d", &[Some("a"), Some("bc")]);
    assert_ne!(ab_c, a_bc);
    assert_ne!(stable_digest("d", &[None]), stable_digest("d", &[Some("")]));
    assert_ne!(
        stable_digest("d1", &[Some("x")]),
        stable_digest("d2", &[Some("x")])
    );
}

#[test]
fn finding_id_ignores_run_and_snapshot_but_tracks_code() {
    // The key has no snapshot/run/timestamp inputs, so reruns keep the ID.
    let base = FindingId::derive(&finding_key());
    let mut changed = finding_key();
    changed.code_fingerprint = Some("sha256:def");
    assert_ne!(base, FindingId::derive(&changed));
    let mut other_rule = finding_key();
    other_rule.rule_id = "aside.history.churn";
    assert_ne!(base, FindingId::derive(&other_rule));
    let mut no_subject = finding_key();
    no_subject.subject_identity = None;
    assert_ne!(base, FindingId::derive(&no_subject));
}

#[test]
fn run_ids_are_bound_to_snapshot_and_attempt() {
    let snap = snapshot();
    let other_snap = SnapshotId::derive(REPO, 42, "base0000", "head2222");
    let a = AnalysisRunId::derive(&run_key(&snap));
    assert_ne!(a, AnalysisRunId::derive(&run_key(&other_snap)));
    let mut retry = run_key(&snap);
    retry.attempt = 1;
    assert_ne!(a, AnalysisRunId::derive(&retry));

    let run = AnalysisRun::queued(&run_key(&snap), AnalysisMode::Static);
    assert_eq!(run.snapshot_id, snap);
    assert!(run.id_is_consistent());
    let mut tampered = run.clone();
    tampered.snapshot_id = other_snap;
    assert!(!tampered.id_is_consistent());
}

#[test]
fn evidence_and_imported_ids_are_deterministic() {
    let key = EvidenceKeyInput {
        kind: EvidenceKind::History,
        source_run_id: None,
        label: "co-change support",
        method: Some("git-log"),
        input_digest: Some("in"),
        value_json: Some(r#"{"denominator":21,"numerator":17}"#),
    };
    assert_eq!(EvidenceId::derive(&key), EvidenceId::derive(&key));
    assert!(EvidenceId::derive(&key).as_str().starts_with("ev_"));

    let native = NativeSourceRef {
        host: "github.com".into(),
        repository_id: REPO.into(),
        source_kind: SourceKind::GithubReviewThread,
        key: NativeSourceKey::Upstream("PRRT_abc".into()),
        external_url: None,
    };
    let id = FindingId::for_imported(REPO, 42, &native);
    assert_eq!(id, FindingId::for_imported(REPO, 42, &native));
    let mut weaker = native.clone();
    weaker.key = NativeSourceKey::ContentFingerprint("PRRT_abc".into());
    assert_ne!(id, FindingId::for_imported(REPO, 42, &weaker));

    let fp = ImportedCheckAnnotation::content_fingerprint(7, "a.rs", 1, 2, "msg", 0);
    assert_eq!(
        fp,
        ImportedCheckAnnotation::content_fingerprint(7, "a.rs", 1, 2, "msg", 0)
    );
    assert!(!fp.is_upstream());
    assert_ne!(
        fp,
        ImportedCheckAnnotation::content_fingerprint(7, "a.rs", 1, 2, "msg", 1)
    );
}

// --- Serde round trips ---------------------------------------------------

#[test]
fn finding_matches_documented_json_shape_and_round_trips() {
    let f = sample_finding();
    assert_eq!(round_trip(&f), f);
    let v = serde_json::to_value(&f).unwrap();
    assert_eq!(v["assertionKind"], "attention_hint");
    assert_eq!(v["kinds"], json!(["temporal_coupling"]));
    assert_eq!(v["applicability"], "current_exact");
    assert_eq!(v["sourceStatus"], "unknown");
    assert_eq!(v["anchor"]["kind"], "file");
    assert_eq!(v["anchor"]["side"], serde_json::Value::Null);
    assert_eq!(v["relatedAnchors"][0]["kind"], "related_context");
    assert_eq!(
        v["severity"],
        json!({"normalized": "unknown", "original": null})
    );
    assert_eq!(v["validation"]["status"], "not_required");
    assert!(v["id"].as_str().unwrap().starts_with("find_"));
}

#[test]
fn other_contracts_round_trip() {
    let snap = snapshot();
    let mut run = AnalysisRun::queued(&run_key(&snap), AnalysisMode::Static);
    run.status = RunStatus::Partial;
    run.coverage.entries.push(CoverageEntry {
        scope: CoverageScope::File {
            file_id: "f1".into(),
            path: "a.rs".into(),
        },
        rule_id: None,
        outcome: ScopeOutcome::Analyzed { findings: 0 },
    });
    assert_eq!(round_trip(&run), run);

    let ev = Evidence {
        schema_version: ANALYSIS_SCHEMA_VERSION,
        id: EvidenceId::new("ev_x"),
        kind: EvidenceKind::Measurement,
        source_run_id: Some(run.id.clone()),
        source_entity_id: None,
        anchor: Some(CodeAnchor::new(AnchorKind::Pr)),
        label: "support".into(),
        value: Some(json!({"numerator": 17})),
        unit: Some("commits".into()),
        numerator: Some(17.0),
        denominator: Some(21.0),
        method: Some("git-log".into()),
        input_digest: None,
        observed_at: "2026-11-20T10:00:00Z".into(),
        limitations: vec!["180-day window".into()],
    };
    assert_eq!(round_trip(&ev), ev);

    let rel = FindingRelation {
        source_finding_id: FindingId::new("find_a"),
        kind: RelationKind::ReferencesThread,
        target: RelationTarget::Thread("PRRT_1".into()),
        method: "anchor-match".into(),
        method_version: "1".into(),
        score: None,
        validated: false,
        created_at: "t".into(),
    };
    assert_eq!(round_trip(&rel), rel);
    assert_eq!(
        serde_json::to_value(&rel.target).unwrap(),
        json!({"kind": "thread", "id": "PRRT_1"})
    );

    let state = FindingUserState::untouched(FindingId::new("find_a"), "s1", "t");
    assert_eq!(round_trip(&state), state);

    let ann = ImportedCheckAnnotation {
        check_run_id: 7,
        key: ImportedCheckAnnotation::content_fingerprint(7, "a.rs", 1, 2, "m", 0),
        path: "a.rs".into(),
        start_line: 1,
        end_line: 2,
        start_column: None,
        end_column: None,
        annotation_level: AnnotationLevel::Failure,
        title: None,
        message: "m".into(),
        raw_details: None,
        head_sha: "h".into(),
        observed_at: "t".into(),
    };
    assert_eq!(round_trip(&ann), ann);
    assert_eq!(
        serde_json::to_value(&ann).unwrap()["key"]["kind"],
        "content_fingerprint"
    );

    let occ = FindingOccurrence {
        finding_id: FindingId::new("find_a"),
        run_id: run.id.clone(),
        snapshot_id: snap,
        observed_at: "t".into(),
        schema_version: ANALYSIS_SCHEMA_VERSION,
    };
    assert_eq!(round_trip(&occ), occ);
}

#[test]
fn unknown_enum_values_degrade_safely() {
    let a: Applicability = serde_json::from_str(r#""future_state""#).unwrap();
    assert_eq!(a, Applicability::Unknown);
    let s: SourceKind = serde_json::from_str(r#""gitlab_thing""#).unwrap();
    assert_eq!(s, SourceKind::OtherAdapter);
    let k: AssertionKind = serde_json::from_str(r#""new_kind""#).unwrap();
    assert_eq!(k, AssertionKind::Informational);
    let sev: Severity = serde_json::from_str(r#""blocker""#).unwrap();
    assert_eq!(sev, Severity::Unknown);
    let st: SourceStatus = serde_json::from_str(r#""archived""#).unwrap();
    assert_eq!(st, SourceStatus::Unknown);
}

// --- Applicability / unknown semantics -----------------------------------

#[test]
fn not_applicable_unknown_and_no_finding_are_distinct() {
    let no_finding = ScopeOutcome::Analyzed { findings: 0 };
    let not_applicable = ScopeOutcome::NotApplicable {
        reason: "binary_file".into(),
    };
    let unknown = ScopeOutcome::Unknown {
        reason: "parser_unsupported".into(),
    };
    let not_run = ScopeOutcome::NotRun {
        reason: "budget_exhausted".into(),
    };

    assert!(no_finding.is_no_finding());
    assert_eq!(no_finding.known_findings(), Some(0));
    for other in [&not_applicable, &unknown, &not_run] {
        assert!(!other.is_no_finding());
        assert_eq!(other.known_findings(), None);
    }
    assert!(not_applicable.is_not_applicable());

    let tags: Vec<_> = [&no_finding, &not_applicable, &unknown, &not_run]
        .iter()
        .map(|o| serde_json::to_value(o).unwrap()["outcome"].clone())
        .collect();
    assert_eq!(
        tags,
        vec![
            json!("analyzed"),
            json!("not_applicable"),
            json!("unknown"),
            json!("not_run")
        ]
    );
}

#[test]
fn missing_coverage_and_counts_are_unknown_not_zero() {
    let cov = AnalysisCoverage::not_run();
    assert_eq!(cov.completeness, Completeness::NotRun);
    assert_eq!(cov.analyzed_files, KnownCount::Unknown);
    assert!(matches!(
        cov.file_outcome("missing"),
        ScopeOutcome::Unknown { .. }
    ));
    assert_eq!(
        serde_json::to_value(KnownCount::Unknown).unwrap(),
        json!({"state": "unknown"})
    );
    assert_eq!(
        serde_json::to_value(KnownCount::Known { value: 0 }).unwrap(),
        json!({"state": "known", "value": 0})
    );
}

#[test]
fn only_succeeded_runs_can_assert_absence() {
    use RunStatus::*;
    for s in [Queued, Running, Partial, Skipped, Cancelled, Failed, Stale] {
        assert!(!s.can_assert_absence(), "{s:?}");
    }
    assert!(Succeeded.can_assert_absence());
    assert!(!Queued.is_terminal() && !Running.is_terminal() && Stale.is_terminal());
}

#[test]
fn applicability_unknown_and_unavailable_are_not_current() {
    use Applicability::*;
    assert!(CurrentExact.is_current() && CurrentRelated.is_current());
    for a in [HistoricalOutdated, Unanchored, Unknown, Unavailable] {
        assert!(!a.is_current());
    }
    assert_ne!(
        serde_json::to_value(Unknown).unwrap(),
        serde_json::to_value(Unavailable).unwrap()
    );
}

// --- Findings never set file reviewed ------------------------------------

#[test]
fn finding_contracts_have_no_review_state_fields() {
    let forbidden = ["reviewed", "acknowledg", "filestatus", "reviewstatus"];
    let check = |name: &str, v: serde_json::Value| {
        fn keys(v: &serde_json::Value, out: &mut Vec<String>) {
            match v {
                serde_json::Value::Object(m) => {
                    for (k, v) in m {
                        out.push(k.to_lowercase());
                        keys(v, out);
                    }
                }
                serde_json::Value::Array(a) => a.iter().for_each(|v| keys(v, out)),
                _ => {}
            }
        }
        let mut all = Vec::new();
        keys(&v, &mut all);
        for k in all {
            assert!(
                !forbidden.iter().any(|f| k.contains(f)),
                "{name} exposes review-state key {k}"
            );
        }
    };
    check(
        "ReviewFinding",
        serde_json::to_value(sample_finding()).unwrap(),
    );
    check(
        "FindingUserState",
        serde_json::to_value(FindingUserState::untouched(FindingId::new("f"), "s", "t")).unwrap(),
    );
    check(
        "AnalysisRun",
        serde_json::to_value(AnalysisRun::queued(
            &run_key(&snapshot()),
            AnalysisMode::Static,
        ))
        .unwrap(),
    );
}

#[test]
fn finding_modules_do_not_reference_review_state_types() {
    // Structural guard: the finding/analysis/imported modules must not
    // depend on acknowledgement or file-status types.
    for (name, src) in [
        ("analysis.rs", include_str!("analysis.rs")),
        ("finding.rs", include_str!("finding.rs")),
        ("imported.rs", include_str!("imported.rs")),
    ] {
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for banned in ["Acknowledgement", "FileStatus", "FileState", "review::"] {
            assert!(!code.contains(banned), "{name} references {banned}");
        }
    }
}

#[test]
fn user_state_cannot_carry_reviewed_flag_or_disposition() {
    let smuggled = json!({
        "findingId": "find_a", "sessionId": "s", "disposition": "untouched",
        "snoozedUntil": null, "note": null, "promotedDraftId": null,
        "updatedAt": "t", "revision": 0, "reviewed": true
    });
    assert!(serde_json::from_value::<FindingUserState>(smuggled).is_err());
    assert!(serde_json::from_str::<UserDisposition>(r#""reviewed""#).is_err());
    assert!(serde_json::from_str::<UserDisposition>(r#""mark_reviewed""#).is_err());
}

#[test]
fn extensions_cannot_smuggle_review_state() {
    let mut f = sample_finding();
    f.extensions.insert("reviewed".into(), json!(true));
    assert_eq!(
        f.validate(),
        Err(vec![FindingContractError::ReservedExtensionKey(
            "reviewed".into()
        )])
    );
}

#[test]
fn generated_typescript_has_no_review_state() {
    use ts_rs::TS;
    for decl in [
        ReviewFinding::decl(&ts_rs::Config::default()),
        FindingUserState::decl(&ts_rs::Config::default()),
        UserDisposition::decl(&ts_rs::Config::default()),
    ] {
        let lower = decl.to_lowercase();
        assert!(!lower.contains("\"reviewed\""), "{decl}");
        assert!(!lower.contains("reviewed:"), "{decl}");
        assert!(!lower.contains("acknowledgement"), "{decl}");
        assert!(!lower.contains("filestatus"), "{decl}");
    }
}

// --- Other invariants ----------------------------------------------------

#[test]
fn high_attention_requires_evidence_and_imports_are_not_facts() {
    let mut f = sample_finding();
    f.attention.level = AttentionLevel::High;
    f.evidence_ids.clear();
    f.source.kind = SourceKind::GithubReviewThread;
    f.assertion_kind = AssertionKind::MeasuredFact;
    assert_eq!(
        f.validate(),
        Err(vec![
            FindingContractError::HighAttentionWithoutEvidence,
            FindingContractError::ImportedAsMeasuredFact,
        ])
    );
}
