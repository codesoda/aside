//! Optional immutable Git history for analysis. This never supplies the PR diff.
use super::{
    GhRunner, PrRef,
    git_fallback::{MAX_COMMAND_BYTES, git, run_git, run_git_bounded, valid_sha},
};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};
use ts_rs::TS;

const DEADLINE: Duration = Duration::from_secs(60);
const MAX_DEPTH: usize = 1000;
const MAX_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AnalysisContext {
    pub source: String,
    pub base_sha: String,
    pub head_sha: String,
    pub comparison_base: Option<String>,
    pub history_count: usize,
    pub shallow: bool,
    pub complete: bool,
    pub reason: Option<String>,
}

impl AnalysisContext {
    pub(crate) fn unavailable(base: &str, head: &str, reason: &str) -> Self {
        Self {
            source: "api-only".into(),
            base_sha: base.into(),
            head_sha: head.into(),
            comparison_base: None,
            history_count: 0,
            shallow: false,
            complete: false,
            reason: Some(reason.into()),
        }
    }
}

fn output(dir: &Path, args: &[&str], deadline: Instant) -> Result<String> {
    let mut command = git(dir);
    command.args(args);
    let (status, bytes) = run_git(command, MAX_COMMAND_BYTES, deadline)?;
    if !status.success() {
        bail!("Git context command failed")
    }
    Ok(String::from_utf8(bytes)?.trim().to_owned())
}

fn inspect(
    dir: &Path,
    base: &str,
    head: &str,
    source: &str,
    deadline: Instant,
) -> Result<AnalysisContext> {
    for sha in [base, head] {
        if output(dir, &["cat-file", "-t", sha], deadline)? != "commit" {
            bail!("captured commit missing")
        }
    }
    let shallow = output(dir, &["rev-parse", "--is-shallow-repository"], deadline)? == "true";
    let comparison_base = output(dir, &["merge-base", base, head], deadline)
        .ok()
        .filter(|s| valid_sha(s));
    let Some(ancestor) = comparison_base.clone() else {
        return Ok(AnalysisContext {
            source: source.into(),
            base_sha: base.into(),
            head_sha: head.into(),
            comparison_base,
            history_count: 0,
            shallow,
            complete: false,
            reason: Some("merge base unavailable within history boundary".into()),
        });
    };
    // Coverage describes history available behind the comparison base, not
    // commits introduced by the PR after that base.
    let limit = (MAX_DEPTH + 1).to_string();
    let count = output(
        dir,
        &[
            "rev-list",
            "--count",
            &format!("--max-count={limit}"),
            &ancestor,
        ],
        deadline,
    )?
    .parse::<usize>()?;
    let limited = count > MAX_DEPTH;
    Ok(AnalysisContext {
        source: source.into(),
        base_sha: base.into(),
        head_sha: head.into(),
        comparison_base,
        history_count: count.min(MAX_DEPTH),
        shallow,
        complete: !limited && !shallow,
        reason: if limited {
            Some("history limit reached".into())
        } else if shallow {
            Some("shallow boundary may hide history".into())
        } else {
            None
        },
    })
}

fn remote_name(remote: &str) -> Option<(String, String)> {
    let remote = remote.trim().trim_end_matches(".git");
    let path = remote
        .strip_prefix("https://github.com/")
        .or_else(|| remote.strip_prefix("git@github.com:"))
        .or_else(|| remote.strip_prefix("ssh://git@github.com/"))?;
    let (owner, repo) = path.split_once('/')?;
    if repo.contains('/')
        || owner.is_empty()
        || repo.is_empty()
        || !path
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_. /".contains(&c))
    {
        return None;
    }
    Some((owner.into(), repo.into()))
}

fn verified_checkout(
    path: &Path,
    gh: &dyn GhRunner,
    expected_id: &str,
    deadline: Instant,
) -> Result<tempfile::TempDir> {
    let canonical = fs::canonicalize(path)?;
    let mut cmd = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(key);
        }
    }
    cmd.arg("-C")
        .arg(&canonical)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let (status, bytes) = run_git(cmd, MAX_COMMAND_BYTES, deadline)?;
    if !status.success() {
        bail!("not a Git checkout")
    }
    let dir = PathBuf::from(String::from_utf8(bytes)?.trim());
    let config_path = dir.join("config");
    if fs::symlink_metadata(&config_path)?.file_type().is_symlink() {
        bail!("symlink checkout config")
    }
    let config = bounded_file(&config_path, 64 * 1024)?;
    let mut section = "";
    let mut origin = None;
    for line in config.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line;
            if ![
                "[core]",
                "[remote \"origin\"]",
                "[user]",
                "[branch \"main\"]",
                "[branch \"master\"]",
            ]
            .contains(&section)
            {
                bail!("unsafe checkout config")
            }
        } else if !line.is_empty() && !line.starts_with(['#', ';']) {
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| anyhow::anyhow!("invalid checkout config"))?;
            let key = key.trim().to_ascii_lowercase();
            if section == "[remote \"origin\"]" && key == "url" {
                origin = Some(value.trim().to_owned());
            } else if !match section {
                "[core]" => [
                    "repositoryformatversion",
                    "filemode",
                    "bare",
                    "logallrefupdates",
                    "ignorecase",
                    "precomposeunicode",
                ]
                .contains(&key.as_str()),
                "[remote \"origin\"]" => key == "fetch",
                "[user]" => ["name", "email"].contains(&key.as_str()),
                _ => ["remote", "merge"].contains(&key.as_str()),
            } {
                bail!("unsafe checkout config")
            }
        }
    }
    let remote = origin.ok_or_else(|| anyhow::anyhow!("origin missing"))?;
    let (owner, repo) =
        remote_name(&remote).ok_or_else(|| anyhow::anyhow!("unsupported or ambiguous origin"))?;
    let url = format!("{owner}/{repo}");
    // A name alone is not identity: ask GitHub for its immutable repository ID.
    let bytes = identity(gh, &url, deadline)?;
    let id = serde_json::from_slice::<serde_json::Value>(&bytes)?["id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    if id != expected_id {
        bail!("repository identity mismatch")
    }
    let snapshot = tempfile::tempdir()?;
    let stage = snapshot.path().join("objects.git");
    let mut init = git(&stage);
    init.args(["init", "--bare", "--quiet"]);
    if !run_git(init, MAX_COMMAND_BYTES, deadline)?.0.success() {
        bail!("snapshot init failed")
    }
    copy_objects(&dir.join("objects"), &stage.join("objects"), deadline)?;
    let shallow = dir.join("shallow");
    if shallow.exists() {
        let content = bounded_file(&shallow, 64 * 1024)?;
        if !content.lines().all(valid_sha) {
            bail!("invalid shallow boundary")
        }
        fs::write(stage.join("shallow"), content)?;
    }
    Ok(snapshot)
}

