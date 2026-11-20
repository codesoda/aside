//! Unified diff parser and canonical per-file review fingerprints.
//!
//! The parser accepts `git diff` output (as produced by `gh pr diff`) and
//! turns it into files → hunks → lines, keeping old/new line numbers and
//! GitHub's per-file patch `position`.

use crate::model::{ChangeKind, ChangedFile, DiffHunk, DiffLine, LineKind, Side};
use crate::util::{file_id, sha256_hex};

/// Bump when the fingerprint algorithm changes. Old acknowledgements with a
/// different version never count as "reviewed".
pub const FINGERPRINT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_lines_per_file: usize,
    pub max_files: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_lines_per_file: 20_000,
            max_files: 3_000,
        }
    }
}

#[derive(Debug, Default)]
pub struct ParsedDiff {
    pub files: Vec<ChangedFile>,
    pub warnings: Vec<String>,
}

#[derive(Default)]
struct FileBuilder {
    old_path: Option<String>,
    new_path: Option<String>,
    rename_from: Option<String>,
    rename_to: Option<String>,
    kind: Option<ChangeKind>,
    old_mode: Option<String>,
    new_mode: Option<String>,
    is_binary: bool,
    blob: Option<String>,
    hunks: Vec<DiffHunk>,
    position: u32,
    line_count: usize,
    truncated: bool,
    old_left: u32,
    new_left: u32,
    old_no: u32,
    new_no: u32,
}

impl FileBuilder {
    fn path(&self) -> String {
        self.rename_to
            .clone()
            .or_else(|| self.new_path.clone())
            .or_else(|| self.old_path.clone())
            .unwrap_or_default()
    }

    fn previous_path(&self, path: &str) -> Option<String> {
        let prev = self.rename_from.clone().or_else(|| self.old_path.clone())?;
        (prev != path).then_some(prev)
    }

    fn in_hunk(&self) -> bool {
        !self.hunks.is_empty() && (self.old_left > 0 || self.new_left > 0)
    }

    fn start_hunk(&mut self, line: &str) -> bool {
        let Some((old_start, old_count, new_start, new_count, header)) = parse_hunk_header(line)
        else {
            return false;
        };
        if !self.hunks.is_empty() {
            // GitHub counts every hunk header after the first as a patch line.
            self.position += 1;
        }
        self.old_left = old_count;
        self.new_left = new_count;
        self.old_no = old_start;
        self.new_no = new_start;
        self.hunks.push(DiffHunk {
            id: String::new(),
            old_start,
            old_count,
            new_start,
            new_count,
            header,
            lines: Vec::new(),
        });
        true
    }

    fn push_line(&mut self, kind: LineKind, text: &str, limits: &Limits) {
        self.position += 1;
        let (old_line, new_line) = match kind {
            LineKind::Context => {
                let pair = (Some(self.old_no), Some(self.new_no));
                self.old_no += 1;
                self.new_no += 1;
                self.old_left = self.old_left.saturating_sub(1);
                self.new_left = self.new_left.saturating_sub(1);
                pair
            }
            LineKind::Deletion => {
                let pair = (Some(self.old_no), None);
                self.old_no += 1;
                self.old_left = self.old_left.saturating_sub(1);
                pair
            }
            LineKind::Addition => {
                let pair = (None, Some(self.new_no));
                self.new_no += 1;
                self.new_left = self.new_left.saturating_sub(1);
                pair
            }
            LineKind::Meta => (None, None),
        };
        if self.line_count >= limits.max_lines_per_file {
            self.truncated = true;
            return;
        }
        self.line_count += 1;
        let position = self.position;
        if let Some(hunk) = self.hunks.last_mut() {
            hunk.lines.push(DiffLine {
                id: String::new(),
                kind,
                text: text.to_string(),
                old_line,
                new_line,
                patch_position: position,
            });
        }
    }

