use serde::{Deserialize, Serialize};
use ts_rs::TS;

// Pull request snapshot (immutable-ish data fetched from GitHub)

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum ChangeKind {
    Added,
    Modified,
    Removed,
    Renamed,
    Copied,
}

impl ChangeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Removed => "removed",
            Self::Renamed => "renamed",
            Self::Copied => "copied",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum LineKind {
    Context,
    Addition,
    Deletion,
    /// `\ No newline at end of file` and similar markers.
    Meta,
}

/// GitHub's review-comment side: LEFT is the base (old) file, RIGHT is the head (new) file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "UPPERCASE")]
#[ts(export)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Left => "LEFT",
            Self::Right => "RIGHT",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "LEFT" => Some(Self::Left),
            "RIGHT" => Some(Self::Right),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DiffLine {
    pub id: String,
    pub kind: LineKind,
    /// Line text without the leading diff marker or trailing newline.
    pub text: String,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    /// GitHub's legacy `position`: 1-based offset into this file's patch.
    pub patch_position: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DiffHunk {
    pub id: String,
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    /// Text after the second `@@` (often the enclosing function).
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ChangedFile {
    /// Stable ID derived from the current path. Not tied to display order.
    pub id: String,
    pub path: String,
    pub previous_path: Option<String>,
    pub change_kind: ChangeKind,
    pub old_mode: Option<String>,
    pub new_mode: Option<String>,
    pub is_binary: bool,
    pub additions: u32,
    pub deletions: u32,
    /// Blob SHA of the new file contents, when known (may be abbreviated).
    pub content_digest: Option<String>,
    /// Canonical review fingerprint of this file's patch. See `diff::fingerprint`.
    pub diff_digest: String,
    pub fingerprint_version: u32,
    /// False when the patch was truncated or missing; such files can't be published to.
    pub patch_complete: bool,
    pub hunks: Vec<DiffHunk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PullRequest {
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub body: String,
    pub author: String,
    pub url: String,
    pub state: String,
    pub is_draft: bool,
    pub base_ref: String,
    pub base_sha: String,
    pub head_ref: String,
    pub head_sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImportedReview {
    pub author: String,
    pub state: String,
    pub body: String,
    pub submitted_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct IssueComment {
    pub author: String,
    pub body: String,
    pub created_at: Option<String>,
}

/// A review thread as fetched from GitHub, before it is merged into local state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubThread {
    pub node_id: String,
    pub path: String,
    pub side: Option<Side>,
    pub line: Option<u32>,
    pub start_line: Option<u32>,
    pub original_line: Option<u32>,
    pub is_outdated: bool,
    pub is_resolved: bool,
    pub comments: Vec<GithubComment>,
    /// REST-only threads have no verified GraphQL anchor or resolution.
    #[serde(default)]
    pub rest_only: bool,
    /// Both sources and every page were enumerated successfully.
    #[serde(default)]
    pub enumeration_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubComment {
    pub node_id: String,
    pub database_id: Option<i64>,
    pub author: String,
    pub body: String,
    pub created_at: String,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub in_reply_to_id: Option<i64>,
}

/// Everything fetched for one PR revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrSnapshot {
    pub schema_version: u32,
    pub pr: PullRequest,
    /// Provider's immutable repository ID, when known.
    pub provider_repository_id: Option<String>,
    pub files: Vec<ChangedFile>,
    pub github_threads: Vec<GithubThread>,
    /// True only when both review-comment APIs were fully enumerated.
    #[serde(default)]
    pub github_threads_complete: bool,
    pub reviews: Vec<ImportedReview>,
    pub issue_comments: Vec<IssueComment>,
    pub warnings: Vec<String>,
    /// False when ingestion could not fetch every patch. Publishing is blocked.
    pub complete: bool,
    pub fetched_at: String,
}
