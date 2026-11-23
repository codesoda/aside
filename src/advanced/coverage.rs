//! Pinned, fail-closed line coverage for added diff rows.
use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{PinnedGit, SourceError, SourceResult};

const MAX_ARTIFACT: usize = 4 * 1024 * 1024;
const MAX_BLOB: usize = 8 * 1024 * 1024;
const MAX_FILES: usize = 1_024;
const MAX_LINES: usize = 16_384;
const MAX_ADDED: usize = 4_096;
const MAX_PATH_BYTES: usize = 1_024;
const MAX_PATH_TOTAL: usize = 1024 * 1024;
const MAX_BLOBS_TOTAL: usize = 32 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    schema: String,
    commit: String,
    tool: Tool,
    files: Vec<FileCoverage>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Tool {
    name: String,
    version: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileCoverage {
    path: String,
    sha256: String,
    lines: Vec<LineCoverage>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LineCoverage {
    line: usize,
    hits: u64,
}

#[derive(Debug)]
struct AddedRow {
    path: String,
    line: usize,
    bytes: Vec<u8>,
    no_newline: bool,
}

fn parse_artifact(bytes: &[u8], head: &str) -> Result<Artifact, &'static str> {
    let artifact: Artifact =
        serde_json::from_slice(bytes).map_err(|_| "invalid_coverage_schema")?;
    if artifact.schema != "aside-coverage/v1" || artifact.commit != head {
        return Err("coverage_commit_or_schema_mismatch");
    }
    if artifact.tool.name.trim().is_empty() || artifact.tool.version.trim().is_empty() {
        return Err("invalid_coverage_tool");
    }
    if artifact.files.len() > MAX_FILES {
        return Err("coverage_file_limit");
    }
    let mut paths = HashSet::new();
    let mut path_bytes = 0usize;
    for file in &artifact.files {
        path_bytes += file.path.len();
        if file.path.len() > MAX_PATH_BYTES || path_bytes > MAX_PATH_TOTAL {
            return Err("coverage_path_limit");
        }
        if !super::valid_repo_path(&file.path) || !paths.insert(&file.path) {
            return Err("invalid_or_duplicate_coverage_path");
        }
        if file.sha256.len() != 64 || !file.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("invalid_coverage_sha256");
        }
        if file.lines.len() > MAX_LINES {
            return Err("coverage_line_limit");
        }
        let mut lines = HashSet::new();
        for row in &file.lines {
            if row.line == 0 || !lines.insert(row.line) {
                return Err("invalid_or_duplicate_coverage_line");
            }
        }
    }
    Ok(artifact)
}

