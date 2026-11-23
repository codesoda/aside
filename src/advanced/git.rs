use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};

use crate::github::git_fallback::{git, run_git, valid_sha};

use super::valid_repo_path;

const GIT_BUDGET: Duration = Duration::from_secs(30);
const TREE_MAX_BYTES: usize = 8 * 1024 * 1024;
const TREE_MAX_ENTRIES: usize = 20_000;
const DIFF_MAX_BYTES: usize = 32 * 1024 * 1024;

type Tree = BTreeMap<String, String>;

pub(super) struct PinnedGit {
    dir: PathBuf,
    pub(super) base_tip: String,
    pub(super) head: String,
    pub(super) comparison_base: Option<String>,
    pub(super) comparison_reason: Option<&'static str>,
    head_valid: bool,
    deadline: Instant,
    trees: RefCell<BTreeMap<String, Tree>>,
}

impl PinnedGit {
    pub(super) fn new(dir: PathBuf, base: &str, head: &str, enabled: bool) -> Self {
        let mut pinned = Self {
            dir,
            base_tip: base.to_ascii_lowercase(),
            head: head.to_ascii_lowercase(),
            comparison_base: None,
            comparison_reason: Some("not_requested"),
            head_valid: false,
            deadline: Instant::now() + GIT_BUDGET,
            trees: RefCell::default(),
        };
        if enabled {
            pinned.head_valid = pinned.is_commit(&pinned.head);
            let comparison = pinned.resolve_comparison();
            match comparison {
                Ok(base) => {
                    pinned.comparison_base = Some(base);
                    pinned.comparison_reason = None;
                }
                Err(reason) => pinned.comparison_reason = Some(reason),
            }
        }
        pinned
    }

    pub(super) fn fresh(&self) -> Self {
        Self {
            dir: self.dir.clone(),
            base_tip: self.base_tip.clone(),
            head: self.head.clone(),
            comparison_base: self.comparison_base.clone(),
            comparison_reason: self.comparison_reason,
            head_valid: self.head_valid,
            deadline: Instant::now() + GIT_BUDGET,
            trees: RefCell::default(),
        }
    }

    fn read(&self, args: &[&str], cap: usize) -> Result<Vec<u8>> {
        ensure!(
            Instant::now() < self.deadline,
            "pinned Git deadline reached"
        );
        let mut cmd = git(&self.dir);
        cmd.env("GIT_NO_REPLACE_OBJECTS", "1")
            .env(
                "GIT_GRAFT_FILE",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_ALLOW_PROTOCOL", "") // Forbid all transports, including promisor fallback.
            .env("GIT_ATTR_NOSYSTEM", "1")
            .args(args);
        let (status, bytes) = run_git(cmd, cap, self.deadline)?;
        ensure!(status.success(), "could not read pinned Git objects");
        Ok(bytes)
    }

    fn is_commit(&self, sha: &str) -> bool {
        self.read(&["cat-file", "-t", sha], 32)
            .is_ok_and(|kind| kind == b"commit\n")
    }

    fn resolve_comparison(&self) -> Result<String, &'static str> {
        if !self.head_valid {
            return Err("head_commit_unavailable");
        }
        if !self.is_commit(&self.base_tip) {
            return Err("base_tip_commit_unavailable");
        }
        if !self
            .read(&["rev-parse", "--is-shallow-repository"], 32)
            .is_ok_and(|b| b == b"false\n")
        {
            return Err("history_shallow_or_unavailable");
        }
        let raw = self
            .read(
                &["merge-base", "--all", &self.base_tip, &self.head],
                16 * 1024,
            )
            .map_err(|_| "merge_base_unavailable_or_limited")?;
        let output = std::str::from_utf8(&raw).map_err(|_| "merge_base_invalid")?;
        let mut lines = output.lines();
        let base = lines
            .next()
            .filter(|sha| valid_sha(sha))
            .ok_or("merge_base_invalid")?;
        if lines.next().is_some() {
            return Err("merge_base_ambiguous");
        }
        if !self.is_commit(base) {
            return Err("merge_base_unavailable_or_limited");
        }
        Ok(base.to_owned())
    }

