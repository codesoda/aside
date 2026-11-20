//! Recover a complete patch from immutable commits without touching a checkout.
//!
//! GitHub may refuse to fetch a SHA that is no longer advertised (for example,
//! after a force push). In that case recovery fails closed; never substitute a
//! moving branch or PR ref for the captured revision.

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use std::{
    io::Read,
    path::Path,
    process::{Command, ExitStatus, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};

use crate::diff::{Limits, ParsedDiff, parse_unified};

const MAX_DIFF_BYTES: usize = 64 * 1024 * 1024;
const MAX_COMMAND_BYTES: usize = 4096;
const MAX_HISTORY_DEPTH: usize = 256;
const GIT_DEADLINE: Duration = Duration::from_secs(60);

pub(super) fn recover(
    pr_ref: &super::PrRef,
    base_sha: &str,
    head_sha: &str,
    limits: &Limits,
) -> Result<ParsedDiff> {
    // PrRef comes from a validated PR URL. Do not interpolate a remote supplied
    // string into a shell command or let Git interpret it as an option.
    let remote = format!("https://github.com/{}/{}.git", pr_ref.owner, pr_ref.repo);
    recover_from_remote(&remote, base_sha, head_sha, limits)
}

fn valid_sha(sha: &str) -> bool {
    sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit())
}

