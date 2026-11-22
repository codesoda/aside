//! Global user config, stored as TOML in the OS config directory
//! (e.g. `~/Library/Application Support/aside/config.toml` on macOS,
//! `~/.config/aside/config.toml` on Linux). Set `ASIDE_CONFIG_DIR` to override.
//!
//! Keys use `module.setting` form, e.g. `aside set browser.auto_open false`.

use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub browser: BrowserConfig,
    pub ai: AiConfig,
    pub repositories: BTreeMap<String, RepositoryConfig>,
}

/// Opt-in only. Credentials remain with the selected CLI; never in the browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AiConfig {
    pub local_url: String,
    pub local_model: String,
    pub claude_enabled: bool,
    pub claude_model: String,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            local_url: "http://127.0.0.1:11434".into(),
            local_model: String::new(),
            claude_enabled: false,
            claude_model: "sonnet".into(),
        }
    }
}

use crate::analysis::ProviderPolicy;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RepositoryConfig {
    pub analysis: AnalysisConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AnalysisConfig {
    pub diff: ProviderPolicy,
    pub history: ProviderPolicy,
    pub structure: ProviderPolicy,
}

impl AnalysisConfig {
    pub fn policies(&self) -> HashMap<String, ProviderPolicy> {
        HashMap::from([
            (crate::diff::facts::PROVIDER_ID.into(), self.diff.clone()),
            ("aside.git-history".into(), self.history.clone()),
            ("aside.structure".into(), self.structure.clone()),
        ])
    }
}

/// Explicit host-qualified identity; never inferred from the current directory.
pub fn repository_key(value: &str) -> Result<String> {
    let parts: Vec<_> = value.split('/').collect();
    if parts.len() != 3
        || parts.iter().any(|s| {
            s.is_empty()
                || *s == "."
                || *s == ".."
                || !s
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        })
    {
        bail!("repository must be HOST/OWNER/REPO (not a URL or local path)");
    }
    Ok(value.to_ascii_lowercase())
}

pub const ANALYSIS_KEYS: &[&str] = &[
    "analysis.diff.enabled",
    "analysis.diff.timeout_ms",
    "analysis.diff.max_input_bytes",
    "analysis.diff.max_input_files",
    "analysis.diff.max_findings",
    "analysis.history.enabled",
    "analysis.history.timeout_ms",
    "analysis.history.max_input_bytes",
    "analysis.history.max_input_files",
    "analysis.history.max_findings",
    "analysis.structure.enabled",
    "analysis.structure.timeout_ms",
    "analysis.structure.max_input_bytes",
    "analysis.structure.max_input_files",
    "analysis.structure.max_findings",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrowserConfig {
    /// Open the review UI in the default browser on launch.
    pub auto_open: bool,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self { auto_open: true }
    }
}

/// Every settable key, with a short description for `aside get`.
pub const KEYS: &[(&str, &str)] = &[
    (
        "browser.auto_open",
        "Open the browser on launch (true/false)",
    ),
    (
        "ai.local_url",
        "Local Ollama URL (loopback IP only; restart Aside)",
    ),
    (
        "ai.local_model",
        "Installed local Ollama model (empty disables; restart Aside)",
    ),
    (
        "ai.claude_enabled",
        "Enable hosted Claude CLI Q&A (true/false; restart Aside)",
    ),
    ("ai.claude_model", "Claude model (restart Aside)"),
];

impl Config {
    pub fn validate(&self) -> Result<()> {
        for (key, repo) in &self.repositories {
            if repository_key(key)? != *key {
                bail!("repository config keys must be lowercase HOST/OWNER/REPO");
            }
            repo.analysis.diff.validate()?;
            repo.analysis.history.validate()?;
            repo.analysis.structure.validate()?;
        }
        Ok(())
    }

    pub fn analysis_for(&self, repository: &str) -> Result<AnalysisConfig> {
        Ok(self
            .repositories
            .get(&repository_key(repository)?)
            .map(|r| r.analysis.clone())
            .unwrap_or_default())
    }

    pub fn get_scoped(&self, key: &str, repo: Option<&str>) -> Result<String> {
        if !key.starts_with("analysis.") {
            if repo.is_some() {
                bail!("--repo is only valid for analysis settings");
            }
            return self.get(key);
        }
        let repo = repo.context("analysis settings require --repo HOST/OWNER/REPO")?;
        let config = self.analysis_for(repo)?;
        let (provider, field) = analysis_key(key)?;
        let policy = match provider {
            "diff" => &config.diff,
            "history" => &config.history,
            "structure" => &config.structure,
            _ => unreachable!("validated analysis key"),
        };
        Ok(match field {
            "enabled" => policy.enabled.to_string(),
            "timeout_ms" => policy
                .timeout_ms
                .map(|v| v.to_string())
                .unwrap_or("default".into()),
            "max_input_bytes" => policy
                .max_input_bytes
                .map(|v| v.to_string())
                .unwrap_or("default".into()),
            "max_input_files" => policy
                .max_input_files
                .map(|v| v.to_string())
                .unwrap_or("default".into()),
            "max_findings" => policy
                .max_findings
                .map(|v| v.to_string())
                .unwrap_or("default".into()),
            _ => unreachable!("validated analysis key"),
        })
    }

    pub fn set_scoped(&mut self, key: &str, value: &str, repo: Option<&str>) -> Result<()> {
        if !key.starts_with("analysis.") {
            if repo.is_some() {
                bail!("--repo is only valid for analysis settings");
            }
            return self.set(key, value);
        }
        let repo =
            repository_key(repo.context("analysis settings require --repo HOST/OWNER/REPO")?)?;
        let (provider, field) = analysis_key(key)?;
        // Validate a copy first: an invalid change never mutates existing settings.
        let mut config = self.repositories.get(&repo).cloned().unwrap_or_default();
        let policy = match provider {
            "diff" => &mut config.analysis.diff,
            "history" => &mut config.analysis.history,
            "structure" => &mut config.analysis.structure,
            _ => unreachable!("validated analysis key"),
        };
        match field {
            "enabled" => policy.enabled = parse_bool(key, value)?,
            "timeout_ms" => policy.timeout_ms = optional_number(value)?,
            "max_input_bytes" => policy.max_input_bytes = optional_number(value)?,
            "max_input_files" => policy.max_input_files = optional_number(value)?,
            "max_findings" => policy.max_findings = optional_number(value)?,
            _ => unreachable!("validated analysis key"),
        }
        policy.validate()?;
        self.repositories.insert(repo, config);
        Ok(())
    }

    pub fn get(&self, key: &str) -> Result<String> {
        Ok(match key {
            "browser.auto_open" => self.browser.auto_open.to_string(),
            "ai.local_url" => self.ai.local_url.clone(),
            "ai.local_model" => self.ai.local_model.clone(),
            "ai.claude_enabled" => self.ai.claude_enabled.to_string(),
            "ai.claude_model" => self.ai.claude_model.clone(),
            _ => return Err(unknown_key(key)),
        })
    }

    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        match key {
            "browser.auto_open" => self.browser.auto_open = parse_bool(key, value)?,
            "ai.local_url" => self.ai.local_url = value.to_owned(),
            "ai.local_model" => self.ai.local_model = value.to_owned(),
            "ai.claude_enabled" => self.ai.claude_enabled = parse_bool(key, value)?,
            "ai.claude_model" => self.ai.claude_model = value.to_owned(),
            _ => return Err(unknown_key(key)),
        }
        Ok(())
    }
}