    fn ensure_tree(&self, commit: &str) -> Result<()> {
        ensure!(
            self.head_valid
                && (commit == self.head || self.comparison_base.as_deref() == Some(commit)),
            "commit is not a verified input"
        );
        if self.trees.borrow().contains_key(commit) {
            return Ok(());
        }
        let output = self.read(
            &["ls-tree", "-r", "-z", "--full-tree", commit],
            TREE_MAX_BYTES,
        )?;
        let mut tree = Tree::new();
        let rows = if output.is_empty() {
            None
        } else {
            Some(
                output
                    .strip_suffix(&[0])
                    .context("unterminated pinned tree output")?,
            )
        };
        let entries = rows.into_iter().flat_map(|rows| rows.split(|b| *b == 0));
        for (index, row) in entries.enumerate() {
            ensure!(index < TREE_MAX_ENTRIES, "pinned tree entry limit reached");
            let row = std::str::from_utf8(row).context("non-UTF-8 pinned tree entry")?;
            let (header, path) = row.split_once('\t').context("invalid pinned tree entry")?;
            let mut parts = header.split(' ');
            let (Some(mode), Some(kind), Some(oid), None) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                anyhow::bail!("invalid pinned tree metadata");
            };
            if matches!(mode, "100644" | "100755") && kind == "blob" {
                ensure!(valid_sha(oid), "invalid pinned blob id");
                ensure!(
                    tree.insert(path.to_owned(), oid.to_owned()).is_none(),
                    "duplicate pinned tree path"
                );
            }
        }
        self.trees.borrow_mut().insert(commit.to_owned(), tree);
        Ok(())
    }

    pub(super) fn tree(&self, commit: &str) -> Result<Vec<String>> {
        self.ensure_tree(commit)?;
        Ok(self.trees.borrow()[commit].keys().cloned().collect())
    }

    pub(super) fn blob(&self, commit: &str, path: &str, bytes: usize) -> Result<Vec<u8>> {
        ensure!(valid_repo_path(path), "unsupported pinned blob path");
        self.ensure_tree(commit)?;
        let trees = self.trees.borrow();
        let oid = trees[commit]
            .get(path)
            .context("regular pinned blob not found")?;
        self.read(&["cat-file", "blob", oid], bytes)
    }

    pub(super) fn diff(&self) -> Result<String> {
        let base = self
            .comparison_base
            .as_deref()
            .context("comparison base unavailable")?;
        let bytes = self.read(
            &[
                "-c",
                "core.quotePath=true",
                "diff-tree",
                "--no-commit-id",
                "-r",
                "-p",
                "--no-textconv",
                "--no-ext-diff",
                "--no-renames",
                "--no-color",
                "--diff-algorithm=myers",
                "--no-indent-heuristic",
                "--submodule=short",
                "--src-prefix=a/",
                "--dst-prefix=b/",
                "--unified=0",
                "--ignore-submodules=none",
                base,
                &self.head,
                "--",
            ],
            DIFF_MAX_BYTES,
        )?;
        String::from_utf8(bytes).context("pinned diff is not UTF-8")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::Path, process::Command};

    fn fixture_git(dir: &Path, args: &[&str]) -> String {
        let result = Command::new("git")
            .current_dir(dir)
            .args([
                "-c",
                "user.name=Aside",
                "-c",
                "user.email=aside@example.invalid",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap().trim().to_owned()
    }

    #[test]
    fn comparison_and_blobs_ignore_local_replacements_and_grafts() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fixture_git(root, &["init", "-q"]);
        fs::write(root.join("file"), b"base\n").unwrap();
        fixture_git(root, &["add", "file"]);
        fixture_git(root, &["commit", "-qm", "base"]);
        let base = fixture_git(root, &["rev-parse", "HEAD"]);
        fs::write(root.join("file"), b"head\n").unwrap();
        fixture_git(root, &["commit", "-qam", "head"]);
        let head = fixture_git(root, &["rev-parse", "HEAD"]);
        fs::write(root.join("file"), b"dirty\n").unwrap();
        let head_blob = fixture_git(root, &["rev-parse", "HEAD:file"]);
        let base_blob = fixture_git(root, &["rev-parse", "HEAD^:file"]);
        fixture_git(root, &["replace", &head_blob, &base_blob]);
        let git_dir = root.join(".git");
        fs::write(git_dir.join("info/grafts"), format!("{head}\n")).unwrap();

        let pinned = PinnedGit::new(git_dir, &base, &head, true);
        assert_eq!(pinned.comparison_base.as_deref(), Some(base.as_str()));
        assert_eq!(pinned.blob(&head, "file", 5).unwrap(), b"head\n");
        assert!(pinned.blob(&head, "file", 4).is_err());
        assert!(pinned.diff().unwrap().contains("-base\n+head\n"));
        assert_eq!(fs::read(root.join("file")).unwrap(), b"dirty\n");
    }

    #[test]
    fn criss_cross_history_has_no_unique_comparison_base() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fixture_git(root, &["init", "-q"]);
        let tree = fixture_git(root, &["write-tree"]);
        let initial = fixture_git(root, &["commit-tree", &tree, "-m", "initial"]);
        let left = fixture_git(root, &["commit-tree", &tree, "-p", &initial, "-m", "left"]);
        let right = fixture_git(root, &["commit-tree", &tree, "-p", &initial, "-m", "right"]);
        let base = fixture_git(
            root,
            &["commit-tree", &tree, "-p", &left, "-p", &right, "-m", "one"],
        );
        let head = fixture_git(
            root,
            &["commit-tree", &tree, "-p", &right, "-p", &left, "-m", "two"],
        );
        let pinned = PinnedGit::new(root.join(".git"), &base, &head, true);
        assert_eq!(pinned.comparison_base, None);
        assert_eq!(pinned.comparison_reason, Some("merge_base_ambiguous"));
        assert!(pinned.diff().is_err());
        assert!(pinned.tree(&head).unwrap().is_empty());
    }
}
