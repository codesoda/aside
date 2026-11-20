use crate::model::{ChangedFile, LineKind};
use crate::util::sha256_hex;

/// Bump when the fingerprint algorithm changes. Old acknowledgements with a
/// different version never count as "reviewed".
pub const FINGERPRINT_VERSION: u32 = 1;

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
