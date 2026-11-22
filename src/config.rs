//! Global user config, stored as TOML in the OS config directory
//! (e.g. `~/Library/Application Support/aside/config.toml` on macOS,
//! `~/.config/aside/config.toml` on Linux). Set `ASIDE_CONFIG_DIR` to override.
//!
//! Keys use `module.setting` form, e.g. `aside set browser.auto_open false`.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub browser: BrowserConfig,
    pub ai: AiConfig,
}

/// Opt-in only. Credentials remain with the selected CLI; never in the browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
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

fn parse_bool(key: &str, value: &str) -> Result<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        _ => bail!("`{key}` expects true or false, got `{value}`"),
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
            toml::from_str(&text).with_context(|| format!("invalid config in {}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(e).with_context(|| format!("could not read {}", path.display())),
    }
}

/// Write to a temp file and rename, so a crash never leaves a half-written config.
pub fn save_to(path: &Path, config: &Config) -> Result<()> {
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
    fn invalid_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[browser]\nauto_open = \"yes please\"\n").unwrap();
        assert!(load_from(&path).is_err());
    }
}
