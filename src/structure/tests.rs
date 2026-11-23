use super::*;
use crate::diff::{Limits, parse_unified};
use crate::model::structure::StructureLimits;

fn input(diff: &str, head: &[(&str, &str)], base: &[(&str, &str)]) -> StructureInput {
    StructureInput {
        repository: "example/repo".into(),
        base_sha: "base".into(),
        head_sha: "head".into(),
        head_blobs: std::iter::once(Blob {
            path: "Cargo.toml".into(),
            bytes: b"[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n".to_vec(),
        })
        .chain(head.iter().map(|(p, b)| Blob {
            path: (*p).into(),
            bytes: b.as_bytes().to_vec(),
        }))
        .collect(),
        base_blobs: base
            .iter()
            .map(|(p, b)| Blob {
                path: (*p).into(),
                bytes: b.as_bytes().to_vec(),
            })
            .collect(),
        changed_files: parse_unified(diff, "head", &Limits::default()).files,
        limits: StructureLimits::default(),
    }
}

#[test]
fn changed_callee_has_cross_file_dependent_and_stable_id() {
    let diff = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-pub fn callee() {}\n+pub fn callee() { let _ = 1; }\n";
    let head = [
        ("src/lib.rs", "mod a; mod b;\n"),
        ("src/a.rs", "pub fn callee() { let _ = 1; }\n"),
        (
            "src/b.rs",
            "use crate::a::callee;\npub fn caller() { callee(); }\n",
        ),
    ];
    let base = [
        ("src/lib.rs", "mod a;\n"),
        ("src/a.rs", "pub fn callee() {}\n"),
    ];
    let result = analyze(&input(diff, &head, &base), &|| false);
    assert_eq!(result.results.len(), 2);
    let changed = result
        .results
        .iter()
        .find(|r| r.entity.snapshot_sha == "head")
        .unwrap();
    assert_eq!(changed.resolved_dependants, Some(2));
    assert_eq!(changed.dependant_paths.len(), 2);
    assert!(changed.dependant_paths.iter().any(|p| {
        p.entities
            .last()
            .is_some_and(|e| e.qualified_name == "crate::b::caller")
    }));
    assert_eq!(changed.anchor.as_ref().unwrap().line, 1);
    assert_eq!(
        result
            .results
            .iter()
            .find(|r| r.entity.snapshot_sha == "base")
            .unwrap()
            .relationships_unavailable
            .as_deref(),
        Some("base_graph_not_built")
    );
    let shifted = input(
        "",
        &[
            ("src/lib.rs", "mod a;\n"),
            ("src/a.rs", "\npub fn callee() { let _ = 1; }\n"),
        ],
        &[],
    );
    assert_eq!(
        changed.entity.id,
        analyze(&shifted, &|| false).entities[0].id
    );
}

#[test]
fn source_mismatch_falls_back_and_cancellation_skips() {
    let diff = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -0,0 +1 @@\n+fn changed() {}\n";
    let i = input(diff, &[("src/lib.rs", "fn changed() { }\n")], &[]);
    let r = analyze(&i, &|| false);
    assert_eq!(
        r.results[0].anchor_reason.as_deref(),
        Some("diff_source_mismatch")
    );
    assert!(r.results[0].anchor.is_none());
    let cancelled = analyze(&i, &|| true);
    assert!(cancelled.entities.is_empty());
    assert!(!cancelled.coverage.skipped_files.is_empty());
}

#[test]
fn caps_and_unsupported_layout_are_explicit() {
    let mut i = input(
        "",
        &[
            ("src/lib.rs", "fn a() {}\nfn b() {}\n"),
            ("custom/else.rs", "fn c() {}"),
        ],
        &[],
    );
    i.limits.max_files = 1;
    i.limits.max_entities_per_file = 1;
    let r = analyze(&i, &|| false);
    assert_eq!(r.coverage.omitted_files, Some(1));
    assert_eq!(r.entities.len(), 1);
    assert!(!r.coverage.complete);
    assert!(!r.coverage.skipped_files.is_empty());
}
