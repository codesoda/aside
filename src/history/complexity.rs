//! Bounded, syntax-aware Rust complexity measurement for historical baselines.

use std::ops::ControlFlow;
use std::time::{Duration, Instant};
use tree_sitter::{Node, ParseOptions, Parser};

const MAX_SOURCE_BYTES: usize = 512 * 1024;
const MAX_VISITED_NODES: usize = 1_000_000;
const PARSE_BUDGET: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Complexity {
    pub max_cyclomatic: u32,
    pub function_count: usize,
}

/// Measure the largest function's cyclomatic complexity. An empty file has no
/// functions and a maximum of zero; parse failures never produce that result.
pub fn measure(source: &[u8], deadline: Instant) -> Result<Complexity, String> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err("Rust source exceeds 512 KiB limit".into());
    }
    if Instant::now() >= deadline {
        return Err("Rust complexity deadline exceeded".into());
    }

    let parse_deadline = deadline.min(Instant::now() + PARSE_BUDGET);
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .map_err(|err| format!("Rust parser initialization failed: {err}"))?;
    let mut check_deadline = |_: &tree_sitter::ParseState| {
        if Instant::now() >= parse_deadline {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = ParseOptions::new().progress_callback(&mut check_deadline);
    let tree = parser.parse_with_options(&mut |offset, _| &source[offset..], None, Some(options));
    // The callback is cooperative: even a returned tree must be discarded if
    // the parser returned after the budget expired.
    if Instant::now() >= parse_deadline {
        return Err("Rust parsing deadline exceeded".into());
    }
    let tree = tree.ok_or_else(|| "Rust parsing cancelled or failed".to_string())?;
    measure_tree(tree.root_node(), deadline, MAX_VISITED_NODES)
}

fn measure_tree(
    root: Node<'_>,
    deadline: Instant,
    max_visited_nodes: usize,
) -> Result<Complexity, String> {
    if root.has_error() {
        return Err("Rust source contains syntax errors".into());
    }

    let mut result = Complexity {
        max_cyclomatic: 0,
        function_count: 0,
    };
    // Each entry carries its enclosing function's complexity. A nested
    // function starts at one instead of contributing to its parent's score.
    let mut scores: Vec<u32> = Vec::new();
    let mut stack = vec![(root, None)];
    let mut visited = 0usize;
    while let Some((node, enclosing)) = stack.pop() {
        check_deadline(deadline)?;
        visited += 1;
        if visited > max_visited_nodes {
            return Err("Rust syntax tree exceeds visited-node limit".into());
        }
        if node.is_error() || node.is_missing() {
            return Err("Rust source contains syntax errors".into());
        }
        if matches!(node.kind(), "macro_invocation" | "macro_definition") {
            continue;
        }
        let current = if node.kind() == "function_item" {
            result.function_count += 1;
            scores.push(1);
            Some(scores.len() - 1)
        } else {
            enclosing
        };
        if let Some(index) = current
            && node.kind() != "function_item"
        {
            let increment = match node.kind() {
                "if_expression" | "while_expression" | "for_expression" | "try_expression" => 1,
                "match_arm" if has_preceding_arm(node, deadline)? => 1,
                "binary_expression" => node
                    .child_by_field_name("operator")
                    .is_some_and(|op| matches!(op.kind(), "&&" | "||"))
                    as u32,
                _ => 0,
            };
            scores[index] = scores[index]
                .checked_add(increment)
                .ok_or_else(|| "Rust complexity overflow".to_string())?;
        }
        // Explicit cap on traversal memory, independent of pathological AST shape.
        if stack.len() + node.child_count() as usize > MAX_SOURCE_BYTES {
            return Err("Rust syntax tree exceeds traversal limit".into());
        }
        for child_index in (0..node.child_count()).rev() {
            check_deadline(deadline)?;
            if let Some(child) = node.child(child_index) {
                stack.push((child, current));
            }
        }
    }
    result.max_cyclomatic = scores.into_iter().max().unwrap_or(0);
    Ok(result)
}

fn check_deadline(deadline: Instant) -> Result<(), String> {
    if Instant::now() >= deadline {
        Err("Rust complexity deadline exceeded".into())
    } else {
        Ok(())
    }
}

fn has_preceding_arm(node: Node<'_>, deadline: Instant) -> Result<bool, String> {
    let mut sibling = node.prev_sibling();
    while let Some(previous) = sibling {
        check_deadline(deadline)?;
        if previous.kind() == "match_arm" {
            return Ok(true);
        }
        sibling = previous.prev_sibling();
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complexity(input: &str) -> Complexity {
        measure(input.as_bytes(), Instant::now() + Duration::from_secs(5)).unwrap()
    }

    #[test]
    fn counts_functions_and_decisions_without_mixing_nested_functions() {
        let result = complexity(
            "fn outer(x: bool) { if x { } fn inner() { while true { } } } fn plain() {} ",
        );
        assert_eq!(
            result,
            Complexity {
                max_cyclomatic: 2,
                function_count: 3
            }
        );
    }

    #[test]
    fn counts_match_arms_boolean_operators_and_try() {
        let result = complexity(
            "fn f(x: Option<bool>, y: bool) -> Option<()> { match x? { Some(a) if a && y || y => (), None => (), } Some(()) }",
        );
        assert_eq!(
            result,
            Complexity {
                max_cyclomatic: 5,
                function_count: 1
            }
        );
    }

    #[test]
    fn counts_loops_and_ignores_macros() {
        let result = complexity(
            "macro_rules! m { () => { if true {} }; } fn f() { m!(); for _ in 0..2 { while true { break; } } }",
        );
        assert_eq!(
            result,
            Complexity {
                max_cyclomatic: 3,
                function_count: 1
            }
        );
    }

    #[test]
    fn traversal_limits_total_visits_not_only_pending_stack() {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse("fn a() {} fn b() {}", None).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        assert_eq!(
            measure_tree(tree.root_node(), deadline, 1).unwrap_err(),
            "Rust syntax tree exceeds visited-node limit"
        );
        assert_eq!(
            measure_tree(tree.root_node(), deadline, MAX_VISITED_NODES)
                .unwrap()
                .function_count,
            2
        );
    }

    #[test]
    fn rejects_invalid_inputs_and_expired_deadlines() {
        assert!(measure(b"fn broken( {", Instant::now() + Duration::from_secs(5)).is_err());
        assert!(
            measure(
                &vec![b' '; MAX_SOURCE_BYTES + 1],
                Instant::now() + Duration::from_secs(5)
            )
            .is_err()
        );
        assert!(measure(b"fn f() {}", Instant::now() - Duration::from_secs(1)).is_err());
        assert_eq!(
            complexity(""),
            Complexity {
                max_cyclomatic: 0,
                function_count: 0
            }
        );
    }
}