    fn finish(self, head_sha: &str, warnings: &mut Vec<String>) -> ChangedFile {
        let path = self.path();
        let previous_path = self.previous_path(&path);
        let kind = self.kind.unwrap_or(if previous_path.is_some() {
            ChangeKind::Renamed
        } else {
            ChangeKind::Modified
        });
        if self.truncated {
            warnings.push(format!(
                "{path}: diff truncated after the per-file line limit"
            ));
        }
        let id = file_id(&path);
        let mut hunks = self.hunks;
        let (mut additions, mut deletions) = (0, 0);
        for (h, hunk) in hunks.iter_mut().enumerate() {
            hunk.id = format!("{id}:h{h}");
            for (l, line) in hunk.lines.iter_mut().enumerate() {
                line.id = format!("{id}:h{h}:l{l}");
                match line.kind {
                    LineKind::Addition => additions += 1,
                    LineKind::Deletion => deletions += 1,
                    _ => {}
                }
            }
        }
        let mut file = ChangedFile {
            id,
            path,
            previous_path,
            change_kind: kind,
            old_mode: self.old_mode,
            new_mode: self.new_mode,
            is_binary: self.is_binary,
            additions,
            deletions,
            content_digest: self.blob,
            diff_digest: String::new(),
            fingerprint_version: FINGERPRINT_VERSION,
            patch_complete: !self.truncated,
            hunks,
        };
        file.diff_digest = fingerprint(&file, head_sha);
        file
    }
}

/// Parse a whole `git diff`. `head_sha` only feeds fingerprints of files whose
/// change can't be described canonically (truncated or unknown binary).
pub fn parse_unified(text: &str, head_sha: &str, limits: &Limits) -> ParsedDiff {
    let mut out = ParsedDiff::default();
    let mut current: Option<FileBuilder> = None;
    let mut skipping_binary_patch = false;

    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }

    for line in lines {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if let Some(done) = current.take() {
                out.files.push(done.finish(head_sha, &mut out.warnings));
            }
            if out.files.len() >= limits.max_files {
                out.warnings.push(format!(
                    "diff has more than {} files; the rest were skipped",
                    limits.max_files
                ));
                return out;
            }
            let (old, new) = parse_git_header_paths(rest);
            current = Some(FileBuilder {
                old_path: old,
                new_path: new,
                ..Default::default()
            });
            skipping_binary_patch = false;
            continue;
        }
        let Some(file) = current.as_mut() else {
            continue;
        };
        if skipping_binary_patch {
            continue;
        }

        if file.in_hunk() {
            match line.as_bytes().first() {
                Some(b' ') => file.push_line(LineKind::Context, &line[1..], limits),
                Some(b'+') => file.push_line(LineKind::Addition, &line[1..], limits),
                Some(b'-') => file.push_line(LineKind::Deletion, &line[1..], limits),
                Some(b'\\') => file.push_line(LineKind::Meta, line, limits),
                // Some tools strip the single space from empty context lines.
                None => file.push_line(LineKind::Context, "", limits),
                _ => {}
            }
            continue;
        }
        if line.starts_with('\\') && !file.hunks.is_empty() {
            file.push_line(LineKind::Meta, line, limits);
        } else if line.starts_with("@@") {
            file.start_hunk(line);
        } else {
            parse_extended_header(file, line, &mut skipping_binary_patch);
        }
    }
    if let Some(done) = current.take() {
        out.files.push(done.finish(head_sha, &mut out.warnings));
    }
    out
}

fn parse_extended_header(file: &mut FileBuilder, line: &str, skipping_binary_patch: &mut bool) {
    if let Some(m) = line.strip_prefix("old mode ") {
        file.old_mode = Some(m.trim().into());
    } else if let Some(m) = line.strip_prefix("new mode ") {
        file.new_mode = Some(m.trim().into());
    } else if let Some(m) = line.strip_prefix("new file mode ") {
        file.kind = Some(ChangeKind::Added);
        file.new_mode = Some(m.trim().into());
    } else if let Some(m) = line.strip_prefix("deleted file mode ") {
        file.kind = Some(ChangeKind::Removed);
        file.old_mode = Some(m.trim().into());
    } else if let Some(p) = line.strip_prefix("rename from ") {
        file.kind = Some(ChangeKind::Renamed);
        file.rename_from = Some(unquote(p));
    } else if let Some(p) = line.strip_prefix("rename to ") {
        file.rename_to = Some(unquote(p));
    } else if let Some(p) = line.strip_prefix("copy from ") {
        file.kind = Some(ChangeKind::Copied);
        file.rename_from = Some(unquote(p));
    } else if let Some(p) = line.strip_prefix("copy to ") {
        file.rename_to = Some(unquote(p));
    } else if let Some(rest) = line.strip_prefix("index ") {
        let range = rest.split_whitespace().next().unwrap_or_default();
        if let Some((_, new)) = range.split_once("..")
            && !new.chars().all(|c| c == '0')
        {
            file.blob = Some(new.to_string());
        }
    } else if line.starts_with("Binary files ") {
        file.is_binary = true;
    } else if line == "GIT binary patch" {
        file.is_binary = true;
        *skipping_binary_patch = true;
    } else if let Some(p) = line.strip_prefix("--- ") {
        file.old_path = strip_side_prefix(p, "a/");
    } else if let Some(p) = line.strip_prefix("+++ ") {
        file.new_path = strip_side_prefix(p, "b/");
    }
}

