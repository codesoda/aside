//! Engines registered with the scheduler. Each wraps an engine that lives
//! elsewhere and adapts it to the [`AnalysisProvider`](super::AnalysisProvider)
//! contract.

mod diff;
mod history;
mod history_cochange;
#[cfg(test)]
mod history_cochange_tests;
#[cfg(test)]
mod history_tests;
mod structure;

pub use diff::DiffFactsProvider;
pub use history::HistoryProvider;
pub use structure::StructureProvider;

#[cfg(test)]
mod tests;
