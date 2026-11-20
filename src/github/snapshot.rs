use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::diff::{self, ApiFile, Limits};
use crate::model::{ImportedReview, IssueComment, PrSnapshot, PullRequest, SCHEMA_VERSION};
use crate::util::now_rfc3339;

use super::{GhRunner, PrRef, runner::run_json, threads::fetch_threads};

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

pub(super) const PR_FIELDS: &str = "number,title,body,url,state,isDraft,author,baseRefName,baseRefOid,headRefName,headRefOid,changedFiles,reviews,comments";

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
