//! Exact-path directional associations, not inferred defects or rename lineage.
//! Only pairs involving changed baseline paths are counted, with a hard work cap.
use std::collections::{BTreeMap, BTreeSet};

use super::reader::History;

pub const MAX_COMMIT_PATHS: usize = 50;
pub const MIN_SUPPORT: usize = 5;
pub const MAX_PAIRS: usize = 50_000;
pub const MAX_HINTS: usize = 100;
pub const METHOD: &str = "aside.git-history.cochange@1";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Association {
    pub from: String,
    pub to: String,
    pub support_a: usize,
    pub support_b: usize,
    pub support_ab: usize,
    /// At most five lexically first commit identities, not a corpus sample.
    pub examples: Vec<(String, i64)>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Evaluation {
    pub associations: Vec<Association>,
    pub qualifying_pairs: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Incomplete {
    Interrupted,
    PairLimit,
}

#[derive(Default)]
struct Pair {
    support: usize,
    examples: BTreeSet<(String, i64)>,
}

pub fn evaluate(
    history: &History,
    changed: &BTreeSet<String>,
    pr_paths: &BTreeSet<String>,
    max_hints: usize,
    cancelled: impl FnMut() -> bool,
) -> Result<Evaluation, Incomplete> {
    evaluate_limited(history, changed, pr_paths, max_hints, MAX_PAIRS, cancelled)
}

fn evaluate_limited(
    history: &History,
    changed: &BTreeSet<String>,
    pr_paths: &BTreeSet<String>,
    max_hints: usize,
    max_pairs: usize,
    mut cancelled: impl FnMut() -> bool,
) -> Result<Evaluation, Incomplete> {
    let mut support = BTreeMap::<&str, usize>::new();
    let mut pairs = BTreeMap::<(&str, &str), Pair>::new();
    if cancelled() {
        return Err(Incomplete::Interrupted);
    }
    for transaction in &history.transactions {
        if cancelled() {
            return Err(Incomplete::Interrupted);
        }
        let paths: Vec<_> = transaction
            .paths
            .iter()
            .filter(|p| history.files.contains_key(*p) && !history.ambiguous.contains(*p))
            .map(String::as_str)
            .collect();
        for path in &paths {
            *support.entry(path).or_default() += 1;
        }
        for a in paths.iter().filter(|p| changed.contains(**p)) {
            if cancelled() {
                return Err(Incomplete::Interrupted);
            }
            for b in paths.iter().filter(|p| !pr_paths.contains(**p) && *p != a) {
                let key = (*a, *b);
                if !pairs.contains_key(&key) && pairs.len() >= max_pairs {
                    return Err(Incomplete::PairLimit);
                }
                let pair = pairs.entry(key).or_default();
                pair.support += 1;
                pair.examples
                    .insert((transaction.id.clone(), transaction.timestamp));
                if pair.examples.len() > 5 {
                    pair.examples.pop_last();
                }
            }
        }
    }
    let mut associations = Vec::new();
    for ((a, b), pair) in pairs {
        if cancelled() {
            return Err(Incomplete::Interrupted);
        }
        let support_a = support[a];
        // Integer comparison includes exactly 0.6, without float boundary drift.
        if pair.support >= MIN_SUPPORT && 5 * pair.support >= 3 * support_a {
            associations.push(Association {
                from: a.into(),
                to: b.into(),
                support_a,
                support_b: support[b],
                support_ab: pair.support,
                examples: pair.examples.into_iter().collect(),
            });
        }
    }
    // Strongest support first, then directional confidence, then exact paths.
    associations.sort_by(|a, b| {
        b.support_ab
            .cmp(&a.support_ab)
            .then_with(|| (b.support_ab * a.support_a).cmp(&(a.support_ab * b.support_a)))
            .then_with(|| a.from.cmp(&b.from))
            .then_with(|| a.to.cmp(&b.to))
    });
    if cancelled() {
        return Err(Incomplete::Interrupted);
    }
    let qualifying_pairs = associations.len();
    associations.truncate(max_hints.min(MAX_HINTS));
    Ok(Evaluation {
        associations,
        qualifying_pairs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::reader::Transaction;

    fn history(rows: &[Vec<&str>]) -> History {
        History {
            base: "base".into(),
            anchor: 100,
            window_start: 0,
            walked: rows.len(),
            counted: rows.len(),
            merges_skipped: 0,
            broad_skipped: 0,
            files: rows
                .iter()
                .flatten()
                .map(|p| (p.to_string(), "blob".into()))
                .collect(),
            touches: BTreeMap::new(),
            ambiguous: BTreeSet::new(),
            transactions: rows
                .iter()
                .enumerate()
                .map(|(n, paths)| Transaction {
                    id: format!("{n:040}"),
                    timestamp: 100,
                    paths: paths.iter().map(|p| p.to_string()).collect(),
                })
                .collect(),
        }
    }
    fn set(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }
    fn run(h: &History, changed: &[&str], excluded: &[&str]) -> Evaluation {
        evaluate(h, &set(changed), &set(excluded), MAX_HINTS, || false).unwrap()
    }

    #[test]
    fn support_and_directional_confidence_thresholds_are_exact() {
        for (together, only_a, expected) in
            [(4, 0, false), (5, 0, true), (6, 4, true), (5, 4, false)]
        {
            let mut rows = vec![vec!["a", "b"]; together];
            rows.extend(vec![vec!["a"]; only_a]);
            rows.extend(vec![vec!["b"]; 20]);
            let h = history(&rows);
            let measured = run(&h, &["a"], &["a"]);
            assert_eq!(!measured.associations.is_empty(), expected);
            assert!(
                run(&h, &["b"], &["b"]).associations.is_empty(),
                "reverse direction must not be inferred"
            );
            if expected {
                let a = &measured.associations[0];
                assert_eq!(
                    (a.support_ab, a.support_a, a.support_b),
                    (together, together + only_a, together + 20)
                );
                assert_eq!(a.examples.len(), 5);
            }
        }
    }

    #[test]
    fn all_pr_paths_and_ambiguous_or_absent_baseline_partners_are_excluded() {
        let mut h = history(&vec![vec!["a", "b", "c", "d"]; 5]);
        h.files.remove("d");
        h.ambiguous.insert("c".into());
        assert_eq!(run(&h, &["a"], &["a"]).associations[0].to, "b");
        assert!(run(&h, &["a"], &["a", "b"]).associations.is_empty());
        h.ambiguous.insert("a".into());
        assert!(run(&h, &["a"], &["a"]).associations.is_empty());
    }

    #[test]
    fn equal_timestamps_preserve_transactions_and_order_is_deterministic() {
        let mut h = history(&vec![vec!["a", "b", "c"]; 6]);
        let expected = run(&h, &["a"], &["a"]);
        h.transactions.reverse();
        assert_eq!(run(&h, &["a"], &["a"]), expected);
        assert_eq!(expected.associations[0].support_ab, 6);
        assert_eq!(expected.associations[0].to, "b");
        let limited = evaluate(&h, &set(&["a"]), &set(&["a"]), 1, || false).unwrap();
        assert_eq!(limited.qualifying_pairs, 2);
        assert_eq!(limited.associations.len(), 1);
        assert_eq!(limited.associations[0], expected.associations[0]);
        let disjoint = history(
            &(0..6)
                .flat_map(|_| [vec!["a"], vec!["b"]])
                .collect::<Vec<_>>(),
        );
        assert!(run(&disjoint, &["a"], &["a"]).associations.is_empty());
    }

    #[test]
    fn pair_work_cap_and_cancellation_fail_closed() {
        let h = history(&vec![vec!["a", "b", "c"]; 6]);
        assert_eq!(
            evaluate_limited(&h, &set(&["a"]), &set(&["a"]), 100, 1, || false),
            Err(Incomplete::PairLimit)
        );
        let mut checks = 0;
        assert_eq!(
            evaluate(&h, &set(&["a"]), &set(&["a"]), 100, || {
                checks += 1;
                checks > 3
            }),
            Err(Incomplete::Interrupted)
        );
    }
}
