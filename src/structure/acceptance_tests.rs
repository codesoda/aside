//! Black-box acceptance cases built from committed Git objects and the production diff parser.
use super::*;
use crate::diff::{Limits, parse_unified};
use crate::model::structure::{Blob, StructureInput, StructureLimits};
use std::{fs, path::Path, process::Command};

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("git installed");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("text Git output")
        .trim_end_matches('\n')
        .to_string()
}

fn committed(dir: &Path, files: &[(&str, &str)], message: &str) -> String {
    // Each commit describes the entire tree, including removals.
    let existing = git(dir, &["ls-files"]);
    for path in existing.lines() {
        fs::remove_file(dir.join(path)).unwrap();
    }
    for (path, body) in files {
        let target = dir.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, body).unwrap();
    }
    git(dir, &["add", "-A"]);
    git(
        dir,
        &[
            "-c",
            "user.name=Acceptance",
            "-c",
            "user.email=acceptance@example.invalid",
            "commit",
            "-q",
            "-m",
            message,
        ],
    );
    git(dir, &["rev-parse", "HEAD"])
}

fn blobs(dir: &Path, sha: &str) -> Vec<Blob> {
    git(dir, &["ls-tree", "-r", "--name-only", sha])
        .lines()
        .map(|path| Blob {
            path: path.to_string(),
            bytes: Command::new("git")
                .current_dir(dir)
                .args(["show", &format!("{sha}:{path}")])
                .output()
                .unwrap()
                .stdout,
        })
        .collect()
}

fn fixture(base: &[(&str, &str)], head: &[(&str, &str)]) -> StructureInput {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    let manifest = (
        "Cargo.toml",
        "[package]\nname = \"acceptance\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    );
    let mut base_files = base.to_vec();
    if !base.iter().any(|(path, _)| *path == "Cargo.toml") {
        base_files.push(manifest);
    }
    let mut head_files = head.to_vec();
    if !head.iter().any(|(path, _)| *path == "Cargo.toml") {
        head_files.push(manifest);
    }
    let base_sha = committed(dir.path(), &base_files, "base");
    let head_sha = committed(dir.path(), &head_files, "head");
    let diff = git(
        dir.path(),
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--find-renames",
            &base_sha,
            &head_sha,
            "--",
        ],
    );
    let parsed = parse_unified(&diff, &head_sha, &Limits::default());
    assert!(
        parsed.warnings.is_empty(),
        "diff warnings: {:?}",
        parsed.warnings
    );
    StructureInput {
        repository: "acceptance/repo".into(),
        base_sha: base_sha.clone(),
        head_sha: head_sha.clone(),
        head_blobs: blobs(dir.path(), &head_sha),
        base_blobs: blobs(dir.path(), &base_sha),
        changed_files: parsed.files,
        limits: StructureLimits::default(),
    }
}

fn result<'a>(output: &'a StructureOutput, name: &str, sha: &str) -> &'a EntityResult {
    output
        .results
        .iter()
        .find(|r| r.entity.qualified_name == name && r.entity.snapshot_sha == sha)
        .unwrap_or_else(|| panic!("missing changed result {name} at {sha}"))
}

#[test]
fn committed_cross_file_graph_has_shortest_paths_sites_and_stable_ids() {
    let base = [
        ("src/lib.rs", "mod api; mod caller;\n"),
        ("src/api.rs", "pub fn target() {}\n"),
        (
            "src/caller.rs",
            "use crate::api::target;\nfn invoke() { target(); target(); }\n",
        ),
    ];
    let head = [
        ("src/lib.rs", "mod api; mod caller;\n"),
        ("src/api.rs", "\npub fn target() { let _ = 1; }\n"),
        (
            "src/caller.rs",
            "use crate::api::target;\nfn invoke() { target(); target(); }\n",
        ),
    ];
    let input = fixture(&base, &head);
    let output = analyze(&input, &|| false);
    let changed = result(&output, "crate::api::target", &input.head_sha);
    assert_eq!(changed.entity.start_line, 2);
    assert_eq!(
        changed.anchor.as_ref().unwrap().snapshot_sha,
        input.head_sha
    );
    assert!(changed.resolved_dependants.unwrap() >= 1);
    let caller = changed
        .dependant_paths
        .iter()
        .find(|p| p.entities.last().unwrap().qualified_name == "crate::caller::invoke")
        .expect("named import resolves to caller");
    assert_eq!(caller.entities.first().unwrap().id, changed.entity.id);
    assert!(
        caller
            .edges
            .iter()
            .all(|e| e.site.snapshot_sha == input.head_sha)
    );
    assert!(
        caller
            .edges
            .iter()
            .any(|e| e.site.path == "src/caller.rs" && e.site.line == 2)
    );
    let shifted = fixture(
        &base,
        &[
            ("src/lib.rs", "mod api; mod caller;\n"),
            ("src/api.rs", "\n\npub fn target() { let _ = 1; }\n"),
            (
                "src/caller.rs",
                "use crate::api::target;\nfn invoke() { target(); target(); }\n",
            ),
        ],
    );
    let shifted_output = analyze(&shifted, &|| false);
    assert_eq!(
        changed.entity.id,
        result(&shifted_output, "crate::api::target", &shifted.head_sha)
            .entity
            .id
    );
}

