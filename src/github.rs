//! GitHub access through the user's authenticated `gh` CLI.
//!
//! `gh` is always run with an argv array, never through a shell. All calls go
//! through [`GhRunner`] so tests can substitute a fake.

use std::{
    io::Write,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::diff::{self, ApiFile, Limits};
use crate::model::{
    GithubComment, GithubThread, ImportedReview, IssueComment, PrSnapshot, PullRequest,
    SCHEMA_VERSION, Side,
};
use crate::util::now_rfc3339;

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
}

pub struct SystemGh;

impl GhRunner for SystemGh {
    fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>> {
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
        if !out.status.success() {
            return Err(classify_error(&String::from_utf8_lossy(&out.stderr)));
        }
        Ok(out.stdout)
    }
}

/// Turn `gh` stderr into a clear, token-free error.
fn classify_error(stderr: &str) -> anyhow::Error {
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

fn run_json<T: for<'de> Deserialize<'de>>(gh: &dyn GhRunner, args: &[&str]) -> Result<T> {
    let out = gh.run(args, None)?;
    serde_json::from_slice(&out)
        .with_context(|| format!("unexpected JSON from `gh {}`", args.join(" ")))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPr {
    number: u64,
    title: String,
    #[serde(default)]
    body: String,
    url: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    is_draft: bool,
    author: Option<GhLogin>,
    base_ref_name: String,
    #[serde(default)]
    base_ref_oid: String,
    head_ref_name: String,
    head_ref_oid: String,
    #[serde(default)]
    changed_files: Option<u64>,
    #[serde(default)]
    reviews: Vec<GhReview>,
    #[serde(default)]
    comments: Vec<GhIssueComment>,
}

#[derive(Deserialize)]
struct GhLogin {
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhReview {
    author: Option<GhLogin>,
    #[serde(default)]
    state: String,
    #[serde(default)]
    body: String,
    submitted_at: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhIssueComment {
    author: Option<GhLogin>,
    #[serde(default)]
    body: String,
    created_at: Option<String>,
}

#[derive(Deserialize)]
struct GhApiFile {
    filename: String,
    previous_filename: Option<String>,
    status: String,
    patch: Option<String>,
    sha: Option<String>,
    #[serde(default)]
    additions: u32,
    #[serde(default)]
    deletions: u32,
}

fn login(a: &Option<GhLogin>) -> String {
    a.as_ref()
        .map(|l| l.login.clone())
        .unwrap_or_else(|| "ghost".into())
}

const PR_FIELDS: &str = "number,title,body,url,state,isDraft,author,baseRefName,baseRefOid,headRefName,headRefOid,changedFiles,reviews,comments";

/// Fetch everything Aside needs for one PR revision.
pub fn fetch_snapshot(gh: &dyn GhRunner, pr_ref: &PrRef, limits: &Limits) -> Result<PrSnapshot> {
    let url = pr_ref.url();
    let meta: GhPr = run_json(gh, &["pr", "view", &url, "--json", PR_FIELDS])?;
    let mut warnings = Vec::new();

    let provider_repository_id = fetch_repo_id(gh, pr_ref)
        .inspect_err(|e| warnings.push(format!("could not read repository ID: {e}")))
        .ok();

    let files = fetch_files(gh, pr_ref, &meta.head_ref_oid, limits, &mut warnings)?;
    let mut complete = files.iter().all(|f| f.patch_complete);
    if let Some(expected) = meta.changed_files
        && (files.len() as u64) < expected
    {
        warnings.push(format!(
            "GitHub reports {expected} changed files but only {} were fetched",
            files.len()
        ));
        complete = false;
    }

    let github_threads = fetch_threads(gh, pr_ref)
        .inspect_err(|e| warnings.push(format!("could not load review threads: {e}")))
        .unwrap_or_default();

    // The diff and paginated files API target a mutable PR number. A force push
    // or base update during ingestion must never yield a complete mixed revision.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Refs {
        base_ref_oid: String,
        head_ref_oid: String,
    }
    let refs: Refs = run_json(gh, &["pr", "view", &url, "--json", "baseRefOid,headRefOid"])
        .context("could not verify PR revision after import")?;
    if meta.base_ref_oid.is_empty()
        || meta.head_ref_oid.is_empty()
        || refs.base_ref_oid.is_empty()
        || refs.head_ref_oid.is_empty()
        || meta.base_ref_oid != refs.base_ref_oid
        || meta.head_ref_oid != refs.head_ref_oid
    {
        bail!("PR base or head changed during import; retry with the latest revision");
    }

    let pr = PullRequest {
        owner: pr_ref.owner.clone(),
        repo: pr_ref.repo.clone(),
        number: meta.number,
        title: meta.title,
        body: meta.body,
        author: login(&meta.author),
        url: meta.url,
        state: meta.state,
        is_draft: meta.is_draft,
        base_ref: meta.base_ref_name,
        base_sha: meta.base_ref_oid,
        head_ref: meta.head_ref_name,
        head_sha: meta.head_ref_oid,
    };
    Ok(PrSnapshot {
        schema_version: SCHEMA_VERSION,
        pr,
        provider_repository_id,
        files,
        github_threads,
        reviews: meta
            .reviews
            .iter()
            .map(|r| ImportedReview {
                author: login(&r.author),
                state: r.state.clone(),
                body: r.body.clone(),
                submitted_at: r.submitted_at.clone(),
            })
            .collect(),
        issue_comments: meta
            .comments
            .iter()
            .map(|c| IssueComment {
                author: login(&c.author),
                body: c.body.clone(),
                created_at: c.created_at.clone(),
            })
            .collect(),
        warnings,
        complete,
        fetched_at: now_rfc3339(),
    })
}

fn fetch_repo_id(gh: &dyn GhRunner, pr_ref: &PrRef) -> Result<String> {
    #[derive(Deserialize)]
    struct Repo {
        id: String,
    }
    let name = format!("{}/{}", pr_ref.owner, pr_ref.repo);
    Ok(run_json::<Repo>(gh, &["repo", "view", &name, "--json", "id"])?.id)
}

/// Aggregate diff from `gh pr diff`, falling back to the REST files API
/// when the diff is too large or unavailable.
fn fetch_files(
    gh: &dyn GhRunner,
    pr_ref: &PrRef,
    head_sha: &str,
    limits: &Limits,
    warnings: &mut Vec<String>,
) -> Result<Vec<crate::model::ChangedFile>> {
    match gh.run(&["pr", "diff", &pr_ref.url(), "--color", "never"], None) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            let mut parsed = diff::parse_unified(&text, head_sha, limits);
            warnings.append(&mut parsed.warnings);
            Ok(parsed.files)
        }
        Err(e) => {
            tracing::warn!("`gh pr diff` failed, using the files API: {e}");
            warnings.push(format!(
                "aggregate diff unavailable ({e}); used the per-file API"
            ));
            let pages: Vec<Vec<GhApiFile>> = run_json(
                gh,
                &[
                    "api",
                    "--paginate",
                    "--slurp",
                    &format!("{}/files?per_page=100", pr_ref.api_base()),
                ],
            )?;
            Ok(pages
                .iter()
                .flatten()
                .map(|f| {
                    diff::file_from_api(
                        &ApiFile {
                            path: &f.filename,
                            previous_path: f.previous_filename.as_deref(),
                            status: &f.status,
                            patch: f.patch.as_deref(),
                            blob_sha: f.sha.as_deref(),
                            additions: f.additions,
                            deletions: f.deletions,
                        },
                        head_sha,
                        limits,
                        warnings,
                    )
                })
                .collect())
        }
    }
}

const THREADS_QUERY: &str =
    "query($owner: String!, $repo: String!, $number: Int!, $cursor: String) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      reviewThreads(first: 100, after: $cursor) {
        pageInfo { hasNextPage endCursor }
        nodes {
          id isResolved isOutdated path line originalLine startLine diffSide
          comments(first: 100) {
            nodes { id databaseId body createdAt author { login } }
          }
        }
      }
    }
  }
}";

