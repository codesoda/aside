//! Adapts the deterministic diff-facts engine (#19, [`crate::diff::facts`])
//! to the provider contract (#22).

use anyhow::{Context, Result, bail};
use futures_util::{FutureExt, future::BoxFuture};

use crate::{
    analysis::{
        AnalysisProvider, ProviderContext, ProviderDescriptor, ProviderKind, ProviderOutput,
    },
    diff::facts::{self, DiffFacts, DiffFactsConfig, DiffFactsInput},
    model::{AnalysisMode, Completeness, RunStatus},
    store::Raw,
    util::now_rfc3339,
};

/// Runs [`facts::scan`] over the pinned snapshot.
pub struct DiffFactsProvider {
    descriptor: ProviderDescriptor,
    config: DiffFactsConfig,
}

impl DiffFactsProvider {
    pub fn new(config: DiffFactsConfig) -> Self {
        Self {
            descriptor: ProviderDescriptor {
                id: facts::PROVIDER_ID.into(),
                kind: ProviderKind::Diff,
                engine_version: facts::ENGINE_VERSION.into(),
                configuration_digest: config.digest(),
                mode: AnalysisMode::Static,
                // Scheduler defaults apply.
                limits: None,
            },
            config,
        }
    }
}

impl Default for DiffFactsProvider {
    fn default() -> Self {
        Self::new(DiffFactsConfig::default())
    }
}

impl AnalysisProvider for DiffFactsProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn run(&self, ctx: ProviderContext) -> BoxFuture<'static, Result<ProviderOutput>> {
        let config = self.config.clone();
        async move {
            // The scheduler already skips over-budget input before calling us;
            // re-check so the bound holds even if this is driven directly.
            let input = &ctx.input;
            if input.input_bytes() > ctx.limits.max_input_bytes
                || input.input_files() > ctx.limits.max_input_files
            {
                bail!("input exceeds provider limits");
            }
            // `scan` is synchronous and linear in diff size; large PRs could
            // stall a runtime worker, so it runs on the blocking pool as the
            // scheduler's contract asks. Dropping this future on cancel or
            // timeout detaches the (pure, side-effect-free) scan.
            let observed_at = now_rfc3339();
            let facts = tokio::task::spawn_blocking(move || {
                let input = &ctx.input;
                facts::scan(
                    &DiffFactsInput {
                        snapshot: &input.snapshot,
                        snapshot_id: &input.snapshot_id,
                        canonical_repo_id: &input.canonical_repo_id,
                        run_id: Some(&ctx.run_id),
                        observed_at: &observed_at,
                    },
                    &config,
                )
            })
            .await
            .context("diff facts scan did not complete")?;
            into_output(facts)
        }
        .boxed()
    }
}

/// `partial` whenever #19 reports anything short of complete coverage, so a
/// scan with unscanned files never presents as a full one (analyze.md §4.4).
fn into_output(facts: DiffFacts) -> Result<ProviderOutput> {
    let status = match facts.coverage.completeness {
        Completeness::Complete => RunStatus::Succeeded,
        _ => RunStatus::Partial,
    };
    Ok(ProviderOutput {
        status,
        coverage: facts.coverage,
        warnings: facts.warnings,
        findings: facts
            .findings
            .into_iter()
            .map(Raw::new)
            .collect::<Result<_>>()?,
        evidence: facts
            .evidence
            .into_iter()
            .map(Raw::new)
            .collect::<Result<_>>()?,
        relations: Vec::new(),
    })
}
