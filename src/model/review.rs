use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::rules::{ExclusionRule, Repository};
use super::snapshot::{ChangedFile, ImportedReview, IssueComment, PullRequest, Side};

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
    /// Per-source completeness and analysis status of the canonical snapshot.
    pub analysis: super::AnalysisSummary,
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