// Standard --no-ext-diff unified diffs only. Never interpret context or removed rows
// as coverage observations. Reject ambiguous headers and unverified row positions.
fn added_rows(diff: &str) -> Result<Vec<AddedRow>, &'static str> {
    let mut rows: Vec<AddedRow> = Vec::new();
    let mut path: Option<String> = None;
    let mut line = 0usize;
    let (mut old_left, mut new_left) = (0usize, 0usize);
    let mut in_hunk = false;
    let mut deleted = false;
    let mut previous_added = false;
    let mut files = 0usize;
    let mut path_bytes = 0usize;
    for text in diff.split_terminator('\n') {
        if in_hunk
            && old_left == 0
            && new_left == 0
            && !text.starts_with("\\ No newline at end of file")
        {
            in_hunk = false;
        }
        if in_hunk {
            if text == "\\ No newline at end of file" {
                if previous_added {
                    rows.last_mut().ok_or("invalid_diff_hunk")?.no_newline = true;
                }
                previous_added = false;
                continue;
            }
            previous_added = false;
            if let Some(content) = text.strip_prefix('+') {
                if new_left == 0 {
                    return Err("invalid_diff_hunk");
                }
                let name = path.as_ref().ok_or("invalid_diff_path")?;
                if rows.len() >= MAX_ADDED {
                    return Err("diff_added_row_limit");
                }
                rows.push(AddedRow {
                    path: name.clone(),
                    line,
                    bytes: content.as_bytes().to_vec(),
                    no_newline: false,
                });
                previous_added = true;
                line = line.checked_add(1).ok_or("invalid_diff_hunk")?;
                new_left -= 1;
            } else if text.starts_with('-') {
                if old_left == 0 {
                    return Err("invalid_diff_hunk");
                }
                old_left -= 1;
            } else if text.starts_with(' ') {
                if old_left == 0 || new_left == 0 {
                    return Err("invalid_diff_hunk");
                }
                old_left -= 1;
                new_left -= 1;
                line = line.checked_add(1).ok_or("invalid_diff_hunk")?;
            } else {
                return Err("invalid_diff_hunk");
            }
        } else if text.starts_with("diff --git ") {
            files += 1;
            if files > MAX_FILES {
                return Err("diff_file_limit");
            }
            path = None;
            deleted = false;
            if text.contains('"') {
                return Err("unsupported_diff_header");
            }
        } else if let Some(name) = text.strip_prefix("+++ b/") {
            path_bytes += name.len();
            if name.len() > MAX_PATH_BYTES || path_bytes > MAX_PATH_TOTAL {
                return Err("diff_path_limit");
            }
            if !super::valid_repo_path(name) {
                return Err("invalid_diff_path");
            }
            path = Some(name.to_owned());
        } else if text == "+++ /dev/null" {
            deleted = true;
            path = None;
        } else if text.starts_with("+++ ")
            || text.starts_with("old mode ")
            || text.starts_with("new mode ")
            || text.starts_with("new file mode ")
            || text.starts_with("index ") && text.contains(" 160000")
            || text.starts_with("Binary files ")
            || text.starts_with("GIT binary patch")
            || text.starts_with("Submodule ")
        {
            return Err("unsupported_diff_content");
        } else if text.starts_with("@@ ") {
            let header = text.split_once(" @@").ok_or("invalid_diff_hunk")?.0;
            let mut parts = header.split_whitespace();
            if parts.next() != Some("@@") {
                return Err("invalid_diff_hunk");
            }
            let old = parts.next().ok_or("invalid_diff_hunk")?;
            let new = parts.next().ok_or("invalid_diff_hunk")?;
            if parts.next().is_some() || !old.starts_with('-') || !new.starts_with('+') {
                return Err("invalid_diff_hunk");
            }
            let coordinate = |part: &str| -> Result<(usize, usize), &'static str> {
                let (start, count) = part[1..].split_once(',').unwrap_or((&part[1..], "1"));
                Ok((
                    start.parse().map_err(|_| "invalid_diff_hunk")?,
                    count.parse().map_err(|_| "invalid_diff_hunk")?,
                ))
            };
            let (_, old_count) = coordinate(old)?;
            (line, new_left) = coordinate(new)?;
            old_left = old_count;
            if path.is_none() && !deleted {
                return Err("invalid_diff_path");
            }
            in_hunk = true;
        } else if text.starts_with("--- ") && !text.starts_with("--- a/") && text != "--- /dev/null"
        {
            return Err("unsupported_diff_header");
        }
    }
    if in_hunk && (old_left != 0 || new_left != 0) {
        return Err("truncated_diff_hunk");
    }
    Ok(rows)
}

fn line_count(blob: &[u8]) -> usize {
    if blob.is_empty() {
        0
    } else {
        blob.iter().filter(|&&b| b == b'\n').count() + usize::from(!blob.ends_with(b"\n"))
    }
}

