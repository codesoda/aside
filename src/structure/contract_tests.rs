use super::*;
use crate::diff::{Limits, parse_unified};
use crate::model::structure::{Blob, StructureLimits};
fn input(files: &[(&str, &str)], diff: &str) -> StructureInput {
    let mut blobs = vec![Blob {
        path: "Cargo.toml".into(),
        bytes: b"[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n".to_vec(),
    }];
    blobs.extend(files.iter().map(|(path, src)| Blob {
        path: (*path).into(),
        bytes: src.as_bytes().to_vec(),
    }));
    StructureInput {
        repository: "r".into(),
        base_sha: "base".into(),
        head_sha: "head".into(),
        head_blobs: blobs,
        base_blobs: vec![],
        changed_files: parse_unified(diff, "head", &Limits::default()).files,
        limits: StructureLimits::default(),
    }
}
#[test]
fn grouped_alias_import_and_shadowing() {
    let i = input(
        &[
            (
                "src/lib.rs",
                "mod api; use crate::api::{target as renamed};\nfn caller() { renamed(); }\nfn shadow(renamed: fn()) { renamed(); }\n",
            ),
            ("src/api.rs", "pub fn target() {}\n"),
        ],
        "",
    );
    let r = analyze(&i, &|| false);
    let callee = r
        .entities
        .iter()
        .find(|e| e.qualified_name == "crate::api::target")
        .unwrap();
    assert!(r.edges.iter().any(|edge| {
        edge.to == callee.id
            && r.entities
                .iter()
                .any(|e| e.id == edge.from && e.qualified_name == "crate::caller")
    }));
    assert!(!r.edges.iter().any(|edge| {
        edge.to == callee.id
            && r.entities
                .iter()
                .any(|e| e.id == edge.from && e.qualified_name == "crate::shadow")
    }));
}
#[test]
fn explicit_targets_shared_files_and_unlinked_sources_fail_closed() {
    let i = input(
        &[
            ("src/lib.rs", "mod common;\n"),
            ("src/main.rs", "mod common;\n"),
            ("src/common.rs", "pub fn shared() {}\n"),
            ("src/orphan.rs", "fn orphan() {}"),
        ],
        "",
    );
    let r = analyze(&i, &|| false);
    assert!(
        !r.entities
            .iter()
            .any(|e| e.qualified_name.ends_with("shared") || e.qualified_name.ends_with("orphan"))
    );
    assert!(
        r.coverage
            .skipped_files
            .iter()
            .any(|s| s.contains("shared across targets"))
    );
    assert!(!r.coverage.complete);
}
#[test]
fn for_pattern_shadowing_never_resolves_to_same_named_function() {
    let i = input(
        &[(
            "src/lib.rs",
            "fn target() {}\nfn caller() { for target in [|| {}] { target(); } }\n",
        )],
        "",
    );
    let r = analyze(&i, &|| false);
    assert!(!r.edges.iter().any(|e| e.kind == "call"));
    assert!(!r.coverage.unresolved_sites.is_empty());
}
#[test]
fn closure_and_generic_shadowing_remain_unresolved() {
    let i = input(
        &[(
            "src/lib.rs",
            "fn target() {}\nfn outer() { let use_it = |target: fn()| target(); use_it(target); }\nfn generic<target>() {}\n",
        )],
        "",
    );
    let r = analyze(&i, &|| false);
    let target = r
        .entities
        .iter()
        .find(|e| e.qualified_name == "crate::target")
        .unwrap();
    assert!(
        !r.edges
            .iter()
            .any(|e| e.to == target.id && e.site.line == 2)
    );
    assert!(
        r.coverage
            .unresolved_sites
            .iter()
            .any(|s| s.reason == "local/parameter shadowing")
    );
}
#[test]
fn full_tree_path_attestation_controls_completeness() {
    let i = input(&[("src/lib.rs", "fn private() {}\n")], "");
    let missing = analyze(&i, &|| false);
    assert!(!missing.coverage.complete);
    let paths = vec!["Cargo.toml".into(), "src/lib.rs".into()];
    let known = analyze_with_corpus(&i, &paths, &|| false);
    assert!(known.coverage.complete);
    assert_eq!(known.coverage.analyzed_files, 1);
    let unsupported =
        analyze_with_corpus(&i, &["src/other.py".into(), "src/lib.rs".into()], &|| false);
    assert!(!unsupported.coverage.complete);
    assert!(
        unsupported
            .coverage
            .skipped_files
            .iter()
            .any(|s| s.contains("language unsupported"))
    );
    let missing_rust =
        analyze_with_corpus(&i, &["src/missing.rs".into(), "src/lib.rs".into()], &|| {
            false
        });
    assert!(!missing_rust.coverage.complete);
}
#[test]
fn inventory_functions_types_constants_modules_and_macros() {
    let i = input(
        &[(
            "src/lib.rs",
            "mod nested { pub fn f() {} }\nstruct Data; enum Variant { A } trait Trait {} type Alias = Data;\nconst LIMIT: usize = 1; static VALUE: usize = 2; macro_rules! hidden { () => { fn generated() {} } }\n",
        )],
        "",
    );
    let r = analyze(&i, &|| false);
    for (kind, name) in [
        ("module", "crate::nested"),
        ("function", "crate::nested::f"),
        ("type", "crate::Data"),
        ("type", "crate::Variant"),
        ("type", "crate::Trait"),
        ("type", "crate::Alias"),
        ("constant", "crate::LIMIT"),
        ("constant", "crate::VALUE"),
        ("macro", "crate::hidden"),
    ] {
        assert!(
            r.entities
                .iter()
                .any(|e| e.kind == kind && e.qualified_name == name),
            "{kind} {name}"
        );
    }
    assert!(
        !r.entities
            .iter()
            .any(|e| e.qualified_name.ends_with("generated"))
    );
}
#[test]
fn ambiguous_rename_predecessor_has_no_lineage() {
    let diff = "diff --git a/src/old.rs b/src/new.rs\nsimilarity index 70%\nrename from src/old.rs\nrename to src/new.rs\n--- a/src/old.rs\n+++ b/src/new.rs\n@@ -1,2 +1 @@\n-fn target() {}\n-fn target() {}\n+fn target() { let _ = 1; }\n";
    let mut i = input(
        &[
            ("src/lib.rs", "mod new;\n"),
            ("src/new.rs", "fn target() { let _ = 1; }\n"),
        ],
        diff,
    );
    i.base_blobs = vec![Blob {
        path: "src/old.rs".into(),
        bytes: b"fn target() {}\nfn target() {}\n".to_vec(),
    }];
    let r = analyze(&i, &|| false);
    assert!(
        r.results
            .iter()
            .filter(|e| e.entity.snapshot_sha == "head")
            .all(|e| !e
                .warnings
                .iter()
                .any(|w| w.starts_with("validated_rename_from:")))
    );
}
#[test]
fn conditional_import_and_namespace_collision_fail_closed() {
    let i = input(
        &[
            (
                "src/lib.rs",
                "mod api; #[cfg(feature = \"x\")] use crate::api::target;\nfn caller() { target(); }\nstruct Duplicate; fn Duplicate() {}\n",
            ),
            ("src/api.rs", "pub fn target() {}\n"),
        ],
        "",
    );
    let r = analyze(&i, &|| false);
    assert!(
        r.entities
            .iter()
            .filter(|e| e.qualified_name == "crate::Duplicate")
            .all(|e| e.ambiguous)
    );
    assert!(!r.edges.iter().any(|e| e.kind == "call"));
    assert!(
        r.coverage
            .unresolved_sites
            .iter()
            .any(|s| s.reason == "conditional import")
    );
}
#[test]
fn named_constant_value_reference_has_site() {
    let i = input(
        &[(
            "src/lib.rs",
            "const VALUE: usize = 1;\nfn caller() { let _ = VALUE; }\n",
        )],
        "",
    );
    let r = analyze(&i, &|| false);
    let from = r
        .entities
        .iter()
        .find(|e| e.qualified_name == "crate::caller")
        .unwrap();
    let to = r
        .entities
        .iter()
        .find(|e| e.qualified_name == "crate::VALUE")
        .unwrap();
    assert!(
        r.edges.iter().any(|e| e.from == from.id
            && e.to == to.id
            && e.kind == "reference"
            && e.site.line == 2)
    );
}
#[test]
fn named_type_reference_has_site_and_dependency() {
    let i = input(
        &[("src/lib.rs", "struct Item;\nfn caller(_: Item) {}\n")],
        "",
    );
    let r = analyze(&i, &|| false);
    let from = r
        .entities
        .iter()
        .find(|e| e.qualified_name == "crate::caller")
        .unwrap();
    let to = r
        .entities
        .iter()
        .find(|e| e.qualified_name == "crate::Item")
        .unwrap();
    assert!(
        r.edges.iter().any(|e| e.from == from.id
            && e.to == to.id
            && e.kind == "reference"
            && e.site.line == 2)
    );
}
#[test]
fn imported_module_alias_resolves_only_unambiguous_calls() {
    let i = input(
        &[
            (
                "src/lib.rs",
                "mod api; use crate::api as renamed;\nfn caller() { renamed::target(); }\n",
            ),
            ("src/api.rs", "pub fn target() {}\n"),
        ],
        "",
    );
    let r = analyze(&i, &|| false);
    let caller = r
        .entities
        .iter()
        .find(|e| e.qualified_name == "crate::caller")
        .unwrap();
    let target = r
        .entities
        .iter()
        .find(|e| e.qualified_name == "crate::api::target")
        .unwrap();
    assert!(
        r.edges
            .iter()
            .any(|e| e.from == caller.id && e.to == target.id)
    );
}
#[test]
fn parse_budget_and_blob_size_are_explicit() {
    let mut i = input(&[("src/lib.rs", "fn value() {}\n")], "");
    i.limits.max_blob_bytes = 4;
    let r = analyze(&i, &|| false);
    assert!(r.entities.is_empty());
    assert!(!r.coverage.complete);
    i.limits.max_blob_bytes = 512 * 1024;
    i.limits.parse_budget_ms = 0;
    let r = analyze(&i, &|| false);
    assert!(r.entities.is_empty());
    assert!(!r.coverage.skipped_files.is_empty());
}
#[test]
fn inline_and_bin_target_module_paths_are_distinct() {
    let i = input(
        &[
            ("src/lib.rs", "mod outer { mod inner; }\n"),
            ("src/outer/inner.rs", "fn lib_only() {}\n"),
            ("src/bin/tool.rs", "mod helper; fn entry() {}\n"),
            ("src/bin/tool/helper.rs", "fn bin_only() {}\n"),
        ],
        "",
    );
    let r = analyze(&i, &|| false);
    assert!(
        r.entities
            .iter()
            .any(|e| e.qualified_name == "crate::outer::inner::lib_only")
    );
    assert!(
        r.entities
            .iter()
            .any(|e| e.qualified_name == "bin:tool::helper::bin_only")
    );
}
#[test]
fn missing_manifest_and_cfg_definition_are_not_resolved() {
    let mut i = input(
        &[(
            "src/lib.rs",
            "#[cfg(feature = \"x\")] fn target() {}\nfn caller() { target(); }\n",
        )],
        "",
    );
    let r = analyze(&i, &|| false);
    assert!(r.edges.is_empty());
    assert!(!r.coverage.unresolved_sites.is_empty());
    i.head_blobs.retain(|b| b.path != "Cargo.toml");
    let missing = analyze(&i, &|| false);
    assert!(missing.entities.is_empty());
    assert!(!missing.coverage.complete);
}
