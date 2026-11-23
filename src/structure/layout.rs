//! Conservative conventional Cargo target and module discovery from pinned blobs.
use crate::model::structure::Blob;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;
use tree_sitter::{Node, ParseOptions, Parser};

pub(super) struct Layout {
    pub modules: BTreeMap<String, String>,
    pub skipped: Vec<String>,
}
fn declarations(node: Node<'_>, bytes: &[u8], prefix: &str, output: &mut Vec<String>) {
    if node.kind() == "mod_item" {
        let mut previous = node.prev_named_sibling();
        while let Some(attr) = previous.filter(|p| p.kind() == "attribute_item") {
            if attr
                .utf8_text(bytes)
                .is_ok_and(|s| s.contains("cfg") || s.contains("path"))
            {
                return;
            }
            previous = attr.prev_named_sibling();
        }
    }
    if node.kind() == "mod_item"
        && let Some(name) = node.child_by_field_name("name")
        && let Ok(s) = name.utf8_text(bytes)
    {
        let qualified = if prefix.is_empty() {
            s.to_owned()
        } else {
            format!("{prefix}/{s}")
        };
        if let Some(body) = node.child_by_field_name("body") {
            declarations(body, bytes, &qualified, output);
        } else {
            output.push(qualified);
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        declarations(child, bytes, prefix, output);
    }
}

pub(super) fn discover(blobs: &[Blob], max_blob_bytes: usize, abort: &impl Fn() -> bool) -> Layout {
    let mut result = Layout {
        modules: BTreeMap::new(),
        skipped: Vec::new(),
    };
    let Some(manifest) = blobs.iter().find(|b| b.path == "Cargo.toml") else {
        result
            .skipped
            .push("Cargo.toml missing from pinned tree".into());
        return result;
    };
    let Ok(raw) = std::str::from_utf8(&manifest.bytes) else {
        result.skipped.push("Cargo.toml not UTF-8".into());
        return result;
    };
    let Ok(config) = raw.parse::<toml::Table>() else {
        result.skipped.push("Cargo.toml invalid".into());
        return result;
    };
    if config.get("package").is_none()
        || config.get("workspace").is_some()
        || config.get("build").is_some()
    {
        result
            .skipped
            .push("workspace/build-script or missing package unsupported".into());
        return result;
    }
    if ["lib", "bin", "example", "test", "bench"]
        .iter()
        .any(|k| config.get(*k).is_some())
    {
        result
            .skipped
            .push("explicit Cargo target configuration unsupported".into());
        return result;
    }
    let Some(package) = config.get("package").and_then(toml::Value::as_table) else {
        result.skipped.push("Cargo package table invalid".into());
        return result;
    };
    let auto = |key: &str| -> Option<bool> {
        match package.get(key) {
            None => Some(true),
            Some(value) => value.as_bool(),
        }
    };
    let (Some(autolib), Some(autobins)) = (auto("autolib"), auto("autobins")) else {
        result
            .skipped
            .push("invalid Cargo automatic target flags".into());
        return result;
    };
    let files: BTreeMap<_, _> = blobs
        .iter()
        .filter(|b| b.path.ends_with(".rs"))
        .map(|b| (b.path.as_str(), b.bytes.as_slice()))
        .collect();
    let mut roots: Vec<(String, String)> = Vec::new();
    if autolib && files.contains_key("src/lib.rs") {
        roots.push(("src/lib.rs".into(), "src/lib.rs".into()));
    }
    if autobins {
        if files.contains_key("src/main.rs") {
            roots.push(("src/main.rs".into(), "src/main.rs".into()));
        }
        for path in files.keys() {
            if let Some(name) = path
                .strip_prefix("src/bin/")
                .and_then(|x| x.strip_suffix(".rs"))
                && !name.contains('/')
            {
                roots.push(((*path).into(), (*path).into()));
            }
        }
    }
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .is_err()
    {
        result.skipped.push("Rust parser unavailable".into());
        return result;
    }
    let mut claims = BTreeMap::<String, BTreeSet<String>>::new();
    for (root, path) in roots {
        let root_scope = if root == "src/lib.rs" {
            "crate".to_owned()
        } else if root == "src/main.rs" {
            "main".to_owned()
        } else {
            format!(
                "bin:{}",
                root.trim_start_matches("src/bin/").trim_end_matches(".rs")
            )
        };
        let mut queue = vec![(path, root_scope, root.clone())];
        let mut seen = BTreeSet::new();
        while let Some((path, module, target)) = queue.pop() {
            if !seen.insert(path.clone()) {
                continue;
            }
            let Some(bytes) = files.get(path.as_str()) else {
                result.skipped.push(format!("{path}: missing module blob"));
                continue;
            };
            claims
                .entry(path.clone())
                .or_default()
                .insert(target.clone());
            if abort() || bytes.len() > max_blob_bytes {
                result
                    .skipped
                    .push(format!("{path}: layout parse budget or size cap"));
                continue;
            }
            let mut progress = |_: &tree_sitter::ParseState| {
                if abort() {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            };
            let Some(tree) = parser.parse_with_options(
                &mut |offset, _| &bytes[offset..],
                None,
                Some(ParseOptions::new().progress_callback(&mut progress)),
            ) else {
                result.skipped.push(format!("{path}: parse failed"));
                continue;
            };
            if tree.root_node().has_error() {
                result.skipped.push(format!("{path}: syntax error"));
                continue;
            }
            let mut names = Vec::new();
            declarations(tree.root_node(), bytes, "", &mut names);
            let parent = if path.ends_with("/mod.rs") {
                path.trim_end_matches("mod.rs")
                    .trim_end_matches('/')
                    .to_owned()
            } else if path == root && path.starts_with("src/bin/") {
                // A flat bin root's modules live beside the root (src/bin/helper.rs),
                // not under a directory named after the binary.
                "src/bin".into()
            } else if path == root {
                "src".into()
            } else {
                path.trim_end_matches(".rs").to_owned()
            };
            for name in names {
                let normal = format!("{parent}/{name}.rs");
                let folder = format!("{parent}/{name}/mod.rs");
                if files.contains_key(normal.as_str()) && files.contains_key(folder.as_str()) {
                    result
                        .skipped
                        .push(format!("{module}::{name}: ambiguous module layout"));
                    continue;
                }
                let child = if files.contains_key(normal.as_str()) {
                    normal
                } else {
                    folder
                };
                queue.push((
                    child,
                    format!("{module}::{}", name.replace('/', "::")),
                    target.clone(),
                ));
            }
            if claims.get(&path).is_some_and(|owners| owners.len() == 1)
                || (path.starts_with("src/bin/") && target != path)
            {
                result.modules.insert(path, module);
            }
        }
    }
    for (path, owners) in claims {
        if owners.len() > 1 {
            // A sibling module file is also an automatic binary root. Keep its
            // module identity for useful edges, but never claim full coverage.
            if !(path.starts_with("src/bin/") && owners.contains(&path)) {
                result.modules.remove(&path);
            }
            result
                .skipped
                .push(format!("{path}: shared across targets"));
        }
    }
    for path in files.keys() {
        if !result.modules.contains_key(*path) {
            result
                .skipped
                .push(format!("{path}: unlinked/ambiguous source"));
        }
    }
    result.skipped.sort();
    result.skipped.dedup();
    result
}
