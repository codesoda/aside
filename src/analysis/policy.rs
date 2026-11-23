//! User policy is local configuration, never an execution permission or secret.

use std::time::Duration;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use super::ProviderLimits;
use crate::model::stable_digest;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderPolicy {
    pub enabled: bool,
    pub timeout_ms: Option<u64>,
    pub max_input_bytes: Option<u64>,
    pub max_input_files: Option<usize>,
    pub max_findings: Option<usize>,
}

impl Default for ProviderPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_ms: None,
            max_input_bytes: None,
            max_input_files: None,
            max_findings: None,
        }
    }
}

impl ProviderPolicy {
    pub fn validate(&self) -> Result<()> {
        for (key, value, maximum) in [
            ("timeout_ms", self.timeout_ms, 600_000),
            ("max_input_bytes", self.max_input_bytes, 256 * 1024 * 1024),
            (
                "max_input_files",
                self.max_input_files.map(|v| v as u64),
                100_000,
            ),
            ("max_findings", self.max_findings.map(|v| v as u64), 10_000),
        ] {
            ensure!(
                value.is_none_or(|v| (1..=maximum).contains(&v)),
                "{key} must be between 1 and {maximum}"
            );
        }
        Ok(())
    }

    /// Partial overrides preserve every unspecified engine-specific limit.
    pub fn limits(&self, defaults: ProviderLimits) -> ProviderLimits {
        ProviderLimits {
            timeout: self
                .timeout_ms
                .map(Duration::from_millis)
                .unwrap_or(defaults.timeout),
            max_input_bytes: self.max_input_bytes.unwrap_or(defaults.max_input_bytes),
            max_input_files: self.max_input_files.unwrap_or(defaults.max_input_files),
            max_findings: self.max_findings.unwrap_or(defaults.max_findings),
        }
    }

    pub fn digest(&self, engine_digest: &str, limits: ProviderLimits) -> String {
        stable_digest(
            "aside-provider-policy/v1",
            &[
                Some(engine_digest),
                Some(if self.enabled { "enabled" } else { "disabled" }),
                Some(&limits.timeout.as_nanos().to_string()),
                Some(&limits.max_input_bytes.to_string()),
                Some(&limits.max_input_files.to_string()),
                Some(&limits.max_findings.to_string()),
            ],
        )
    }
}
