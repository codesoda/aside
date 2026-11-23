//! Conservative pinned-object Rust structural inventory and reverse dependencies.
#[cfg(test)]
mod acceptance_tests;
#[cfg(test)]
mod contract_tests;
#[cfg(test)]
mod extra_tests;
mod layout;
#[cfg(test)]
mod tests;
use crate::model::structure::{
    Blob, CorpusCoverage, Edge, Entity, EntityPath, EntityResult, Site, StructureInput,
    StructureOutput,
};
use crate::model::{ChangeKind, LineKind};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::{Duration, Instant},
};
use tree_sitter::{Node, Parser};

fn line(n: Node<'_>) -> u32 {
    n.start_position().row as u32 + 1
}
fn text<'a>(n: Node<'_>, bytes: &'a [u8]) -> &'a str {
    n.utf8_text(bytes).unwrap_or("")
}
fn scope(path: &str) -> Option<String> {
    if path == "src/lib.rs" {
        return Some("crate".into());
    }
    if path == "src/main.rs" {
        return Some("main".into());
    }
    if let Some(name) = path
        .strip_prefix("src/bin/")
        .and_then(|s| s.strip_suffix(".rs"))
        && !name.contains('/')
    {
        return Some(format!("bin:{name}"));
    }
    if let Some(rest) = path
        .strip_prefix("src/")
        .and_then(|s| s.strip_suffix(".rs"))
    {
        if rest == "mod" || rest.starts_with("bin/") {
            return None;
        }
        let name = rest.strip_suffix("/mod").unwrap_or(rest).replace('/', "::");
        return Some(format!("crate::{name}"));
    }
    None
}
fn direct_imports(raw: &str) -> Option<Vec<(String, String)>> {
    if raw.contains('*') || raw.starts_with("pub ") {
        return None;
    }
    let mut items = Vec::new();
    if let Some(open) = raw.find('{') {
        let prefix = raw[..open].trim_end_matches("::");
        let inner = raw[open + 1..].strip_suffix('}')?;
        let mut depth = 0usize;
        let mut start = 0;
        for (index, ch) in inner
            .char_indices()
            .chain(std::iter::once((inner.len(), ',')))
        {
            match ch {
                '{' => depth += 1,
                '}' => depth = depth.checked_sub(1)?,
                ',' if depth == 0 => {
                    let part = inner[start..index].trim();
                    if !part.is_empty() {
                        items.extend(direct_imports(&format!("{prefix}::{part}"))?);
                    }
                    start = index + 1;
                }
                _ => {}
            }
        }
        if depth != 0 {
            return None;
        }
    } else {
        let (path, alias) = raw
            .split_once(" as ")
            .unwrap_or((raw, raw.rsplit("::").next()?));
        if path.is_empty() || alias.is_empty() || path.ends_with("::self") {
            return None;
        }
        items.push((alias.into(), path.into()));
    }
    Some(items)
}
fn conditional(n: Node<'_>, bytes: &[u8]) -> bool {
    let mut current = Some(n);
    while let Some(node) = current {
        let mut previous = node.prev_named_sibling();
        while let Some(attribute) = previous.filter(|p| p.kind() == "attribute_item") {
            if text(attribute, bytes).contains("cfg(")
                || text(attribute, bytes).contains("cfg_attr(")
            {
                return true;
            }
            previous = attribute.prev_named_sibling();
        }
        current = node.parent();
    }
    false
}
fn normalize_path(module: &str, path: &str) -> Option<String> {
    if let Some(rest) = path.strip_prefix("crate::") {
        let root = module.split("::").next()?;
        return Some(format!("{root}::{rest}"));
    }
    if let Some(rest) = path.strip_prefix("self::") {
        return Some(format!("{module}::{rest}"));
    }
    let mut parent = module;
    let mut rest = path;
    while let Some(next) = rest.strip_prefix("super::") {
        parent = parent.rsplit_once("::")?.0;
        rest = next;
    }
    Some(if rest == path {
        format!("{module}::{path}")
    } else {
        format!("{parent}::{rest}")
    })
}
fn id(repo: &str, kind: &str, name: &str) -> String {
    format!(
        "entity:{}",
        hex::encode(Sha256::digest(format!("{repo}\0{kind}\0{name}").as_bytes()))
    )
}
fn names_pattern(n: Node<'_>, bytes: &[u8], name: &str) -> bool {
    if matches!(n.kind(), "identifier" | "type_identifier") && text(n, bytes) == name {
        return true;
    }
    let mut cursor = n.walk();
    n.named_children(&mut cursor)
        .any(|child| names_pattern(child, bytes, name))
}
fn condition_shadows(condition: Node<'_>, n: Node<'_>, bytes: &[u8], name: &str) -> bool {
    let mut cursor = condition.walk();
    let conditions: Vec<_> = if condition.kind() == "let_chain" {
        condition.named_children(&mut cursor).collect()
    } else {
        vec![condition]
    };
    conditions.into_iter().any(|part| {
        part.kind() == "let_condition"
            && (n.start_byte() >= condition.end_byte() || n.start_byte() >= part.end_byte())
            && part
                .child_by_field_name("pattern")
                .is_some_and(|p| names_pattern(p, bytes, name))
    })
}
fn shadowed(n: Node<'_>, bytes: &[u8], name: &str) -> bool {
    if name.contains("::") || name.is_empty() {
        return false;
    }
    let mut parent = n.parent();
    while let Some(node) = parent {
        if matches!(node.kind(), "for_expression" | "match_arm")
            && node
                .child_by_field_name("pattern")
                .is_some_and(|p| names_pattern(p, bytes, name))
        {
            return true;
        }
        if matches!(node.kind(), "if_expression" | "while_expression")
            && node
                .child_by_field_name("condition")
                .is_some_and(|condition| {
                    // An if-let binding does not escape into the else arm. A binding
                    // in a chain is available only after its own value expression.
                    let in_scope = if node.kind() == "if_expression" {
                        node.child_by_field_name("alternative")
                            .is_none_or(|alternative| n.start_byte() < alternative.start_byte())
                    } else {
                        true
                    };
                    in_scope && condition_shadows(condition, n, bytes, name)
                })
        {
            return true;
        }
        if matches!(
            node.kind(),
            "function_item"
                | "closure_expression"
                | "impl_item"
                | "struct_item"
                | "enum_item"
                | "union_item"
                | "type_item"
                | "trait_item"
        ) && node
            .child_by_field_name("type_parameters")
            .is_some_and(|p| names_pattern(p, bytes, name))
        {
            return true;
        }
        if matches!(
            node.kind(),
            "function_item" | "closure_expression" | "impl_item"
        ) && let Some(parameters) = node.child_by_field_name("parameters")
        {
            let mut cursor = parameters.walk();
            if parameters.named_children(&mut cursor).any(|parameter| {
                if parameter.kind() == "parameter" {
                    parameter
                        .child_by_field_name("pattern")
                        .is_some_and(|p| names_pattern(p, bytes, name))
                } else {
                    node.kind() == "closure_expression" && names_pattern(parameter, bytes, name)
                }
            }) {
                return true;
            }
        }
        if node.kind() == "block" {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.start_byte() >= n.start_byte() {
                    break;
                }
                if child.kind() == "let_declaration"
                    && child
                        .child_by_field_name("pattern")
                        .is_some_and(|p| names_pattern(p, bytes, name))
                {
                    return true;
                }
            }
        }
        parent = node.parent();
    }
    false
}
fn unsupported_lexical_import(n: Node<'_>, bytes: &[u8], name: &str) -> bool {
    let binding = name.split("::").next().unwrap_or(name);
    if matches!(binding, "crate" | "self" | "super") {
        return false;
    }
    let mut current = n;
    while let Some(parent) = current.parent() {
        if matches!(parent.kind(), "block" | "source_file" | "declaration_list") {
            let mut cursor = parent.walk();
            for sibling in parent.named_children(&mut cursor) {
                if sibling.kind() != "use_declaration" {
                    continue;
                }
                let raw = text(sibling, bytes)
                    .trim_start_matches("use ")
                    .trim_end_matches(';')
                    .trim();
                if raw.contains('*')
                    || (parent.kind() == "block"
                        && direct_imports(raw)
                            .is_none_or(|items| items.iter().any(|(alias, _)| alias == binding)))
                {
                    return true;
                }
            }
        }
        current = parent;
    }
    false
}
fn site(path: &str, sha: &str, n: Node<'_>, reason: &str) -> Site {
    Site {
        path: path.into(),
        snapshot_sha: sha.into(),
        line: line(n),
        reason: reason.into(),
    }
}
struct Inventory {
    entities: Vec<Entity>,
    references: Vec<(String, String, Site, &'static str)>,
    imports: Vec<(String, String, Site)>,
    unresolved: Vec<Site>,
    skipped: Vec<String>,
    analyzed_files: usize,
}
impl Inventory {
    fn new() -> Self {
        Self {
            entities: vec![],
            references: vec![],
            imports: vec![],
            unresolved: vec![],
            skipped: vec![],
            analyzed_files: 0,
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn walk(
        &mut self,
        n: Node<'_>,
        bytes: &[u8],
        repo: &str,
        sha: &str,
        path: &str,
        module: &str,
        owner: Option<&str>,
        limit: usize,
    ) {
        let kind = n.kind();
        let category = match kind {
            "function_item" => Some("function"),
            "struct_item" | "enum_item" | "union_item" | "trait_item" | "type_item" => Some("type"),
            "const_item" | "static_item" => Some("constant"),
            "macro_definition" => Some("macro"),
            "mod_item" => Some("module"),
            _ => None,
        };
        let name = n
            .child_by_field_name("name")
            .map(|x| text(x, bytes))
            .unwrap_or("");
        let qualified = if let Some(o) = owner {
            format!("{o}::{name}")
        } else {
            format!("{module}::{name}")
        };
        if let Some(cat) = category {
            if !name.is_empty() && self.entities.iter().filter(|e| e.path == path).count() < limit {
                let prefix = text(n, bytes).split(name).next().unwrap_or("");
                let conditional = conditional(n, bytes);
                if conditional {
                    self.unresolved
                        .push(site(path, sha, n, "conditional definition"));
                }
                self.entities.push(Entity {
                    id: id(repo, cat, &qualified),
                    qualified_name: qualified.clone(),
                    kind: cat.into(),
                    path: path.into(),
                    start_line: line(n),
                    end_line: n.end_position().row as u32 + 1,
                    snapshot_sha: sha.into(),
                    ambiguous: conditional,
                    public: prefix.contains("pub ") || prefix.contains("pub("),
                });
            } else {
                self.skipped
                    .push(format!("{path}: entity cap or missing name"));
            }
        }
        if kind == "use_declaration" {
            if conditional(n, bytes) {
                self.unresolved
                    .push(site(path, sha, n, "conditional import"));
                return;
            }
            let raw = text(n, bytes)
                .trim_start_matches("use ")
                .trim_end_matches(';')
                .trim();
            if n.parent().is_some_and(|parent| parent.kind() == "block") {
                self.unresolved
                    .push(site(path, sha, n, "block-local import unsupported"));
            } else if let Some(imports) = direct_imports(raw) {
                for (alias, target) in imports {
                    self.imports.push((
                        format!("{module}::{alias}"),
                        target,
                        site(path, sha, n, "direct_import"),
                    ));
                }
            } else {
                self.unresolved
                    .push(site(path, sha, n, "glob/re-export/ambiguous import"));
            }
        }
        if matches!(kind, "call_expression" | "macro_invocation") {
            let target = n
                .child_by_field_name("function")
                .or_else(|| n.named_child(0))
                .map(|x| text(x, bytes))
                .unwrap_or("");
            let associated = target.rsplit_once("::").is_some_and(|(owner, _)| {
                owner
                    .rsplit("::")
                    .next()
                    .is_some_and(|segment| segment.chars().next().is_some_and(char::is_uppercase))
            });
            let reason = if kind == "macro_invocation" {
                "macro invocation"
            } else if target.contains('.') || associated {
                "associated/method/dynamic dispatch"
            } else {
                "unresolved call"
            };
            let source = owner.unwrap_or(module).to_string();
            if shadowed(n, bytes, target) || unsupported_lexical_import(n, bytes, target) {
                self.unresolved
                    .push(site(path, sha, n, "local/parameter shadowing"));
            } else if reason == "unresolved call" && !target.is_empty() {
                self.references
                    .push((source, target.into(), site(path, sha, n, reason), "call"));
            } else {
                self.unresolved.push(site(path, sha, n, reason));
            }
        }
        if kind == "identifier"
            && owner.is_some()
            && n.parent().is_some_and(|p| {
                p.child_by_field_name("value")
                    .is_some_and(|v| v.id() == n.id())
                    || p.kind() == "arguments"
            })
        {
            let name = text(n, bytes);
            if shadowed(n, bytes, name) || unsupported_lexical_import(n, bytes, name) {
                self.unresolved
                    .push(site(path, sha, n, "local shadowed reference"));
            } else {
                self.references.push((
                    owner.unwrap_or(module).into(),
                    name.into(),
                    site(path, sha, n, "value_reference"),
                    "reference",
                ));
            }
        }
        if kind == "identifier"
            && owner.is_some()
            && n.parent().is_some_and(|p| {
                matches!(
                    p.kind(),
                    "binary_expression"
                        | "unary_expression"
                        | "return_expression"
                        | "array_expression"
                        | "tuple_expression"
                        | "index_expression"
                        | "field_expression"
                        | "reference_expression"
                        | "assignment_expression"
                )
            })
        {
            self.unresolved
                .push(site(path, sha, n, "unsupported value reference position"));
        }
        if kind == "type_identifier"
            && owner.is_some()
            && n.parent().is_some_and(|p| {
                p.kind() == "scoped_type_identifier"
                    || p.child_by_field_name("name")
                        .is_none_or(|named| named.id() != n.id())
            })
        {
            let parent = n.parent();
            let target = parent
                .filter(|p| p.kind() == "scoped_type_identifier")
                .map(|p| text(p, bytes))
                .unwrap_or_else(|| text(n, bytes));
            if shadowed(n, bytes, target) || unsupported_lexical_import(n, bytes, target) {
                self.unresolved
                    .push(site(path, sha, n, "shadowed/unsupported type reference"));
            } else if parent.is_none_or(|p| {
                p.kind() != "scoped_type_identifier"
                    || p.child_by_field_name("name")
                        .is_some_and(|name| name.id() == n.id())
            }) {
                self.references.push((
                    owner.unwrap_or(module).into(),
                    target.into(),
                    site(path, sha, n, "type_reference"),
                    "reference",
                ));
            }
        }
        // Avoid traversing bodies of macro definitions: syntax is not expanded Rust.
        if kind == "macro_definition" {
            return;
        }
        let next_module = if kind == "mod_item" && n.child_by_field_name("body").is_some() {
            qualified.as_str()
        } else {
            module
        };
        let impl_owner = n
            .child_by_field_name("type")
            .map(|x| format!("{module}::{}", text(x, bytes)));
        let next_owner = if matches!(
            kind,
            "function_item"
                | "struct_item"
                | "enum_item"
                | "union_item"
                | "type_item"
                | "trait_item"
        ) {
            Some(qualified.as_str())
        } else if kind == "impl_item" {
            impl_owner.as_deref()
        } else {
            owner
        };
        let mut cursor = n.walk();
        for child in n.named_children(&mut cursor) {
            self.walk(
                child,
                bytes,
                repo,
                sha,
                path,
                next_module,
                next_owner,
                limit,
            );
        }
    }
}
fn inventory(
    blobs: &[Blob],
    repo: &str,
    sha: &str,
    input: &StructureInput,
    cancelled: &impl Fn() -> bool,
    start: Instant,
    modules: Option<&BTreeMap<String, String>>,
) -> Inventory {
    let mut result = Inventory::new();
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .is_err()
    {
        result.skipped.push("Rust parser unavailable".into());
        return result;
    }
    for blob in blobs {
        if cancelled() || start.elapsed() > Duration::from_millis(input.limits.parse_budget_ms) {
            result
                .skipped
                .push("cancelled or parse budget exhausted".into());
            break;
        }
        if blob.path == "Cargo.toml" {
            continue;
        }
        let Some(module) = modules
            .and_then(|m| m.get(&blob.path).cloned())
            .or_else(|| modules.is_none().then(|| scope(&blob.path)).flatten())
        else {
            result
                .skipped
                .push(format!("{}: unsupported layout/language", blob.path));
            continue;
        };
        if blob.bytes.len() > input.limits.max_blob_bytes {
            result.skipped.push(format!("{}: blob size cap", blob.path));
            continue;
        }
        let mut progress = |_: &tree_sitter::ParseState| {
            if cancelled() || start.elapsed() >= Duration::from_millis(input.limits.parse_budget_ms)
            {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        };
        if let Some(tree) = parser.parse_with_options(
            &mut |offset, _| &blob.bytes[offset..],
            None,
            Some(tree_sitter::ParseOptions::new().progress_callback(&mut progress)),
        ) {
            if tree.root_node().has_error() {
                result.skipped.push(format!("{}: syntax error", blob.path));
                continue;
            }
            result.analyzed_files += 1;
            result.walk(
                tree.root_node(),
                &blob.bytes,
                repo,
                sha,
                &blob.path,
                &module,
                None,
                input.limits.max_entities_per_file,
            );
        } else {
            result
                .skipped
                .push(format!("{}: parse cancelled", blob.path));
        }
    }
    let mut counts = BTreeMap::new();
    let mut namespace_counts = BTreeMap::new();
    for e in &result.entities {
        *counts.entry(e.id.clone()).or_insert(0usize) += 1;
        *namespace_counts
            .entry(e.qualified_name.clone())
            .or_insert(0usize) += 1;
    }
    for e in &mut result.entities {
        e.ambiguous |= counts[&e.id] > 1 || namespace_counts[&e.qualified_name] > 1;
        if e.ambiguous {
            e.id = id(
                repo,
                &e.kind,
                &format!("{}\0{}\0{}", e.qualified_name, e.path, e.start_line),
            );
        }
    }
    result
        .entities
        .sort_by(|a, b| (&a.id, &a.path, a.start_line).cmp(&(&b.id, &b.path, b.start_line)));
    result
}
fn resolve(inv: &mut Inventory) -> Vec<Edge> {
    let definitions: BTreeMap<_, _> = inv
        .entities
        .iter()
        .filter(|e| !e.ambiguous)
        .map(|e| (e.qualified_name.clone(), e.id.clone()))
        .collect();
    let callable: BTreeMap<_, _> = inv
        .entities
        .iter()
        .filter(|e| e.kind == "function" && !e.ambiguous)
        .map(|e| (e.qualified_name.clone(), e.id.clone()))
        .collect();
    let mut imports = BTreeMap::<String, Vec<String>>::new();
    for (alias, target, _) in &inv.imports {
        imports
            .entry(alias.clone())
            .or_default()
            .push(target.clone());
    }
    let mut edges = Vec::new();
    for (alias, target, site) in &inv.imports {
        let module = alias.rsplit_once("::").map(|x| x.0).unwrap_or("crate");
        let normalized = normalize_path(module, target);
        if let (Some(from), Some(to)) = (
            definitions.get(module),
            normalized.as_ref().and_then(|name| definitions.get(name)),
        ) {
            edges.push(Edge {
                from: from.clone(),
                to: to.clone(),
                kind: "import".into(),
                site: site.clone(),
            });
        } else {
            inv.unresolved.push(site.clone());
        }
    }
    for (source, target, site, edge_kind) in &inv.references {
        let module = source.rsplit_once("::").map(|x| x.0).unwrap_or(source);
        let Some(candidate) = normalize_path(module, target) else {
            inv.unresolved.push(site.clone());
            continue;
        };
        let alias = imports.get(&candidate);
        let resolved_name = if let Some(targets) = alias {
            (targets.len() == 1)
                .then(|| normalize_path(module, &targets[0]))
                .flatten()
        } else {
            let prefixes: Vec<_> = imports
                .iter()
                .filter_map(|(name, targets)| {
                    candidate
                        .strip_prefix(name)
                        .and_then(|suffix| suffix.strip_prefix("::"))
                        .map(|suffix| (targets, suffix))
                })
                .collect();
            if prefixes.len() == 1 && prefixes[0].0.len() == 1 {
                normalize_path(module, &prefixes[0].0[0])
                    .map(|name| format!("{name}::{}", prefixes[0].1))
            } else if prefixes.is_empty() {
                Some(candidate.clone())
            } else {
                None
            }
        };
        let import_collision = resolved_name
            .as_ref()
            .is_some_and(|name| name != &candidate && definitions.contains_key(&candidate));
        let resolved = if import_collision {
            None
        } else {
            resolved_name.as_ref().and_then(|name| {
                if *edge_kind == "call" {
                    callable.get(name)
                } else {
                    definitions.get(name)
                }
            })
        };
        if let (Some(from), Some(to)) = (definitions.get(source), resolved) {
            edges.push(Edge {
                from: from.clone(),
                to: to.clone(),
                kind: (*edge_kind).into(),
                site: site.clone(),
            });
        } else {
            inv.unresolved.push(site.clone());
        }
    }
    edges.sort_by(|a, b| {
        (&a.from, &a.to, &a.site.path, a.site.line).cmp(&(
            &b.from,
            &b.to,
            &b.site.path,
            b.site.line,
        ))
    });
    edges.dedup_by(|a, b| {
        a.from == b.from && a.to == b.to && a.site.path == b.site.path && a.site.line == b.site.line
    });
    edges
}

/// Analyze immutable pinned objects for tests without full tree attestation.
#[cfg(test)]
pub fn analyze(input: &StructureInput, cancelled: &impl Fn() -> bool) -> StructureOutput {
    analyze_with_corpus(input, &[], cancelled)
}

/// `tree_paths` is the full pinned HEAD tree path inventory. An empty list fails
/// completeness closed; callers can still retrieve useful resolved paths.
pub fn analyze_with_corpus(
    input: &StructureInput,
    tree_paths: &[String],
    cancelled: &impl Fn() -> bool,
) -> StructureOutput {
    let start = Instant::now();
    let manifest = input
        .head_blobs
        .iter()
        .find(|b| b.path == "Cargo.toml")
        .cloned();
    let mut head: Vec<_> = input
        .head_blobs
        .iter()
        .filter(|b| b.path != "Cargo.toml")
        .cloned()
        .collect();
    let changed: BTreeSet<_> = input
        .changed_files
        .iter()
        .map(|f| f.path.as_str())
        .collect();
    head.sort_by_key(|b| {
        (
            !changed.contains(b.path.as_str()),
            scope(&b.path).is_none(),
            b.path.clone(),
        )
    });
    let omitted_files = head.len().saturating_sub(input.limits.max_files);
    head.truncate(input.limits.max_files);
    if let Some(manifest) = manifest {
        head.push(manifest);
    }
    let budget_exhausted =
        || cancelled() || start.elapsed() >= Duration::from_millis(input.limits.parse_budget_ms);
    let layout = layout::discover(&head, input.limits.max_blob_bytes, &budget_exhausted);
    let mut inv = inventory(
        &head,
        &input.repository,
        &input.head_sha,
        input,
        cancelled,
        start,
        Some(&layout.modules),
    );
    inv.skipped.extend(layout.skipped);
    for file in &input.changed_files {
        if !file.patch_complete {
            inv.skipped.push(format!(
                "{}: incomplete patch; omitted changed entities unknown",
                file.path
            ));
        }
    }
    if tree_paths.is_empty() {
        inv.skipped
            .push("full pinned tree path inventory unavailable".into());
    } else {
        let provided: BTreeSet<_> = input.head_blobs.iter().map(|b| b.path.as_str()).collect();
        for path in tree_paths {
            if path.ends_with(".rs") && !provided.contains(path.as_str()) {
                inv.skipped
                    .push(format!("{path}: Rust tree object not supplied"));
            }
            if [
                ".py", ".ts", ".tsx", ".js", ".jsx", ".go", ".java", ".c", ".cpp",
            ]
            .iter()
            .any(|ext| path.ends_with(ext))
            {
                inv.skipped.push(format!("{path}: language unsupported"));
            }
        }
    }
    let edges = resolve(&mut inv);
    let base_layout = layout::discover(
        &input.base_blobs,
        input.limits.max_blob_bytes,
        &budget_exhausted,
    );
    let base = inventory(
        &input.base_blobs,
        &input.repository,
        &input.base_sha,
        input,
        cancelled,
        start,
        input
            .base_blobs
            .iter()
            .any(|b| b.path == "Cargo.toml")
            .then_some(&base_layout.modules),
    );
    for failure in base.skipped.iter().filter(|failure| {
        input.changed_files.iter().any(|file| {
            let previous = file.previous_path.as_deref().unwrap_or(&file.path);
            failure.starts_with(&format!("{previous}:"))
        })
    }) {
        inv.skipped.push(format!("base: {failure}"));
    }
    let mut changed_entities =
        BTreeMap::<(String, String, u32), (Entity, Option<Site>, Option<String>)>::new();
    let mut unmapped = Vec::new();
    for file in &input.changed_files {
        for h in &file.hunks {
            for row in &h.lines {
                let (kind, path, sha, blob, candidates) = match row.kind {
                    LineKind::Addition => (
                        "addition",
                        file.path.as_str(),
                        input.head_sha.as_str(),
                        head.iter().find(|b| b.path == file.path),
                        &inv.entities,
                    ),
                    LineKind::Deletion => {
                        let p = file.previous_path.as_deref().unwrap_or(&file.path);
                        (
                            "deletion",
                            p,
                            input.base_sha.as_str(),
                            input.base_blobs.iter().find(|b| b.path == p),
                            &base.entities,
                        )
                    }
                    _ => continue,
                };
                let number = if kind == "addition" {
                    row.new_line
                } else {
                    row.old_line
                };
                let Some(number) = number else { continue };
                let valid = blob
                    .and_then(|b| b.bytes.split(|c| *c == b'\n').nth(number as usize - 1))
                    .is_some_and(|b| b == row.text.as_bytes());
                let anchor = valid.then(|| Site {
                    path: path.into(),
                    snapshot_sha: sha.into(),
                    line: number,
                    reason: kind.into(),
                });
                let matches: Vec<_> = candidates
                    .iter()
                    .filter(|e| e.path == path && e.start_line <= number && number <= e.end_line)
                    .collect();
                if let Some(e) = matches
                    .into_iter()
                    .min_by_key(|e| e.end_line - e.start_line)
                {
                    changed_entities
                        .entry((e.id.clone(), e.snapshot_sha.clone(), e.start_line))
                        .or_insert_with(|| {
                            (
                                e.clone(),
                                anchor,
                                (!valid).then(|| "diff_source_mismatch".into()),
                            )
                        });
                } else {
                    unmapped.push(Site {
                        path: path.into(),
                        snapshot_sha: sha.into(),
                        line: number,
                        reason: if valid {
                            "unmapped_changed_row"
                        } else {
                            "diff_source_mismatch"
                        }
                        .into(),
                    });
                }
            }
        }
        if file.hunks.is_empty() {
            inv.skipped
                .push(format!("{}: changed file without patch", file.path));
        }
    }
    let mut results = Vec::new();
    let omitted_results = changed_entities
        .len()
        .saturating_sub(input.limits.max_results);
    let by_id: BTreeMap<_, _> = inv
        .entities
        .iter()
        .map(|e| (e.id.clone(), e.clone()))
        .collect();
    for (_, (entity, anchor, anchor_reason)) in
        changed_entities.into_iter().take(input.limits.max_results)
    {
        if cancelled() {
            inv.skipped.push("cancelled".into());
            break;
        }
        let removed = entity.snapshot_sha == input.base_sha;
        let mut lineage = Vec::new();
        if !removed && !entity.ambiguous {
            for file in input.changed_files.iter().filter(|f| {
                f.change_kind == ChangeKind::Renamed && f.patch_complete && f.path == entity.path
            }) {
                if let Some(old) = file
                    .previous_path
                    .as_ref()
                    .filter(|old| input.base_blobs.iter().any(|b| &b.path == *old))
                {
                    lineage.extend(
                        base.entities
                            .iter()
                            .filter(|candidate| {
                                &candidate.path == old
                                    && candidate.kind == entity.kind
                                    && candidate.qualified_name.rsplit("::").next()
                                        == entity.qualified_name.rsplit("::").next()
                                    && !candidate.ambiguous
                            })
                            .map(|candidate| candidate.id.clone()),
                    );
                }
            }
        }
        let rename_from = (lineage.len() == 1).then(|| lineage[0].clone());
        let mut result = EntityResult {
            entity: entity.clone(),
            anchor,
            anchor_reason,
            resolved_dependants: None,
            direct_dependants: None,
            within_depth_dependants: None,
            reachable_dependants: None,
            depth_truncated: false,
            visits_truncated: false,
            direct_callees: vec![],
            direct_callee_entities: vec![],
            dependant_paths: vec![],
            relationships_unavailable: removed.then(|| "base_graph_not_built".into()),
            complete: false,
            warnings: vec![],
        };
        if !removed {
            result.direct_dependants = Some(
                edges
                    .iter()
                    .filter(|edge| edge.to == entity.id)
                    .map(|edge| &edge.from)
                    .collect::<BTreeSet<_>>()
                    .len(),
            );
            result.direct_callees = edges
                .iter()
                .filter(|e| e.from == entity.id)
                .cloned()
                .collect();
            result.direct_callee_entities = result
                .direct_callees
                .iter()
                .filter_map(|edge| by_id.get(&edge.to).cloned())
                .collect();
            result
                .direct_callee_entities
                .sort_by(|a, b| a.id.cmp(&b.id));
            result.direct_callee_entities.dedup_by(|a, b| a.id == b.id);
            let mut queue =
                VecDeque::from([(entity.id.clone(), vec![entity.clone()], Vec::<Edge>::new())]);
            let mut seen = BTreeSet::from([entity.id.clone()]);
            let mut depth_truncated = false;
            let mut visit_truncated = false;
            let mut traversal_cancelled = false;
            while let Some((id, path, via)) = queue.pop_front() {
                if cancelled() {
                    traversal_cancelled = true;
                    break;
                }
                if path.len() > 1 && result.dependant_paths.len() < input.limits.max_paths {
                    result.dependant_paths.push(EntityPath {
                        entities: path.clone(),
                        edges: via.clone(),
                    });
                }
                for edge in edges.iter().filter(|e| e.to == id) {
                    if cancelled() {
                        traversal_cancelled = true;
                        break;
                    }
                    if seen.contains(&edge.from) || !by_id.contains_key(&edge.from) {
                        continue;
                    }
                    if path.len() > input.limits.max_depth {
                        depth_truncated = true;
                        continue;
                    }
                    if seen.len() >= input.limits.max_visited {
                        visit_truncated = true;
                        continue;
                    }
                    if seen.insert(edge.from.clone())
                        && let Some(next) = by_id.get(&edge.from)
                    {
                        let mut p = path.clone();
                        p.push(next.clone());
                        let mut v = via.clone();
                        v.push(edge.clone());
                        queue.push_back((edge.from.clone(), p, v));
                    }
                }
            }
            result.depth_truncated = depth_truncated;
            result.visits_truncated = visit_truncated;
            result.within_depth_dependants =
                (!visit_truncated && !traversal_cancelled).then_some(seen.len().saturating_sub(1));
            result.reachable_dependants =
                (!depth_truncated && !visit_truncated && !traversal_cancelled)
                    .then_some(seen.len().saturating_sub(1));
            result.resolved_dependants = result.reachable_dependants;
            if depth_truncated {
                result.warnings.push("depth_cap".into());
            }
            if visit_truncated {
                result.warnings.push("visit_cap".into());
            }
            if traversal_cancelled {
                result.warnings.push("cancelled".into());
            }
            if seen.len().saturating_sub(1) > input.limits.max_paths {
                result.warnings.push("path_cap".into());
            }
        }
        if let Some(previous) = rename_from {
            result
                .warnings
                .push(format!("validated_rename_from:{previous}"));
        }
        if entity.ambiguous {
            result.warnings.push("ambiguous_entity_id".into());
        }
        if entity.public {
            result.warnings.push("out_of_corpus_exposure".into());
        }
        result.complete = !removed
            && result
                .warnings
                .iter()
                .all(|w| w.starts_with("validated_rename_from:"))
            && inv.unresolved.is_empty()
            && inv.skipped.is_empty()
            && omitted_files == 0
            && omitted_results == 0;
        results.push(result);
    }
    let result_truncated = results.iter().any(|result| {
        result.depth_truncated
            || result.visits_truncated
            || result
                .warnings
                .iter()
                .any(|w| w == "path_cap" || w == "cancelled")
    });
    let corpus_complete = !result_truncated
        && inv.skipped.is_empty()
        && inv.unresolved.is_empty()
        && omitted_files == 0
        && omitted_results == 0
        && !cancelled();
    let coverage = CorpusCoverage { analyzed_files: inv.analyzed_files, analyzed_entities: inv.entities.len(), skipped_files: inv.skipped,
        unresolved_sites: inv.unresolved, unmapped_changed_rows: unmapped, omitted_files: Some(omitted_files), omitted_results,
        complete: corpus_complete && !inv.entities.iter().any(|e| e.public || e.ambiguous), limitations: vec!["conventional Cargo src layout only; no cross-crate, macro expansion, method dispatch, or base graph".into()], parser_version: format!("tree-sitter-rust {}", tree_sitter::Language::new(tree_sitter_rust::LANGUAGE).abi_version()) };
    let digest = format!(
        "{}:{}:{}:{}:{}:{}:{}:{}",
        input.limits.max_files,
        input.limits.max_blob_bytes,
        input.limits.max_entities_per_file,
        input.limits.max_visited,
        input.limits.max_paths,
        input.limits.max_results,
        input.limits.max_depth,
        input.limits.parse_budget_ms
    );
    StructureOutput {
        repository: input.repository.clone(),
        base_sha: input.base_sha.clone(),
        head_sha: input.head_sha.clone(),
        limits_digest: digest,
        entities: inv.entities,
        edges,
        results,
        coverage,
    }
}
