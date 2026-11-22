use super::*;
use crate::{
    analysis::AnalysisInput,
    demo,
    github::context::{AnalysisContext, HistoryObjects},
    model::{
        AnalysisMode, AnalysisRun, RunKeyInput, SnapshotId,
        structure::{CorpusCoverage, Edge, Entity, EntityPath, EntityResult, Site},
    },
    session::Session,
    store::Store,
};

fn context() -> ProviderContext {
    let snapshot = demo::snapshot(0);
    let session = Session::new(
        snapshot.clone(),
        Store::open_in_memory().unwrap(),
        None,
        None,
    )
    .unwrap();
    let input = AnalysisInput::new(&session.pr_id, &session.repository.id, snapshot);
    let run = AnalysisRun::queued(
        &RunKeyInput {
            snapshot_id: &input.snapshot_id,
            provider_id: ID,
            engine_version: VERSION,
            configuration_digest: "fixture",
            input_digest: "fixture",
            attempt: 1,
        },
        AnalysisMode::Static,
    );
    ProviderContext {
        run_id: run.id,
        attempt: 1,
        limits: ProviderLimits::default(),
        input: Arc::new(input),
    }
}
#[test]
fn left_anchor_uses_comparison_base_not_pr_base_tip() {
    let mut ctx = context();
    let input = Arc::make_mut(&mut ctx.input);
    input.history_context = Some(crate::github::context::AnalysisContext {
        source: "fixture".into(),
        base_sha: input.base_sha.clone(),
        head_sha: input.head_sha.clone(),
        comparison_base: Some("merge-base".into()),
        history_count: 0,
        shallow: false,
        complete: true,
        reason: None,
    });
    let path = input.snapshot.files[0].path.clone();
    let site = Site {
        path: path.clone(),
        line: 4,
        snapshot_sha: "merge-base".into(),
        reason: "deletion".into(),
    };
    let a = anchor(&ctx, &path, Some("entity"), Some(&site));
    assert_eq!(a.side, Some(Side::Left));
    assert_eq!(a.commit_sha.as_deref(), Some("merge-base"));
    let unproven = Site {
        snapshot_sha: "unrelated".into(),
        ..site
    };
    let a = anchor(&ctx, &path, Some("entity"), Some(&unproven));
    assert_eq!(a.kind, AnchorKind::Entity);
    assert_eq!(a.line, None);
}
#[test]
fn mixed_files_distinguish_non_source_and_unsupported_language() {
    let mut ctx = context();
    let input = Arc::make_mut(&mut ctx.input);
    let snapshot = Arc::make_mut(&mut input.snapshot);
    for path in ["README.md", "src/example.go"] {
        let mut file = snapshot.files[0].clone();
        file.path = path.into();
        snapshot.files.push(file);
    }
    let result = StructureOutput {
        repository: input.canonical_repo_id.clone(),
        base_sha: input.base_sha.clone(),
        head_sha: input.head_sha.clone(),
        limits_digest: "test".into(),
        entities: vec![],
        edges: vec![],
        results: vec![],
        coverage: CorpusCoverage {
            analyzed_files: 0,
            analyzed_entities: 0,
            skipped_files: vec![],
            unresolved_sites: vec![],
            unmapped_changed_rows: vec![],
            omitted_files: Some(0),
            omitted_results: 0,
            complete: false,
            limitations: vec![],
            parser_version: "fixture".into(),
        },
    };
    let out = present(&ctx, result).unwrap();
    assert!(out.coverage.entries.iter().any(|entry| matches!((&entry.scope, &entry.outcome), (CoverageScope::File { path, .. }, ScopeOutcome::NotApplicable { reason }) if path == "README.md" && reason == "non_source_file")));
    assert!(out.coverage.entries.iter().any(|entry| matches!((&entry.scope, &entry.outcome), (CoverageScope::File { path, .. }, ScopeOutcome::Unknown { reason }) if path == "src/example.go" && reason == "language_unsupported")));
}
#[test]
fn missing_context_is_explicitly_partial() {
    let ctx = context();
    let output = scan(ctx, &AtomicBool::new(false)).unwrap();
    assert_eq!(output.status, RunStatus::Partial);
    assert_eq!(
        output.coverage.reason_codes,
        ["structure_context_unavailable"]
    );
    assert!(output.findings.is_empty());
}
#[test]
fn scan_supplies_competing_base_module_forms_for_changed_file() {
    for (changed, competitor) in [
        ("src/foo.rs", "src/foo/mod.rs"),
        ("src/foo/mod.rs", "src/foo.rs"),
    ] {
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
        std::fs::create_dir_all(root.join("src/foo")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(root.join("src/lib.rs"), "mod foo;\n").unwrap();
        std::fs::write(root.join("src/foo.rs"), "pub fn old() {}\n").unwrap();
        std::fs::write(root.join("src/foo/mod.rs"), "pub fn old() {}\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "ambiguous base"]);
        let base = git(&["rev-parse", "HEAD"]);
        std::fs::remove_file(root.join(competitor)).unwrap();
        std::fs::write(root.join(changed), "pub fn new() {}\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "head"]);
        let head = git(&["rev-parse", "HEAD"]);

        let mut ctx = context();
        let input = Arc::make_mut(&mut ctx.input);
        input.base_sha = base.clone();
        input.head_sha = head.clone();
        let file = &mut Arc::make_mut(&mut input.snapshot).files[0];
        file.path = changed.into();
        file.previous_path = None;
        file.hunks[0].lines = vec![crate::model::DiffLine {
            id: "deleted".into(),
            kind: crate::model::LineKind::Deletion,
            text: "pub fn old() {}".into(),
            old_line: Some(1),
            new_line: None,
            patch_position: 1,
        }];
        input.history_context = Some(AnalysisContext {
            source: "fixture".into(),
            base_sha: base.clone(),
            head_sha: head,
            comparison_base: Some(base.clone()),
            history_count: 1,
            shallow: false,
            complete: true,
            reason: None,
        });
        input.history_objects = Some(HistoryObjects::fixture(temp, root.join(".git")));
        let output = scan(ctx, &AtomicBool::new(false)).unwrap();
        let coverage = &output.evidence[0].value().value.as_ref().unwrap()["coverage"];
        assert!(
            coverage["skipped_files"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| {
                    s.as_str()
                        .unwrap()
                        .contains(&format!("base: {changed}: unsupported layout/language"))
                }),
            "base ambiguity must not validate {changed}: {coverage}"
        );
        assert!(!output.evidence.iter().any(|e| {
            e.value().value.as_ref().is_some_and(|v| {
                v["entity"]["snapshot_sha"] == base && v["entity"]["path"] == changed
            })
        }));
    }
}
#[test]
fn scan_supplies_full_pinned_head_tree_inventory() {
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
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(root.join("src/lib.rs"), "fn local() {}\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "fixture"]);
    let head = git(&["rev-parse", "HEAD"]);

    let mut ctx = context();
    let input = Arc::make_mut(&mut ctx.input);
    input.base_sha = head.clone();
    input.head_sha = head.clone();
    Arc::make_mut(&mut input.snapshot).files.clear();
    input.history_context = Some(AnalysisContext {
        source: "fixture".into(),
        base_sha: head.clone(),
        head_sha: head.clone(),
        comparison_base: Some(head.clone()),
        history_count: 1,
        shallow: false,
        complete: true,
        reason: None,
    });
    input.history_objects = Some(HistoryObjects::fixture(temp, root.join(".git")));
    let output = scan(ctx.clone(), &AtomicBool::new(false)).unwrap();
    assert_eq!(output.status, RunStatus::Succeeded, "{:?}", output.warnings);
    assert_eq!(output.coverage.completeness, Completeness::Complete);
    assert_eq!(
        output.coverage.analyzed_files,
        KnownCount::Known { value: 1 }
    );
    assert_eq!(output.findings.len(), 1);
    assert_eq!(
        output.evidence[0].value().value.as_ref().unwrap()["coverage"]["omitted_files"],
        0
    );
    // A retained HEAD still produces inventory when the comparison base is
    // missing. The precise reason must be visible to session retry policy.
    let input = Arc::make_mut(&mut ctx.input);
    input.history_context.as_mut().unwrap().comparison_base = None;
    let missing = scan(ctx.clone(), &AtomicBool::new(false)).unwrap();
    assert_eq!(missing.status, RunStatus::Partial);
    assert!(
        missing
            .coverage
            .reason_codes
            .contains(&"comparison_base_unavailable".into())
    );
    assert_eq!(
        missing.coverage.analyzed_files,
        KnownCount::Known { value: 1 }
    );
    assert!(
        missing.evidence[0].value().value.as_ref().unwrap()["coverage"]["limitations"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("comparison_base_unavailable"))
    );
    Arc::make_mut(&mut ctx.input)
        .history_context
        .as_mut()
        .unwrap()
        .comparison_base = Some(head.clone());
    let restored = scan(ctx.clone(), &AtomicBool::new(false)).unwrap();
    assert_eq!(restored.status, RunStatus::Succeeded);
    assert!(
        !restored
            .coverage
            .reason_codes
            .contains(&"comparison_base_unavailable".into())
    );

    // A non-Rust path is never read as a blob by the adapter, but the complete
    // tree inventory still lets the engine report unsupported corpus exposure.
    std::fs::write(root.join("other.go"), "package fixture\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "unsupported source"]);
    let new_head = git(&["rev-parse", "HEAD"]);
    let input = Arc::make_mut(&mut ctx.input);
    input.head_sha = new_head.clone();
    input.history_context.as_mut().unwrap().head_sha = new_head;
    let output = scan(ctx.clone(), &AtomicBool::new(false)).unwrap();
    assert_eq!(output.status, RunStatus::Partial);
    assert!(
        !output
            .coverage
            .reason_codes
            .contains(&"structure_object_read_failed".into())
    );
    let skipped = &output.evidence[0].value().value.as_ref().unwrap()["coverage"]["skipped_files"];
    assert!(skipped.as_array().unwrap().iter().any(|path| {
        path.as_str()
            .unwrap()
            .contains("other.go: language unsupported")
    }));

    // Non-regular tree entries are not followed or counted as a complete corpus.
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("src/lib.rs", root.join("other.py")).unwrap();
        git(&["add", "other.py"]);
    }
    git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{head},vendor"),
    ]);
    git(&["commit", "-qm", "non-regular tree entries"]);
    let linked_head = git(&["rev-parse", "HEAD"]);
    let input = Arc::make_mut(&mut ctx.input);
    input.head_sha = linked_head.clone();
    input.history_context.as_mut().unwrap().head_sha = linked_head;
    let linked = scan(ctx.clone(), &AtomicBool::new(false)).unwrap();
    assert_eq!(linked.status, RunStatus::Partial);
    let skipped = &linked.evidence[0].value().value.as_ref().unwrap()["coverage"]["skipped_files"];
    assert!(
        skipped
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s.as_str().unwrap().contains("vendor: gitlink excluded"))
    );
    #[cfg(unix)]
    assert!(
        skipped
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s.as_str().unwrap().contains("other.py: symlink excluded"))
    );

    let blob = git(&["rev-parse", "HEAD:src/lib.rs"]);
    let object = root.join(".git/objects").join(&blob[..2]).join(&blob[2..]);
    let bytes = std::fs::read(&object).unwrap();
    std::fs::remove_file(&object).unwrap();
    let failed = scan(ctx.clone(), &AtomicBool::new(false)).unwrap();
    assert_eq!(failed.status, RunStatus::Partial);
    assert!(
        failed
            .coverage
            .reason_codes
            .contains(&"structure_object_read_failed".into())
    );
    std::fs::write(&object, bytes).unwrap();
    let recovered = scan(ctx, &AtomicBool::new(false)).unwrap();
    assert!(
        !recovered
            .coverage
            .reason_codes
            .contains(&"structure_object_read_failed".into())
    );
}
#[test]
fn typed_evidence_and_entity_relations_survive_serialization() {
    let ctx = context();
    let path = ctx.input.snapshot.files[0].path.clone();
    let entity = Entity {
        id: "entity-a".into(),
        qualified_name: "crate::a".into(),
        kind: "function".into(),
        path: path.clone(),
        start_line: 1,
        end_line: 3,
        snapshot_sha: ctx.input.head_sha.clone(),
        ambiguous: false,
        public: false,
    };
    let dependent = Entity {
        id: "entity-b".into(),
        qualified_name: "crate::b".into(),
        ..entity.clone()
    };
    let site = Site {
        path: path.clone(),
        line: 1,
        snapshot_sha: ctx.input.head_sha.clone(),
        reason: "addition".into(),
    };
    let edge = Edge {
        from: dependent.id.clone(),
        to: entity.id.clone(),
        kind: "call".into(),
        site: site.clone(),
    };
    let transitive = Entity {
        id: "entity-c".into(),
        qualified_name: "crate::c".into(),
        path: "src/other.rs".into(),
        ..entity.clone()
    };
    let transitive_edge = Edge {
        from: transitive.id.clone(),
        to: dependent.id.clone(),
        kind: "call".into(),
        site: Site {
            path: transitive.path.clone(),
            ..site.clone()
        },
    };
    let result = StructureOutput {
        repository: ctx.input.canonical_repo_id.clone(),
        base_sha: ctx.input.base_sha.clone(),
        head_sha: ctx.input.head_sha.clone(),
        limits_digest: "test".into(),
        entities: vec![entity.clone(), dependent.clone()],
        edges: vec![edge.clone()],
        results: vec![EntityResult {
            entity: entity.clone(),
            anchor: Some(site),
            anchor_reason: None,
            resolved_dependants: Some(1),
            direct_dependants: Some(1),
            within_depth_dependants: Some(1),
            reachable_dependants: Some(1),
            depth_truncated: false,
            visits_truncated: false,
            direct_callees: vec![Edge {
                from: entity.id.clone(),
                to: "callee-id".into(),
                kind: "call".into(),
                site: Site {
                    path: path.clone(),
                    line: 2,
                    snapshot_sha: ctx.input.head_sha.clone(),
                    reason: "call".into(),
                },
            }],
            direct_callee_entities: vec![Entity {
                id: "callee-id".into(),
                qualified_name: "crate::callee".into(),
                path: "src/callee.rs".into(),
                start_line: 7,
                end_line: 9,
                ..entity.clone()
            }],
            dependant_paths: vec![
                EntityPath {
                    entities: vec![entity.clone(), dependent.clone()],
                    edges: vec![edge.clone()],
                },
                EntityPath {
                    entities: vec![entity, dependent, transitive],
                    edges: vec![edge, transitive_edge],
                },
            ],
            relationships_unavailable: None,
            complete: true,
            warnings: vec![],
        }],
        coverage: CorpusCoverage {
            analyzed_files: 1,
            analyzed_entities: 2,
            skipped_files: vec![],
            unresolved_sites: vec![],
            unmapped_changed_rows: vec![],
            omitted_files: Some(0),
            omitted_results: 0,
            complete: true,
            limitations: vec![],
            parser_version: "fixture".into(),
        },
    };
    let mut rerun = ctx.clone();
    rerun.run_id = crate::model::AnalysisRunId::new("another-run");
    let repeated = present(&rerun, result.clone()).unwrap();
    let mut changed = result.clone();
    changed.results[0]
        .warnings
        .push("new source observation".into());
    let changed = present(&rerun, changed).unwrap();
    let output = present(&ctx, result).unwrap();
    assert_ne!(
        output.findings[1].value().id,
        changed.findings[1].value().id
    );
    assert_eq!(
        output.findings[1].value().id,
        repeated.findings[1].value().id
    );
    assert_ne!(
        output.evidence[1].value().id,
        repeated.evidence[1].value().id
    );
    assert_eq!(output.findings.len(), 2);
    assert_eq!(output.evidence.len(), 2);
    assert_eq!(output.relations.len(), 1);
    let finding = output.findings[1].value();
    assert_eq!(finding.validate(), Ok(()));
    assert_eq!(
        finding.source.rule_id.as_deref(),
        Some("aside.structure.blast_radius")
    );
    assert_eq!(finding.assertion_kind, AssertionKind::MeasuredFact);
    assert_eq!(finding.kinds, [FindingKind::BlastRadius]);
    assert_eq!(finding.severity.normalized, Severity::Info);
    assert_eq!(
        output.findings[0].value().attention.level,
        AttentionLevel::None
    );
    assert!(
        finding
            .related_anchors
            .iter()
            .all(|a| a.kind == AnchorKind::RelatedContext
                && a.commit_sha.as_deref() == Some(ctx.input.head_sha.as_str())
                && a.diff_digest.is_none()
                && a.line.is_none())
    );
    assert!(
        finding
            .related_anchors
            .iter()
            .any(|a| a.current_path.as_deref() == Some("src/other.rs"))
    );
    assert_eq!(finding.evidence_ids[0], output.evidence[1].value().id);
    let evidence = output.evidence[1].value().value.as_ref().unwrap();
    assert_eq!(
        evidence["direct_callee_entities"][0]["qualified_name"],
        "crate::callee"
    );
    assert_eq!(
        evidence["direct_callee_entities"][0]["path"],
        "src/callee.rs"
    );
    assert_eq!(evidence["direct_callee_entities"][0]["start_line"], 7);
    assert_eq!(evidence["direct_callee_entities"][0]["end_line"], 9);
    assert_eq!(
        evidence["direct_callee_entities"][0]["snapshot_sha"],
        ctx.input.head_sha
    );
    assert_eq!(
        output.evidence[1].value().value.as_ref().unwrap()["resolved_dependants"],
        1
    );
    assert_eq!(output.relations[0].source_finding_id, finding.id);
    assert!(matches!(&output.relations[0].target, RelationTarget::Entity(id) if id == "entity-b"));

    // Commit the adapter's actual batch, then read both typed links through
    // the public store API. Pruning after a newer snapshot removes old rows.
    let finding_id = finding.id.clone();
    let session = Session::new(
        (*ctx.input.snapshot).clone(),
        Store::open_in_memory().unwrap(),
        None,
        None,
    )
    .unwrap();
    let mut store = session.store.lock().unwrap();
    store
        .register_analysis_snapshot(
            &session.pr_id,
            &ctx.input.snapshot_id,
            &ctx.input.base_sha,
            &ctx.input.head_sha,
            None,
        )
        .unwrap();
    store
        .set_current_analysis_snapshot(&session.pr_id, &ctx.input.snapshot_id)
        .unwrap();
    let mut run = AnalysisRun::queued(
        &RunKeyInput {
            snapshot_id: &ctx.input.snapshot_id,
            provider_id: ID,
            engine_version: VERSION,
            configuration_digest: "fixture",
            input_digest: "fixture",
            attempt: 1,
        },
        AnalysisMode::Static,
    );
    assert_eq!(run.id, ctx.run_id);
    store
        .record_analysis_run(&session.pr_id, &Raw::new(run.clone()).unwrap())
        .unwrap();
    run.status = output.status;
    run.started_at = Some(now_rfc3339());
    run.finished_at = Some(now_rfc3339());
    run.coverage = output.coverage;
    let batch = crate::store::AnalysisBatch {
        run: Raw::new(run).unwrap(),
        findings: output.findings,
        evidence: output.evidence,
        relations: output.relations,
    };
    store.commit_analysis_batch(&batch).unwrap();
    let persisted = store.list_finding_evidence(&finding_id).unwrap();
    assert_eq!(persisted.len(), 1);
    assert_eq!(
        persisted[0].value().value.as_ref().unwrap()["resolved_dependants"],
        1
    );
    assert_eq!(store.list_finding_relations(&finding_id).unwrap().len(), 1);
    let next = SnapshotId::derive(
        &ctx.input.canonical_repo_id,
        ctx.input.pr_number,
        &ctx.input.base_sha,
        "new-head",
    );
    store
        .register_analysis_snapshot(&session.pr_id, &next, &ctx.input.base_sha, "new-head", None)
        .unwrap();
    store
        .set_current_analysis_snapshot(&session.pr_id, &next)
        .unwrap();
    store.prune_analysis(&session.pr_id, 0).unwrap();
    assert!(store.list_finding_evidence(&finding_id).unwrap().is_empty());
    assert!(
        store
            .list_finding_relations(&finding_id)
            .unwrap()
            .is_empty()
    );
}
