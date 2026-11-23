//! Deliberately narrow, pinned Rust call graph. No checkout source is read.
use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tree_sitter::{Node, ParseOptions, Parser};

const MAX_FILES: usize = 128;
const MAX_BYTES: usize = 512 * 1024;
const MAX_NODES: usize = 100_000;
const MAX_EDGES: usize = 2_000;
const MAX_HOPS: usize = 16;
const BUDGET: Duration = Duration::from_secs(8);

#[derive(Clone)]
struct Site {
    path: String,
    line: usize,
}
impl Site {
    fn value(&self) -> Value {
        json!({"path":self.path,"line":self.line})
    }
}
struct Alias {
    target: String,
    site: Site,
}
struct Call {
    from: String,
    target: String,
    site: Site,
}
#[derive(Default)]
struct Graph {
    defs: BTreeMap<String, Site>,
    aliases: BTreeMap<String, Alias>,
    calls: Vec<Call>,
    modules: BTreeMap<String, String>,
    declarations: BTreeMap<String, usize>,
    reasons: BTreeSet<String>,
    files: usize,
    bytes: usize,
    nodes: usize,
}
fn text<'a>(node: Node<'_>, bytes: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(bytes.get(node.byte_range())?).ok()
}
fn site(path: &str, node: Node<'_>) -> Site {
    Site {
        path: path.to_owned(),
        line: node.start_position().row + 1,
    }
}
fn qualified(s: &str) -> bool {
    let mut parts = s.split("::");
    parts.next() == Some("crate")
        && parts.clone().count() > 0
        && parts.all(|p| {
            !p.is_empty()
                && p.chars().enumerate().all(|(i, c)| {
                    if i == 0 {
                        c == '_' || c.is_ascii_alphabetic()
                    } else {
                        c == '_' || c.is_ascii_alphanumeric()
                    }
                })
        })
}
fn children(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    (0..node.named_child_count()).filter_map(move |i| node.named_child(i as u32))
}
fn is_public(node: Node<'_>, bytes: &[u8]) -> bool {
    children(node).any(|n| n.kind() == "visibility_modifier" && text(n, bytes) == Some("pub"))
}
fn has_attributes(node: Node<'_>) -> bool {
    children(node).any(|n| n.kind().contains("attribute"))
}
fn insert_unique<T>(
    map: &mut BTreeMap<String, T>,
    key: String,
    value: T,
    reasons: &mut BTreeSet<String>,
) {
    if map.insert(key.clone(), value).is_some() {
        reasons.insert(format!("duplicate declaration: {key}"));
    }
}
fn scan_calls(
    node: Node<'_>,
    bytes: &[u8],
    path: &str,
    from: &str,
    graph: &mut Graph,
    deadline: Instant,
) {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if Instant::now() > deadline || graph.nodes >= MAX_NODES {
            graph.reasons.insert("node/time limit".into());
            return;
        }
        graph.nodes += 1;
        if n != node
            && matches!(
                n.kind(),
                "function_item" | "closure_expression" | "macro_invocation"
            )
        {
            graph.reasons.insert(format!(
                "nested function, closure or macro: {path}:{}",
                n.start_position().row + 1
            ));
            continue;
        }
        if n.kind().contains("attribute") {
            graph.reasons.insert(format!(
                "attributed syntax: {path}:{}",
                n.start_position().row + 1
            ));
            continue;
        }
        if n.kind() == "call_expression"
            && let Some(f) = n.child_by_field_name("function")
        {
            if f.kind() == "scoped_identifier" {
                if let Some(target) = text(f, bytes).filter(|s| qualified(s)) {
                    if graph.calls.len() < MAX_EDGES {
                        graph.calls.push(Call {
                            from: from.to_owned(),
                            target: target.to_owned(),
                            site: site(path, n),
                        });
                    } else {
                        graph.reasons.insert("edge limit".into());
                        return;
                    }
                } else {
                    graph.reasons.insert(format!(
                        "unsupported call: {path}:{}",
                        n.start_position().row + 1
                    ));
                }
            } else if text(f, bytes).is_some_and(|s| s.contains("crate::")) {
                graph.reasons.insert(format!(
                    "unsupported qualified call: {path}:{}",
                    n.start_position().row + 1
                ));
            }
        }
        for child in children(n) {
            stack.push(child);
        }
    }
}
fn scan_module(
    git: &super::PinnedGit,
    paths: &BTreeSet<String>,
    path: &str,
    module: &str,
    graph: &mut Graph,
    deadline: Instant,
) {
    if Instant::now() > deadline || graph.files >= MAX_FILES || graph.bytes >= MAX_BYTES {
        graph.reasons.insert("file/byte/time limit".into());
        return;
    }
    if graph.modules.insert(module.into(), path.into()).is_some() {
        graph.reasons.insert(format!("module repeated: {module}"));
        return;
    }
    let bytes = match git.blob(&git.head, path, MAX_BYTES - graph.bytes) {
        Ok(bytes) => bytes,
        Err(_) => {
            graph.reasons.insert(format!("blob unavailable: {path}"));
            return;
        }
    };
    graph.files += 1;
    graph.bytes += bytes.len();
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .is_err()
    {
        graph.reasons.insert("Rust parser unavailable".into());
        return;
    }
    // Interrupt parsing itself, not only the AST walk. A bounded input chunk
    // avoids an unbounded callback copy while the parser is still running.
    let mut read = |offset: usize, _| {
        bytes
            .get(offset..bytes.len().min(offset.saturating_add(4096)))
            .unwrap_or(&[])
    };
    let mut progress = |_: &tree_sitter::ParseState| {
        if Instant::now() >= deadline {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = ParseOptions::default().progress_callback(&mut progress);
    let Some(tree) = parser.parse_with_options(&mut read, None, Some(options)) else {
        graph.reasons.insert(format!("parse failed: {path}"));
        return;
    };
    let root = tree.root_node();
    if root.has_error() || std::str::from_utf8(&bytes).is_err() {
        graph.reasons.insert(format!("invalid Rust source: {path}"));
        return;
    }
    let mut next = Vec::new();
    let mut attributed = false;
    for item in children(root) {
        if Instant::now() > deadline || graph.nodes >= MAX_NODES {
            graph.reasons.insert("node/time limit".into());
            break;
        }
        graph.nodes += 1;
        if item.kind().contains("attribute") {
            attributed = true;
            graph.reasons.insert(format!(
                "attributed syntax: {path}:{}",
                item.start_position().row + 1
            ));
            continue;
        }
        if attributed || has_attributes(item) {
            attributed = false;
            graph.reasons.insert(format!(
                "conditional/attributed syntax: {path}:{}",
                item.start_position().row + 1
            ));
            continue;
        }
        // Track all top-level names, not just public functions and reexports.
        // A private import or a unit/tuple struct can occupy the same value name.
        if let Some(name) = item
            .child_by_field_name("name")
            .and_then(|n| text(n, &bytes))
        {
            let key = format!("{module}::{name}");
            *graph.declarations.entry(key).or_default() += 1;
        }
        match item.kind() {
            "mod_item" => {
                let Some(name) = item
                    .child_by_field_name("name")
                    .and_then(|n| text(n, &bytes))
                else {
                    graph.reasons.insert(format!("module name unknown: {path}"));
                    continue;
                };
                if !is_public(item, &bytes) || item.child_by_field_name("body").is_some() {
                    graph.reasons.insert(format!(
                        "private or inline module: {path}:{}",
                        item.start_position().row + 1
                    ));
                    continue;
                }
                let base = if path.ends_with("/mod.rs") {
                    path.trim_end_matches("mod.rs").to_owned()
                } else if path == "src/lib.rs" || path == "src/main.rs" {
                    "src/".into()
                } else {
                    format!("{}/", path.trim_end_matches(".rs"))
                };
                let flat = format!("{base}{name}.rs");
                let dir = format!("{base}{name}/mod.rs");
                match (paths.contains(&flat), paths.contains(&dir)) {
                    (true, false) => next.push((flat, format!("{module}::{name}"))),
                    (false, true) => next.push((dir, format!("{module}::{name}"))),
                    _ => {
                        graph
                            .reasons
                            .insert(format!("missing/ambiguous module: {module}::{name}"));
                    }
                }
            }
            "function_item" => {
                if !is_public(item, &bytes) {
                    continue;
                }
                if let Some(name_node) = item.child_by_field_name("name")
                    && let Some(name) = text(name_node, &bytes)
                {
                    let key = format!("{module}::{name}");
                    insert_unique(
                        &mut graph.defs,
                        key.clone(),
                        site(path, item),
                        &mut graph.reasons,
                    );
                    scan_calls(item, &bytes, path, &key, graph, deadline);
                }
            }
            "use_declaration" => {
                if !is_public(item, &bytes) {
                    graph.reasons.insert(format!(
                        "private import: {path}:{}",
                        item.start_position().row + 1
                    ));
                    continue;
                }
                // Accept precisely one explicit absolute named import, optionally renamed.
                let Some(raw) = text(item, &bytes) else {
                    continue;
                };
                let Some(body) = raw
                    .strip_prefix("pub use ")
                    .and_then(|s| s.strip_suffix(';'))
                else {
                    graph.reasons.insert(format!(
                        "unsupported reexport: {path}:{}",
                        item.start_position().row + 1
                    ));
                    continue;
                };
                let (target, name) = match body.split_once(" as ") {
                    Some((t, n)) => (t, n),
                    None => (body, body.rsplit("::").next().unwrap_or("")),
                };
                if !qualified(target)
                    || !name.chars().enumerate().all(|(i, c)| {
                        if i == 0 {
                            c == '_' || c.is_ascii_alphabetic()
                        } else {
                            c == '_' || c.is_ascii_alphanumeric()
                        }
                    })
                {
                    graph.reasons.insert(format!(
                        "unsupported reexport: {path}:{}",
                        item.start_position().row + 1
                    ));
                    continue;
                }
                insert_unique(
                    &mut graph.aliases,
                    format!("{module}::{name}"),
                    Alias {
                        target: target.into(),
                        site: site(path, item),
                    },
                    &mut graph.reasons,
                );
            }
            "struct_item" | "enum_item" | "union_item" | "type_item" | "const_item"
            | "static_item" | "macro_invocation" | "macro_definition" | "impl_item"
            | "trait_item" => {
                graph.reasons.insert(format!(
                    "unresolved generated/associated code: {path}:{}",
                    item.start_position().row + 1
                ));
            }
            _ => {}
        }
    }
    if attributed {
        graph.reasons.insert(format!("orphan attribute: {path}"));
    }
    for (child, name) in next {
        scan_module(git, paths, &child, &name, graph, deadline);
    }
}
fn resolve(target: &str, graph: &Graph) -> Result<(String, Vec<Value>), &'static str> {
    let mut current = target.to_owned();
    let mut sites = Vec::new();
    let mut seen = BTreeSet::new();
    for _ in 0..MAX_HOPS {
        if !seen.insert(current.clone()) {
            return Err("reexport cycle");
        }
        if graph.defs.contains_key(&current) {
            return Ok((current, sites));
        }
        if let Some(alias) = graph.aliases.get(&current) {
            sites.push(alias.site.value());
            current = alias.target.clone();
            continue;
        }
        // A module itself can be reexported, e.g. crate::api::run -> crate::inner::run.
        // Probe only prefixes of this path, not every alias in the corpus.
        let (alias, suffix) = current
            .match_indices("::")
            .find_map(|(i, _)| {
                graph
                    .aliases
                    .get(&current[..i])
                    .map(|a| (a, &current[i + 2..]))
            })
            .ok_or("unresolved target")?;
        let mut module_target = alias.target.as_str();
        let mut module_hops = 0;
        while let Some(next) = graph.aliases.get(module_target) {
            module_hops += 1;
            if module_hops >= MAX_HOPS {
                return Err("module reexport cycle or hop limit");
            }
            module_target = &next.target;
        }
        if !graph.modules.contains_key(module_target) {
            return Err("reexport prefix does not establish a module");
        }
        sites.push(alias.site.value());
        current = format!("{}::{suffix}", alias.target);
    }
    Err("reexport hop limit")
}

pub(super) fn run(git: &super::PinnedGit) -> super::SourceResult {
    let deadline = Instant::now() + BUDGET;
    let paths = git
        .tree(&git.head)
        .map_err(|_| super::SourceError::new("unknown", "pinned_tree_unavailable"))?;
    let paths: BTreeSet<_> = paths.into_iter().collect();
    let root = match (paths.contains("src/lib.rs"), paths.contains("src/main.rs")) {
        (true, false) => "src/lib.rs",
        (false, true) => "src/main.rs",
        _ => {
            return Err(super::SourceError::new(
                "unknown",
                "ambiguous_or_missing_rust_root",
            ));
        }
    };
    let manifest = git
        .blob(&git.head, "Cargo.toml", MAX_BYTES)
        .ok()
        .and_then(|raw| String::from_utf8(raw).ok())
        .and_then(|raw| raw.parse::<toml::Table>().ok());
    let valid_manifest = manifest.as_ref().is_some_and(|table| {
        table.contains_key("package")
            && !table.contains_key("lib")
            && !table.contains_key("bin")
            && !table.contains_key("workspace")
            && !table.contains_key("features")
            && !table.contains_key("target")
            && table["package"].as_table().is_some_and(|p| {
                !p.contains_key("build")
                    && !p.contains_key("autobins")
                    && !p.contains_key("autolib")
            })
            && !paths.contains("build.rs")
            && !paths
                .iter()
                .any(|p| p.starts_with(".cargo/") || p.starts_with("src/bin/"))
    });
    if !valid_manifest {
        return Err(super::SourceError::new(
            "unknown",
            "unsupported_or_missing_pinned_manifest",
        ));
    }
    let mut graph = Graph::default();
    scan_module(git, &paths, root, "crate", &mut graph, deadline);
    let mut results = Vec::new();
    for name in graph.defs.keys().chain(graph.aliases.keys()) {
        if graph.declarations.get(name).is_some_and(|count| *count > 1) {
            graph
                .reasons
                .insert(format!("shadowed declaration: {name}"));
        }
    }
    let incomplete = !graph.reasons.is_empty();
    for call in &graph.calls {
        if Instant::now() >= deadline {
            graph.reasons.insert("time limit".into());
            results.push(json!({"state":"unknown","kind":"CALL","from":call.from,"target":call.target,"call":call.site.value(),"reason":"time limit"}));
            continue;
        }
        match resolve(&call.target, &graph) {
            Ok((_, via)) if via.is_empty() && !incomplete => {},
            Ok((target, via)) if !incomplete => results.push(json!({"state":"known","kind":"CALL","from":call.from,"target":target,"call":call.site.value(),"via":via,"definition":graph.defs[&target].value()})),
            Ok((target, via)) => results.push(json!({"state":"unknown","kind":"CALL","from":call.from,"target":target,"call":call.site.value(),"via":via,"definition":graph.defs[&target].value(),"reason":"incomplete graph"})),
            Err(reason) => results.push(json!({"state":"unknown","kind":"CALL","from":call.from,"target":call.target,"call":call.site.value(),"reason":reason})),
        }
    }
    let partial = !graph.reasons.is_empty() || results.iter().any(|v| v["state"] == "unknown");
    Ok(json!({
        "state": if partial {"partial"} else {"known"},
        "coverage":{"language":"rust","scope":"reachable conventional Rust src/lib.rs or src/main.rs module paths only (not repository-wide); public external modules/functions; explicit absolute single-item pub use; fully qualified crate calls","files_scanned":graph.files,"bytes_scanned":graph.bytes,"nodes_visited":graph.nodes},
        "reasons":graph.reasons,"limits":{"max_files":MAX_FILES,"max_bytes":MAX_BYTES,"max_nodes":MAX_NODES,"max_edges":MAX_EDGES,"max_reexport_hops":MAX_HOPS,"deadline_ms":BUDGET.as_millis()},
        "provenance":{"head":git.head,"source":"pinned_git_blobs","parser":"tree-sitter-rust"},
        "results":results
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pinned_reexport_chain_and_unsupported_layout() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let git_cmd = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        git_cmd(&["init", "-q"]);
        git_cmd(&["config", "user.name", "Fixture"]);
        git_cmd(&["config", "user.email", "fixture@example.com"]);
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub mod inner;\npub use crate::inner::service as bridge;\npub use crate::bridge as api;\npub fn caller() { crate::api::run(); crate::inner::service::run(); }\n").unwrap();
        std::fs::write(root.join("src/inner.rs"), "pub mod service;\n").unwrap();
        std::fs::create_dir(root.join("src/inner")).unwrap();
        std::fs::write(root.join("src/inner/service.rs"), "pub fn run() {}\n").unwrap();
        git_cmd(&["add", "."]);
        git_cmd(&["commit", "-qm", "fixture"]);
        let sha = git_cmd(&["rev-parse", "HEAD"]);
        let pinned = super::super::PinnedGit::new(root.join(".git"), &sha, &sha, true);
        // The worktree must not influence object-backed evidence.
        std::fs::write(root.join("src/inner/service.rs"), "pub fn decoy() {}\n").unwrap();
        let report = run(&pinned).unwrap();
        assert_eq!(report["state"], "known", "{report}");
        assert_eq!(report["results"][0]["target"], "crate::inner::service::run");
        assert_eq!(report["results"].as_array().unwrap().len(), 1);
        assert_eq!(report["results"][0]["via"].as_array().unwrap().len(), 2);
        assert_eq!(
            report["results"][0]["definition"]["path"],
            "src/inner/service.rs"
        );
        std::fs::write(
            root.join("src/lib.rs"),
            "pub mod inner;\npub mod missing;\npub fn caller() { crate::inner::service::run(); }\n",
        )
        .unwrap();
        git_cmd(&["add", "."]);
        git_cmd(&["commit", "-qm", "missing module"]);
        let second = git_cmd(&["rev-parse", "HEAD"]);
        let pinned = super::super::PinnedGit::new(root.join(".git"), &sha, &second, true);
        let report = run(&pinned).unwrap();
        assert_eq!(report["state"], "partial");
        assert_eq!(report["results"][0]["state"], "unknown");
    }

    #[test]
    fn qualification_is_strict() {
        assert!(qualified("crate::api::run"));
        assert!(!qualified("api::run"));
        assert!(!qualified("crate::api::{run}"));
    }
    #[test]
    fn alias_chain_keeps_intermediate_sites_and_rejects_cycle() {
        let mut graph = Graph::default();
        let at = Site {
            path: "src/lib.rs".into(),
            line: 1,
        };
        graph.defs.insert("crate::inner::run".into(), at.clone());
        graph
            .modules
            .insert("crate::inner".into(), "src/inner.rs".into());
        graph.aliases.insert(
            "crate::api".into(),
            Alias {
                target: "crate::bridge".into(),
                site: at.clone(),
            },
        );
        graph.aliases.insert(
            "crate::bridge".into(),
            Alias {
                target: "crate::inner".into(),
                site: at.clone(),
            },
        );
        let (target, via) = resolve("crate::api::run", &graph).unwrap();
        assert_eq!(target, "crate::inner::run");
        assert_eq!(via.len(), 2);
        graph.aliases.insert(
            "crate::bridge".into(),
            Alias {
                target: "crate::api".into(),
                site: at,
            },
        );
        assert!(resolve("crate::api::run", &graph).is_err());
    }

    #[test]
    fn limits_and_function_prefix_fail_closed() {
        let mut graph = Graph::default();
        let at = Site {
            path: "src/lib.rs".into(),
            line: 1,
        };
        graph.defs.insert("crate::function".into(), at.clone());
        graph.defs.insert("crate::target".into(), at.clone());
        graph.aliases.insert(
            "crate::api".into(),
            Alias {
                target: "crate::function".into(),
                site: at.clone(),
            },
        );
        assert!(resolve("crate::api::target", &graph).is_err());
        for i in 0..MAX_HOPS {
            graph.aliases.insert(
                format!("crate::hop{i}"),
                Alias {
                    target: format!("crate::hop{}", i + 1),
                    site: at.clone(),
                },
            );
        }
        assert_eq!(
            resolve("crate::hop0", &graph).err(),
            Some("reexport hop limit")
        );
    }

    #[test]
    fn pinned_manifest_attributes_and_collisions_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let git_cmd = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        git_cmd(&["init", "-q"]);
        git_cmd(&["config", "user.name", "Fixture"]);
        git_cmd(&["config", "user.email", "fixture@example.com"]);
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub use crate::api as alias;\npub fn api() {}\npub fn caller() { #[cfg(any())] { crate::alias(); } }\n").unwrap();
        git_cmd(&["add", "."]);
        git_cmd(&["commit", "-qm", "attributed"]);
        let first = git_cmd(&["rev-parse", "HEAD"]);
        let pinned = super::super::PinnedGit::new(root.join(".git"), &first, &first, true);
        let report = run(&pinned).unwrap();
        assert_eq!(report["state"], "partial", "{report}");
        assert_eq!(report["results"][0]["state"], "unknown");

        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[lib]\npath = \"actual.rs\"\n").unwrap();
        std::fs::write(root.join("actual.rs"), "pub fn api() {}\n").unwrap();
        git_cmd(&["add", "."]);
        git_cmd(&["commit", "-qm", "custom root"]);
        let second = git_cmd(&["rev-parse", "HEAD"]);
        let pinned = super::super::PinnedGit::new(root.join(".git"), &first, &second, true);
        assert!(run(&pinned).is_err());

        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub use crate::api as alias;\npub fn api() {}\nuse crate::api as alias;\npub fn caller() { crate::alias(); }\n").unwrap();
        git_cmd(&["add", "."]);
        git_cmd(&["commit", "-qm", "collision"]);
        let third = git_cmd(&["rev-parse", "HEAD"]);
        let pinned = super::super::PinnedGit::new(root.join(".git"), &first, &third, true);
        let report = run(&pinned).unwrap();
        assert_eq!(report["results"][0]["state"], "unknown", "{report}");
    }
}
