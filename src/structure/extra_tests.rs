use super::*;
use crate::diff::{Limits, parse_unified};
use crate::model::structure::{Blob, StructureInput, StructureLimits};

fn fixture(src: &str) -> StructureInput {
    let diff = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -0,0 +1 @@\n+fn root() {}\n";
    StructureInput {
        repository: "r".into(),
        base_sha: "b".into(),
        head_sha: "h".into(),
        head_blobs: vec![
            Blob {
                path: "src/lib.rs".into(),
                bytes: src.as_bytes().into(),
            },
            Blob {
                path: "Cargo.toml".into(),
                bytes: b"[package]\nname = \"test\"\nversion = \"0.1.0\"\n".to_vec(),
            },
        ],
        base_blobs: vec![],
        changed_files: parse_unified(diff, "h", &Limits::default()).files,
        limits: StructureLimits::default(),
    }
}
#[test]
fn cycles_do_not_repeat_and_depth_is_bounded() {
    let mut i =
        fixture("fn root() {}\nfn a() { root(); b(); }\nfn b() { a(); }\nfn c() { b(); }\n");
    i.limits.max_depth = 1;
    let r = analyze(&i, &|| false);
    assert_eq!(r.results[0].resolved_dependants, None);
    assert!(r.results[0].warnings.contains(&"depth_cap".into()));
    assert_eq!(r.results[0].dependant_paths.len(), 1);
    i.limits.max_depth = 4;
    let r = analyze(&i, &|| false);
    assert_eq!(r.results[0].resolved_dependants, Some(3));
    assert_eq!(r.results[0].dependant_paths.len(), 3);
}
#[test]
fn visit_cap_and_zero_edge() {
    let mut i = fixture("fn root() {}\nfn a() { root(); }\nfn b() { a(); }\n");
    i.limits.max_visited = 1;
    let r = analyze(&i, &|| false);
    assert_eq!(r.results[0].resolved_dependants, None);
    assert!(!r.results[0].complete);
    assert!(r.results[0].warnings.contains(&"visit_cap".into()));
    i.head_blobs[0].bytes = b"fn root() {}\n".to_vec();
    let r = analyze(&i, &|| false);
    assert_eq!(r.results[0].resolved_dependants, Some(0));
}

#[test]
fn wide_frontier_never_exceeds_visit_cap() {
    let mut src = String::from("fn root() {}\n");
    for n in 0..250 {
        src.push_str(&format!("fn caller_{n}() {{ root(); }}\n"));
    }
    let mut input = fixture(&src);
    input.limits.max_visited = 200;
    input.limits.max_paths = 300;
    let result = analyze(&input, &|| false);
    assert_eq!(result.results[0].resolved_dependants, None);
    assert_eq!(result.results[0].direct_dependants, Some(250));
    assert_eq!(result.results[0].within_depth_dependants, None);
    assert_eq!(result.results[0].reachable_dependants, None);
    assert!(result.results[0].visits_truncated);
    assert!(!result.coverage.complete);
    assert_eq!(result.results[0].dependant_paths.len(), 199);
    assert!(result.results[0].warnings.contains(&"visit_cap".into()));
}

#[test]
fn depth_frontier_marks_private_chain_incomplete() {
    let mut input = fixture(
        "fn root() {}\nfn a() { root(); }\nfn b() { a(); }\nfn c() { b(); }\nfn d() { c(); }\nfn e() { d(); }\n",
    );
    input.limits.max_depth = 4;
    let result = analyze(&input, &|| false);
    assert_eq!(result.results[0].resolved_dependants, None);
    assert_eq!(result.results[0].direct_dependants, Some(1));
    assert_eq!(result.results[0].within_depth_dependants, Some(4));
    assert_eq!(result.results[0].reachable_dependants, None);
    assert!(result.results[0].depth_truncated);
    assert!(!result.results[0].visits_truncated);
    assert!(!result.results[0].complete);
    assert!(result.results[0].warnings.contains(&"depth_cap".into()));
    let complete = analyze_with_corpus(&input, &["src/lib.rs".into()], &|| false);
    assert!(!complete.results[0].complete);
    assert!(!complete.coverage.complete);
}

