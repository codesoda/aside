use std::{
    io::Write,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

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

    pub fn api_base(&self) -> String {
        format!("repos/{}/{}/pulls/{}", self.owner, self.repo, self.number)
    }
}

/// Parse `https://github.com/<owner>/<repo>/pull/<n>[/...]`.
pub fn parse_pr_url(input: &str) -> Result<PrRef> {
    let rest = input
        .trim()
        .strip_prefix("https://")
        .or_else(|| input.trim().strip_prefix("http://"))
        .context("PR URL must start with https://")?;
    let mut parts = rest.split(['/', '?', '#']);
    let host = parts.next().unwrap_or_default();
    if !host.eq_ignore_ascii_case("github.com") && !host.eq_ignore_ascii_case("www.github.com") {
        bail!("unsupported host `{host}`: only github.com is supported in v1");
    }
    let valid = |s: &&str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    let owner = parts
        .next()
        .filter(valid)
        .context("missing or invalid owner")?;
    let repo = parts
        .next()
        .filter(valid)
        .context("missing or invalid repository")?;
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

/// Runs `gh` with arguments and optional stdin, returning stdout.
pub trait GhRunner: Send + Sync {
    fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>>;

    /// GraphQL requests may return usable data alongside errors and a nonzero exit.
    fn run_graphql(&self, args: &[&str]) -> Result<Vec<u8>> {
        self.run(args, None)
    }
}

pub struct SystemGh;

impl SystemGh {
    fn run_impl(&self, args: &[&str], stdin: Option<&[u8]>, graphql: bool) -> Result<Vec<u8>> {
        tracing::debug!(command = %args.first().copied().unwrap_or_default(), "running gh");
        let mut child = Command::new("gh")
            .args(args)
            .env("GH_PROMPT_DISABLED", "1")
            .env("NO_COLOR", "1")
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("`gh` not found on PATH; install the GitHub CLI")?;
        if let Some(input) = stdin {
            child.stdin.take().context("gh stdin")?.write_all(input)?;
        }
        let out = child.wait_with_output()?;
        output_stdout(out, graphql)
    }
}

impl GhRunner for SystemGh {
    fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>> {
        self.run_impl(args, stdin, false)
    }

    fn run_graphql(&self, args: &[&str]) -> Result<Vec<u8>> {
        self.run_impl(args, None, true)
    }
}

pub(super) fn output_stdout(out: std::process::Output, graphql: bool) -> Result<Vec<u8>> {
    if !out.status.success() {
        // Never log or quote stdout: it may contain private PR content.
        if graphql
            && serde_json::from_slice::<serde_json::Value>(&out.stdout)
                .ok()
                .is_some_and(|v| {
                    v.get("errors")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|e| !e.is_empty())
                        && v.get("data")
                            .and_then(serde_json::Value::as_object)
                            .is_some_and(|data| data.values().any(|value| !value.is_null()))
                })
        {
            return Ok(out.stdout);
        }
        return Err(classify_error(&String::from_utf8_lossy(&out.stderr)));
    }
    Ok(out.stdout)
}

/// Turn `gh` stderr into a clear, token-free error.
pub(super) fn classify_error(stderr: &str) -> anyhow::Error {
    let lower = stderr.to_lowercase();
    let first_line = stderr
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("unknown error")
        .trim();
    if lower.contains("gh auth login")
        || lower.contains("authentication")
        || lower.contains("http 401")
    {
        anyhow!("GitHub authentication failed; run `gh auth login`")
    } else if lower.contains("rate limit") {
        anyhow!("GitHub rate limit reached; try again later ({first_line})")
    } else if lower.contains("could not resolve")
        || lower.contains("http 404")
        || lower.contains("not found")
    {
        anyhow!("PR or repository not found, or you don't have access ({first_line})")
    } else {
        anyhow!("gh failed: {first_line}")
    }
}

pub fn check_gh(gh: &dyn GhRunner) -> Result<()> {
    gh.run(&["auth", "status"], None).map(|_| ()).map_err(|e| {
        anyhow!("`gh` is not ready: {e}. Install the GitHub CLI and run `gh auth login`")
    })
}

pub(super) fn run_graphql_json<T: for<'de> Deserialize<'de>>(
    gh: &dyn GhRunner,
    args: &[&str],
) -> Result<T> {
    let out = gh.run_graphql(args)?;
    serde_json::from_slice(&out).context("unexpected JSON from `gh api graphql`")
}

pub(super) fn run_json<T: for<'de> Deserialize<'de>>(
    gh: &dyn GhRunner,
    args: &[&str],
) -> Result<T> {
    let out = gh.run(args, None)?;
    serde_json::from_slice(&out)
        .with_context(|| format!("unexpected JSON from `gh {}`", args.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::output_stdout;
    use std::process::Command;

    #[test]
    fn nonzero_graphql_retains_only_valid_partial_data() {
        let mut out = Command::new("sh")
            .args(["-c", "printf '%s' \"$1\"; printf 'request failed' >&2; exit 1", "sh", "{\"data\":{\"repository\":{\"id\":\"R1\"}},\"errors\":[{\"message\":\"forbidden\"}]}"])
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(output_stdout(out.clone(), true).is_ok());
        assert!(
            output_stdout(out.clone(), false)
                .unwrap_err()
                .to_string()
                .contains("request failed")
        );
        for invalid in [
            br#"{"data":null,"errors":[{"message":"forbidden"}]}"#.as_slice(),
            br#"{"data":{"repository":null},"errors":[{"message":"forbidden"}]}"#,
            br#"{"data":{"repository":{}},"errors":[]}"#,
            b"not JSON",
        ] {
            out.stdout = invalid.to_vec();
            assert!(
                output_stdout(out.clone(), true)
                    .unwrap_err()
                    .to_string()
                    .contains("request failed")
            );
        }
    }
}