fn analysis_key(key: &str) -> Result<(&str, &str)> {
    if !ANALYSIS_KEYS.contains(&key) {
        bail!(
            "unknown analysis setting; use `aside get --repo HOST/OWNER/REPO` to list supported keys"
        );
    }
    Ok(key
        .strip_prefix("analysis.")
        .expect("validated prefix")
        .split_once('.')
        .expect("validated key"))
}

fn optional_number<T: std::str::FromStr>(value: &str) -> Result<Option<T>> {
    if value == "default" {
        return Ok(None);
    }
    value
        .parse()
        .map(Some)
        .map_err(|_| anyhow::anyhow!("expected a positive integer or default"))
}

fn parse_bool(key: &str, value: &str) -> Result<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        _ => bail!("`{key}` expects true or false"),
    }
}

fn unknown_key(key: &str) -> anyhow::Error {
    let known: Vec<&str> = KEYS.iter().map(|(k, _)| *k).collect();
    anyhow::anyhow!(
        "unknown config key `{key}`. Known keys: {}",
        known.join(", ")
    )
}

pub fn dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("ASIDE_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    Ok(dirs::config_dir()
        .context("could not find the OS config directory")?
        .join("aside"))
}

pub fn path() -> Result<PathBuf> {
    Ok(dir()?.join("config.toml"))
}