#[test]
fn block_imports_never_leak_to_siblings_or_nested_blocks() {
    let input = fixture(
        "fn root() {}\nfn first() { use crate::root as alias; alias(); { use crate::root as nested; nested(); } }\nfn second() { alias(); nested(); }\n",
    );
    let result = analyze(&input, &|| false);
    assert!(result.edges.iter().all(|edge| edge.kind != "call"));
    assert!(
        result
            .coverage
            .unresolved_sites
            .iter()
            .any(|s| s.reason == "block-local import unsupported")
    );
}

#[test]
fn stacked_cfg_and_enclosing_cfg_module_fail_closed() {
    let input = fixture(
        "#[cfg(feature = \"x\")]\n#[inline]\nfn root() {}\n#[cfg(feature = \"x\")]\nmod gated { fn hidden() {} }\nfn caller() { root(); gated::hidden(); }\n",
    );
    let result = analyze(&input, &|| false);
    assert!(result.edges.is_empty());
    assert!(
        result
            .entities
            .iter()
            .find(|e| e.qualified_name == "crate::root")
            .unwrap()
            .ambiguous
    );
    assert!(
        result
            .entities
            .iter()
            .find(|e| e.qualified_name == "crate::gated::hidden")
            .unwrap()
            .ambiguous
    );
}

#[test]
fn block_import_collision_and_glob_fail_closed() {
    let input = fixture(
        "fn target() {}\nmod other { pub fn target() {} }\nmod alias { pub fn target() {} }\nfn caller() { target(); use crate::other::target; }\nfn glob() { target(); use crate::other::*; }\nfn qualified() { alias::target(); use crate::other as alias; }\n",
    );
    let result = analyze(&input, &|| false);
    assert!(result.edges.iter().all(|e| e.kind != "call"));
    assert!(
        result
            .coverage
            .unresolved_sites
            .iter()
            .any(|s| s.reason == "local/parameter shadowing")
    );
}

#[test]
fn generic_and_scoped_types_do_not_alias_local_definition() {
    let input = fixture(
        "struct Item {}\nmod other { pub struct Item {} }\nfn generic<Item>(value: Item) {}\nfn scoped(value: crate::other::Item) {}\n",
    );
    let result = analyze(&input, &|| false);
    let local = result
        .entities
        .iter()
        .find(|e| e.qualified_name == "crate::Item")
        .unwrap();
    let other = result
        .entities
        .iter()
        .find(|e| e.qualified_name == "crate::other::Item")
        .unwrap();
    assert!(
        !result
            .edges
            .iter()
            .any(|e| e.to == local.id && e.kind == "reference")
    );
    assert!(
        result
            .edges
            .iter()
            .any(|e| e.to == other.id && e.kind == "reference")
    );
}

#[test]
fn stacked_external_cfg_module_is_not_discovered() {
    let mut input = fixture("#[cfg(feature = \"x\")]\n#[allow(dead_code)]\nmod gated;\n");
    input.head_blobs.push(Blob {
        path: "src/gated.rs".into(),
        bytes: b"pub fn hidden() {}\n".to_vec(),
    });
    let layout = super::layout::discover(&input.head_blobs, input.limits.max_blob_bytes, &|| false);
    assert!(!layout.modules.contains_key("src/gated.rs"));
    assert!(
        analyze(&input, &|| false)
            .entities
            .iter()
            .all(|e| e.qualified_name != "crate::gated::hidden")
    );
}

