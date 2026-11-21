use super::{Limits, fingerprint, parse_unified};
use crate::model::{ChangeKind, ChangedFile, DiffLine, LineKind, Side};

use super::parser::FileBuilder;

/// Find the diff line a review comment can attach to.
pub fn find_line(file: &ChangedFile, side: Side, line: u32) -> Option<&DiffLine> {
    file.hunks
        .iter()
        .flat_map(|h| &h.lines)
        .find(|l| match side {
            Side::Right => {
                l.new_line == Some(line) && matches!(l.kind, LineKind::Context | LineKind::Addition)
            }
            Side::Left => {
                l.old_line == Some(line) && matches!(l.kind, LineKind::Context | LineKind::Deletion)
            }
        })
}

/// Build a file from GitHub's REST `pulls/{n}/files` entry (fallback path).
pub struct ApiFile<'a> {
    pub path: &'a str,
    pub previous_path: Option<&'a str>,
    pub status: &'a str,
    pub patch: Option<&'a str>,
    pub blob_sha: Option<&'a str>,
    pub additions: u32,
    pub deletions: u32,
}

pub fn file_from_api(
    api: &ApiFile,
    head_sha: &str,
    limits: &Limits,
    warnings: &mut Vec<String>,
) -> ChangedFile {
    let kind = match api.status {
        "added" => ChangeKind::Added,
        "removed" => ChangeKind::Removed,
        "renamed" => ChangeKind::Renamed,
        "copied" => ChangeKind::Copied,
        _ => ChangeKind::Modified,
    };
    let old = api.previous_path.unwrap_or(api.path);
    let mut text = format!("diff --git a/{old} b/{}\n", api.path);
    if kind == ChangeKind::Renamed {
        text.push_str(&format!("rename from {old}\nrename to {}\n", api.path));
    }
    let old_side = if kind == ChangeKind::Added {
        "/dev/null".to_string()
    } else {
        format!("a/{old}")
    };
    let new_side = if kind == ChangeKind::Removed {
        "/dev/null".to_string()
    } else {
        format!("b/{}", api.path)
    };
    text.push_str(&format!("--- {old_side}\n+++ {new_side}\n"));
    if let Some(patch) = api.patch {
        text.push_str(patch);
        text.push('\n');
    }
    let mut parsed = parse_unified(&text, head_sha, limits);
    warnings.append(&mut parsed.warnings);
    let mut file = parsed
        .files
        .pop()
        .unwrap_or_else(|| FileBuilder::default().finish(head_sha, warnings));
    file.change_kind = kind;
    file.content_digest = api.blob_sha.map(str::to_string);
    if api.patch.is_none() {
        if kind != ChangeKind::Renamed || api.additions != 0 || api.deletions != 0 {
            warnings.push(format!("{}: GitHub did not return a patch", api.path));
            file.patch_complete = false;
        }
        // GitHub omits the patch for oversized files and for files dropped by
        // aggregate truncation; in the truncation case it also zeroes these
        // counts. They are display-only best effort here: the file is already
        // flagged incomplete above, and the temporary-Git recovery in
        // `github::snapshot` supplies the true patch when it validates.
        file.additions = api.additions;
        file.deletions = api.deletions;
    } else if !file.is_binary
        && (file.additions != api.additions || file.deletions != api.deletions)
    {
        warnings.push(format!(
            "{}: patch line counts do not match GitHub's additions/deletions",
            api.path
        ));
        file.patch_complete = false;
    }
    file.diff_digest = fingerprint(&file, head_sha);
    file
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_whole_hunk_is_incomplete_and_head_bound() {
        let api = ApiFile {
            path: "a.txt",
            previous_path: None,
            status: "modified",
            patch: Some("@@ -1 +1 @@\n-old\n+new"),
            blob_sha: Some("blob"),
            additions: 2,
            deletions: 1,
        };
        let mut warnings = Vec::new();
        let first = file_from_api(&api, "head-one", &Limits::default(), &mut warnings);
        let second = file_from_api(&api, "head-two", &Limits::default(), &mut warnings);
        assert!(!first.patch_complete);
        assert_ne!(first.diff_digest, second.diff_digest);
    }

    #[test]
    fn explicit_binary_patch_does_not_require_text_line_counts() {
        let api = ApiFile {
            path: "image.png",
            previous_path: None,
            status: "modified",
            patch: Some("GIT binary patch\nliteral 0"),
            blob_sha: Some("blob"),
            additions: 1,
            deletions: 1,
        };
        let mut warnings = Vec::new();
        let file = file_from_api(&api, "head", &Limits::default(), &mut warnings);
        assert!(file.is_binary);
        assert!(file.patch_complete);
    }

    #[test]
    fn missing_zero_count_patch_is_not_proof_of_binary() {
        let api = ApiFile {
            path: "empty.txt",
            previous_path: None,
            status: "modified",
            patch: None,
            blob_sha: Some("blob"),
            additions: 0,
            deletions: 0,
        };
        let mut warnings = Vec::new();
        let file = file_from_api(&api, "head", &Limits::default(), &mut warnings);
        assert!(!file.patch_complete);
        assert!(!file.is_binary);
    }
}