fn fetch_threads(gh: &dyn GhRunner, pr_ref: &PrRef) -> Result<Vec<GithubThread>> {
    let mut threads = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..50 {
        let query = format!("query={THREADS_QUERY}");
        let owner = format!("owner={}", pr_ref.owner);
        let repo = format!("repo={}", pr_ref.repo);
        let number = format!("number={}", pr_ref.number);
        let mut args = vec![
            "api", "graphql", "-f", &query, "-f", &owner, "-f", &repo, "-F", &number,
        ];
        let cursor_arg = cursor.as_ref().map(|c| format!("cursor={c}"));
        if let Some(c) = &cursor_arg {
            args.extend(["-f", c]);
        }
        let v: Value = run_json(gh, &args)?;
        let page = &v["data"]["repository"]["pullRequest"]["reviewThreads"];
        for node in page["nodes"].as_array().into_iter().flatten() {
            threads.push(parse_thread(node));
        }
        if page["pageInfo"]["hasNextPage"].as_bool() != Some(true) {
            return Ok(threads);
        }
        cursor = page["pageInfo"]["endCursor"].as_str().map(str::to_string);
    }
    bail!("too many review thread pages")
}

fn parse_thread(node: &Value) -> GithubThread {
    let num = |k: &str| node[k].as_u64().map(|n| n as u32);
    GithubThread {
        node_id: node["id"].as_str().unwrap_or_default().into(),
        path: node["path"].as_str().unwrap_or_default().into(),
        side: node["diffSide"].as_str().and_then(Side::parse),
        line: num("line"),
        start_line: num("startLine"),
        original_line: num("originalLine"),
        is_outdated: node["isOutdated"].as_bool().unwrap_or(false),
        is_resolved: node["isResolved"].as_bool().unwrap_or(false),
        comments: node["comments"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| GithubComment {
                node_id: c["id"].as_str().unwrap_or_default().into(),
                database_id: c["databaseId"].as_i64(),
                author: c["author"]["login"].as_str().unwrap_or("ghost").into(),
                body: c["body"].as_str().unwrap_or_default().into(),
                created_at: c["createdAt"].as_str().unwrap_or_default().into(),
            })
            .collect(),
    }
}

