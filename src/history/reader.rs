//! Bounded reads of immutable Git objects. Never opens a checkout or follows refs.
use crate::github::git_fallback::{git, run_git, valid_sha};
use anyhow::{Context, Result, bail};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

const MAX_OUTPUT: usize = 16 * 1024 * 1024;
const MAX_PATHS: usize = 200_000;
const MAX_COMMITS: usize = 1_000;
const MAX_BLOB: usize = 512 * 1024;

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IncompleteHistory {
    pub reason: String,
    pub base_sha: String,
    pub anchor_unix: Option<i64>,
    pub window_start_unix: Option<i64>,
    pub available_paths: Option<usize>,
    pub walked: usize,
    pub counted: usize,
    pub merge_skipped: usize,
    pub commit_cap: usize,
    pub output_limit_bytes: usize,
    pub tree_path_limit: usize,
    pub blob_limit_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct Transaction {
    pub id: String,
    pub timestamp: i64,
    pub paths: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub struct History {
    pub base: String,
    pub anchor: i64,
    pub window_start: i64,
    pub walked: usize,
    pub counted: usize,
    pub merges_skipped: usize,
    /// Co-change only: commits wider than MAX_COMMIT_PATHS, before filtering.
    pub broad_skipped: usize,
    pub transactions: Vec<Transaction>,
    pub files: BTreeMap<String, String>,
    pub touches: BTreeMap<String, Vec<i64>>,
    pub ambiguous: BTreeSet<String>,
}

fn command(dir: &Path, args: &[&str], cap: usize, deadline: Instant) -> Result<Vec<u8>> {
    if Instant::now() >= deadline {
        bail!("history deadline reached")
    }
    let mut cmd = git(dir);
    cmd.args(args);
    let (status, bytes) = run_git(cmd, cap, deadline)?;
    if !status.success() {
        bail!("history Git object read failed")
    }
    Ok(bytes)
}

fn ascii_number(bytes: &[u8]) -> Result<i64> {
    let text = std::str::from_utf8(bytes)?;
    if text.is_empty() || !text.bytes().all(|c| c.is_ascii_digit()) {
        bail!("invalid Git timestamp")
    }
    Ok(text.parse()?)
}

fn path(bytes: &[u8]) -> Result<String> {
    let text = std::str::from_utf8(bytes)?;
    if text.is_empty() || text.starts_with('/') || text.contains('\0') {
        bail!("invalid Git path")
    }
    Ok(text.to_owned())
}

fn tree(dir: &Path, base: &str, deadline: Instant) -> Result<BTreeMap<String, String>> {
    let bytes = command(dir, &["ls-tree", "-r", "-z", base], MAX_OUTPUT, deadline)?;
    parse_tree(&bytes)
}

fn parse_tree(bytes: &[u8]) -> Result<BTreeMap<String, String>> {
    parse_tree_limited(bytes, MAX_PATHS)
}

fn parse_tree_limited(bytes: &[u8], max_paths: usize) -> Result<BTreeMap<String, String>> {
    if !bytes.is_empty() && !bytes.ends_with(&[0]) {
        bail!("partial tree record")
    }
    let mut files = BTreeMap::new();
    if bytes.is_empty() {
        return Ok(files);
    }
    for (index, row) in bytes[..bytes.len() - 1].split(|c| *c == 0).enumerate() {
        if index >= max_paths {
            bail!("tree path limit reached")
        }
        let (metadata, name) = row
            .split_once_byte(b'\t')
            .context("malformed tree record")?;
        let parts: Vec<_> = metadata.split(|c| *c == b' ').collect();
        if parts.len() != 3 || parts[1] != b"blob" || !valid_sha(std::str::from_utf8(parts[2])?) {
            if parts.len() == 3 && parts[0] == b"160000" {
                continue;
            }
            bail!("malformed tree object")
        }
        if parts[0] != b"100644" && parts[0] != b"100755" {
            continue;
        }
        let name = path(name)?;
        if files
            .insert(name, std::str::from_utf8(parts[2])?.to_owned())
            .is_some()
        {
            bail!("duplicate baseline path")
        }
    }
    Ok(files)
}

trait SplitByte {
    fn split_once_byte(&self, byte: u8) -> Option<(&[u8], &[u8])>;
}
impl SplitByte for [u8] {
    fn split_once_byte(&self, byte: u8) -> Option<(&[u8], &[u8])> {
        let i = self.iter().position(|c| *c == byte)?;
        Some((&self[..i], &self[i + 1..]))
    }
}

fn diff_records(bytes: &[u8], max_paths: usize) -> Result<Vec<(u8, String)>> {
    if !bytes.is_empty() && !bytes.ends_with(&[0]) {
        bail!("partial diff-tree record")
    }
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let mut records = bytes[..bytes.len() - 1].split(|c| *c == 0);
    let mut result = Vec::new();
    while let Some(status) = records.next() {
        if result.len() >= max_paths {
            bail!("diff path limit reached")
        }
        let name = records.next().context("partial diff-tree path")?;
        if !matches!(status, b"A" | b"M" | b"D" | b"T") {
            bail!("unsupported diff-tree status")
        }
        result.push((status[0], path(name)?));
    }
    Ok(result)
}

fn parse_log(log: &[u8]) -> Result<Vec<(String, i64, String)>> {
    parse_log_limited(log, MAX_COMMITS)
}

fn parse_log_limited(log: &[u8], max_commits: usize) -> Result<Vec<(String, i64, String)>> {
    if log.is_empty() || !log.ends_with(b"\n") {
        bail!("partial history metadata")
    }
    let mut commits = Vec::new();
    for line in log[..log.len() - 1].split(|c| *c == b'\n') {
        let row: Vec<_> = line.split(|c| *c == 0).collect();
        if row.len() != 3 {
            bail!("malformed history metadata")
        }
        let sha = std::str::from_utf8(row[0])?;
        if !valid_sha(sha) {
            bail!("malformed commit id")
        }
        let parents = std::str::from_utf8(row[2])?;
        if !parents.is_empty() && !parents.split(' ').all(valid_sha) {
            bail!("malformed parent id")
        }
        commits.push((sha.to_owned(), ascii_number(row[1])?, parents.to_owned()));
        if commits.len() > max_commits {
            bail!("history commit cap reached")
        }
    }
    Ok(commits)
}

/// The returned population is complete only on success. An oversized log,
/// malformed record, missing object, shallow boundary or commit cap is an error.
/// Dates are filtered only after all commits are traversed, never by Git's
/// `--since` (timestamps need not be ordered along ancestry).
#[cfg(test)]
pub fn read(dir: &Path, base: &str, deadline: Instant) -> Result<History> {
    read_with_cancel(dir, base, deadline, &AtomicBool::new(false))
}

#[cfg(test)]
pub fn read_with_cancel(
    dir: &Path,
    base: &str,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<History> {
    read_inner(
        dir,
        base,
        deadline,
        cancelled,
        &mut IncompleteHistory::default(),
    )
}

pub fn read_diagnostic(
    dir: &Path,
    base: &str,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> std::result::Result<History, Box<IncompleteHistory>> {
    let mut diagnostic = IncompleteHistory {
        base_sha: base.into(),
        commit_cap: MAX_COMMITS,
        output_limit_bytes: MAX_OUTPUT,
        tree_path_limit: MAX_PATHS,
        blob_limit_bytes: MAX_BLOB,
        ..IncompleteHistory::default()
    };
    read_inner(dir, base, deadline, cancelled, &mut diagnostic).map_err(|err| {
        diagnostic.reason = if cancelled.load(Ordering::Relaxed) {
            "cancelled"
        } else if Instant::now() >= deadline {
            "deadline"
        } else if err.to_string().contains("cap") {
            "commit_cap"
        } else if err.to_string().contains("shallow") {
            "shallow_boundary"
        } else if err.to_string().contains("limit") {
            "resource_limit"
        } else {
            "malformed_or_missing_object"
        }
        .into();
        Box::new(diagnostic)
    })
}

fn read_inner(
    dir: &Path,
    base: &str,
    deadline: Instant,
    cancelled: &AtomicBool,
    diagnostic: &mut IncompleteHistory,
) -> Result<History> {
    if cancelled.load(Ordering::Relaxed) || Instant::now() >= deadline {
        bail!("history cancelled or deadline reached")
    }
    if !valid_sha(base) {
        bail!("invalid pinned comparison base")
    }
    let shallow = command(dir, &["rev-parse", "--is-shallow-repository"], 32, deadline)?;
    if shallow != b"false\n" {
        bail!("shallow or malformed Git boundary")
    }
    let kind = command(dir, &["cat-file", "-t", base], 32, deadline)?;
    if kind != b"commit\n" {
        bail!("baseline is not a commit")
    }
    let files = tree(dir, base, deadline)?;
    diagnostic.available_paths = Some(files.len());
    let log = command(
        dir,
        &[
            "log",
            "--format=%H%x00%ct%x00%P",
            "--no-patch",
            "--max-count=1001",
            base,
        ],
        MAX_OUTPUT,
        deadline,
    )?;
    diagnostic.walked = log.iter().filter(|byte| **byte == b'\n').count();
    if let Some(first) = log.split(|byte| *byte == b'\n').next() {
        let row: Vec<_> = first.split(|byte| *byte == 0).collect();
        if row.len() == 3
            && valid_sha(std::str::from_utf8(row[0]).unwrap_or(""))
            && let Ok(anchor) = ascii_number(row[1])
        {
            diagnostic.anchor_unix = Some(anchor);
            diagnostic.window_start_unix = Some(anchor.saturating_sub(180 * 24 * 60 * 60));
        }
    }
    let commits = parse_log(&log)?;
    let anchor = commits.first().context("baseline history absent")?.1;
    let window_start = anchor.saturating_sub(180 * 24 * 60 * 60);
    diagnostic.anchor_unix = Some(anchor);
    diagnostic.window_start_unix = Some(window_start);
    diagnostic.walked = commits.len();
    let mut result = History {
        base: base.into(),
        anchor,
        window_start,
        walked: commits.len(),
        counted: 0,
        merges_skipped: 0,
        broad_skipped: 0,
        transactions: Vec::new(),
        files,
        touches: BTreeMap::new(),
        ambiguous: BTreeSet::new(),
    };
    let mut deleted = BTreeSet::new();
    let mut added = BTreeSet::new();
    for (sha, timestamp, parents) in commits {
        if cancelled.load(Ordering::Relaxed) || Instant::now() >= deadline {
            bail!("history cancelled or deadline reached")
        }
        let parent: Vec<_> = if parents.is_empty() {
            vec![]
        } else {
            parents.split(' ').collect()
        };
        if parent.len() > 1 {
            result.merges_skipped += 1;
            diagnostic.merge_skipped = result.merges_skipped;
            continue;
        }
        if timestamp < window_start || timestamp > anchor {
            continue;
        }
        let bytes = if let Some(parent) = parent.first() {
            command(
                dir,
                &[
                    "diff-tree",
                    "--no-commit-id",
                    "--name-status",
                    "-z",
                    "-r",
                    "--no-renames",
                    parent,
                    &sha,
                ],
                MAX_OUTPUT,
                deadline,
            )?
        } else {
            command(
                dir,
                &[
                    "diff-tree",
                    "--root",
                    "--no-commit-id",
                    "--name-status",
                    "-z",
                    "-r",
                    "--no-renames",
                    &sha,
                ],
                MAX_OUTPUT,
                deadline,
            )?
        };
        let mut touched = BTreeSet::new();
        let mut all_paths = BTreeSet::new();
        for (status, name) in diff_records(&bytes, MAX_PATHS)? {
            if status == b'D' {
                deleted.insert(name.clone());
            }
            if status == b'A' {
                added.insert(name.clone());
            }
            if deleted.contains(&name) && added.contains(&name) {
                result.ambiguous.insert(name.clone());
            }
            all_paths.insert(name.clone());
            if result.files.contains_key(&name) {
                touched.insert(name);
            }
        }
        for name in touched {
            result.touches.entry(name).or_default().push(timestamp);
        }
        // Breadth includes deleted/non-baseline paths, so filtering cannot
        // turn a bulk change into a spurious narrow association.
        if all_paths.len() > super::cochange::MAX_COMMIT_PATHS {
            result.broad_skipped += 1;
        } else {
            result.transactions.push(Transaction {
                id: sha,
                timestamp,
                paths: all_paths,
            });
        }
        result.counted += 1;
        diagnostic.counted = result.counted;
    }
    if cancelled.load(Ordering::Relaxed) || Instant::now() >= deadline {
        bail!("history cancelled or deadline reached")
    }
    Ok(result)
}

#[cfg(test)]
pub fn blob(dir: &Path, sha: &str, deadline: Instant) -> Result<Vec<u8>> {
    blob_with_cancel(dir, sha, deadline, &AtomicBool::new(false))
}

pub fn blob_with_cancel(
    dir: &Path,
    sha: &str,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>> {
    if cancelled.load(Ordering::Relaxed) || Instant::now() >= deadline {
        bail!("history cancelled or deadline reached")
    }
    if !valid_sha(sha) {
        bail!("invalid blob id")
    }
    let size = command(dir, &["cat-file", "-s", sha], 32, deadline)?;
    let size = ascii_number(size.strip_suffix(b"\n").context("malformed blob size")?)?;
    if size > MAX_BLOB as i64 {
        bail!("blob limit reached")
    }
    if cancelled.load(Ordering::Relaxed) {
        bail!("history cancelled")
    }
    let bytes = command(dir, &["cat-file", "blob", sha], MAX_BLOB, deadline)?;
    if bytes.len() as i64 != size {
        bail!("partial blob")
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    #[test]
    fn metadata_and_tree_bounds_fail_closed() {
        let sha = "a".repeat(40);
        let record = format!("{sha}\01\0\n");
        assert_eq!(parse_log(record.as_bytes()).unwrap().len(), 1);
        assert!(parse_log(&record.repeat(MAX_COMMITS + 1).into_bytes()).is_err());
        assert!(parse_log(&record.as_bytes()[..record.len() - 1]).is_err());
        assert!(parse_log(format!("{sha}\0invalid\0\n").as_bytes()).is_err());
        let entry = format!("100644 blob {sha}\tfile\0");
        assert!(parse_tree(&entry.repeat(MAX_PATHS + 1).into_bytes()).is_err());
        assert!(parse_tree(&entry.as_bytes()[..entry.len() - 1]).is_err());
        assert!(parse_tree(b"malformed\0").is_err());
        assert!(parse_tree(b"").unwrap().is_empty());
        assert!(parse_tree_limited(entry.as_bytes(), 0).is_err());
        assert_eq!(parse_tree_limited(entry.as_bytes(), 1).unwrap().len(), 1);
        assert!(parse_log_limited(record.as_bytes(), 0).is_err());
        assert_eq!(parse_log_limited(record.as_bytes(), 1).unwrap().len(), 1);
        assert!(diff_records(b"", 0).unwrap().is_empty());
        assert!(diff_records(b"M\0a.rs\0", 0).is_err());
        assert_eq!(
            diff_records(b"M\0a.rs\0", 1).unwrap(),
            vec![(b'M', "a.rs".into())]
        );
        assert!(diff_records(b"M\0a.rs", 1).is_err());
        assert!(diff_records(b"M\0", 1).is_err());
    }

    #[test]
    fn child_output_cap_is_enforced() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf 123456789"]);
        assert!(
            run_git(
                command,
                8,
                Instant::now() + std::time::Duration::from_secs(2)
            )
            .is_err()
        );
    }

    #[test]
    fn paths_and_timestamps_reject_partial_data() {
        assert!(path(b"").is_err());
        assert!(ascii_number(b"123x").is_err());
        assert_eq!(ascii_number(b"123").unwrap(), 123);
    }
    #[test]
    fn shallow_boundary_and_blob_limit_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let run = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{:?}", out);
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };
        run(&["init", "-q"]);
        run(&["config", "user.name", "Fixture"]);
        run(&["config", "user.email", "fixture@example.com"]);
        std::fs::write(dir.join("large.rs"), vec![b'x'; 512 * 1024 + 1]).unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "root"]);
        let sha = run(&["rev-parse", "HEAD"]);
        let git_dir = dir.join(".git");
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let h = read(&git_dir, &sha, deadline).unwrap();
        assert!(blob(&git_dir, &h.files["large.rs"], deadline).is_err());
        assert!(blob(&git_dir, &h.files["large.rs"], Instant::now()).is_err());
        assert!(read(&git_dir, &sha, Instant::now()).is_err());
        let cancelled = AtomicBool::new(true);
        assert!(read_with_cancel(&git_dir, &sha, deadline, &cancelled).is_err());
        assert!(blob_with_cancel(&git_dir, &h.files["large.rs"], deadline, &cancelled).is_err());
        let incomplete =
            read_diagnostic(&git_dir, &sha, Instant::now(), &AtomicBool::new(false)).unwrap_err();
        assert_eq!(incomplete.reason, "deadline");
        assert_eq!(incomplete.commit_cap, 1000);
        std::fs::write(git_dir.join("shallow"), format!("{sha}\n")).unwrap();
        assert!(read(&git_dir, &sha, deadline).is_err());
        let incomplete =
            read_diagnostic(&git_dir, &sha, deadline, &AtomicBool::new(false)).unwrap_err();
        assert_eq!(incomplete.reason, "shallow_boundary");
        assert_eq!(incomplete.base_sha, sha);
        assert_eq!(incomplete.commit_cap, 1000);
        assert!(incomplete.anchor_unix.is_none());
        std::fs::remove_file(git_dir.join("shallow")).unwrap();
        let missing = read_diagnostic(&git_dir, &"f".repeat(40), deadline, &AtomicBool::new(false))
            .unwrap_err();
        assert_eq!(missing.reason, "malformed_or_missing_object");
        assert!(missing.available_paths.is_none());
    }

    #[test]
    fn empty_root_tree_and_unchanged_commit_are_valid() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let run = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };
        run(&["init", "-q"]);
        run(&["config", "user.name", "Fixture"]);
        run(&["config", "user.email", "fixture@example.com"]);
        run(&["commit", "--allow-empty", "-qm", "root"]);
        run(&["commit", "--allow-empty", "-qm", "unchanged"]);
        let result = read(
            &dir.join(".git"),
            &run(&["rev-parse", "HEAD"]),
            Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(result.walked, 2);
        assert_eq!(result.counted, 2);
        assert!(result.files.is_empty());
        assert!(result.touches.is_empty());
    }

    #[test]
    fn disordered_timestamps_do_not_cut_off_older_ancestors() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let run = |args: &[&str], date: Option<&str>| {
            let mut command = Command::new("git");
            command.arg("-C").arg(dir).args(args);
            if let Some(date) = date {
                command
                    .env("GIT_AUTHOR_DATE", date)
                    .env("GIT_COMMITTER_DATE", date);
            }
            let out = command.output().unwrap();
            assert!(out.status.success(), "{:?}", out);
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };
        run(&["init", "-q"], None);
        run(&["config", "user.name", "Fixture"], None);
        run(&["config", "user.email", "fixture@example.com"], None);
        std::fs::write(dir.join("a.rs"), "a").unwrap();
        run(&["add", "."], None);
        run(&["commit", "-qm", "ancestor"], Some("2023-05-01T00:00:00Z"));
        std::fs::write(dir.join("a.rs"), "ab").unwrap();
        run(
            &["commit", "-qam", "out-of-window-parent"],
            Some("2020-01-01T00:00:00Z"),
        );
        std::fs::write(dir.join("a.rs"), "abc").unwrap();
        run(&["commit", "-qam", "base"], Some("2023-06-01T00:00:00Z"));
        let sha = run(&["rev-parse", "HEAD"], None);
        let history = read(
            &dir.join(".git"),
            &sha,
            Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(history.walked, 3);
        assert_eq!(history.counted, 2);
        assert_eq!(history.touches["a.rs"].len(), 2);
    }

    #[test]
    fn historical_delete_and_reintroduction_suppresses_lineage() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let run = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };
        run(&["init", "-q"]);
        run(&["config", "user.name", "Fixture"]);
        run(&["config", "user.email", "fixture@example.com"]);
        std::fs::write(dir.join("old.rs"), "fn old() {}").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "root"]);
        run(&["mv", "old.rs", "new.rs"]);
        run(&["commit", "-qm", "renamed"]);
        std::fs::write(dir.join("old.rs"), "fn reintroduced() {}").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "reintroduced"]);
        let h = read(
            &dir.join(".git"),
            &run(&["rev-parse", "HEAD"]),
            Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();
        assert!(h.ambiguous.contains("old.rs"));
        assert_eq!(h.files.len(), 2);
        assert_eq!(h.touches["old.rs"].len(), 3);
    }

    #[test]
    fn cochange_bulk_filter_counts_all_distinct_paths_before_baseline_filtering() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Fixture"]);
        git(&["config", "user.email", "fixture@example.com"]);
        for name in ["a", "b"] {
            std::fs::write(dir.join(name), "0").unwrap();
        }
        for n in 0..49 {
            std::fs::write(dir.join(format!("old-{n}")), "0").unwrap();
        }
        git(&["add", "."]);
        git(&["commit", "-qm", "51 paths"]);
        // 49 deleted paths plus two modified paths: still broad even though
        // only a/b survive in the next tree.
        for n in 0..49 {
            git(&["rm", "-q", &format!("old-{n}")]);
        }
        for name in ["a", "b"] {
            std::fs::write(dir.join(name), "1").unwrap();
        }
        git(&["commit", "-qam", "51 paths again"]);
        for n in 0..48 {
            std::fs::write(dir.join(format!("new-{n}")), "0").unwrap();
        }
        for name in ["a", "b"] {
            std::fs::write(dir.join(name), "2").unwrap();
        }
        git(&["add", "."]);
        git(&["commit", "-qm", "exactly 50 paths"]);
        let base = git(&["rev-parse", "HEAD"]);
        let h = read(
            &dir.join(".git"),
            &base,
            Instant::now() + std::time::Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(h.counted, 3);
        assert_eq!(h.broad_skipped, 2);
        assert_eq!(h.transactions.len(), 1);
        assert_eq!(h.transactions[0].id, base);
        assert_eq!(h.transactions[0].paths.len(), 50);
        assert_eq!(
            h.touches["a"].len(),
            3,
            "bulk filtering must not change churn"
        );
        assert_eq!(h.touches["b"].len(), 3);
    }

    #[test]
    fn baseline_excludes_newer_commit_and_counts_zero_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let run = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{:?}", out);
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };
        run(&["init", "-q"]);
        run(&["config", "user.name", "Fixture"]);
        run(&["config", "user.email", "fixture@example.com"]);
        std::fs::write(dir.join("a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(dir.join("untouched.rs"), "fn b() {}\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "root"]);
        std::fs::write(dir.join("a.rs"), "fn a() { false; }\n").unwrap();
        run(&["commit", "-qam", "base"]);
        let base = run(&["rev-parse", "HEAD"]);
        std::fs::write(dir.join("a.rs"), "fn a() { true; }\n").unwrap();
        run(&["commit", "-qam", "head"]);
        let git_dir = dir.join(".git");
        let h = read(
            &git_dir,
            &base,
            Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(h.walked, 2);
        assert_eq!(h.files.len(), 2);
        assert_eq!(h.touches.get("a.rs").unwrap().len(), 2);
        assert_eq!(h.touches.get("untouched.rs").unwrap().len(), 1);
    }
}