#[test]
fn shadowed_and_unsupported_sites_never_become_false_caller_edges() {
    let input = fixture(
        &[("src/lib.rs", "fn target() {}\n")],
        &[
            (
                "src/lib.rs",
                "fn target() { let _ = 1; }\nfn local() { let target = || {}; target(); }\nfn parameter(target: fn()) { target(); }\nfn dynamic(x: &Thing) { x.target(); }\nstruct Thing;\n",
            ),
            ("custom/other.rs", "fn external() { target(); }\n"),
            ("src/other.py", "target()\n"),
        ],
    );
    let output = analyze(&input, &|| false);
    let changed = result(&output, "crate::target", &input.head_sha);
    assert!(!output.edges.iter().any(|e| e.to == changed.entity.id
        && (output.entities.iter().any(|x| x.id == e.from
            && (x.qualified_name.ends_with("::local")
                || x.qualified_name.ends_with("::parameter"))))));
    assert!(changed.dependant_paths.is_empty());
    assert!(!changed.complete);
    assert!(!output.coverage.skipped_files.is_empty());
    assert!(!output.coverage.unresolved_sites.is_empty());
}

#[test]
fn deletion_uses_base_anchor_and_never_claims_base_relationships() {
    let input = fixture(
        &[
            ("src/lib.rs", "mod gone;\n"),
            ("src/gone.rs", "pub fn old() {}\n"),
        ],
        &[("src/lib.rs", "\n")],
    );
    let output = analyze(&input, &|| false);
    let deleted = result(&output, "crate::gone::old", &input.base_sha);
    let anchor = deleted.anchor.as_ref().expect("verified deleted row");
    assert_eq!(
        (
            anchor.path.as_str(),
            anchor.line,
            anchor.snapshot_sha.as_str()
        ),
        ("src/gone.rs", 1, input.base_sha.as_str())
    );
    assert_eq!(
        deleted.relationships_unavailable.as_deref(),
        Some("base_graph_not_built")
    );
    assert_eq!(deleted.resolved_dependants, None);
    assert!(deleted.dependant_paths.is_empty());
}

#[test]
fn depth_visit_and_shuffled_blob_order_are_deterministic() {
    let input = fixture(
        &[("src/lib.rs", "fn target() {}\n")],
        &[(
            "src/lib.rs",
            "fn target() { let _ = 1; }\nfn one() { target(); }\nfn two() { one(); }\nfn three() { two(); }\nfn four() { three(); }\nfn five() { four(); }\n",
        )],
    );
    let mut limited = input.clone();
    limited.limits.max_depth = 4;
    let out = analyze(&limited, &|| false);
    let changed = result(&out, "crate::target", &input.head_sha);
    // The depth bound limits paths, not the total number of reachable dependants.
    assert_eq!(changed.dependant_paths.len(), 4);
    assert!(
        !changed
            .dependant_paths
            .iter()
            .any(|p| p.entities.last().unwrap().qualified_name == "crate::five")
    );
    limited.limits.max_visited = 2;
    let capped = analyze(&limited, &|| false);
    let changed = result(&capped, "crate::target", &input.head_sha);
    assert!(!changed.complete);
    assert!(changed.warnings.iter().any(|w| w.contains("visit")));
    let mut shuffled = input.clone();
    shuffled.head_blobs.reverse();
    shuffled.base_blobs.reverse();
    let a = analyze(&input, &|| false);
    let b = analyze(&shuffled, &|| false);
    assert_eq!(
        serde_json::to_value(a).unwrap(),
        serde_json::to_value(b).unwrap()
    );
}

