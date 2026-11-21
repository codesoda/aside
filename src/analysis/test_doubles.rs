//! Test doubles for the provider contract. They replay the offline #17
//! fixture (`fixtures/analysis/demo-cli-14921/`) or misbehave on purpose.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use anyhow::{Result, bail};
use futures_util::{FutureExt, future::BoxFuture};
use tokio::sync::{Notify, Semaphore};

use super::{
    AnalysisProvider, ProviderContext, ProviderDescriptor, ProviderKind, ProviderLimits,
    ProviderOutput,
};
use crate::{
    demo::intelligence,
    model::{AnalysisCoverage, AnalysisMode, Completeness, KnownCount, RunStatus},
    store::Raw,
};

/// Fixture provider IDs, one per engine family the fixture covers.
pub const FEEDBACK: &str = "github.import";
pub const DIFF: &str = "aside.static";
pub const HISTORY: &str = "aside.git-history";

pub enum Behaviour {
    /// Return the fixture's findings for this provider ID.
    Fixture,
    /// Signal `started`, wait for a permit from `gate`, then behave like
    /// `Fixture`. Lets a test hold a provider in flight deterministically.
    Gated {
        gate: Arc<Semaphore>,
        started: Arc<Notify>,
    },
    Panic,
    Hang,
    Error,
    /// Return more findings than `max_findings` allows.
    Flood(usize),
}

pub struct Double {
    descriptor: ProviderDescriptor,
    behaviour: Behaviour,
    /// Number of times `run` was entered.
    pub calls: Arc<AtomicUsize>,
}

impl Double {
    pub fn new(id: &str, kind: ProviderKind, behaviour: Behaviour) -> Arc<Self> {
        Self::with_limits(id, kind, behaviour, None)
    }

    pub fn with_limits(
        id: &str,
        kind: ProviderKind,
        behaviour: Behaviour,
        limits: Option<ProviderLimits>,
    ) -> Arc<Self> {
        Arc::new(Self {
            descriptor: ProviderDescriptor {
                id: id.into(),
                kind,
                engine_version: "test-1".into(),
                configuration_digest: "cfg".into(),
                mode: if id == FEEDBACK {
                    AnalysisMode::GithubImport
                } else {
                    AnalysisMode::Static
                },
                limits,
            },
            behaviour,
            calls: Arc::new(AtomicUsize::new(0)),
        })
    }

    pub fn gated(id: &str, kind: ProviderKind) -> (Arc<Self>, Arc<Semaphore>, Arc<Notify>) {
        let gate = Arc::new(Semaphore::new(0));
        let started = Arc::new(Notify::new());
        let double = Self::new(
            id,
            kind,
            Behaviour::Gated {
                gate: Arc::clone(&gate),
                started: Arc::clone(&started),
            },
        );
        (double, gate, started)
    }
}

/// The fixture's findings and evidence for `provider_id`, re-pinned to the
/// run and snapshot in `ctx`.
pub fn fixture_output(provider_id: &str, ctx: &ProviderContext) -> Result<ProviderOutput> {
    let bundle = intelligence::load(false);
    let mut findings = Vec::new();
    let mut evidence = Vec::new();
    for mut finding in bundle.findings {
        if finding.source.provider_id != provider_id
            || finding.observed_snapshot_id != bundle.manifest.snapshot_id
        {
            continue;
        }
        finding.observed_snapshot_id = ctx.input.snapshot_id.clone();
        finding.source.run_id = Some(ctx.run_id.clone());
        for id in &finding.evidence_ids {
            if let Some(ev) = bundle.evidence.iter().find(|e| &e.id == id) {
                let mut ev = ev.clone();
                ev.source_run_id = Some(ctx.run_id.clone());
                evidence.push(Raw::new(ev)?);
            }
        }
        findings.push(Raw::new(finding)?);
    }
    let files = ctx.input.input_files() as u32;
    Ok(ProviderOutput {
        status: RunStatus::Succeeded,
        coverage: AnalysisCoverage {
            completeness: Completeness::Complete,
            eligible_files: KnownCount::Known { value: files },
            analyzed_files: KnownCount::Known { value: files },
            ..AnalysisCoverage::not_run()
        },
        warnings: Vec::new(),
        findings,
        evidence,
        relations: Vec::new(),
    })
}

impl AnalysisProvider for Double {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn run(&self, ctx: ProviderContext) -> BoxFuture<'static, Result<ProviderOutput>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let id = self.descriptor.id.clone();
        match &self.behaviour {
            Behaviour::Fixture => async move { fixture_output(&id, &ctx) }.boxed(),
            Behaviour::Gated { gate, started } => {
                let (gate, started) = (Arc::clone(gate), Arc::clone(started));
                async move {
                    started.notify_one();
                    gate.acquire().await?.forget();
                    fixture_output(&id, &ctx)
                }
                .boxed()
            }
            Behaviour::Panic => panic!("deliberate provider panic"),
            Behaviour::Hang => futures_util::future::pending().boxed(),
            Behaviour::Error => async { bail!("deliberate provider error") }.boxed(),
            Behaviour::Flood(n) => {
                let n = *n;
                async move {
                    let mut out = fixture_output(DIFF, &ctx)?;
                    let one = out.findings[0].clone();
                    out.findings = vec![one; n];
                    Ok(out)
                }
                .boxed()
            }
        }
    }
}
