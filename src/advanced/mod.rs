//! Local, read-only experiments. This dispatch never constructs a review session.

mod coverage;
mod git;
mod graph;
mod sarif;

use std::{
    fs, io,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Result, ensure};
use clap::Args;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use git::PinnedGit;

#[derive(Args)]
pub(crate) struct Options {
    #[arg(long)]
    git_dir: PathBuf,
    #[arg(long)]
    base: String,
    #[arg(long)]
    head: String,
    #[arg(long)]
    graph_reexports: bool,
    #[arg(long)]
    coverage: Option<PathBuf>,
    #[arg(long, requires = "repository")]
    sarif_head: Option<PathBuf>,
    #[arg(long, requires = "sarif_head")]
    sarif_base: Option<PathBuf>,
    #[arg(long, requires = "sarif_head")]
    sarif_head_metadata: Option<PathBuf>,
    #[arg(long, requires = "sarif_base")]
    sarif_base_metadata: Option<PathBuf>,
    /// Producer's repository identity. This is a local assertion, not verified with a host.
    #[arg(long)]
    repository: Option<String>,
}

type SourceResult = Result<Value, SourceError>;

#[derive(Debug)]
struct SourceError {
    state: &'static str,
    reason: &'static str,
}

impl SourceError {
    const fn new(state: &'static str, reason: &'static str) -> Self {
        Self { state, reason }
    }

    fn into_value(self) -> Value {
        json!({"state": self.state, "coverage": "unknown", "reasons": [self.reason],
               "limits": null, "provenance": null, "results": null})
    }
}

fn source(enabled: bool, run: impl FnOnce() -> SourceResult) -> Value {
    if enabled {
        run().unwrap_or_else(SourceError::into_value)
    } else {
        SourceError::new("disabled", "not_requested").into_value()
    }
}

pub(crate) fn run(options: Options) -> Result<()> {
    ensure!(
        crate::github::git_fallback::valid_sha(&options.base)
            && crate::github::git_fallback::valid_sha(&options.head),
        "--base and --head must be full 40-digit Git commit IDs"
    );
    let enabled =
        options.graph_reexports || options.coverage.is_some() || options.sarif_head.is_some();
    let git = PinnedGit::new(options.git_dir, &options.base, &options.head, enabled);
    let graph = source(options.graph_reexports, || graph::run(&git.fresh()));
    let coverage = source(options.coverage.is_some(), || {
        coverage::run(
            &git.fresh(),
            options.coverage.as_deref().expect("enabled coverage"),
        )
    });
    let sarif = source(options.sarif_head.is_some(), || {
        sarif::run(
            &git.fresh(),
            options
                .repository
                .as_deref()
                .expect("SARIF requires repository"),
            options.sarif_head.as_deref().expect("enabled SARIF"),
            options.sarif_base.as_deref(),
            options.sarif_head_metadata.as_deref(),
            options.sarif_base_metadata.as_deref(),
        )
    });
    let report = json!({
        "schema": "aside-advanced-sources/v0",
        "baseTip": git.base_tip,
        "head": git.head,
        "comparisonBase": git.comparison_base,
        "comparisonReasons": git.comparison_reason.into_iter().collect::<Vec<_>>(),
        "githubPermissions": "not_requested",
        "sources": {"graph": graph, "coverage": coverage, "sarif": sarif}
    });
    serde_json::to_writer(io::stdout().lock(), &report)?;
    println!();
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn valid_repo_path(path: &str) -> bool {
    // A portable subset of Git paths. In particular, reject drive/URI prefixes,
    // backslash separators, and path components that can escape a repository.
    !path.is_empty()
        && !path.contains(['\\', ':'])
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn artifact_io(error: io::Error) -> SourceError {
    match error.kind() {
        io::ErrorKind::PermissionDenied => {
            SourceError::new("forbidden", "artifact_permission_denied")
        }
        io::ErrorKind::NotFound => SourceError::new("unavailable", "artifact_not_found"),
        _ => SourceError::new("unavailable", "artifact_read_failed"),
    }
}

fn read_artifact(path: &Path, max_bytes: usize) -> Result<Vec<u8>, SourceError> {
    let initial = fs::symlink_metadata(path).map_err(artifact_io)?;
    if !initial.is_file() {
        return Err(SourceError::new("invalid", "artifact_not_regular"));
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    // Do not follow a final symlink or block opening a FIFO swapped in after
    // the initial check. Recheck the opened handle before reading its bytes.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        #[cfg(target_os = "linux")]
        options.custom_flags(0x20000 | 0x800); // O_NOFOLLOW | O_NONBLOCK
        #[cfg(target_os = "macos")]
        options.custom_flags(0x100 | 0x4); // O_NOFOLLOW | O_NONBLOCK
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options.open(path).map_err(artifact_io)?;
    if !file.metadata().map_err(artifact_io)?.is_file() {
        return Err(SourceError::new("invalid", "artifact_not_regular"));
    }
    let mut bytes = Vec::new();
    file.take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(artifact_io)?;
    if bytes.len() > max_bytes {
        return Err(SourceError::new("partial", "artifact_byte_limit"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_read_is_bounded_and_absence_is_not_empty() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("artifact");
        assert_eq!(read_artifact(&file, 4).unwrap_err().state, "unavailable");
        fs::write(&file, b"abcd").unwrap();
        assert_eq!(read_artifact(&file, 4).unwrap(), b"abcd");
        assert_eq!(
            read_artifact(&file, 3).unwrap_err().reason,
            "artifact_byte_limit"
        );
        assert_eq!(
            read_artifact(dir.path(), 4).unwrap_err().reason,
            "artifact_not_regular"
        );
        assert_eq!(
            artifact_io(io::ErrorKind::PermissionDenied.into()).state,
            "forbidden"
        );
    }

    #[cfg(unix)]
    #[test]
    fn artifact_final_symlink_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("artifact");
        let link = dir.path().join("link");
        fs::write(&file, b"abcd").unwrap();
        std::os::unix::fs::symlink(file, &link).unwrap();
        assert_eq!(
            read_artifact(&link, 4).unwrap_err().reason,
            "artifact_not_regular"
        );
    }
}
