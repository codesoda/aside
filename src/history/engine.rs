//! Pure percentile and hotspot rules over a complete pinned population.
use std::collections::BTreeMap;

use super::reader::History;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rank {
    pub below: usize,
    pub population: usize,
}
impl Rank {
    pub fn percent(self) -> f64 {
        100.0 * self.below as f64 / self.population as f64
    }
    fn at_least(self, threshold: usize) -> bool {
        self.below.saturating_mul(100) >= threshold.saturating_mul(self.population)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Measurement {
    pub commits: usize,
    pub days_since_change: Option<i64>,
    pub churn_rank: Option<Rank>,
    pub complexity: Option<u32>,
    pub complexity_rank: Option<Rank>,
    pub hotspot_index: Option<f64>,
    pub churn_attention: bool,
    pub hotspot_attention: bool,
}

/// Baseline Rust population includes each touched `.rs` regular file even if
/// its parse failed. A failed member makes complexity rankings unavailable;
/// it does not erase valid churn counts or turn unknown into zero complexity.
/// `complexity` contains only successful parser measurements.
#[cfg(test)]
pub fn evaluate(
    history: &History,
    complexity: &BTreeMap<String, u32>,
) -> BTreeMap<String, Measurement> {
    evaluate_checked(history, complexity, || false).expect("uninterrupted evaluation")
}

pub fn evaluate_checked(
    history: &History,
    complexity: &BTreeMap<String, u32>,
    mut cancelled: impl FnMut() -> bool,
) -> Option<BTreeMap<String, Measurement>> {
    if cancelled() {
        return None;
    }
    let churn: BTreeMap<_, _> = history
        .files
        .keys()
        .map(|path| (path.clone(), history.touches.get(path).map_or(0, Vec::len)))
        .collect();
    let rust: Vec<_> = history
        .files
        .keys()
        .filter(|path| path.ends_with(".rs") && history.touches.contains_key(*path))
        .collect();
    let mut churn_values: Vec<_> = churn.values().copied().collect();
    churn_values.sort_unstable();
    let mut complexity_values: Vec<_> = rust
        .iter()
        .filter_map(|path| complexity.get(*path).copied())
        .collect();
    complexity_values.sort_unstable();
    let complexity_complete = rust.len() <= 500
        && rust.iter().all(|path| complexity.contains_key(*path))
        && history.ambiguous.is_empty();
    let churn_complete = history.ambiguous.is_empty();
    let mut result = BTreeMap::new();
    for path in history.files.keys() {
        if cancelled() {
            return None;
        }
        let commits = churn[path];
        let churn_rank = (churn_complete && churn.len() >= 20).then(|| Rank {
            below: churn_values.partition_point(|count| *count < commits),
            population: churn.len(),
        });
        let parsed = complexity.get(path).copied();
        let complexity_rank =
            (complexity_complete && rust.len() >= 20 && parsed.is_some()).then(|| {
                let value = parsed.expect("complexity rank requires parsed value");
                Rank {
                    below: complexity_values.partition_point(|count| *count < value),
                    population: rust.len(),
                }
            });
        let hotspot_index = churn_rank
            .zip(complexity_rank)
            .map(|(a, b)| a.percent() * b.percent() / 100.0);
        let days_since_change = history
            .touches
            .get(path)
            .and_then(|times| times.iter().max())
            .map(|latest| (history.anchor - latest) / 86_400);
        let churn_attention = commits >= 5 && churn_rank.is_some_and(|r| r.at_least(90));
        let hotspot_attention = commits >= 5
            && churn_rank.is_some_and(|r| r.at_least(75))
            && complexity_rank.is_some_and(|r| r.at_least(75));
        result.insert(
            path.clone(),
            Measurement {
                commits,
                days_since_change,
                churn_rank,
                complexity: parsed,
                complexity_rank,
                hotspot_index,
                churn_attention,
                hotspot_attention,
            },
        );
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    fn history() -> History {
        let files = (0..20)
            .map(|n| (format!("{n:02}.rs"), format!("{n:040}")))
            .collect();
        let touches = (0..20)
            .map(|n| (format!("{n:02}.rs"), vec![100_000; n]))
            .collect();
        History {
            base: "b".into(),
            anchor: 100_000,
            window_start: 0,
            walked: 20,
            counted: 20,
            merges_skipped: 0,
            broad_skipped: 0,
            transactions: Vec::new(),
            files,
            touches,
            ambiguous: BTreeSet::new(),
        }
    }
    #[test]
    fn exact_ranks_and_unknown_parser_preserves_churn() {
        let h = history();
        let complexity = (0..20).map(|n| (format!("{n:02}.rs"), n as u32)).collect();
        let measured = evaluate(&h, &complexity);
        let best = &measured["19.rs"];
        assert_eq!(
            best.churn_rank,
            Some(Rank {
                below: 19,
                population: 20
            })
        );
        assert_eq!(best.hotspot_index, Some(90.25));
        assert!(best.churn_attention && best.hotspot_attention);
        let zero = &measured["00.rs"];
        assert_eq!(zero.commits, 0);
        assert_eq!(zero.days_since_change, None);
        assert_eq!(zero.churn_rank.unwrap().below, 0);
        let incomplete: BTreeMap<_, _> = complexity
            .into_iter()
            .filter(|(path, _)| path != "01.rs")
            .collect();
        let measured = evaluate(&h, &incomplete);
        assert!(measured["19.rs"].churn_rank.is_some());
        assert!(measured["19.rs"].complexity_rank.is_none());
        assert!(!measured["19.rs"].hotspot_attention);
    }
    #[test]
    fn cancellation_stops_ranking_without_partial_result() {
        let h = history();
        let mut checks = 0;
        let result = evaluate_checked(&h, &BTreeMap::new(), || {
            checks += 1;
            checks == 3
        });
        assert!(result.is_none());
        assert_eq!(checks, 3);
    }

    #[test]
    fn rust_population_limit_preserves_churn_and_suppresses_hotspots() {
        let mut h = history();
        for n in 20..=500 {
            let path = format!("{n:03}.rs");
            h.files.insert(path.clone(), format!("{n:040}"));
            h.touches.insert(path, vec![h.anchor; 5]);
        }
        let complexity = h.files.keys().map(|path| (path.clone(), 2)).collect();
        let measured = evaluate(&h, &complexity);
        assert_eq!(measured.len(), 501);
        assert!(
            measured
                .values()
                .all(|m| m.complexity_rank.is_none() && m.hotspot_index.is_none())
        );
        assert!(measured["19.rs"].churn_rank.is_some());
    }

    #[test]
    fn ambiguous_member_suppresses_every_rank() {
        let mut h = history();
        h.ambiguous.insert("03.rs".into());
        let measured = evaluate(&h, &BTreeMap::new());
        assert!(measured.values().all(|v| v.churn_rank.is_none()));
    }
}