#[test]
fn base_layout_uses_declared_module_scope_for_changed_child() {
    let mut input = fixture("mod helper;\nfn root() {}\n");
    input.base_blobs = vec![
        input.head_blobs[1].clone(),
        Blob {
            path: "src/lib.rs".into(),
            bytes: b"mod helper;\n".to_vec(),
        },
        Blob {
            path: "src/helper.rs".into(),
            bytes: b"fn old() {}\n".to_vec(),
        },
    ];
    input.changed_files = parse_unified("diff --git a/src/helper.rs b/src/helper.rs\n--- a/src/helper.rs\n+++ b/src/helper.rs\n@@ -1 +1 @@\n-fn old() {}\n+fn new() {}\n", "h", &Limits::default()).files;
    input.head_blobs.push(Blob {
        path: "src/helper.rs".into(),
        bytes: b"fn new() {}\n".to_vec(),
    });
    let result = analyze(&input, &|| false);
    assert!(
        result
            .results
            .iter()
            .any(|r| r.entity.qualified_name == "crate::helper::old"
                && r.relationships_unavailable.as_deref() == Some("base_graph_not_built"))
    );
}

#[test]
fn base_binary_child_deletion_keeps_target_scope() {
    let mut input = fixture("fn root() {}\n");
    input.base_blobs = vec![
        input.head_blobs[1].clone(),
        Blob {
            path: "src/bin/tool.rs".into(),
            bytes: b"mod child;\n".to_vec(),
        },
        Blob {
            path: "src/bin/child.rs".into(),
            bytes: b"fn old() {}\n".to_vec(),
        },
    ];
    input.changed_files = parse_unified("diff --git a/src/bin/child.rs b/src/bin/child.rs\n--- a/src/bin/child.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-fn old() {}\n", "h", &Limits::default()).files;
    let result = analyze(&input, &|| false);
    assert!(
        result
            .results
            .iter()
            .any(|r| r.entity.qualified_name == "bin:tool::child::old"
                && r.relationships_unavailable.as_deref() == Some("base_graph_not_built"))
    );
}

#[test]
fn binary_root_import_and_base_identity_match() {
    let mut input = fixture("fn root() {}\nfn caller() { crate::root(); }\n");
    input.head_blobs[0].path = "src/main.rs".into();
    input.base_blobs.push(Blob {
        path: "src/main.rs".into(),
        bytes: b"fn root() {}\n".to_vec(),
    });
    let result = analyze(&input, &|| false);
    assert!(
        result
            .entities
            .iter()
            .any(|e| e.qualified_name == "main::root")
    );
    assert_eq!(result.edges.iter().filter(|e| e.kind == "call").count(), 1);
    let base = inventory(
        &input.base_blobs,
        &input.repository,
        &input.base_sha,
        &input,
        &|| false,
        std::time::Instant::now(),
        None,
    );
    assert_eq!(
        base.entities[0].id,
        result
            .entities
            .iter()
            .find(|e| e.qualified_name == "main::root")
            .unwrap()
            .id
    );
}

#[test]
fn direct_callees_embed_entity_details() {
    let input = fixture("fn root() { callee(); }\nfn callee() {}\n");
    let result = analyze(&input, &|| false);
    assert_eq!(result.results[0].direct_callee_entities.len(), 1);
    assert_eq!(
        result.results[0].direct_callee_entities[0].qualified_name,
        "crate::callee"
    );
}

#[test]
fn base_parse_failures_are_visible_in_coverage() {
    let mut input = fixture("fn root() {}\n");
    input.base_blobs.push(Blob {
        path: "src/lib.rs".into(),
        bytes: b"fn root( {\n".to_vec(),
    });
    let result = analyze_with_corpus(&input, &["src/lib.rs".into()], &|| false);
    assert!(!result.coverage.complete);
    assert!(
        result
            .coverage
            .skipped_files
            .iter()
            .any(|reason| reason.starts_with("base: src/lib.rs:"))
    );
}

#[test]
fn self_and_repeated_super_direct_imports_resolve() {
    let input = fixture(
        "fn root() {}\nmod outer { fn target() {} mod inner { use self::local::thing as local_thing; use super::super::root as top; use super::target as parent; mod local { pub fn thing() {} } fn caller() { local_thing(); top(); parent(); } } }\n",
    );
    let result = analyze(&input, &|| false);
    let calls: Vec<_> = result
        .edges
        .iter()
        .filter(|edge| edge.kind == "call")
        .collect();
    assert_eq!(calls.len(), 3);
}
