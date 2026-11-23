//! Black-box contract tests for the opt-in, local-only advanced-source prototype.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

struct Fixture {
    root: tempfile::TempDir,
    initial: String,
    base_tip: String,
    head: String,
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        fs::write(repo.join("lib.rs"), "pub fn original() {}\n").unwrap();
        git(&repo, &["add", "lib.rs"]);
        git(&repo, &["commit", "-qm", "initial"]);
        let initial = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["checkout", "-qb", "base"]);
        fs::write(repo.join("base.rs"), "pub fn base_only() {}\n").unwrap();
        git(&repo, &["add", "base.rs"]);
        git(&repo, &["commit", "-qm", "base tip"]);
        let base_tip = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["checkout", "-qb", "head", &initial]);
        fs::write(
            repo.join("lib.rs"),
            "pub fn original() {}\npub fn added() {}\n",
        )
        .unwrap();
        git(&repo, &["add", "lib.rs"]);
        git(&repo, &["commit", "-qm", "head"]);
        let head = git(&repo, &["rev-parse", "HEAD"]);
        Self {
            root,
            initial,
            base_tip,
            head,
        }
    }

    fn repo(&self) -> PathBuf {
        self.root.path().join("repo")
    }

    fn run(&self, extra: &[&str]) -> Output {
        self.run_with_path(extra, None)
    }

    fn run_with_path(&self, extra: &[&str], path: Option<&std::ffi::OsStr>) -> Output {
        self.run_at(&self.base_tip, &self.head, extra, path)
    }

    fn run_at(
        &self,
        base: &str,
        head: &str,
        extra: &[&str],
        path: Option<&std::ffi::OsStr>,
    ) -> Output {
        let config = self.root.path().join("config");
        let data = self.root.path().join("aside-data");
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(&data).unwrap();
        // A review config is deliberately invalid: this command must dispatch before loading it.
        fs::write(config.join("config.toml"), "invalid = [\n").unwrap();
        let git_dir = self.repo().join(".git");
        let args = [
            "prototype-sources",
            "--git-dir",
            git_dir.to_str().unwrap(),
            "--base",
            base,
            "--head",
            head,
        ];
        let mut command = Command::new(env!("CARGO_BIN_EXE_aside"));
        if let Some(path) = path {
            command.env("PATH", path);
        }
        command
            .args(args)
            .args(extra)
            .env("ASIDE_CONFIG_DIR", config)
            .env("ASIDE_DATA_DIR", data)
            .env_remove("GH_TOKEN")
            .env_remove("GITHUB_TOKEN")
            .output()
            .unwrap()
    }

    fn coverage(&self) -> PathBuf {
        let path = self.root.path().join("coverage.json");
        let blob = fs::read(self.repo().join("lib.rs")).unwrap();
        let digest = hex::encode(Sha256::digest(blob));
        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "schema": "aside-coverage/v1", "commit": self.head,
                "tool": { "name": "fixture", "version": "1" },
                "files": [{ "path": "lib.rs", "sha256": digest,
                    "lines": [{ "line": 2, "hits": 0 }] }]
            }))
            .unwrap(),
        )
        .unwrap();
        path
    }
}

fn success_json(output: Output) -> Value {
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).expect("one JSON envelope on stdout")
}

