use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

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
pub(super) struct GhApiFile {
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

    let files = fetch_files(
        gh,
        pr_ref,
        &meta.base_ref_oid,
        &meta.head_ref_oid,
        meta.changed_files,
        limits,
        &mut warnings,
    )?;
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

    let thread_import = fetch_threads(gh, pr_ref);
    for problem in &thread_import.problems {
        warnings.push(format!("review threads incomplete: {problem}"));
    }
    // TODO(#27): persist this per-source completeness and surface it over SSE
    // instead of only through `warnings`.
    let github_threads_completeness = thread_import.completeness;
    let github_threads = thread_import.threads;

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
        github_threads_completeness,
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

/// Aggregate diff from `gh pr diff`, checked against the paginated REST manifest.
/// A mismatched or unavailable diff falls back to the per-file API patches.
fn fetch_files(
    gh: &dyn GhRunner,
    pr_ref: &PrRef,
    base_sha: &str,
    head_sha: &str,
    expected_changed_files: Option<u64>,
    limits: &Limits,
    warnings: &mut Vec<String>,
) -> Result<Vec<crate::model::ChangedFile>> {
    let aggregate = gh.run(&["pr", "diff", &pr_ref.url(), "--color", "never"], None);
    // This request is mandatory even when the aggregate diff succeeded. Failure
    // must not turn an unverified patch into a complete review snapshot.
    let pages: Vec<Vec<GhApiFile>> = run_json(
        gh,
        &[
            "api",
            "--paginate",
            "--slurp",
            &format!("{}/files?per_page=100", pr_ref.api_base()),
        ],
    )
    .context("could not fetch paginated PR files manifest")?;
    let manifest: Vec<GhApiFile> = pages.into_iter().flatten().collect();
    let mut manifest_paths = HashSet::new();
    if manifest
        .iter()
        .any(|f| !manifest_paths.insert(f.filename.as_str()))
    {
        bail!("PR files manifest contains duplicate paths");
    }

    if let Ok(bytes) = &aggregate {
        let text = String::from_utf8_lossy(bytes);
        let parsed = diff::parse_unified(&text, head_sha, limits);
        if validated_aggregate(&parsed.files, &manifest, expected_changed_files, false) {
            warnings.extend(parsed.warnings);
            return Ok(parsed.files);
        }
        warnings
            .push("aggregate diff did not match the files manifest; used the per-file API".into());
    } else if let Err(e) = aggregate {
        tracing::warn!("`gh pr diff` failed, using the files API: {e}");
        warnings.push(format!(
            "aggregate diff unavailable ({e}); used the per-file API"
        ));
    }
    let api_files: Vec<_> = manifest
        .iter()
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
        .collect();
    // Test fakes never invoke external Git; the recovery module is tested with local repos.
    if needs_recovery(&api_files, manifest.len(), expected_changed_files) && !cfg!(test) {
        match super::git_fallback::recover(pr_ref, base_sha, head_sha, limits) {
            Ok(recovered)
                if validated_aggregate(
                    &recovered.files,
                    &manifest,
                    expected_changed_files,
                    true,
                ) =>
            {
                warnings.push("recovered complete diff from captured Git commits".into());
                return Ok(recovered.files);
            }
            Ok(_) => warnings.push(
                "temporary Git diff did not match PR files metadata; patch remains incomplete"
                    .into(),
            ),
            Err(e) => warnings.push(format!(
                "temporary Git recovery unavailable: {e}; patch remains incomplete"
            )),
        }
    }
    Ok(api_files)
}

pub(super) fn needs_recovery(
    api_files: &[crate::model::ChangedFile],
    manifest_count: usize,
    expected: Option<u64>,
) -> bool {
    api_files.iter().any(|f| !f.patch_complete)
        || expected.is_some_and(|count| count > manifest_count as u64)
}

/// Aggregate diffs require an exact manifest. Recovery may include files omitted
/// by the API, but every known manifest entry must still match exactly.
pub(super) fn validated_aggregate(
    files: &[crate::model::ChangedFile],
    manifest: &[GhApiFile],
    expected: Option<u64>,
    recovered: bool,
) -> bool {
    if !recovered && files.len() != manifest.len() {
        return false;
    }
    if expected.is_some_and(|count| count != files.len() as u64) {
        return false;
    }
    if files.len() < manifest.len()
        || (recovered && expected.is_none() && files.len() != manifest.len())
    {
        return false;
    }
    let by_path: HashMap<_, _> = manifest.iter().map(|f| (f.filename.as_str(), f)).collect();
    let mut seen = HashSet::new();
    let mut matched = 0;
    files.iter().all(|file| {
        if !seen.insert(file.path.as_str()) || !file.patch_complete {
            return false;
        }
        let Some(api) = by_path.get(file.path.as_str()) else {
            return recovered && expected.is_some();
        };
        matched += 1;
        let expected_kind = match api.status.as_str() {
            "added" => Some(crate::model::ChangeKind::Added),
            "modified" => Some(crate::model::ChangeKind::Modified),
            "removed" => Some(crate::model::ChangeKind::Removed),
            "renamed" => Some(crate::model::ChangeKind::Renamed),
            "copied" => Some(crate::model::ChangeKind::Copied),
            _ => None,
        };
        expected_kind.is_none_or(|kind| file.change_kind == kind)
            && api
                .previous_filename
                .as_deref()
                .is_none_or(|old| file.previous_path.as_deref() == Some(old))
            && file.additions == api.additions
            && file.deletions == api.deletions
            && file.hunks.iter().all(|hunk| {
                let old = hunk
                    .lines
                    .iter()
                    .filter(|l| {
                        matches!(
                            l.kind,
                            crate::model::LineKind::Context | crate::model::LineKind::Deletion
                        )
                    })
                    .count();
                let new = hunk
                    .lines
                    .iter()
                    .filter(|l| {
                        matches!(
                            l.kind,
                            crate::model::LineKind::Context | crate::model::LineKind::Addition
                        )
                    })
                    .count();
                old == hunk.old_count as usize && new == hunk.new_count as usize
            })
    }) && matched == manifest.len()
}
