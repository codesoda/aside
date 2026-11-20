//! Review-intelligence analysis: the provider contract and the scheduler
//! that runs providers in the background without blocking review (#22).

mod provider;
mod providers;
mod scheduler;

#[cfg(test)]
pub(crate) mod test_doubles;

#[allow(unused_imports)] // TODO(#33): rest of the API is consumed over HTTP.
pub use provider::{
    AnalysisInput, AnalysisProvider, INPUT_DIGEST_DOMAIN, ProviderContext, ProviderDescriptor,
    ProviderKind, ProviderLimits, ProviderOutput,
};
#[allow(unused_imports)]
pub use scheduler::{Clock, RunHandle, RunReport, Scheduler, SchedulerConfig, SystemClock, reason};

use std::sync::Arc;

/// Engines registered for live and demo sessions.
///
/// Diff facts (#19). TODO(#23, #24, #25): GitHub feedback/CI imports.
/// TODO(#28): history. TODO(#31, #32): structural.
pub fn default_providers() -> Vec<Arc<dyn AnalysisProvider>> {
    vec![Arc::new(providers::DiffFactsProvider::default())]
}