pub fn load() -> Result<Config> {
    load_from(&path()?)
}

pub fn save(config: &Config) -> Result<()> {
    save_to(&path()?, config)
}

/// A missing file means "all defaults". A broken file is an error, so we
/// never silently ignore a setting the user made.
pub fn load_from(path: &Path) -> Result<Config> {
    match fs::read_to_string(path) {
        Ok(text) => {
            // TOML errors embed source snippets, which may contain a misplaced
            // credential. Never echo config contents into diagnostics.
            let config: Config = toml::from_str(&text).map_err(|_| {
                anyhow::anyhow!("invalid or unsupported config in {}", path.display())
            })?;
            config.validate()?;
            Ok(config)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(e).with_context(|| format!("could not read {}", path.display())),
    }
}

/// Write to a temp file and rename, so a crash never leaves a half-written config.
pub fn save_to(path: &Path, config: &Config) -> Result<()> {
    config.validate()?;
    let parent = path.parent().context("config path has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("could not create {}", parent.display()))?;
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, toml::to_string_pretty(config)?)
        .with_context(|| format!("could not write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("could not write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_auto_open() {
        assert!(Config::default().browser.auto_open);
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load_from(&dir.path().join("config.toml")).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn set_save_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/config.toml");
        let mut config = Config::default();
        config.set("browser.auto_open", "false").unwrap();
        save_to(&path, &config).unwrap();
        let loaded = load_from(&path).unwrap();
        assert!(!loaded.browser.auto_open);
        assert_eq!(loaded.get("browser.auto_open").unwrap(), "false");
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[browser]\n").unwrap();
        assert!(load_from(&path).unwrap().browser.auto_open);
    }

    #[test]
    fn ai_is_opt_in_and_round_trips_without_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut config = Config::default();
        assert!(config.ai.local_model.is_empty());
        assert!(!config.ai.claude_enabled);
        config
            .set("ai.local_url", "http://127.0.0.1:11434")
            .unwrap();
        config.set("ai.local_model", "local-test-model").unwrap();
        config.set("ai.claude_enabled", "true").unwrap();
        config.set("ai.claude_model", "sonnet").unwrap();
        save_to(&path, &config).unwrap();
        assert_eq!(load_from(&path).unwrap(), config);
        assert_eq!(config.get("ai.local_model").unwrap(), "local-test-model");
        assert_eq!(config.get("ai.claude_enabled").unwrap(), "true");
        assert!(config.set("ai.api_key", "never-stored").is_err());
        assert!(config.set("ai.claude_enabled", "maybe").is_err());
    }

    #[test]
    fn rejects_bad_keys_and_values() {
        let mut config = Config::default();
        assert!(config.set("browser.nope", "true").is_err());
        assert!(config.set("browser.auto_open", "maybe").is_err());
        assert!(config.get("nope").is_err());
    }

    #[test]
    fn repository_settings_roundtrip_and_require_explicit_scope() {
        let mut config = Config::default();
        assert!(
            config
                .set_scoped("analysis.diff.enabled", "false", None)
                .is_err()
        );
        config
            .set_scoped(
                "analysis.diff.enabled",
                "false",
                Some("github.com/Owner/Repo"),
            )
            .unwrap();
        config
            .set_scoped(
                "analysis.history.timeout_ms",
                "5000",
                Some("github.com/owner/repo"),
            )
            .unwrap();
        config
            .set_scoped(
                "analysis.structure.enabled",
                "false",
                Some("github.com/owner/repo"),
            )
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        save_to(&path, &config).unwrap();
        let loaded = load_from(&path).unwrap();
        assert_eq!(config, loaded);
        assert!(
            !loaded
                .analysis_for("github.com/owner/repo")
                .unwrap()
                .structure
                .enabled
        );
        assert!(
            loaded
                .analysis_for("github.com/other/repo")
                .unwrap()
                .structure
                .enabled
        );
        assert!(
            loaded
                .analysis_for("github.com/owner/repo")
                .unwrap()
                .policies()
                .contains_key("aside.structure")
        );
        assert!(
            !loaded
                .analysis_for("github.com/OWNER/REPO")
                .unwrap()
                .diff
                .enabled
        );
        assert!(
            loaded
                .analysis_for("enterprise.test/owner/repo")
                .unwrap()
                .diff
                .enabled
        );
        assert!(
            loaded
                .analysis_for("github.com/other/repo")
                .unwrap()
                .diff
                .enabled
        );
        assert_eq!(
            loaded
                .analysis_for("github.com/owner/repo")
                .unwrap()
                .history
                .timeout_ms,
            Some(5000)
        );
        assert_eq!(
            loaded
                .analysis_for("github.com/owner/repo")
                .unwrap()
                .history
                .max_input_bytes,
            None
        );
        assert!(
            loaded
                .analysis_for("github.com/owner/repo")
                .unwrap()
                .policies()
                .contains_key(crate::diff::facts::PROVIDER_ID)
        );
        config
            .set_scoped(
                "analysis.history.timeout_ms",
                "default",
                Some("github.com/owner/repo"),
            )
            .unwrap();
        assert_eq!(
            config
                .get_scoped("analysis.history.timeout_ms", Some("github.com/owner/repo"))
                .unwrap(),
            "default"
        );
    }

    #[test]
    fn malformed_or_secret_settings_are_rejected_without_mutation() {
        let mut config = Config::default();
        for (key, value) in [
            ("analysis.diff.timeout_ms", "0"),
            ("analysis.history.timeout_ms", "600001"),
            ("analysis.diff.max_input_bytes", "-1"),
            ("analysis.history.max_input_files", "100001"),
            ("analysis.diff.max_findings", "10001"),
            ("analysis.history.token", "secret"),
            ("analysis.structure.timeout_ms", "0"),
            ("analysis.structure.token", "secret"),
            ("analysis.model.enabled", "true"),
            ("analysis.diff.enabled", "maybe"),
        ] {
            assert!(
                config
                    .set_scoped(key, value, Some("github.com/o/r"))
                    .is_err(),
                "{key}"
            );
            assert_eq!(config, Config::default());
        }
        for key in [
            "o/r",
            "https://github.com/o/r",
            "github.com/o/..",
            "github.com//r",
            "/tmp/repo",
        ] {
            assert!(repository_key(key).is_err());
        }
        assert!(
            config
                .set_scoped("browser.auto_open", "false", Some("github.com/o/r"))
                .is_err()
        );
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        for text in [
            "[repositories.\"github.com/o/r\".analysis.history]\ntoken = 'secret'\n",
            "[repositories.\"github.com/o/r\".analysis.history]\ntimeout_ms = 0\n",
            "[repositories.\"github.com/O/R\".analysis.diff]\nenabled = false\n",
            "[repositories.\"github.com/o/r\".analysis.model]\nenabled = true\n",
        ] {
            fs::write(&path, text).unwrap();
            let error = load_from(&path).unwrap_err();
            assert!(!format!("{error:#}").contains("secret"));
        }
    }

    #[test]
    fn partial_limits_preserve_defaults_and_digest_effective_values() {
        use crate::analysis::ProviderLimits;
        let defaults = ProviderLimits {
            timeout: std::time::Duration::from_secs(17),
            ..ProviderLimits::default()
        };
        let policy = ProviderPolicy {
            max_input_files: Some(42),
            ..ProviderPolicy::default()
        };
        let effective = policy.limits(defaults);
        assert_eq!(effective.timeout, defaults.timeout);
        assert_eq!(effective.max_input_files, 42);
        assert_eq!(effective.max_input_bytes, defaults.max_input_bytes);
        let inherited = ProviderPolicy::default();
        let explicit = ProviderPolicy {
            timeout_ms: Some(17_000),
            ..ProviderPolicy::default()
        };
        assert_eq!(
            inherited.digest("engine", inherited.limits(defaults)),
            explicit.digest("engine", explicit.limits(defaults))
        );
        assert_ne!(
            policy.digest("engine", effective),
            inherited.digest("engine", defaults)
        );
    }

    #[test]
    fn invalid_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[browser]\nauto_open = \"yes please\"\n").unwrap();
        assert!(load_from(&path).is_err());
    }
}
