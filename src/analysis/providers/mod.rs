//! Engines registered with the scheduler. Each wraps an engine that lives
//! elsewhere and adapts it to the [`AnalysisProvider`](super::AnalysisProvider)
//! contract.

mod diff;

pub use diff::DiffFactsProvider;

#[cfg(test)]
mod tests;