#[test]
fn zero_edge_and_callee_only_changed_entities_still_emit_results() {
    let input = fixture(
        &[("src/lib.rs", "fn leaf() {}\n")],
        &[(
            "src/lib.rs",
            "fn leaf() { let _ = 1; }\nfn caller() { leaf(); }\nfn isolated() {}\n",
        )],
    );
    let output = analyze(&input, &|| false);
    let caller = result(&output, "crate::caller", &input.head_sha);
    assert_eq!(caller.resolved_dependants, Some(0));
    assert!(caller.direct_callees.iter().any(|e| {
        output
            .entities
            .iter()
            .any(|x| x.id == e.to && x.qualified_name == "crate::leaf")
    }));
    let isolated = result(&output, "crate::isolated", &input.head_sha);
    assert_eq!(isolated.resolved_dependants, Some(0));
    assert!(isolated.direct_callees.is_empty());
}

#[test]
fn renamed_module_file_preserves_identity_but_not_base_relationships() {
    let root = "mod api;\n";
    let old = "// Stable module body.\n// Retain enough context for Git rename detection.\npub fn target() { let _ = 0; }\nfn neighbor() {}\n";
    let new = "// Stable module body.\n// Retain enough context for Git rename detection.\npub fn target() { let _ = 1; }\nfn neighbor() {}\n";
    let input = fixture(
        &[("src/lib.rs", root), ("src/api.rs", old)],
        &[("src/lib.rs", root), ("src/api/mod.rs", new)],
    );
    assert!(input.changed_files.iter().any(|f| f.path == "src/api/mod.rs" && f.previous_path.as_deref() == Some("src/api.rs")), "fixture must exercise Git rename parsing");
    let output = analyze(&input, &|| false);
    let head = result(&output, "crate::api::target", &input.head_sha);
    let base = result(&output, "crate::api::target", &input.base_sha);
    assert_eq!(head.entity.id, base.entity.id);
    assert!(
        head.warnings
            .iter()
            .any(|w| w == &format!("validated_rename_from:{}", base.entity.id))
    );
    assert_eq!(head.anchor.as_ref().unwrap().snapshot_sha, input.head_sha);
    assert_eq!(base.anchor.as_ref().unwrap().snapshot_sha, input.base_sha);
    assert_eq!(base.resolved_dependants, None);
    assert_eq!(
        base.relationships_unavailable.as_deref(),
        Some("base_graph_not_built")
    );
}

#[test]
fn collision_is_marked_ambiguous_and_cannot_resolve_a_caller() {
    // Duplicate declarations are parseable syntax, but not a unique definition.
    let input = fixture(
        &[("src/lib.rs", "fn target() {}\n")],
        &[(
            "src/lib.rs",
            "fn target() { let _ = 1; }\nfn target() {}\nfn caller() { target(); }\n",
        )],
    );
    let output = analyze(&input, &|| false);
    let collisions: Vec<_> = output
        .entities
        .iter()
        .filter(|e| e.qualified_name == "crate::target")
        .collect();
    assert_eq!(collisions.len(), 2);
    assert!(collisions.iter().all(|e| e.ambiguous));
    assert!(
        !output
            .edges
            .iter()
            .any(|e| collisions.iter().any(|c| c.id == e.to))
    );
    assert!(!output.coverage.complete);
}

#[test]
fn explicit_custom_layout_and_other_languages_never_claim_complete_coverage() {
    let manifest =
        "[package]\nname = \"custom\"\nversion = \"0.1.0\"\n[lib]\npath = \"custom/lib.rs\"\n";
    let input = fixture(
        &[
            ("Cargo.toml", manifest),
            ("custom/lib.rs", "pub fn target() {}\n"),
        ],
        &[
            ("Cargo.toml", manifest),
            ("custom/lib.rs", "pub fn target() { let _ = 1; }\n"),
            ("src/other.py", "target()\n"),
        ],
    );
    let output = analyze(&input, &|| false);
    assert!(!output.coverage.complete);
    assert!(
        output
            .coverage
            .skipped_files
            .iter()
            .any(|reason| reason.contains("custom/lib.rs"))
    );
    assert!(
        output
            .coverage
            .skipped_files
            .iter()
            .any(|reason| reason.contains("other.py"))
    );
    assert!(
        !output
            .edges
            .iter()
            .any(|e| e.site.path == "custom/lib.rs" || e.site.path == "src/other.py")
    );
}
