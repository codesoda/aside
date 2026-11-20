//! Unified diff parser and canonical per-file review fingerprints.
//!
//! The parser accepts `git diff` output (as produced by `gh pr diff`) and
//! turns it into files → hunks → lines, keeping old/new line numbers and
//! GitHub's per-file patch `position`.

use crate::model::ChangedFile;

mod api;
mod fingerprint;
mod parser;

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

pub use api::{ApiFile, file_from_api, find_line};
pub use fingerprint::{FINGERPRINT_VERSION, fingerprint};
pub use parser::parse_unified;
// Public API for callers outside this binary's internal module graph.
#[allow(unused_imports)]
pub use parser::unquote;
