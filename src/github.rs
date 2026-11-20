//! GitHub access through the user's authenticated `gh` CLI.
//! Always invoke `gh` with an argv array, never through a shell.
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::process::Command;

use crate::model::{ChangeKind, ChangedFile, PullRequest};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrRef {
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

impl PrRef {
    pub fn url(&self) -> String {
        format!(
            "https://github.com/{}/{}/pull/{}",
            self.owner, self.repo, self.number
        )
    }
}

/// Parse `https://github.com/<owner>/<repo>/pull/<n>[/...]`.
pub fn parse_pr_url(input: &str) -> Result<PrRef> {
    let rest = input
        .strip_prefix("https://")
        .or_else(|| input.strip_prefix("http://"))
        .context("PR URL must start with https://")?;
    let mut parts = rest.split(['/', '?', '#']);
    let host = parts.next().unwrap_or_default();
    if !host.eq_ignore_ascii_case("github.com") && !host.eq_ignore_ascii_case("www.github.com") {
        bail!("unsupported host `{host}`: only github.com is supported in v1");
    }
    let owner = parts
        .next()
        .filter(|s| !s.is_empty())
        .context("missing owner")?;
    let repo = parts
        .next()
        .filter(|s| !s.is_empty())
        .context("missing repository")?;
    if parts.next() != Some("pull") {
        bail!("URL is not a pull request URL (expected /pull/<number>)");
    }
    let number = parts
        .next()
        .and_then(|n| n.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .context("missing or invalid PR number")?;
    Ok(PrRef {
        owner: owner.into(),
        repo: repo.into(),
        number,
    })
}

pub fn check_gh() -> Result<()> {
    let status = Command::new("gh")
        .args(["auth", "status"])
        .output()
        .context("`gh` not found on PATH; install the GitHub CLI")?;
    if !status.status.success() {
        bail!("`gh` is not authenticated; run `gh auth login`");
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPr {
    number: u64,
    title: String,
    url: String,
    author: GhAuthor,
    base_ref_name: String,
    head_ref_name: String,
    head_ref_oid: String,
    #[serde(default)]
    files: Vec<GhFile>,
}

#[derive(Deserialize)]
struct GhAuthor {
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhFile {
    path: String,
    additions: u64,
    deletions: u64,
    #[serde(default)]
    change_type: Option<String>,
}

pub fn fetch_pr(pr: &PrRef) -> Result<(PullRequest, Vec<ChangedFile>)> {
    let out = Command::new("gh")
        .args([
            "pr",
            "view",
            &pr.url(),
            "--json",
            "number,title,url,author,baseRefName,headRefName,headRefOid,files",
        ])
        .output()
        .context("failed to run `gh pr view`")?;
    if !out.status.success() {
        bail!(
            "`gh pr view` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let gh: GhPr = serde_json::from_slice(&out.stdout).context("unexpected `gh pr view` JSON")?;

    let files = gh
        .files
        .into_iter()
        .enumerate()
        .map(|(i, f)| ChangedFile {
            id: format!("f{}", i + 1),
            path: f.path,
            previous_path: None,
            change_kind: match f.change_type.as_deref() {
                Some("ADDED") => ChangeKind::Added,
                Some("DELETED") => ChangeKind::Removed,
                Some("RENAMED") => ChangeKind::Renamed,
                _ => ChangeKind::Modified,
            },
            additions: f.additions,
            deletions: f.deletions,
        })
        .collect();

    let pull = PullRequest {
        owner: pr.owner.clone(),
        repo: pr.repo.clone(),
        number: gh.number,
        title: gh.title,
        author: gh.author.login,
        url: gh.url,
        base_ref: gh.base_ref_name,
        head_ref: gh.head_ref_name,
        head_sha: gh.head_ref_oid,
    };
    Ok((pull, files))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pr_url() {
        let pr = parse_pr_url("https://github.com/acme/widget/pull/42/files").unwrap();
        assert_eq!(
            pr,
            PrRef {
                owner: "acme".into(),
                repo: "widget".into(),
                number: 42
            }
        );
    }

    #[test]
    fn rejects_other_hosts_and_paths() {
        assert!(parse_pr_url("https://gitlab.com/a/b/pull/1").is_err());
        assert!(parse_pr_url("https://github.com/a/b/issues/1").is_err());
        assert!(parse_pr_url("https://github.com/a/b/pull/x").is_err());
    }
}