fn identity(gh: &dyn GhRunner, url: &str, deadline: Instant) -> Result<Vec<u8>> {
    #[cfg(test)]
    let _ = deadline;
    #[cfg(test)]
    return gh.run(&["repo", "view", url, "--json", "id"], None);
    #[cfg(not(test))]
    {
        let _ = gh;
        let mut cmd = Command::new("gh");
        cmd.args(["repo", "view", url, "--json", "id"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let (status, bytes) = run_git(cmd, MAX_COMMAND_BYTES, deadline)?;
        if !status.success() {
            bail!("repository identity lookup failed")
        }
        Ok(bytes)
    }
}

fn bounded_file(path: &Path, cap: u64) -> Result<String> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.len() > cap {
        bail!("unsafe or oversized Git metadata")
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(cap + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        bail!("oversized Git metadata")
    }
    Ok(String::from_utf8(bytes)?)
}

fn copy_objects(source: &Path, destination: &Path, deadline: Instant) -> Result<()> {
    fn walk(
        src: &Path,
        dst: &Path,
        depth: usize,
        count: &mut usize,
        bytes: &mut u64,
        deadline: Instant,
    ) -> Result<()> {
        if depth > 4 || Instant::now() >= deadline {
            bail!("object snapshot limit reached")
        }
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            *count += 1;
            if *count > 100_000 || Instant::now() >= deadline {
                bail!("object snapshot limit reached")
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let allowed = if depth == 0 {
                name == "pack" || (name.len() == 2 && name.bytes().all(|b| b.is_ascii_hexdigit()))
            } else if depth == 1 && src.file_name().is_some_and(|n| n == "pack") {
                name.ends_with(".pack") || name.ends_with(".idx") || name.ends_with(".rev")
            } else {
                name.len() == 38 && name.bytes().all(|b| b.is_ascii_hexdigit())
            };
            if !allowed {
                continue;
            }
            let meta = fs::symlink_metadata(entry.path())?;
            let target = dst.join(name.as_ref());
            if meta.is_dir() {
                fs::create_dir_all(&target)?;
                walk(&entry.path(), &target, depth + 1, count, bytes, deadline)?;
            } else if meta.is_file() {
                *bytes = bytes
                    .checked_add(meta.len())
                    .ok_or_else(|| anyhow::anyhow!("object size overflow"))?;
                if *bytes > MAX_BYTES {
                    bail!("object snapshot exceeds limit")
                }
                fs::copy(entry.path(), target)?;
            } else {
                bail!("unsafe object entry")
            }
        }
        Ok(())
    }
    if !fs::symlink_metadata(source)?.is_dir() {
        bail!("unsafe object directory")
    }
    walk(source, destination, 0, &mut 0, &mut 0, deadline)
}

pub(super) fn size(dir: &Path, deadline: Instant) -> Result<u64> {
    size_with(dir, deadline, false, &|_| {})
}

pub(super) fn size_live(dir: &Path, deadline: Instant) -> Result<u64> {
    size_with(dir, deadline, true, &|_| {})
}

fn size_with(
    dir: &Path,
    deadline: Instant,
    allow_missing_children: bool,
    before_entry: &impl Fn(&Path),
) -> Result<u64> {
    fn walk(
        path: &Path,
        total: &mut u64,
        entries: &mut usize,
        depth: usize,
        deadline: Instant,
        allow_missing_children: bool,
        before_entry: &impl Fn(&Path),
    ) -> Result<()> {
        if Instant::now() >= deadline || depth > 16 {
            bail!("cache traversal limit reached")
        }
        let meta = match fs::symlink_metadata(path) {
            Err(error)
                if allow_missing_children
                    && depth > 0
                    && error.kind() == std::io::ErrorKind::NotFound =>
            {
                return Ok(());
            }
            result => result?,
        };
        if meta.file_type().is_symlink() || !(meta.is_file() || meta.is_dir()) {
            bail!("unsafe entry in Git cache")
        }
        *entries += 1;
        if *entries > 100_000 {
            bail!("cache traversal limit reached")
        }
        if meta.is_file() {
            *total = total
                .checked_add(meta.len())
                .ok_or_else(|| anyhow::anyhow!("cache size overflow"))?;
        } else {
            let children = match fs::read_dir(path) {
                Err(error)
                    if allow_missing_children
                        && depth > 0
                        && error.kind() == std::io::ErrorKind::NotFound =>
                {
                    return Ok(());
                }
                result => result?,
            };
            for entry in children {
                let child = entry?.path();
                before_entry(&child);
                walk(
                    &child,
                    total,
                    entries,
                    depth + 1,
                    deadline,
                    allow_missing_children,
                    before_entry,
                )?;
            }
        }
        Ok(())
    }
    let mut total = 0;
    walk(
        dir,
        &mut total,
        &mut 0,
        0,
        deadline,
        allow_missing_children,
        before_entry,
    )?;
    Ok(total)
}

fn trusted_cache(dir: &Path, id: &str, pr: &PrRef, deadline: Instant) -> Result<()> {
    if fs::symlink_metadata(dir)?.file_type().is_symlink() {
        bail!("symlink cache")
    }
    if size(dir, deadline)? > MAX_BYTES {
        bail!("cache storage limit reached")
    }
    for forbidden in [
        "refs/replace",
        "info/grafts",
        "objects/info/alternates",
        "objects/info/http-alternates",
        "config.worktree",
    ] {
        if dir.join(forbidden).exists() {
            bail!("unsafe cache history override")
        }
    }
    let marker = dir.join("aside-provenance");
    if fs::symlink_metadata(&marker)?.file_type().is_symlink() {
        bail!("symlink provenance")
    }
    let expected = format!("{id} {}/{}\n", pr.owner, pr.repo);
    if bounded_file(&marker, 4096)? != expected {
        bail!("cache provenance mismatch")
    }
    let config = bounded_file(&dir.join("config"), 64 * 1024)?;
    // Never execute a cache-supplied include, hook, transport, or object store.
    let safe = config.lines().all(|line| {
        let line = line.trim();
        line.is_empty()
            || line == "[core]"
            || [
                "repositoryformatversion =",
                "filemode =",
                "bare =",
                "logallrefupdates =",
                "ignorecase =",
                "precomposeunicode =",
            ]
            .iter()
            .any(|key| line.starts_with(key))
    });
    if !safe
        || !config.lines().any(|line| line.trim() == "bare = true")
        || !config
            .lines()
            .any(|line| line.trim() == "repositoryformatversion = 0")
    {
        bail!("unsafe cache config")
    }
    Ok(())
}

struct Lock(PathBuf);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn lock(path: PathBuf, deadline: Instant) -> Result<Lock> {
    loop {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(Lock(path)),
            Err(e)
                if e.kind() == std::io::ErrorKind::AlreadyExists && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(50))
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// A private, validated object snapshot retained for the lifetime of analysis.
#[derive(Debug, Clone)]
pub(crate) struct HistoryObjects {
    pub dir: PathBuf,
    _guard: std::sync::Arc<tempfile::TempDir>,
}

#[cfg(test)]
impl HistoryObjects {
    pub(crate) fn fixture(guard: tempfile::TempDir, dir: PathBuf) -> Self {
        Self {
            dir,
            _guard: std::sync::Arc::new(guard),
        }
    }
}

/// Resolve context and retain an isolated object snapshot. Do not expose a
/// path into the user's checkout or a mutable cache to provider tasks.
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_owned(
    gh: &dyn GhRunner,
    pr: &PrRef,
    provider_id: Option<&str>,
    base: &str,
    head: &str,
    checkout: Option<&Path>,
    fetch: bool,
    cache_root: &Path,
) -> (AnalysisContext, Option<HistoryObjects>) {
    let context = resolve(gh, pr, provider_id, base, head, checkout, fetch, cache_root);
    retain_resolved(
        gh,
        pr,
        provider_id,
        base,
        head,
        checkout,
        cache_root,
        context,
    )
}

#[allow(clippy::too_many_arguments)]
fn retain_resolved(
    gh: &dyn GhRunner,
    pr: &PrRef,
    provider_id: Option<&str>,
    base: &str,
    head: &str,
    checkout: Option<&Path>,
    cache_root: &Path,
    context: AnalysisContext,
) -> (AnalysisContext, Option<HistoryObjects>) {
    let Some(id) = provider_id else {
        return (context, None);
    };
    let deadline = Instant::now() + DEADLINE;
    let snapshot = (|| -> Result<(tempfile::TempDir, AnalysisContext)> {
        if context.source == "checkout" {
            let dir = verified_checkout(
                checkout.ok_or_else(|| anyhow::anyhow!("checkout missing"))?,
                gh,
                id,
                deadline,
            )?;
            let inspected = inspect(
                &dir.path().join("objects.git"),
                base,
                head,
                "checkout",
                deadline,
            )?;
            return Ok((dir, inspected));
        }
        if context.source != "cache" && context.source != "fetched-cache" {
            bail!("Git context unavailable")
        }
        let cache = cache_root.join("git-context").join(format!("{id}.git"));
        trusted_cache(&cache, id, pr, deadline)?;
        let temp = tempfile::tempdir()?;
        let stage = temp.path().join("objects.git");
        let mut init = git(&stage);
        init.args(["init", "--bare", "--quiet"]);
        if !run_git(init, MAX_COMMAND_BYTES, deadline)?.0.success() {
            bail!("snapshot init failed")
        }
        copy_objects(&cache.join("objects"), &stage.join("objects"), deadline)?;
        if cache.join("shallow").exists() {
            let content = bounded_file(&cache.join("shallow"), 64 * 1024)?;
            if !content.lines().all(valid_sha) {
                bail!("invalid shallow boundary")
            }
            fs::write(stage.join("shallow"), content)?;
        }
        let inspected = inspect(&stage, base, head, &context.source, deadline)?;
        Ok((temp, inspected))
    })();
    match snapshot {
        Ok((guard, inspected)) => {
            let dir = guard.path().join("objects.git");
            (
                inspected,
                Some(HistoryObjects {
                    dir,
                    _guard: std::sync::Arc::new(guard),
                }),
            )
        }
        Err(_) => (
            AnalysisContext::unavailable(base, head, "validated Git objects could not be retained"),
            None,
        ),
    }
}

/// Resolve a supplied checkout, then owned cache, then (only by consent) fetch.
/// Any failure returns API-only coverage, never changes diff completeness.
#[allow(clippy::too_many_arguments)]
pub fn resolve(
    gh: &dyn GhRunner,
    pr: &PrRef,
    provider_id: Option<&str>,
    base: &str,
    head: &str,
    checkout: Option<&Path>,
    fetch: bool,
    cache_root: &Path,
) -> AnalysisContext {
    let remote = format!("https://github.com/{}/{}.git", pr.owner, pr.repo);
    resolve_remote(
        gh,
        pr,
        provider_id,
        base,
        head,
        checkout,
        fetch,
        cache_root,
        &remote,
    )
}

// A private transport seam lets tests exercise the real fetch and publication path
// against a local Git repository without contacting GitHub.
#[allow(clippy::too_many_arguments)]
fn resolve_remote(
    gh: &dyn GhRunner,
    pr: &PrRef,
    provider_id: Option<&str>,
    base: &str,
    head: &str,
    checkout: Option<&Path>,
    fetch: bool,
    cache_root: &Path,
    remote: &str,
) -> AnalysisContext {
    resolve_remote_with_path(
        gh,
        pr,
        provider_id,
        base,
        head,
        checkout,
        fetch,
        cache_root,
        remote,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn resolve_remote_with_path(
    gh: &dyn GhRunner,
    pr: &PrRef,
    provider_id: Option<&str>,
    base: &str,
    head: &str,
    checkout: Option<&Path>,
    fetch: bool,
    cache_root: &Path,
    remote: &str,
    fetch_path: Option<&Path>,
) -> AnalysisContext {
    if !valid_sha(base) || !valid_sha(head) {
        return AnalysisContext::unavailable(base, head, "invalid captured commit");
    }
    let Some(id) = provider_id.filter(|s| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    }) else {
        return AnalysisContext::unavailable(base, head, "repository identity unavailable");
    };
    let deadline = Instant::now() + DEADLINE;
    if let Some(path) = checkout
        && let Ok(dir) = verified_checkout(path, gh, id, deadline)
    {
        let result = inspect(
            &dir.path().join("objects.git"),
            base,
            head,
            "checkout",
            deadline,
        );
        if let Ok(context) = result {
            return context;
        }
    }
    let root = cache_root.join("git-context");
    let dir = root.join(format!("{id}.git"));
    // Reject the root before reading cache contents through it.
    if root.exists() && fs::symlink_metadata(&root).is_ok_and(|m| m.file_type().is_symlink()) {
        return AnalysisContext::unavailable(base, head, "symlink cache root");
    }
    if trusted_cache(&dir, id, pr, deadline).is_ok()
        && let Ok(context) = inspect(&dir, base, head, "cache", deadline)
    {
        return context;
    }
    if !fetch {
        return AnalysisContext::unavailable(
            base,
            head,
            "fetch not consented or context unavailable",
        );
    }
    let result = (|| -> Result<AnalysisContext> {
        if root.exists() && fs::symlink_metadata(&root)?.file_type().is_symlink() {
            bail!("symlink cache root")
        }
        fs::create_dir_all(&root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        }
        let _guard = lock(root.join(format!("{id}.lock")), deadline)?;
        if trusted_cache(&dir, id, pr, deadline).is_ok()
            && let Ok(context) = inspect(&dir, base, head, "cache", deadline)
        {
            return Ok(context);
        }
        if dir.exists() && trusted_cache(&dir, id, pr, deadline).is_err() {
            bail!("untrusted cache exists")
        }
        let staging = tempfile::tempdir_in(&root)?;
        let stage = staging.path().join("objects.git");
        {
            let mut init = git(&stage);
            init.args(["init", "--bare", "--quiet"]);
            if !run_git(init, MAX_COMMAND_BYTES, deadline)?.0.success() {
                bail!("cache initialization failed")
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&stage, fs::Permissions::from_mode(0o700))?;
            }
        }
        if dir.exists() {
            copy_objects(&dir.join("objects"), &stage.join("objects"), deadline)?;
            let shallow = dir.join("shallow");
            if shallow.exists() {
                let content = bounded_file(&shallow, 64 * 1024)?;
                if !content.lines().all(valid_sha) {
                    bail!("invalid shallow boundary")
                }
                fs::write(stage.join("shallow"), content)?;
            }
        }
        for sha in [base, head] {
            let mut cmd = git(&stage);
            if let Some(path) = fetch_path {
                cmd.env("PATH", path);
            }
            cmd.args([
                "-c",
                "credential.helper=",
                "-c",
                "credential.helper=!gh auth git-credential",
                "fetch",
                "--quiet",
                "--no-tags",
                "--depth=1000",
                "--",
                remote,
                sha,
            ]);
            if !run_git_bounded(
                cmd,
                MAX_COMMAND_BYTES,
                deadline,
                Some((staging.path(), MAX_BYTES)),
            )?
            .0
            .success()
            {
                bail!("captured commit fetch failed")
            }
            if size(staging.path(), deadline)? > MAX_BYTES {
                bail!("cache storage limit reached")
            }
        }
        let context = inspect(&stage, base, head, "fetched-cache", deadline)?;
        fs::write(
            stage.join("aside-provenance"),
            format!("{id} {}/{}\n", pr.owner, pr.repo),
        )?;
        trusted_cache(&stage, id, pr, deadline)?;
        if dir.exists() {
            let old = staging.path().join("previous.git");
            fs::rename(&dir, &old)?;
            if let Err(error) = fs::rename(&stage, &dir) {
                let _ = fs::rename(&old, &dir);
                return Err(error.into());
            }
        } else {
            fs::rename(&stage, &dir)?;
        }
        Ok(context)
    })();
    result.unwrap_or_else(|_| {
        AnalysisContext::unavailable(base, head, "fetch failed or limits reached")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_names_reject_credentials_and_other_hosts() {
        assert_eq!(
            remote_name("git@github.com:Acme/repo.git"),
            Some(("Acme".into(), "repo".into()))
        );
        assert!(remote_name("https://token@github.com/acme/repo").is_none());
        assert!(remote_name("https://evil.example/acme/repo").is_none());
    }
    #[test]
    fn consent_off_is_api_only() {
        let gh = super::super::fake::FakeGh::default();
        let dir = tempfile::tempdir().unwrap();
        let pr = PrRef {
            owner: "acme".into(),
            repo: "repo".into(),
            number: 1,
        };
        let context = resolve(
            &gh,
            &pr,
            Some("R_1"),
            &"a".repeat(40),
            &"b".repeat(40),
            None,
            false,
            dir.path(),
        );
        assert_eq!(context.source, "api-only");
        assert!(!dir.path().join("git-context").exists());
    }

    fn run(path: &Path, args: &[&str]) -> String {
        let result = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap().trim().into()
    }

    #[test]
    fn retained_context_describes_second_object_snapshot_not_earlier_inspection() {
        let repo = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        run(repo.path(), &["init", "--quiet"]);
        run(repo.path(), &["config", "user.email", "test@example.com"]);
        run(repo.path(), &["config", "user.name", "Test"]);
        run(
            repo.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/acme/repo.git",
            ],
        );
        fs::write(repo.path().join("file"), "base").unwrap();
        run(repo.path(), &["add", "file"]);
        run(repo.path(), &["commit", "--quiet", "-m", "base"]);
        let base = run(repo.path(), &["rev-parse", "HEAD"]);
        fs::write(repo.path().join("file"), "head").unwrap();
        run(repo.path(), &["commit", "--quiet", "-am", "head"]);
        let head = run(repo.path(), &["rev-parse", "HEAD"]);
        let gh = super::super::fake::FakeGh::default();
        gh.on(&["repo", "view"], Ok(r#"{"id":"R_1"}"#));
        let pr = PrRef {
            owner: "acme".into(),
            repo: "repo".into(),
            number: 1,
        };
        let first = resolve(
            &gh,
            &pr,
            Some("R_1"),
            &base,
            &head,
            Some(repo.path()),
            false,
            cache.path(),
        );
        assert!(first.complete);
        // A shallow boundary appears between resolution and retention.
        fs::write(repo.path().join(".git/shallow"), format!("{base}\n")).unwrap();
        let (retained, objects) = retain_resolved(
            &gh,
            &pr,
            Some("R_1"),
            &base,
            &head,
            Some(repo.path()),
            cache.path(),
            first,
        );
        assert!(objects.is_some());
        assert!(retained.shallow);
        assert!(!retained.complete);
        assert_eq!(retained.source, "checkout");
    }

    #[test]
    fn verified_dirty_checkout_reports_pinned_history_without_mutation() {
        let repo = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        run(repo.path(), &["init", "--quiet"]);
        run(repo.path(), &["config", "user.email", "test@example.com"]);
        run(repo.path(), &["config", "user.name", "Test"]);
        run(
            repo.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/acme/repo.git",
            ],
        );
        fs::write(repo.path().join("file"), "base").unwrap();
        run(repo.path(), &["add", "file"]);
        run(repo.path(), &["commit", "--quiet", "-m", "base"]);
        let base = run(repo.path(), &["rev-parse", "HEAD"]);
        fs::write(repo.path().join("file"), "head").unwrap();
        run(repo.path(), &["commit", "--quiet", "-am", "head"]);
        let head = run(repo.path(), &["rev-parse", "HEAD"]);
        fs::write(repo.path().join("file"), "dirty-canary").unwrap();
        // Local replacements must not change the pinned object history.
        run(repo.path(), &["replace", &base, &head]);
        let gh = super::super::fake::FakeGh::default();
        gh.on(&["repo", "view"], Ok(r#"{"id":"R_1"}"#));
        let pr = PrRef {
            owner: "acme".into(),
            repo: "repo".into(),
            number: 1,
        };
        let found = resolve(
            &gh,
            &pr,
            Some("R_1"),
            &base,
            &head,
            Some(repo.path()),
            false,
            cache.path(),
        );
        assert_eq!(found.source, "checkout");
        assert_eq!(found.comparison_base.as_deref(), Some(base.as_str()));
        assert_eq!(found.history_count, 1);
        assert!(found.complete);
        assert_eq!(
            fs::read_to_string(repo.path().join("file")).unwrap(),
            "dirty-canary"
        );
        assert!(!cache.path().join("git-context").exists());
        let mismatch = resolve(
            &gh,
            &pr,
            Some("R_2"),
            &base,
            &head,
            Some(repo.path()),
            false,
            cache.path(),
        );
        assert_eq!(mismatch.source, "api-only");
    }

    #[test]
    fn linked_worktree_uses_common_objects_without_checkout_mutation() {
        let repo = tempfile::tempdir().unwrap();
        let linked = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        run(repo.path(), &["init", "--quiet"]);
        run(repo.path(), &["config", "user.email", "test@example.com"]);
        run(repo.path(), &["config", "user.name", "Test"]);
        run(
            repo.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/acme/repo.git",
            ],
        );
        fs::write(repo.path().join("file"), "base").unwrap();
        run(repo.path(), &["add", "file"]);
        run(repo.path(), &["commit", "-qm", "base"]);
        let base = run(repo.path(), &["rev-parse", "HEAD"]);
        fs::write(repo.path().join("file"), "head").unwrap();
        run(repo.path(), &["commit", "-qam", "head"]);
        let head = run(repo.path(), &["rev-parse", "HEAD"]);
        run(
            repo.path(),
            &[
                "worktree",
                "add",
                "--detach",
                linked.path().to_str().unwrap(),
            ],
        );
        let gh = super::super::fake::FakeGh::default();
        gh.on(&["repo", "view"], Ok(r#"{"id":"R_1"}"#));
        let pr = PrRef {
            owner: "acme".into(),
            repo: "repo".into(),
            number: 1,
        };
        let found = resolve(
            &gh,
            &pr,
            Some("R_1"),
            &base,
            &head,
            Some(linked.path()),
            false,
            cache.path(),
        );
        assert_eq!(found.source, "checkout");
        assert_eq!(found.comparison_base.as_deref(), Some(base.as_str()));
    }

    #[test]
    fn shallow_checkout_preserves_history_boundary() {
        let repo = tempfile::tempdir().unwrap();
        let shallow = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        run(repo.path(), &["init", "--quiet"]);
        run(repo.path(), &["config", "user.email", "test@example.com"]);
        run(repo.path(), &["config", "user.name", "Test"]);
        fs::write(repo.path().join("file"), "base").unwrap();
        run(repo.path(), &["add", "file"]);
        run(repo.path(), &["commit", "-qm", "base"]);
        fs::write(repo.path().join("file"), "head").unwrap();
        run(repo.path(), &["commit", "-qam", "head"]);
        let url = format!("file://{}", repo.path().display());
        run(
            shallow.path(),
            &["clone", "--quiet", "--depth=1", &url, "checkout"],
        );
        let checkout = shallow.path().join("checkout");
        run(
            &checkout,
            &[
                "remote",
                "set-url",
                "origin",
                "https://github.com/acme/repo.git",
            ],
        );
        let head = run(&checkout, &["rev-parse", "HEAD"]);
        let gh = super::super::fake::FakeGh::default();
        gh.on(&["repo", "view"], Ok(r#"{"id":"R_1"}"#));
        let pr = PrRef {
            owner: "acme".into(),
            repo: "repo".into(),
            number: 1,
        };
        let found = resolve(
            &gh,
            &pr,
            Some("R_1"),
            &head,
            &head,
            Some(&checkout),
            false,
            cache.path(),
        );
        assert_eq!(found.source, "checkout");
        assert!(found.shallow);
        assert!(!found.complete);
    }

    #[test]
    fn cache_traversal_counts_files_and_directories_and_deadline() {
        let root = tempfile::tempdir().unwrap();
        let mut dir = root.path().to_path_buf();
        for i in 0..15 {
            dir = dir.join(format!("d{i}"));
            fs::create_dir(&dir).unwrap();
        }
        fs::write(dir.join("file"), b"abc").unwrap();
        assert_eq!(size(root.path(), Instant::now() + DEADLINE).unwrap(), 3);
        assert!(size(root.path(), Instant::now()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_object_source_and_cache_root_are_rejected() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("objects");
        let destination = root.path().join("destination");
        symlink(root.path(), &source).unwrap();
        assert!(copy_objects(&source, &destination, Instant::now() + DEADLINE).is_err());
        let cache = root.path().join("cache");
        fs::create_dir(&cache).unwrap();
        symlink(&cache, root.path().join("git-context")).unwrap();
        let gh = super::super::fake::FakeGh::default();
        let pr = PrRef {
            owner: "acme".into(),
            repo: "repo".into(),
            number: 1,
        };
        let found = resolve(
            &gh,
            &pr,
            Some("R_1"),
            &"a".repeat(40),
            &"b".repeat(40),
            None,
            false,
            root.path(),
        );
        assert_eq!(found.source, "api-only");
        assert_eq!(found.reason.as_deref(), Some("symlink cache root"));
    }

    #[test]
    fn comparison_base_history_excludes_pr_commits() {
        let repo = tempfile::tempdir().unwrap();
        run(repo.path(), &["init", "--quiet"]);
        run(repo.path(), &["config", "user.email", "test@example.com"]);
        run(repo.path(), &["config", "user.name", "Test"]);
        fs::write(repo.path().join("file"), "ancestor").unwrap();
        run(repo.path(), &["add", "."]);
        run(repo.path(), &["commit", "-qm", "ancestor"]);
        let ancestor = run(repo.path(), &["rev-parse", "HEAD"]);
        for n in 0..4 {
            fs::write(repo.path().join("file"), format!("head {n}")).unwrap();
            run(repo.path(), &["commit", "-qam", "head"]);
        }
        let head = run(repo.path(), &["rev-parse", "HEAD"]);
        let context = inspect(
            &repo.path().join(".git"),
            &ancestor,
            &head,
            "checkout",
            Instant::now() + DEADLINE,
        )
        .unwrap();
        assert_eq!(context.history_count, 1);
        assert_eq!(context.comparison_base.as_deref(), Some(ancestor.as_str()));
    }

    #[test]
    fn hostile_cache_and_symlink_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("cache.git");
        run(
            root.path(),
            &["init", "--bare", "--quiet", dir.to_str().unwrap()],
        );
        let pr = PrRef {
            owner: "acme".into(),
            repo: "repo".into(),
            number: 1,
        };
        fs::write(dir.join("aside-provenance"), "R_1 acme/repo\n").unwrap();
        let deadline = Instant::now() + DEADLINE;
        assert!(trusted_cache(&dir, "R_2", &pr, deadline).is_err());
        assert!(trusted_cache(&dir, "R_1", &pr, deadline).is_ok());
        fs::create_dir_all(dir.join("refs/replace")).unwrap();
        assert!(trusted_cache(&dir, "R_1", &pr, deadline).is_err());
        fs::remove_dir(dir.join("refs/replace")).unwrap();
        fs::write(dir.join("config"), "[include]\npath = /tmp/hostile\n").unwrap();
        assert!(trusted_cache(&dir, "R_1", &pr, deadline).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(root.path(), dir.join("objects/hostile")).unwrap();
            assert!(trusted_cache(&dir, "R_1", &pr, deadline).is_err());
        }
    }

    #[test]
    fn disk_monitor_tolerates_concurrent_removal_only() {
        let root = tempfile::tempdir().unwrap();
        let removed = root.path().join("removed.pack");
        let kept = root.path().join("kept.pack");
        fs::write(&removed, b"old").unwrap();
        fs::write(&kept, b"newer").unwrap();
        let (request, receiver) = std::sync::mpsc::channel();
        let (reply, acknowledged) = std::sync::mpsc::channel();
        let removed_for_worker = removed.clone();
        let worker = thread::spawn(move || {
            receiver.recv().unwrap();
            fs::remove_file(removed_for_worker).unwrap();
            reply.send(()).unwrap();
        });
        let observed = size_with(root.path(), Instant::now() + DEADLINE, true, &|path| {
            if path == removed {
                request.send(()).unwrap();
                acknowledged.recv().unwrap();
            }
        });
        worker.join().unwrap();
        assert_eq!(observed.unwrap(), 5);
        assert!(size_live(&removed, Instant::now() + DEADLINE).is_err());
        assert!(size(&removed, Instant::now() + DEADLINE).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn disk_monitor_stops_writer_during_transfer() {
        let root = tempfile::tempdir().unwrap();
        let mut command = Command::new("dd");
        command
            .args(["if=/dev/zero", "of=payload", "bs=65536", "count=10000"])
            .current_dir(root.path());
        let error = run_git_bounded(
            command,
            4096,
            Instant::now() + Duration::from_secs(5),
            Some((root.path(), 1024 * 1024)),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Git storage limit reached during transfer"
        );
        let size_after = size(root.path(), Instant::now() + DEADLINE).unwrap();
        assert!(size_after > 1024 * 1024);
        assert_eq!(
            size(root.path(), Instant::now() + DEADLINE).unwrap(),
            size_after
        );
    }

    #[cfg(unix)]
    #[test]
    fn disk_monitor_cancels_descendant_writer() {
        let root = tempfile::tempdir().unwrap();
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                "dd if=/dev/zero of=payload bs=65536 count=10000 2>/dev/null & echo $! > writer.pid; wait",
            ])
            .current_dir(root.path());
        let error = run_git_bounded(
            command,
            4096,
            Instant::now() + Duration::from_secs(5),
            Some((root.path(), 1024 * 1024)),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Git storage limit reached during transfer"
        );
        let pid = fs::read_to_string(root.path().join("writer.pid")).unwrap();
        let pid = pid.trim();
        assert!(size(root.path(), Instant::now() + DEADLINE).unwrap() > 1024 * 1024);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let observed = Command::new("ps")
                .args(["-o", "stat=", "-p", pid])
                .output()
                .unwrap();
            let state = String::from_utf8(observed.stdout).unwrap();
            // An orphan can remain as a zombie until PID 1 reaps it.
            if !observed.status.success()
                || state.is_empty()
                || matches!(state.trim().chars().next(), Some('Z' | 'X'))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "descendant writer survived cancellation"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    #[test]
    fn disk_monitor_continues_after_stdout_eof() {
        let root = tempfile::tempdir().unwrap();
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                "exec 1>&-; dd if=/dev/zero of=payload bs=65536 count=10000 2>/dev/null; sleep 5",
            ])
            .current_dir(root.path());
        let start = Instant::now();
        assert!(
            run_git_bounded(
                command,
                4096,
                start + Duration::from_secs(3),
                Some((root.path(), 1024 * 1024))
            )
            .is_err()
        );
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn metadata_reads_are_bounded() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config");
        fs::write(&path, vec![b'x'; 65 * 1024]).unwrap();
        assert!(bounded_file(&path, 64 * 1024).is_err());
    }

    #[test]
    fn concurrent_locks_are_exclusive() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("lock");
        let guard = lock(path.clone(), Instant::now() + DEADLINE).unwrap();
        let second = thread::spawn(move || lock(path, Instant::now() + Duration::from_millis(150)));
        assert!(second.join().unwrap().is_err());
        drop(guard);
    }

    fn revisions() -> (tempfile::TempDir, String, String, String) {
        let repo = tempfile::tempdir().unwrap();
        run(repo.path(), &["init", "--quiet"]);
        run(repo.path(), &["config", "user.email", "test@example.com"]);
        run(repo.path(), &["config", "user.name", "Test"]);
        fs::write(repo.path().join("file"), "base").unwrap();
        run(repo.path(), &["add", "file"]);
        run(repo.path(), &["commit", "-qm", "base"]);
        let base = run(repo.path(), &["rev-parse", "HEAD"]);
        fs::write(repo.path().join("file"), "head").unwrap();
        run(repo.path(), &["commit", "-qam", "head"]);
        let head = run(repo.path(), &["rev-parse", "HEAD"]);
        fs::write(repo.path().join("file"), "next").unwrap();
        run(repo.path(), &["commit", "-qam", "next"]);
        let next = run(repo.path(), &["rev-parse", "HEAD"]);
        (repo, base, head, next)
    }

    fn test_pr() -> PrRef {
        PrRef {
            owner: "acme".into(),
            repo: "repo".into(),
            number: 1,
        }
    }

    #[test]
    fn consented_fetch_publishes_both_revisions_and_reuses_cache_without_transport() {
        let (repo, base, head, next) = revisions();
        let cache = tempfile::tempdir().unwrap();
        let gh = super::super::fake::FakeGh::default();
        let pr = test_pr();
        let remote = repo.path().to_str().unwrap();
        let found = resolve_remote(
            &gh,
            &pr,
            Some("R_1"),
            &base,
            &head,
            None,
            true,
            cache.path(),
            remote,
        );
        assert_eq!(found.source, "fetched-cache", "{found:?}");
        assert_eq!(found.comparison_base.as_deref(), Some(base.as_str()));
        let dir = cache.path().join("git-context/R_1.git");
        assert!(trusted_cache(&dir, "R_1", &pr, Instant::now() + DEADLINE).is_ok());
        assert_eq!(run(&dir, &["cat-file", "-t", &base]), "commit");
        assert_eq!(run(&dir, &["cat-file", "-t", &head]), "commit");
        let reused = resolve_remote(
            &gh,
            &pr,
            Some("R_1"),
            &base,
            &head,
            None,
            true,
            cache.path(),
            "/nonexistent/transport",
        );
        assert_eq!(reused.source, "cache", "{reused:?}");
        let refreshed = resolve_remote(
            &gh,
            &pr,
            Some("R_1"),
            &head,
            &next,
            None,
            true,
            cache.path(),
            remote,
        );
        assert_eq!(refreshed.source, "fetched-cache", "{refreshed:?}");
        assert_eq!(run(&dir, &["cat-file", "-t", &next]), "commit");
        let reused_next = resolve(
            &gh,
            &pr,
            Some("R_1"),
            &head,
            &next,
            None,
            false,
            cache.path(),
        );
        assert_eq!(reused_next.source, "cache", "{reused_next:?}");
    }

    #[cfg(unix)]
    #[test]
    fn credential_canary_stays_out_of_argv_context_and_cache_on_authenticated_fetch() {
        use std::os::unix::fs::PermissionsExt;

        let (repo, base, head, _) = revisions();
        let cache = tempfile::tempdir().unwrap();
        let tools = tempfile::tempdir().unwrap();
        let canary = "credential-canary-do-not-persist-7a3f";
        let real_git =
            String::from_utf8(Command::new("which").arg("git").output().unwrap().stdout).unwrap();
        let real_git = real_git.trim();
        let path = tools.path();
        let argv = path.join("argv");
        let used = path.join("credential-used");
        let remote = "https://github.com/acme/repo.git";
        let gh_script = format!(
            "#!/bin/sh\n[ \"$1 $2 $3\" = 'auth git-credential get' ] || exit 1\nprintf used > '{}'\nprintf 'username=git\\npassword={}\\n'\n",
            used.display(),
            canary
        );
        // The fake transport obtains credentials through the configured helper,
        // then redirects only the remote argument to local pinned objects.
        let git_script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\ncase \" $* \" in\n  *' fetch '*)\n    case \" $* \" in *'credential.helper=!gh auth git-credential'*) ;; *) exit 2;; esac\n    credentials=$(printf 'protocol=https\\nhost=github.com\\n\\n' | '{}' -c credential.helper= -c 'credential.helper=!gh auth git-credential' credential fill) || exit 3\n    case \"$credentials\" in *'password={}'*) ;; *) exit 4;; esac\n    original_count=$#\n    while [ \"$original_count\" -gt 0 ]; do\n      arg=$1; shift; original_count=$((original_count - 1))\n      if [ \"$arg\" = '{}' ]; then arg='{}'; fi\n      set -- \"$@\" \"$arg\"\n    done\n    ;;\nesac\nexec '{}' \"$@\"\n",
            argv.display(),
            real_git,
            canary,
            remote,
            repo.path().display(),
            real_git
        );
        for (name, script) in [("gh", gh_script), ("git", git_script)] {
            let file = path.join(name);
            fs::write(&file, script).unwrap();
            fs::set_permissions(&file, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let original_path = std::env::var_os("PATH").unwrap_or_default();
        let child_path = PathBuf::from(format!(
            "{}:{}",
            path.display(),
            original_path.to_string_lossy()
        ));
        let found = resolve_remote_with_path(
            &super::super::fake::FakeGh::default(),
            &test_pr(),
            Some("R_1"),
            &base,
            &head,
            None,
            true,
            cache.path(),
            remote,
            Some(&child_path),
        );
        assert_eq!(
            found.source,
            "fetched-cache",
            "{found:?}; helper called={}; argv={}",
            used.exists(),
            fs::read_to_string(&argv).unwrap_or_default()
        );
        assert!(used.exists(), "credential helper was not invoked");
        assert!(!fs::read_to_string(&argv).unwrap().contains(canary));
        assert!(!format!("{found:?}").contains(canary));
        assert!(!serde_json::to_string(&found).unwrap().contains(canary));
        let dir = cache.path().join("git-context/R_1.git");
        assert!(trusted_cache(&dir, "R_1", &test_pr(), Instant::now() + DEADLINE).is_ok());
        for name in [
            "config",
            "aside-provenance",
            "FETCH_HEAD",
            "shallow",
            "packed-refs",
        ] {
            let file = dir.join(name);
            if file.exists() {
                assert!(
                    !fs::read_to_string(&file).unwrap().contains(canary),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn failed_fetch_keeps_api_only_and_never_publishes_cache() {
        let (_repo, base, head, _) = revisions();
        let cache = tempfile::tempdir().unwrap();
        let gh = super::super::fake::FakeGh::default();
        let pr = test_pr();
        let found = resolve_remote(
            &gh,
            &pr,
            Some("R_1"),
            &base,
            &head,
            None,
            true,
            cache.path(),
            "/nonexistent/transport",
        );
        assert_eq!(found.source, "api-only");
        assert_eq!(
            found.reason.as_deref(),
            Some("fetch failed or limits reached")
        );
        assert!(!cache.path().join("git-context/R_1.git").exists());
        assert!(!cache.path().join("git-context/R_1.lock").exists());
    }

    #[test]
    fn concurrent_resolve_publishes_once_and_second_reuses_cache() {
        let (repo, base, head, _) = revisions();
        let cache = tempfile::tempdir().unwrap();
        let root = cache.path().to_path_buf();
        let remote = repo.path().to_str().unwrap().to_owned();
        let workers: Vec<_> = (0..2)
            .map(|_| {
                let (root, remote, base, head) =
                    (root.clone(), remote.clone(), base.clone(), head.clone());
                thread::spawn(move || {
                    let gh = super::super::fake::FakeGh::default();
                    resolve_remote(
                        &gh,
                        &test_pr(),
                        Some("R_1"),
                        &base,
                        &head,
                        None,
                        true,
                        &root,
                        &remote,
                    )
                })
            })
            .collect();
        let sources: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap().source)
            .collect();
        assert!(sources.contains(&"fetched-cache".to_owned()), "{sources:?}");
        assert!(sources.contains(&"cache".to_owned()), "{sources:?}");
        assert!(
            trusted_cache(
                &root.join("git-context/R_1.git"),
                "R_1",
                &test_pr(),
                Instant::now() + DEADLINE
            )
            .is_ok()
        );
    }

    #[test]
    fn cache_reused_without_network_and_shallow_coverage() {
        let repo = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        run(repo.path(), &["init", "--quiet"]);
        run(repo.path(), &["config", "user.email", "test@example.com"]);
        run(repo.path(), &["config", "user.name", "Test"]);
        fs::write(repo.path().join("file"), "base").unwrap();
        run(repo.path(), &["add", "."]);
        run(repo.path(), &["commit", "--quiet", "-m", "base"]);
        let base = run(repo.path(), &["rev-parse", "HEAD"]);
        fs::write(repo.path().join("file"), "head").unwrap();
        run(repo.path(), &["commit", "--quiet", "-am", "head"]);
        let head = run(repo.path(), &["rev-parse", "HEAD"]);
        let dir = root.path().join("git-context/R_1.git");
        fs::create_dir_all(dir.parent().unwrap()).unwrap();
        let result = Command::new("git")
            .args(["init", "--bare", "--quiet"])
            .arg(&dir)
            .status()
            .unwrap();
        assert!(result.success());
        run(
            &dir,
            &[
                "fetch",
                "--quiet",
                "--depth=2",
                "--",
                repo.path().to_str().unwrap(),
                &head,
            ],
        );
        fs::write(dir.join("aside-provenance"), "R_1 acme/repo\n").unwrap();
        let gh = super::super::fake::FakeGh::default();
        let pr = PrRef {
            owner: "acme".into(),
            repo: "repo".into(),
            number: 1,
        };
        let found = resolve(
            &gh,
            &pr,
            Some("R_1"),
            &base,
            &head,
            None,
            false,
            root.path(),
        );
        assert_eq!(found.source, "cache");
        assert!(found.shallow);
        assert!(!found.complete);
        assert!(gh.calls.lock().unwrap().is_empty());
    }
}