/// Current head SHA, read fresh right before publishing.
pub fn fetch_head_sha(gh: &dyn GhRunner, pr_ref: &PrRef) -> Result<String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Head {
        head_ref_oid: String,
    }
    Ok(run_json::<Head>(gh, &["pr", "view", &pr_ref.url(), "--json", "headRefOid"])?.head_ref_oid)
}

/// One inline comment inside a new GitHub review.
#[derive(Debug, Clone)]
pub struct NewReviewComment {
    pub path: String,
    pub side: Side,
    pub line: u32,
    pub start_line: Option<u32>,
    pub body: String,
}

/// Create a review in one API call. Returns the review's database ID.
pub fn create_review(
    gh: &dyn GhRunner,
    pr_ref: &PrRef,
    commit_id: &str,
    event: &str,
    body: &str,
    comments: &[NewReviewComment],
) -> Result<i64> {
    let comments: Vec<Value> = comments
        .iter()
        .map(|c| {
            let mut v =
                json!({ "path": c.path, "side": c.side.as_str(), "line": c.line, "body": c.body });
            if let Some(start) = c.start_line {
                v["start_line"] = json!(start);
                v["start_side"] = json!(c.side.as_str());
            }
            v
        })
        .collect();
    let payload =
        json!({ "commit_id": commit_id, "event": event, "body": body, "comments": comments });
    let out = gh.run(
        &[
            "api",
            "--method",
            "POST",
            &format!("{}/reviews", pr_ref.api_base()),
            "--input",
            "-",
        ],
        Some(payload.to_string().as_bytes()),
    )?;
    let v: Value = serde_json::from_slice(&out).context("unexpected JSON from create review")?;
    v["id"].as_i64().context("create review response has no id")
}