fn parse_hunk_header(line: &str) -> Option<(u32, u32, u32, u32, String)> {
    let rest = line.strip_prefix("@@ -")?;
    let (ranges, header) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let parse = |r: &str| -> Option<(u32, u32)> {
        match r.split_once(',') {
            Some((s, c)) => Some((s.parse().ok()?, c.parse().ok()?)),
            None => Some((r.parse().ok()?, 1)),
        }
    };
    let (os, oc) = parse(old)?;
    let (ns, nc) = parse(new)?;
    Some((os, oc, ns, nc, header.trim_start().to_string()))
}

/// `--- a/x` / `+++ b/x` / `/dev/null`, possibly C-quoted.
fn strip_side_prefix(raw: &str, prefix: &str) -> Option<String> {
    let path = unquote(raw.trim_end_matches('\t'));
    if path == "/dev/null" {
        return None;
    }
    Some(
        path.strip_prefix(prefix)
            .map(str::to_string)
            .unwrap_or(path),
    )
}

/// Best-effort paths from `diff --git a/X b/Y`. `---`/`+++` and rename lines override them.
fn parse_git_header_paths(rest: &str) -> (Option<String>, Option<String>) {
    let (a, b) = if rest.starts_with('"') {
        let (first, remaining) = split_quoted(rest);
        (first, unquote(remaining.trim_start()))
    } else if let Some((a, b)) = split_symmetric(rest) {
        (a, b)
    } else if let Some(idx) = rest.rfind(" b/") {
        (rest[..idx].to_string(), unquote(&rest[idx + 1..]))
    } else {
        return (None, None);
    };
    let strip = |p: String, pre: &str| p.strip_prefix(pre).map(str::to_string).unwrap_or(p);
    (Some(strip(a, "a/")), Some(strip(b, "b/")))
}

/// For unrenamed files the header is `a/P b/P`; split it exactly in half.
fn split_symmetric(rest: &str) -> Option<(String, String)> {
    if rest.len().is_multiple_of(2) {
        return None;
    }
    let half = rest.len() / 2;
    let (a, b) = (rest.get(..half)?, rest.get(half + 1..)?);
    (a.strip_prefix("a/")? == b.strip_prefix("b/")?).then(|| (a.to_string(), b.to_string()))
}

fn split_quoted(s: &str) -> (String, &str) {
    let bytes = s.as_bytes();
    let mut i = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return (unquote(&s[..=i]), &s[i + 1..]),
            _ => i += 1,
        }
    }
    (unquote(s), "")
}