fn git(git_dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("--git-dir")
        .arg(git_dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    command
}

// Read in a worker so a stalled pipe cannot hold the caller past the deadline.
// On every error, including timeout and oversized output, kill Git and its helpers.
fn run_git(mut command: Command, cap: usize, deadline: Instant) -> Result<(ExitStatus, Vec<u8>)> {
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .stdout(Stdio::piped())
        .spawn()
        .context("could not start Git")?;
    let stdout = child.stdout.take().context("Git stdout missing")?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.take((cap + 1) as u64).read_to_end(&mut bytes);
        let _ = tx.send((result, bytes));
    });
    let result = (|| {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let (read_result, bytes) = rx.recv_timeout(remaining).context("Git output timed out")?;
        read_result.context("could not read Git output")?;
        if bytes.len() > cap {
            bail!("Git output exceeds recovery limit");
        }
        loop {
            if let Some(status) = child.try_wait().context("could not wait for Git")? {
                return Ok((status, bytes));
            }
            if Instant::now() >= deadline {
                bail!("Git command timed out");
            }
            thread::sleep(Duration::from_millis(10));
        }
    })();
    if result.is_err() {
        #[cfg(unix)]
        {
            // The spawned child leads its own process group; a negative PID
            // targets helpers even if the leader has already exited.
            unsafe extern "C" {
                fn kill(pid: i32, sig: i32) -> i32;
            }
            if let Ok(pid) = i32::try_from(child.id()) {
                // SAFETY: kill is called with the owned child's process-group ID.
                unsafe { kill(-pid, 9) }; // SIGKILL
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

fn recover_from_remote(
    remote: &str,
    base_sha: &str,
    head_sha: &str,
    limits: &Limits,
) -> Result<ParsedDiff> {
    if !valid_sha(base_sha) || !valid_sha(head_sha) {
        bail!("captured base/head must be full 40-character commit SHAs");
    }
    let temp = tempfile::tempdir().context("could not create temporary Git directory")?;
    let dir = temp.path().join("objects.git");
    let deadline = Instant::now() + GIT_DEADLINE;
    let mut init = git(&dir);
    init.args(["init", "--bare", "--quiet"]);
    let (status, _) = run_git(init, MAX_COMMAND_BYTES, deadline)
        .context("could not initialize temporary bare repository")?;
    if !status.success() {
        bail!("could not initialize temporary bare repository");
    }
    for sha in [base_sha, head_sha] {
        let mut fetch = git(&dir);
        // Clear inherited credential helpers and ask the authenticated gh
        // CLI only for this command; never modify the user's Git config.
        let depth = format!("--depth={MAX_HISTORY_DEPTH}");
        fetch
            .args([
                "-c",
                "credential.helper=",
                "-c",
                "credential.helper=!gh auth git-credential",
            ])
            .args(["fetch", "--quiet", "--no-tags", &depth, "--", remote, sha]);
        let (status, _) = run_git(fetch, MAX_COMMAND_BYTES, deadline)
            .context("could not fetch captured commit")?;
        if !status.success() {
            bail!("could not fetch captured commit {sha}; GitHub may no longer advertise it");
        }
    }
    // Both histories are bounded. If their common ancestor lies beyond the
    // boundary, reject rather than silently diffing the two tips.
    let mut merge_base = git(&dir);
    merge_base.args(["merge-base", base_sha, head_sha]);
    let (status, output) = run_git(merge_base, MAX_COMMAND_BYTES, deadline)?;
    if !status.success() {
        bail!("could not establish merge base within {MAX_HISTORY_DEPTH} commits");
    }
    let ancestor = std::str::from_utf8(&output)?.trim();
    if !valid_sha(ancestor) {
        bail!("Git returned an invalid merge base");
    }
    let mut diff = git(&dir);
    diff.args([
        "-c",
        "diff.external=",
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--find-renames",
        "--full-index",
        ancestor,
        head_sha,
        "--",
    ]);
    let (status, output) =
        run_git(diff, MAX_DIFF_BYTES, deadline).context("could not run temporary Git diff")?;
    if !status.success() {
        bail!("temporary Git diff failed");
    }
    let text = std::str::from_utf8(&output).context("Git diff is not UTF-8")?;
    let parsed = parse_unified(text, head_sha, limits);
    if parsed
        .warnings
        .iter()
        .any(|warning| warning.contains("files; the rest were skipped"))
        || parsed.files.iter().any(|file| !file.patch_complete)
    {
        bail!("temporary Git diff exceeded parsing limits");
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn rejects_invalid_shas_before_running_git() {
        let result = recover_from_remote("ignored", "main", &"a".repeat(40), &Limits::default());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("full 40-character")
        );
    }

    #[test]
    fn recovers_exact_commits_from_a_local_remote_without_a_checkout_change() {
        let repo = tempfile::tempdir().unwrap();
        run(repo.path(), &["init", "--quiet"]);
        run(repo.path(), &["config", "user.email", "test@example.com"]);
        run(repo.path(), &["config", "user.name", "Test"]);
        std::fs::write(repo.path().join("a.txt"), "before\n").unwrap();
        run(repo.path(), &["add", "a.txt"]);
        run(repo.path(), &["commit", "--quiet", "-m", "base"]);
        let base = run(repo.path(), &["rev-parse", "HEAD"]);
        std::fs::write(repo.path().join("a.txt"), "after\n").unwrap();
        run(repo.path(), &["commit", "--quiet", "-am", "head"]);
        let head = run(repo.path(), &["rev-parse", "HEAD"]);
        let parsed = recover_from_remote(
            repo.path().to_str().unwrap(),
            &base,
            &head,
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(parsed.files.len(), 1);
        assert!(parsed.files[0].patch_complete);
        assert_eq!(run(repo.path(), &["rev-parse", "HEAD"]), head);
        assert_eq!(
            std::fs::read_to_string(repo.path().join("a.txt")).unwrap(),
            "after\n"
        );
    }

    #[test]
    fn diffs_divergent_head_from_merge_base_not_base_tip() {
        let repo = tempfile::tempdir().unwrap();
        run(repo.path(), &["init", "--quiet"]);
        run(repo.path(), &["config", "user.email", "test@example.com"]);
        run(repo.path(), &["config", "user.name", "Test"]);
        std::fs::write(repo.path().join("base.txt"), "common\n").unwrap();
        run(repo.path(), &["add", "."]);
        run(repo.path(), &["commit", "--quiet", "-m", "common"]);
        let common = run(repo.path(), &["rev-parse", "HEAD"]);
        std::fs::write(repo.path().join("head.txt"), "head\n").unwrap();
        run(repo.path(), &["add", "."]);
        run(repo.path(), &["commit", "--quiet", "-m", "head"]);
        let head = run(repo.path(), &["rev-parse", "HEAD"]);
        run(repo.path(), &["checkout", "--quiet", "-b", "base", &common]);
        std::fs::write(repo.path().join("base-only.txt"), "base\n").unwrap();
        run(repo.path(), &["add", "."]);
        run(repo.path(), &["commit", "--quiet", "-m", "base tip"]);
        let base = run(repo.path(), &["rev-parse", "HEAD"]);
        let parsed = recover_from_remote(
            repo.path().to_str().unwrap(),
            &base,
            &head,
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(parsed.files.len(), 1);
        assert_eq!(parsed.files[0].path, "head.txt");
    }

    #[test]
    fn rejects_unrelated_histories_without_a_merge_base() {
        let repo = tempfile::tempdir().unwrap();
        run(repo.path(), &["init", "--quiet"]);
        run(repo.path(), &["config", "user.email", "test@example.com"]);
        run(repo.path(), &["config", "user.name", "Test"]);
        std::fs::write(repo.path().join("a.txt"), "a\n").unwrap();
        run(repo.path(), &["add", "."]);
        run(repo.path(), &["commit", "--quiet", "-m", "base"]);
        let base = run(repo.path(), &["rev-parse", "HEAD"]);
        run(repo.path(), &["checkout", "--quiet", "--orphan", "other"]);
        run(repo.path(), &["rm", "--quiet", "-rf", "."]);
        std::fs::write(repo.path().join("b.txt"), "b\n").unwrap();
        run(repo.path(), &["add", "."]);
        run(repo.path(), &["commit", "--quiet", "-m", "head"]);
        let head = run(repo.path(), &["rev-parse", "HEAD"]);
        let error = recover_from_remote(
            repo.path().to_str().unwrap(),
            &base,
            &head,
            &Limits::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("could not establish merge base"));
    }

    #[test]
    fn git_runner_bounds_output_and_stalled_children() {
        let mut large = Command::new("sh");
        large.args(["-c", "yes x"]);
        assert!(run_git(large, 32, Instant::now() + Duration::from_secs(2)).is_err());

        let mut stuck = Command::new("sh");
        stuck.args(["-c", "sleep 2"]);
        let start = Instant::now();
        assert!(run_git(stuck, 32, start + Duration::from_millis(100)).is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn git_runner_cleans_up_descendants_on_timeout_and_oversize() {
        for oversized in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let pid_file = dir.path().join("descendant.pid");
            let mut command = Command::new("sh");
            command.env("PID_FILE", &pid_file).args([
                "-c",
                if oversized {
                    "sleep 30 & echo $! > \"$PID_FILE\"; printf 'too much output'; wait"
                } else {
                    "sleep 30 & echo $! > \"$PID_FILE\"; wait"
                },
            ]);
            let result = run_git(
                command,
                if oversized { 4 } else { 32 },
                Instant::now() + Duration::from_millis(500),
            );
            assert!(result.is_err());
            let pid: u32 = std::fs::read_to_string(&pid_file)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            let state = || {
                Command::new("ps")
                    .args(["-p", &pid.to_string(), "-o", "stat="])
                    .output()
                    .unwrap()
                    .stdout
            };
            let start = Instant::now();
            while !state().is_empty()
                && !state().starts_with(b"Z")
                && start.elapsed() < Duration::from_secs(2)
            {
                thread::sleep(Duration::from_millis(10));
            }
            let remaining = state();
            if !remaining.is_empty() && !remaining.starts_with(b"Z") {
                let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
                panic!(
                    "descendant {pid} survived {}",
                    if oversized { "oversize" } else { "timeout" }
                );
            }
        }
    }

    #[test]
    fn rejects_unavailable_commit_and_parser_truncation() {
        let repo = tempfile::tempdir().unwrap();
        run(repo.path(), &["init", "--quiet"]);
        run(repo.path(), &["config", "user.email", "test@example.com"]);
        run(repo.path(), &["config", "user.name", "Test"]);
        std::fs::write(repo.path().join("a.txt"), "before\n").unwrap();
        run(repo.path(), &["add", "a.txt"]);
        run(repo.path(), &["commit", "--quiet", "-m", "base"]);
        let base = run(repo.path(), &["rev-parse", "HEAD"]);
        std::fs::write(repo.path().join("a.txt"), "after\n").unwrap();
        run(repo.path(), &["commit", "--quiet", "-am", "head"]);
        let head = run(repo.path(), &["rev-parse", "HEAD"]);
        let remote = repo.path().to_str().unwrap();
        assert!(recover_from_remote(remote, &"0".repeat(40), &head, &Limits::default()).is_err());
        assert!(
            recover_from_remote(
                remote,
                &base,
                &head,
                &Limits {
                    max_lines_per_file: 1,
                    max_files: 1
                }
            )
            .is_err()
        );
    }
}