/// A created review comment: (database id, path, line, body).
pub type ReviewCommentRecord = (i64, String, Option<u32>, String);

pub fn review_comments(
    gh: &dyn GhRunner,
    pr_ref: &PrRef,
    review_id: i64,
) -> Result<Vec<ReviewCommentRecord>> {
    let pages: Vec<Vec<Value>> = run_json(
        gh,
        &[
            "api",
            "--paginate",
            "--slurp",
            &format!(
                "{}/reviews/{review_id}/comments?per_page=100",
                pr_ref.api_base()
            ),
        ],
    )?;
    Ok(pages
        .iter()
        .flatten()
        .filter_map(|c| {
            Some((
                c["id"].as_i64()?,
                c["path"].as_str()?.to_string(),
                c["line"].as_u64().map(|n| n as u32),
                c["body"].as_str().unwrap_or_default().to_string(),
            ))
        })
        .collect())
}

/// Reply to an existing review comment. Returns the new comment's database ID.
pub fn reply_to_comment(
    gh: &dyn GhRunner,
    pr_ref: &PrRef,
    comment_id: i64,
    body: &str,
) -> Result<i64> {
    let payload = json!({ "body": body });
    let out = gh.run(
        &[
            "api",
            "--method",
            "POST",
            &format!("{}/comments/{comment_id}/replies", pr_ref.api_base()),
            "--input",
            "-",
        ],
        Some(payload.to_string().as_bytes()),
    )?;
    let v: Value = serde_json::from_slice(&out).context("unexpected JSON from reply")?;
    v["id"].as_i64().context("reply response has no id")
}

#[cfg(test)]
pub mod fake {
    //! Scripted `gh` for tests. Never touches the network.
    use super::*;
    use std::sync::Mutex;

    type ScriptedResponse = (Vec<String>, Result<Vec<u8>, String>);

    #[derive(Default)]
    pub struct FakeGh {
        /// (argv prefix, response). First matching prefix wins.
        pub responses: Mutex<Vec<ScriptedResponse>>,
        pub calls: Mutex<Vec<(Vec<String>, Option<String>)>>,
    }

    impl FakeGh {
        pub fn on(&self, prefix: &[&str], response: Result<&str, &str>) {
            self.responses.lock().unwrap().push((
                prefix.iter().map(|s| s.to_string()).collect(),
                response
                    .map(|s| s.as_bytes().to_vec())
                    .map_err(str::to_string),
            ));
        }