#[test]
fn all_disabled_dispatches_without_review_config_or_storage_and_keeps_worktree() {
    let fixture = Fixture::new();
    let before = git(&fixture.repo(), &["status", "--porcelain"]);
    let fake_bin = fixture.root.path().join("fake-bin");
    fs::create_dir(&fake_bin).unwrap();
    let gh = fake_bin.join("gh");
    let called = fixture.root.path().join("gh-called");
    fs::write(
        &gh,
        format!("#!/bin/sh\ntouch '{}'\nexit 91\n", called.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = std::env::join_paths(
        std::iter::once(fake_bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let value = success_json(fixture.run_with_path(&[], Some(&path)));
    assert_eq!(value["schema"], "aside-advanced-sources/v0");
    assert_eq!(value["baseTip"], fixture.base_tip);
    assert_eq!(value["head"], fixture.head);
    assert!(value["comparisonBase"].is_null());
    assert_eq!(value["comparisonReasons"], json!(["not_requested"]));
    assert_eq!(value["githubPermissions"], "not_requested");
    for name in ["graph", "coverage", "sarif"] {
        let source = &value["sources"][name];
        assert_eq!(source["state"], "disabled", "{name}: {source}");
        assert_eq!(source["coverage"], "unknown", "{name}: {source}");
        for field in ["reasons", "limits", "provenance", "results"] {
            assert!(
                source.get(field).is_some(),
                "{name}.{field} missing: {source}"
            );
        }
    }
    assert!(!called.exists(), "prototype unexpectedly invoked gh");
    assert_eq!(git(&fixture.repo(), &["status", "--porcelain"]), before);
    assert!(
        fs::read_dir(fixture.root.path().join("aside-data"))
            .unwrap()
            .next()
            .is_none()
    );
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("config/config.toml")).unwrap(),
        "invalid = [\n"
    );
}

#[test]
fn malformed_sarif_does_not_suppress_valid_coverage_or_change_graph_default() {
    let fixture = Fixture::new();
    let coverage = fixture.coverage();
    let sarif = fixture.root.path().join("bad.sarif");
    fs::write(&sarif, "{not JSON").unwrap();
    let value = success_json(fixture.run(&[
        "--coverage",
        coverage.to_str().unwrap(),
        "--sarif-head",
        sarif.to_str().unwrap(),
        "--repository",
        "github.com/example/repo",
    ]));
    assert_eq!(value["baseTip"], fixture.base_tip);
    assert_eq!(value["comparisonBase"], fixture.initial);
    assert_eq!(value["sources"]["graph"]["state"], "disabled");
    assert_eq!(
        value["sources"]["coverage"]["state"], "available",
        "{value}"
    );
    assert_eq!(
        value["sources"]["coverage"]["coverage"],
        json!({"covered": 0, "uncovered": 1, "unknown": 0}),
        "{value}"
    );
    assert_eq!(
        value["sources"]["coverage"]["results"][0]["state"],
        "uncovered"
    );
    assert_ne!(value["sources"]["sarif"]["state"], "disabled", "{value}");
    assert_ne!(value["sources"]["sarif"]["state"], "available", "{value}");
    assert_eq!(value["githubPermissions"], "not_requested");
}

#[test]
fn invalid_commit_and_unavailable_comparison_fail_closed() {
    let fixture = Fixture::new();
    let bad_sha = fixture.run_at(&fixture.base_tip, "not-a-40-hex-sha", &[], None);
    assert!(!bad_sha.status.success(), "{bad_sha:?}");
    assert!(String::from_utf8_lossy(&bad_sha.stderr).contains("full 40-digit"));
    let coverage = fixture.coverage();
    let missing = success_json(fixture.run_at(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        &fixture.head,
        &["--coverage", coverage.to_str().unwrap()],
        None,
    ));
    assert!(missing["comparisonBase"].is_null(), "{missing}");
    assert!(
        missing["comparisonReasons"]
            .as_array()
            .is_some_and(|r| !r.is_empty())
    );
    assert_ne!(
        missing["sources"]["coverage"]["state"], "available",
        "{missing}"
    );
}

#[cfg(unix)]
#[test]
fn denied_local_artifact_is_not_missing_or_clean() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    let coverage = fixture.coverage();
    fs::set_permissions(&coverage, fs::Permissions::from_mode(0o000)).unwrap();
    // Root and privileged CI sandboxes can still open mode-000 files.
    if fs::File::open(&coverage).is_ok() {
        return;
    }
    let value = success_json(fixture.run(&["--coverage", coverage.to_str().unwrap()]));
    assert_eq!(
        value["sources"]["coverage"]["state"], "forbidden",
        "{value}"
    );
    assert_eq!(value["sources"]["coverage"]["coverage"], "unknown");
    assert_eq!(
        value["sources"]["coverage"]["reasons"],
        json!(["artifact_permission_denied"])
    );
}

#[test]
fn sarif_paired_exports_classify_against_the_pinned_merge_base() {
    let fixture = Fixture::new();
    let paired = |stem: &str, revision: &str, fingerprints: &[&str]| {
        let artifact = fixture.root.path().join(format!("{stem}.sarif"));
        let sidecar = fixture.root.path().join(format!("{stem}.json"));
        let source = git(&fixture.repo(), &["show", &format!("{revision}:lib.rs")]);
        let blob = format!("{source}\n"); // fixture lines are terminated by one newline
        let results: Vec<_> = fingerprints
            .iter()
            .map(|fp| {
                json!({
                    "ruleId":"synthetic", "fingerprints":{"stable":fp},
                    "locations":[{"physicalLocation":{"artifactLocation":{"uri":"lib.rs"}}}]
                })
            })
            .collect();
        let report = serde_json::to_vec(&json!({"version":"2.1.0", "runs":[{
            "tool":{"driver":{"name":"fixture","version":"1"}},
            "versionControlProvenance":[{"revisionId":revision}],
            "invocations":[{"executionSuccessful":true}], "results":results
        }]}))
        .unwrap();
        let metadata = json!({"schema_version":1, "repository":"example/repo", "commit":revision,
            "export_sha256":hex::encode(Sha256::digest(&report)), "analyzed_paths":[{
                "path":"lib.rs", "blob_sha256":hex::encode(Sha256::digest(blob))
            }], "tool":{"name":"fixture","version":"1"}, "enabled_rules":["synthetic"],
            "scanner_policy":"v1","config_policy":"v1","export_policy":"all",
            "suppression_policy":"none","fingerprint":{"key":"stable","version":"1"},
            "execution_complete":true,"export_complete":true});
        fs::write(&artifact, report).unwrap();
        fs::write(&sidecar, serde_json::to_vec(&metadata).unwrap()).unwrap();
        (artifact, sidecar)
    };
    let (base, base_meta) = paired("base", &fixture.initial, &["kept"]);
    let (head, head_meta) = paired("head", &fixture.head, &["kept", "new"]);
    let value = success_json(fixture.run(&[
        "--repository",
        "example/repo",
        "--sarif-head",
        head.to_str().unwrap(),
        "--sarif-head-metadata",
        head_meta.to_str().unwrap(),
        "--sarif-base",
        base.to_str().unwrap(),
        "--sarif-base-metadata",
        base_meta.to_str().unwrap(),
    ]));
    assert_eq!(value["comparisonBase"], fixture.initial);
    let source = &value["sources"]["sarif"];
    assert_eq!(source["coverage"], "complete", "{source}");
    assert_eq!(source["results"][0]["classification"], "existing");
    assert_eq!(source["results"][1]["classification"], "introduced");
}

#[test]
fn sarif_requires_explicit_repository_identity() {
    let fixture = Fixture::new();
    let path = fixture.root.path().join("head.sarif");
    fs::write(&path, "{}").unwrap();
    let output = fixture.run(&["--sarif-head", path.to_str().unwrap()]);
    assert!(!output.status.success(), "{output:?}");
}