/// Undo git's C-style path quoting: `"a/tab\there"`, octal UTF-8 bytes, etc.
pub fn unquote(s: &str) -> String {
    let Some(inner) = s.strip_prefix('"').and_then(|x| x.strip_suffix('"')) else {
        return s.to_string();
    };
    let bytes = inner.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 >= bytes.len() {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        let c = bytes[i + 1];
        i += 2;
        match c {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'a' => out.push(7),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'v' => out.push(11),
            b'0'..=b'7' => {
                let mut v = u32::from(c - b'0');
                let mut n = 1;
                while n < 3 && i < bytes.len() && (b'0'..=b'7').contains(&bytes[i]) {
                    v = v * 8 + u32::from(bytes[i] - b'0');
                    i += 1;
                    n += 1;
                }
                out.push(v as u8);
            }
            other => out.push(other),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Canonical review fingerprint (version 1).
///
/// Includes: path and previous path, change kind, modes, binary marker and
/// content digest for binaries, and every hunk's line kinds and exact text
/// (CR and no-newline markers included). Hunk start offsets are excluded: if
/// the same change shifts because of edits elsewhere in the file, the review
/// meaning is the same. Truncated patches and binaries without a content
/// digest include the head SHA, so they always need re-review when the head
/// moves (fail closed).
pub fn fingerprint(file: &ChangedFile, head_sha: &str) -> String {
    let mut c = String::with_capacity(256);
    c.push_str(&format!("aside-fingerprint-v{FINGERPRINT_VERSION}\n"));
    c.push_str(&format!("path:{}\n", escape(&file.path)));
    c.push_str(&format!(
        "prev:{}\n",
        escape(file.previous_path.as_deref().unwrap_or(""))
    ));
    c.push_str(&format!("kind:{}\n", file.change_kind.as_str()));
    c.push_str(&format!(
        "mode:{}>{}\n",
        file.old_mode.as_deref().unwrap_or(""),
        file.new_mode.as_deref().unwrap_or("")
    ));
    if file.is_binary {
        match &file.content_digest {
            Some(blob) => c.push_str(&format!("binary:{blob}\n")),
            None => c.push_str(&format!("binary-unknown:{head_sha}\n")),
        }
    }
    if !file.patch_complete {
        c.push_str(&format!("incomplete:{head_sha}\n"));
    }
    for hunk in &file.hunks {
        c.push_str(&format!("@@{},{}\n", hunk.old_count, hunk.new_count));
        for line in &hunk.lines {
            let marker = match line.kind {
                LineKind::Context => ' ',
                LineKind::Addition => '+',
                LineKind::Deletion => '-',
                LineKind::Meta => '\\',
            };
            c.push(marker);
            c.push_str(&escape(&line.text));
            c.push('\n');
        }
    }
    sha256_hex(c.as_bytes())
}

/// Escape newlines and backslashes so the canonical form is unambiguous.
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n")
}

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
        if api.additions + api.deletions > 0 {
            warnings.push(format!(
                "{}: GitHub did not return a patch (file too large)",
                api.path
            ));
            file.patch_complete = false;
        } else if kind != ChangeKind::Renamed {
            file.is_binary = true;
        }
        file.additions = api.additions;
        file.deletions = api.deletions;
    }
    file.diff_digest = fingerprint(&file, head_sha);
    file
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,4 @@ fn main
 one
-two
+TWO
+three
 four
@@ -10,2 +11,2 @@
 ten
-eleven
+ELEVEN
\\ No newline at end of file
diff --git a/old name.txt b/new name.txt
similarity index 90%
rename from old name.txt
rename to new name.txt
index 3333333..4444444 100644
--- a/old name.txt
+++ b/new name.txt
@@ -1 +1 @@
-a
+b
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 5555555..0000000
--- a/gone.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-x
-y
diff --git a/img.png b/img.png
new file mode 100644
index 0000000..6666666
Binary files /dev/null and b/img.png differ
diff --git a/run.sh b/run.sh
old mode 100644
new mode 100755
";

    fn parse(text: &str) -> ParsedDiff {
        parse_unified(text, "head", &Limits::default())
    }

    #[test]
    fn parses_files_hunks_and_lines() {
        let d = parse(SAMPLE);
        assert_eq!(d.files.len(), 5);
        let f = &d.files[0];
        assert_eq!(f.path, "src/lib.rs");
        assert_eq!(f.change_kind, ChangeKind::Modified);
        assert_eq!((f.additions, f.deletions), (3, 2));
        assert_eq!(f.hunks.len(), 2);
        assert_eq!(f.hunks[0].header, "fn main");
        let l = &f.hunks[0].lines;
        assert_eq!((l[0].old_line, l[0].new_line), (Some(1), Some(1)));
        assert_eq!(
            (l[1].kind, l[1].old_line, l[1].new_line),
            (LineKind::Deletion, Some(2), None)
        );
        assert_eq!((l[2].kind, l[2].new_line), (LineKind::Addition, Some(2)));
        assert_eq!((l[4].old_line, l[4].new_line), (Some(3), Some(4)));
        assert_eq!(f.content_digest.as_deref(), Some("2222222"));
    }

    #[test]
    fn patch_positions_follow_github_rules() {
        let f = &parse(SAMPLE).files[0];
        let positions: Vec<u32> = f
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter().map(|l| l.patch_position))
            .collect();
        // First hunk lines 1..=5, second header counts as 6, then 7..=10.
        assert_eq!(positions, vec![1, 2, 3, 4, 5, 7, 8, 9, 10]);
        assert_eq!(f.hunks[1].lines[3].kind, LineKind::Meta);
    }

    #[test]
    fn detects_rename_delete_binary_and_mode() {
        let d = parse(SAMPLE);
        assert_eq!(d.files[1].change_kind, ChangeKind::Renamed);
        assert_eq!(d.files[1].path, "new name.txt");
        assert_eq!(d.files[1].previous_path.as_deref(), Some("old name.txt"));
        assert_eq!(d.files[2].change_kind, ChangeKind::Removed);
        assert_eq!(d.files[2].path, "gone.txt");
        assert_eq!(d.files[2].content_digest, None);
        assert!(d.files[3].is_binary);
        assert_eq!(d.files[3].change_kind, ChangeKind::Added);
        assert_eq!(d.files[4].path, "run.sh");
        assert_eq!(d.files[4].new_mode.as_deref(), Some("100755"));
        assert!(d.files[4].hunks.is_empty());
    }

    #[test]
    fn unquotes_c_style_paths() {
        let text = "diff --git \"a/t\\303\\251st.txt\" \"b/t\\303\\251st.txt\"\n--- \"a/t\\303\\251st.txt\"\n+++ \"b/t\\303\\251st.txt\"\n@@ -1 +1 @@\n-a\n+b\n";
        assert_eq!(parse(text).files[0].path, "tést.txt");
    }

    #[test]
    fn keeps_crlf_in_text() {
        let text = "diff --git a/w.txt b/w.txt\n--- a/w.txt\n+++ b/w.txt\n@@ -1 +1 @@\n-a\r\n+a\n";
        let f = &parse(text).files[0];
        assert_eq!(f.hunks[0].lines[0].text, "a\r");
        assert_ne!(f.hunks[0].lines[0].text, f.hunks[0].lines[1].text);
    }

    #[test]
    fn fingerprint_ignores_offsets_but_not_content() {
        let a = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n ctx\n-a\n+b\n";
        let shifted =
            "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -40,2 +41,2 @@ other\n ctx\n-a\n+b\n";
        let changed = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n ctx\n-a\n+c\n";
        let context = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n CTX\n-a\n+b\n";
        let fa = &parse(a).files[0].diff_digest;
        assert_eq!(
            fa,
            &parse_unified(a, "other-head", &Limits::default()).files[0].diff_digest
        );
        assert_eq!(fa, &parse(shifted).files[0].diff_digest);
        assert_ne!(fa, &parse(changed).files[0].diff_digest);
        assert_ne!(fa, &parse(context).files[0].diff_digest);
    }

    #[test]
    fn truncated_patch_fingerprint_depends_on_head() {
        let text =
            "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,3 +1,3 @@\n-a\n-b\n-c\n+d\n+e\n+f\n";
        let limits = Limits {
            max_lines_per_file: 2,
            ..Default::default()
        };
        let one = parse_unified(text, "h1", &limits);
        let two = parse_unified(text, "h2", &limits);
        assert!(!one.files[0].patch_complete);
        assert_eq!(one.warnings.len(), 1);
        assert_ne!(one.files[0].diff_digest, two.files[0].diff_digest);
    }

    #[test]
    fn finds_comment_lines_by_side() {
        let f = &parse(SAMPLE).files[0];
        assert_eq!(find_line(f, Side::Right, 2).unwrap().text, "TWO");
        assert_eq!(find_line(f, Side::Left, 2).unwrap().text, "two");
        assert!(find_line(f, Side::Right, 99).is_none());
    }

    #[test]
    fn builds_files_from_rest_api_entries() {
        let mut w = Vec::new();
        let limits = Limits::default();
        let f = file_from_api(
            &ApiFile {
                path: "src/a.rs",
                previous_path: None,
                status: "modified",
                patch: Some("@@ -1 +1 @@\n-a\n+b"),
                blob_sha: Some("abc"),
                additions: 1,
                deletions: 1,
            },
            "head",
            &limits,
            &mut w,
        );
        assert_eq!((f.additions, f.deletions, f.hunks.len()), (1, 1, 1));
        let big = file_from_api(
            &ApiFile {
                path: "big.sql",
                previous_path: None,
                status: "modified",
                patch: None,
                blob_sha: None,
                additions: 9000,
                deletions: 0,
            },
            "head",
            &limits,
            &mut w,
        );
        assert!(!big.patch_complete);
        assert_eq!(w.len(), 1);
    }
}
