//! Typed domain model shared by the API, persistence and the frontend.
//!
//! Every type here derives `ts_rs::TS`; `cargo test` writes matching
//! TypeScript declarations into `web/src/generated/`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Version of the JSON contract served to the browser and agents.
pub const SCHEMA_VERSION: u32 = 1;

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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubComment {
    pub node_id: String,
    pub database_id: Option<i64>,
    pub author: String,
    pub body: String,
    pub created_at: String,
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
    pub reviews: Vec<ImportedReview>,
    pub issue_comments: Vec<IssueComment>,
    pub warnings: Vec<String>,
    /// False when ingestion could not fetch every patch. Publishing is blocked.
    pub complete: bool,
    pub fetched_at: String,
}

// Repository rules

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum RuleAction {
    Include,
    Exclude,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum RuleKind {
    File,
    Directory,
    Glob,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ExclusionRule {
    pub id: String,
    pub action: RuleAction,
    pub kind: RuleKind,
    pub expression: String,
    pub enabled: bool,
    /// Evaluation order. Later rules win.
    pub order: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Repository {
    pub id: String,
    pub host: String,
    pub owner: String,
    pub name: String,
    pub provider_id: Option<String>,
}

// Review state (mutable, PR-scoped)

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum Actor {
    Human,
    Agent,
    Github,
}

impl Actor {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
            Self::Github => "github",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "agent" => Self::Agent,
            "github" => Self::Github,
            _ => Self::Human,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum FileStatus {
    Ignored,
    ReviewedCurrent,
    ChangedSinceReviewed,
    Unreviewed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FileScope {
    pub ignored: bool,
    /// The last enabled matching rule, which decides the outcome.
    pub effective_rule_id: Option<String>,
    /// Every enabled rule that matches, in evaluation order.
    pub matching_rule_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FileState {
    pub file_id: String,
    pub path: String,
    pub status: FileStatus,
    /// Review status ignoring scope, so re-including a file shows its real state.
    pub review_status: FileStatus,
    pub scope: FileScope,
    pub last_reviewed_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Progress {
    pub total: u32,
    pub in_scope: u32,
    pub reviewed: u32,
    pub changed_since_reviewed: u32,
    pub unreviewed: u32,
    pub ignored: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Acknowledgement {
    pub id: String,
    pub path: String,
    /// `reviewed` or `cleared` (the reviewer unmarked it).
    pub kind: String,
    pub fingerprint_version: u32,
    pub diff_digest: String,
    pub content_digest: Option<String>,
    pub head_sha: String,
    pub base_sha: String,
    pub actor: Actor,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum ThreadSource {
    Local,
    Github,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum AnchorState {
    /// The anchored line still exists with the same text.
    Current,
    /// The file exists but the anchored line changed or moved.
    Outdated,
    /// The file is no longer in the PR.
    Orphaned,
}

impl AnchorState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Outdated => "outdated",
            Self::Orphaned => "orphaned",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "current" => Self::Current,
            "orphaned" => Self::Orphaned,
            _ => Self::Outdated,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum PublishState {
    /// Local draft, never sent to GitHub.
    Draft,
    Published,
    /// Came from GitHub.
    Imported,
}

impl PublishState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Published => "published",
            Self::Imported => "imported",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "published" => Self::Published,
            "imported" => Self::Imported,
            _ => Self::Draft,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Comment {
    pub id: String,
    pub thread_id: String,
    pub author_kind: Actor,
    pub author: String,
    pub body: String,
    pub created_at: String,
    pub updated_at: String,
    pub publish_state: PublishState,
    pub github_comment_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ReviewThread {
    pub id: String,
    pub source: ThreadSource,
    pub github_thread_id: Option<String>,
    pub path: String,
    /// Current file ID when the path is still in the PR.
    pub file_id: Option<String>,
    pub side: Side,
    pub line: Option<u32>,
    pub start_line: Option<u32>,
    /// Text of the anchored line when the thread was created.
    pub line_text: Option<String>,
    pub anchor_state: AnchorState,
    pub anchor_reason: Option<String>,
    pub resolved: bool,
    pub head_sha: String,
    pub created_at: String,
    pub comments: Vec<Comment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[ts(export)]
pub enum Verdict {
    Approve,
    RequestChanges,
    Comment,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "APPROVE",
            Self::RequestChanges => "REQUEST_CHANGES",
            Self::Comment => "COMMENT",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "APPROVE" => Some(Self::Approve),
            "REQUEST_CHANGES" => Some(Self::RequestChanges),
            "COMMENT" => Some(Self::Comment),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ReviewState {
    /// `in-progress`, `finished` or `published`.
    pub lifecycle: String,
    pub verdict: Option<Verdict>,
    pub summary: String,
    pub finished_at: Option<String>,
    pub published_at: Option<String>,
}

/// Initial snapshot for the browser and agents: PR data plus derived review state.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SessionView {
    pub schema_version: u32,
    pub session_id: String,
    pub demo: bool,
    /// Sequence number of the latest event included in this view.
    #[ts(type = "number")]
    pub sequence: u64,
    /// Who the caller is authenticated as.
    pub actor: Actor,
    pub repository: Repository,
    pub pr: PullRequest,
    pub files: Vec<ChangedFile>,
    pub file_states: Vec<FileState>,
    pub progress: Progress,
    pub rules: Vec<ExclusionRule>,
    pub threads: Vec<ReviewThread>,
    pub reviews: Vec<ImportedReview>,
    pub issue_comments: Vec<IssueComment>,
    pub review: ReviewState,
    pub warnings: Vec<String>,
    pub complete: bool,
    pub fetched_at: String,
}

/// Envelope for every SSE and stdout event.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EventEnvelope {
    pub schema_version: u32,
    pub session_id: String,
    #[ts(type = "number")]
    pub sequence: u64,
    pub event_type: String,
    pub timestamp: String,
    #[ts(type = "unknown")]
    pub payload: serde_json::Value,
}