fn verify_rows(rows: &[AddedRow], blobs: &HashMap<String, Vec<u8>>) -> Result<(), &'static str> {
    // Sort references, not row payloads: results still follow diff order.
    let mut ordered: Vec<_> = rows.iter().collect();
    ordered.sort_unstable_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    for group in ordered.chunk_by(|a, b| a.path == b.path) {
        let blob = blobs.get(&group[0].path).ok_or("missing_pinned_blob")?;
        let count = line_count(blob);
        let mut lines = blob.split(|&b| b == b'\n');
        let mut current = 0;
        let mut actual = None;
        for row in group {
            if row.line == 0 || row.line > count {
                return Err("diff_row_out_of_bounds");
            }
            while current < row.line {
                actual = lines.next();
                current += 1;
            }
            if actual != Some(row.bytes.as_slice())
                || row.no_newline != (row.line == count && !blob.ends_with(b"\n"))
            {
                return Err("diff_row_mismatch");
            }
        }
    }
    Ok(())
}

fn evaluate(
    artifact: Artifact,
    rows: &[AddedRow],
    blobs: &HashMap<String, Vec<u8>>,
    artifact_digest: &str,
) -> Result<Value, &'static str> {
    let mut coverage = HashMap::new();
    for file in &artifact.files {
        let blob = blobs.get(&file.path).ok_or("missing_pinned_blob")?;
        if super::sha256(blob) != file.sha256 {
            return Err("coverage_blob_mismatch");
        }
        let count = line_count(blob);
        for row in &file.lines {
            if row.line > count {
                return Err("coverage_line_out_of_bounds");
            }
            coverage.insert((file.path.as_str(), row.line), row.hits);
        }
    }
    verify_rows(rows, blobs)?;
    let mut unknown = 0;
    let results: Vec<_> = rows.iter().map(|row| {
        let hits = coverage.get(&(row.path.as_str(), row.line)).copied();
        if hits.is_none() { unknown += 1; }
        json!({"path": row.path, "line": row.line, "hits": hits,
            "state": match hits { Some(0) => "uncovered", Some(_) => "covered", None => "unknown" }})
    }).collect();
    let state = if unknown == 0 { "available" } else { "partial" };
    Ok(
        json!({"state": state, "coverage": {"covered": results.iter().filter(|r| r["state"] == "covered").count(),
        "uncovered": results.iter().filter(|r| r["state"] == "uncovered").count(), "unknown": unknown},
        "reasons": if unknown == 0 { Vec::<&str>::new() } else { vec!["missing_line_coverage"] },
        "limits": ["added_lines_only", "missing_lines_are_unknown", "pinned_head_blobs_only",
            "artifact_max_bytes=4194304", "blob_max_bytes=8388608", "blobs_total_max_bytes=33554432",
            "files_max=1024", "added_rows_max=4096", "lines_per_file_max=16384",
            "path_max_bytes=1024", "paths_total_max_bytes=1048576"],
        "provenance": {"artifact_sha256": artifact_digest, "schema": artifact.schema, "commit": artifact.commit,
            "tool": {"name": artifact.tool.name, "version": artifact.tool.version}, "comparison_base": Value::Null},
        "results": results}),
    )
}

fn checked_blob_total(total: usize, next: usize) -> Result<usize, &'static str> {
    let sum = total.checked_add(next).ok_or("coverage_blob_total_limit")?;
    if sum > MAX_BLOBS_TOTAL {
        return Err("coverage_blob_total_limit");
    }
    Ok(sum)
}