        pub fn calls_matching(&self, prefix: &[&str]) -> Vec<(Vec<String>, Option<String>)> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(a, _)| {
                    a.starts_with(&prefix.iter().map(|s| s.to_string()).collect::<Vec<_>>())
                })
                .cloned()
                .collect()
        }
    }

    impl GhRunner for FakeGh {
        fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>> {
            let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
            self.calls.lock().unwrap().push((
                args.clone(),
                stdin.map(|s| String::from_utf8_lossy(s).into_owned()),
            ));
            let responses = self.responses.lock().unwrap();
            let found = responses
                .iter()
                .find(|(prefix, _)| args.starts_with(prefix));
            match found {
                Some((_, Ok(out))) => Ok(out.clone()),
                Some((_, Err(e))) => Err(anyhow!("{e}")),
                None => Err(anyhow!("fake gh: no response for {args:?}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeGh;
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
        assert!(parse_pr_url("https://github.com/a;rm/b/pull/1").is_err());
    }

    #[test]
    fn classifies_errors_without_echoing_secrets() {
        assert!(
            classify_error("To get started with GitHub CLI, please run:  gh auth login")
                .to_string()
                .contains("authentication")
        );
        assert!(
            classify_error("API rate limit exceeded")
                .to_string()
                .contains("rate limit")
        );
        assert!(
            classify_error("GraphQL: Could not resolve to a PullRequest")
                .to_string()
                .contains("not found")
        );
    }

    fn pr_json() -> String {
        json!({
            "number": 7, "title": "T", "body": "B", "url": "https://github.com/o/r/pull/7", "state": "OPEN",
            "isDraft": false, "author": {"login": "alice"}, "baseRefName": "main", "baseRefOid": "base1",
            "headRefName": "feat", "headRefOid": "head1", "changedFiles": 1,
            "reviews": [{"author": {"login": "bob"}, "state": "COMMENTED", "body": "hi", "submittedAt": "2026-01-01T00:00:00Z"}],
            "comments": [{"author": {"login": "carol"}, "body": "c", "createdAt": "2026-01-01T00:00:00Z"}]
        })
        .to_string()
    }

    fn threads_json() -> String {
        json!({"data": {"repository": {"pullRequest": {"reviewThreads": {
            "pageInfo": {"hasNextPage": false, "endCursor": null},
            "nodes": [{"id": "T1", "isResolved": false, "isOutdated": false, "path": "a.rs", "line": 1,
                "originalLine": 1, "startLine": null, "diffSide": "RIGHT",
                "comments": {"nodes": [{"id": "C1", "databaseId": 11, "body": "why?", "createdAt": "2026-01-01T00:00:00Z", "author": {"login": "bob"}}]}}]
        }}}}})
        .to_string()
    }

    #[test]
    fn fetches_snapshot_through_gh() {
        let gh = FakeGh::default();
        gh.on(&["pr", "view"], Ok(&pr_json()));
        gh.on(&["repo", "view"], Ok(r#"{"id":"R_1"}"#));
        gh.on(
            &["pr", "diff"],
            Ok("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n"),
        );
        gh.on(&["api", "graphql"], Ok(&threads_json()));
        let pr = PrRef {
            owner: "o".into(),
            repo: "r".into(),
            number: 7,
        };
        let snap = fetch_snapshot(&gh, &pr, &Limits::default()).unwrap();
        assert_eq!(snap.pr.head_sha, "head1");
        assert_eq!(snap.provider_repository_id.as_deref(), Some("R_1"));
        assert_eq!(snap.files.len(), 1);
        assert!(snap.complete);
        assert_eq!(snap.github_threads[0].comments[0].database_id, Some(11));
        assert_eq!(snap.reviews[0].author, "bob");
        assert!(snap.warnings.is_empty());
    }

    // A force push can happen after the metadata request, while the diff and
    // thread requests still return successful, same-sized responses.
    struct MovingPrGh {
        fake: FakeGh,
        views: std::sync::atomic::AtomicUsize,
        changed: String,
    }

    impl GhRunner for MovingPrGh {
        fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>> {
            if args.starts_with(&["pr", "view"])
                && self.views.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0
            {
                return Ok(self.changed.as_bytes().to_vec());
            }
            self.fake.run(args, stdin)
        }
    }

    #[test]
    fn rejects_force_push_between_metadata_and_diff_even_with_same_file_count() {
        let fake = FakeGh::default();
        fake.on(&["pr", "view"], Ok(&pr_json()));
        fake.on(&["repo", "view"], Ok(r#"{"id":"R_1"}"#));
        fake.on(
            &["pr", "diff"],
            Ok("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n"),
        );
        fake.on(&["api", "graphql"], Ok(&threads_json()));
        let mut changed: Value = serde_json::from_str(&pr_json()).unwrap();
        changed["headRefOid"] = json!("head2");
        let gh = MovingPrGh {
            fake,
            views: std::sync::atomic::AtomicUsize::new(0),
            changed: changed.to_string(),
        };
        let pr = PrRef {
            owner: "o".into(),
            repo: "r".into(),
            number: 7,
        };
        assert!(fetch_snapshot(&gh, &pr, &Limits::default()).is_err());
    }

    #[test]
    fn rejects_base_change_during_import() {
        let fake = FakeGh::default();
        fake.on(&["pr", "view"], Ok(&pr_json()));
        fake.on(&["repo", "view"], Ok(r#"{"id":"R_1"}"#));
        fake.on(
            &["pr", "diff"],
            Ok("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n"),
        );
        fake.on(&["api", "graphql"], Ok(&threads_json()));
        let mut changed: Value = serde_json::from_str(&pr_json()).unwrap();
        changed["baseRefOid"] = json!("base2");
        let gh = MovingPrGh {
            fake,
            views: std::sync::atomic::AtomicUsize::new(0),
            changed: changed.to_string(),
        };
        let pr = PrRef {
            owner: "o".into(),
            repo: "r".into(),
            number: 7,
        };
        assert!(
            fetch_snapshot(&gh, &pr, &Limits::default())
                .unwrap_err()
                .to_string()
                .contains("base or head changed")
        );
    }

    #[test]
    fn rejects_force_push_during_paginated_files_fallback() {
        let fake = FakeGh::default();
        fake.on(&["pr", "view"], Ok(&pr_json()));
        fake.on(&["repo", "view"], Ok(r#"{"id":"R_1"}"#));
        fake.on(&["pr", "diff"], Err("diff unavailable"));
        fake.on(&["api", "--paginate"], Ok(r#"[[{"filename":"a.rs","status":"modified","patch":"@@ -1 +1 @@\n-x\n+y","sha":"s1","additions":1,"deletions":1}]]"#));
        fake.on(&["api", "graphql"], Ok(&threads_json()));
        let mut changed: Value = serde_json::from_str(&pr_json()).unwrap();
        changed["headRefOid"] = json!("head2");
        let gh = MovingPrGh {
            fake,
            views: std::sync::atomic::AtomicUsize::new(0),
            changed: changed.to_string(),
        };
        let pr = PrRef {
            owner: "o".into(),
            repo: "r".into(),
            number: 7,
        };
        assert!(
            fetch_snapshot(&gh, &pr, &Limits::default())
                .unwrap_err()
                .to_string()
                .contains("base or head changed")
        );
        assert_eq!(gh.fake.calls_matching(&["api", "--paginate"]).len(), 1);
    }

    #[test]
    fn fails_closed_if_revision_cannot_be_rechecked() {
        let fake = FakeGh::default();
        fake.on(
            &[
                "pr",
                "view",
                "https://github.com/o/r/pull/7",
                "--json",
                PR_FIELDS,
            ],
            Ok(&pr_json()),
        );
        fake.on(&["repo", "view"], Ok(r#"{"id":"R_1"}"#));
        fake.on(
            &["pr", "diff"],
            Ok("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n"),
        );
        fake.on(&["api", "graphql"], Ok(&threads_json()));
        let pr = PrRef {
            owner: "o".into(),
            repo: "r".into(),
            number: 7,
        };
        assert!(
            fetch_snapshot(&fake, &pr, &Limits::default())
                .unwrap_err()
                .to_string()
                .contains("could not verify PR revision")
        );
    }

    #[test]
    fn falls_back_to_files_api_and_flags_missing_files() {
        let gh = FakeGh::default();
        let mut pr = serde_json::from_str::<Value>(&pr_json()).unwrap();
        pr["changedFiles"] = json!(2);
        gh.on(&["pr", "view"], Ok(&pr.to_string()));
        gh.on(&["repo", "view"], Ok(r#"{"id":"R_1"}"#));
        gh.on(&["pr", "diff"], Err("HTTP 406: diff too large"));
        gh.on(
            &["api", "--paginate"],
            Ok(r#"[[{"filename":"a.rs","status":"modified","patch":"@@ -1 +1 @@\n-x\n+y","sha":"s1","additions":1,"deletions":1}]]"#),
        );
        gh.on(&["api", "graphql"], Err("boom"));
        let pr = PrRef {
            owner: "o".into(),
            repo: "r".into(),
            number: 7,
        };
        let snap = fetch_snapshot(&gh, &pr, &Limits::default()).unwrap();
        assert_eq!(snap.files.len(), 1);
        assert!(
            !snap.complete,
            "1 of 2 files must mark the snapshot incomplete"
        );
        assert!(snap.warnings.iter().any(|w| w.contains("review threads")));
    }
}
