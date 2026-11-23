//! Configuration commands are offline and never require a live review session.
use std::{
    path::Path,
    process::{Command, Output},
};

fn run(config: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aside"))
        .env("ASIDE_CONFIG_DIR", config)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn repository_cli_roundtrips_and_does_not_change_global_defaults() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path();
    let set = run(
        config,
        &[
            "set",
            "analysis.history.enabled",
            "false",
            "--repo",
            "github.com/Owner/Repo",
        ],
    );
    assert!(set.status.success(), "{set:?}");
    let get = run(
        config,
        &[
            "get",
            "analysis.history.enabled",
            "--repo",
            "github.com/owner/repo",
        ],
    );
    assert!(get.status.success());
    assert_eq!(String::from_utf8(get.stdout).unwrap(), "false\n");
    let other = run(
        config,
        &[
            "get",
            "analysis.history.enabled",
            "--repo",
            "other.host/owner/repo",
        ],
    );
    assert_eq!(String::from_utf8(other.stdout).unwrap(), "true\n");
    let structure = run(
        config,
        &[
            "set",
            "analysis.structure.enabled",
            "false",
            "--repo",
            "github.com/owner/repo",
        ],
    );
    assert!(structure.status.success(), "{structure:?}");
    let structure = run(
        config,
        &[
            "get",
            "analysis.structure.enabled",
            "--repo",
            "github.com/owner/repo",
        ],
    );
    assert_eq!(String::from_utf8(structure.stdout).unwrap(), "false\n");
    let global = run(config, &["get", "browser.auto_open"]);
    assert_eq!(String::from_utf8(global.stdout).unwrap(), "true\n");
    let all = run(config, &["get", "--repo", "github.com/owner/repo"]);
    assert!(all.status.success());
    assert!(
        String::from_utf8(all.stdout)
            .unwrap()
            .contains("analysis.structure.timeout_ms = default")
    );
}

#[test]
fn invalid_cli_settings_never_write_or_echo_values() {
    let root = tempfile::tempdir().unwrap();
    for args in [
        vec!["set", "analysis.history.enabled", "false"],
        vec![
            "set",
            "analysis.history.timeout_ms",
            "0",
            "--repo",
            "github.com/o/r",
        ],
        vec![
            "set",
            "analysis.structure.token",
            "credential-canary",
            "--repo",
            "github.com/o/r",
        ],
        vec![
            "set",
            "analysis.history.token",
            "credential-canary",
            "--repo",
            "github.com/o/r",
        ],
        vec![
            "set",
            "analysis.history.enabled",
            "credential-canary",
            "--repo",
            "github.com/o/r",
        ],
    ] {
        let output = run(root.path(), &args);
        assert!(!output.status.success());
        assert!(
            !String::from_utf8(output.stderr)
                .unwrap()
                .contains("credential-canary")
        );
        assert!(!root.path().join("config.toml").exists());
    }
}