pub(super) fn run(git: &PinnedGit, file: &Path) -> SourceResult {
    let bytes = super::read_artifact(file, MAX_ARTIFACT)?;
    let digest = super::sha256(&bytes);
    let artifact = parse_artifact(&bytes, &git.head)
        .map_err(|reason| SourceError::new("unavailable", reason))?;
    let diff = git
        .diff()
        .map_err(|_| SourceError::new("unavailable", "pinned_diff_unavailable"))?;
    let rows = added_rows(&diff).map_err(|reason| SourceError::new("unavailable", reason))?;
    let paths: HashSet<_> = artifact
        .files
        .iter()
        .map(|f| f.path.as_str())
        .chain(rows.iter().map(|r| r.path.as_str()))
        .collect();
    if paths.len() > MAX_FILES
        || paths.iter().any(|p| p.len() > MAX_PATH_BYTES)
        || paths.iter().map(|p| p.len()).sum::<usize>() > MAX_PATH_TOTAL
    {
        return Err(SourceError::new("unavailable", "coverage_path_limit"));
    }
    let mut blobs = HashMap::new();
    let mut total = 0usize;
    for path in paths {
        let remaining = MAX_BLOBS_TOTAL - total;
        let blob = git
            .blob(&git.head, path, MAX_BLOB.min(remaining))
            .map_err(|_| SourceError::new("unavailable", "pinned_blob_unavailable"))?;
        total = checked_blob_total(total, blob.len())
            .map_err(|reason| SourceError::new("unavailable", reason))?;
        blobs.insert(path.to_owned(), blob);
    }
    let mut value = evaluate(artifact, &rows, &blobs, &digest)
        .map_err(|reason| SourceError::new("unavailable", reason))?;
    value["provenance"]["comparison_base"] = json!(git.comparison_base);
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(lines: Value) -> (Artifact, Vec<AddedRow>, HashMap<String, Vec<u8>>) {
        let blob = b"old\nzero\npositive\nunknown\n".to_vec();
        let input = json!({"schema":"aside-coverage/v1", "commit":"head", "tool":{"name":"test","version":"1"},
            "files":[{"path":"a.txt","sha256":super::super::sha256(&blob), "lines":lines}]});
        let artifact = parse_artifact(&serde_json::to_vec(&input).unwrap(), "head").unwrap();
        let rows = added_rows("diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1,4 @@\n old\n+zero\n+positive\n+unknown\n").unwrap();
        (artifact, rows, HashMap::from([("a.txt".into(), blob)]))
    }
    #[test]
    fn zero_positive_and_missing_are_distinct() {
        let (artifact, rows, blobs) = fixture(json!([{"line":2,"hits":0},{"line":3,"hits":2}]));
        let output = evaluate(artifact, &rows, &blobs, "digest").unwrap();
        assert_eq!(output["state"], "partial");
        assert_eq!(
            output["coverage"],
            json!({"covered":1,"uncovered":1,"unknown":1})
        );
        assert_eq!(output["results"][0]["state"], "uncovered");
        assert_eq!(output["results"][1]["state"], "covered");
        assert_eq!(output["results"][2]["state"], "unknown");
        assert!(output["results"][2]["hits"].is_null());
        assert_eq!(output["provenance"]["artifact_sha256"], "digest");
    }
    #[test]
    fn header_like_added_line_and_removed_eof_marker() {
        let diff = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1,2 @@\n-old\n\\ No newline at end of file\n++++ b/looks-like-header\n+tail\n";
        let rows = added_rows(diff).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].bytes, b"+++ b/looks-like-header");
        assert!(!rows[0].no_newline);
        assert_eq!(rows[1].line, 2);
        assert!(added_rows("diff --git a/a b/a\nBinary files a/a and b/a differ\n").is_err());
        assert!(added_rows("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1,2 @@\n+x\n").is_err());
    }
    #[test]
    fn parses_real_git_diff_with_header_like_added_text() {
        use std::{fs, process::Command};
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .current_dir(dir.path())
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.invalid"]);
        git(&["config", "user.name", "Test"]);
        fs::write(dir.path().join("a.txt"), "old\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-qm", "base"]);
        let base = git(&["rev-parse", "HEAD"]);
        fs::write(
            dir.path().join("a.txt"),
            "+++ b/looks-like-header\npositive\n",
        )
        .unwrap();
        git(&["commit", "-qam", "head"]);
        let head = git(&["rev-parse", "HEAD"]);
        let diff = git(&[
            "diff-tree",
            "--no-commit-id",
            "-r",
            "-p",
            "--no-renames",
            "--unified=0",
            base.trim(),
            head.trim(),
        ]);
        let rows = added_rows(&diff).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].bytes, b"+++ b/looks-like-header");
        let blob = fs::read(dir.path().join("a.txt")).unwrap();
        verify_rows(&rows, &HashMap::from([("a.txt".into(), blob)])).unwrap();
    }
    #[test]
    fn aggregate_blob_limit_and_large_linear_verification() {
        assert_eq!(
            checked_blob_total(24 * 1024 * 1024, 8 * 1024 * 1024),
            Ok(MAX_BLOBS_TOTAL)
        );
        assert_eq!(
            checked_blob_total(MAX_BLOBS_TOTAL, 1),
            Err("coverage_blob_total_limit")
        );
        let mut blob = Vec::new();
        let mut rows = Vec::new();
        for line in 1..=MAX_ADDED {
            blob.extend_from_slice(b"value\n");
            rows.push(AddedRow {
                path: "large.txt".into(),
                line,
                bytes: b"value".to_vec(),
                no_newline: false,
            });
        }
        blob.extend(std::iter::repeat_n(b'x', 4 * 1024 * 1024));
        verify_rows(&rows, &HashMap::from([("large.txt".into(), blob)])).unwrap();
    }
    #[test]
    fn empty_changes_and_unsupported_headers() {
        assert!(added_rows("diff --git a/a b/a\ndeleted file mode 100644\n--- a/a\n+++ /dev/null\n@@ -1 +0,0 @@\n-old\n").unwrap().is_empty());
        assert!(
            added_rows("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +0,0 @@\n-old\n")
                .unwrap()
                .is_empty()
        );
        assert!(added_rows("diff --git a/a b/a\nold mode 100644\nnew mode 100755\n").is_err());
        assert!(added_rows("diff --git \"a/a b\" \"b/a b\"\n").is_err());
        let rows =
            added_rows("diff --git a/a b b/a b\n--- a/a b\n+++ b/a b\n@@ -0,0 +1 @@\n+hello\n")
                .unwrap();
        assert_eq!(rows[0].path, "a b");
        let artifact = parse_artifact(&serde_json::to_vec(&json!({"schema":"aside-coverage/v1", "commit":"head", "tool":{"name":"test","version":"1"}, "files":[]})).unwrap(), "head").unwrap();
        let output = evaluate(artifact, &[], &HashMap::new(), "digest").unwrap();
        assert_eq!(output["state"], "available");
        assert_eq!(
            output["coverage"],
            json!({"covered":0,"uncovered":0,"unknown":0})
        );
    }
    #[test]
    fn malformed_mismatched_and_out_of_bounds_fail_closed() {
        let (artifact, rows, mut blobs) = fixture(json!([{"line":99,"hits":0}]));
        assert_eq!(
            evaluate(artifact, &rows, &blobs, "digest").unwrap_err(),
            "coverage_line_out_of_bounds"
        );
        let (artifact, rows, _) = fixture(json!([]));
        blobs.insert("a.txt".into(), b"different".to_vec());
        assert_eq!(
            evaluate(artifact, &rows, &blobs, "digest").unwrap_err(),
            "coverage_blob_mismatch"
        );
        let (artifact, mut rows, blobs) = fixture(json!([]));
        rows[0].bytes = b"wrong".to_vec();
        assert_eq!(
            evaluate(artifact, &rows, &blobs, "digest").unwrap_err(),
            "diff_row_mismatch"
        );
        let bad = br#"{"schema":"aside-coverage/v1","commit":"head","tool":{"name":"x","version":"1"},"files":[{"path":"../bad","sha256":"none","lines":[]}]}"#;
        assert!(parse_artifact(bad, "head").is_err());
        let input = json!({"schema":"aside-coverage/v1","commit":"other","tool":{"name":"x","version":"1"},"files":[]});
        assert_eq!(
            parse_artifact(&serde_json::to_vec(&input).unwrap(), "head").err(),
            Some("coverage_commit_or_schema_mismatch")
        );
    }
}
