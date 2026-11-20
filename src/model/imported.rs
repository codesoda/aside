//! Native imported-source contracts (analyze.md §§9, 10.4, 10.5).
//!
//! Imported GitHub records keep their own native state. A finding may *point
//! at* a native record through [`NativeSourceRef`] but never replaces it, and
//! a local disposition never changes the native state (§10.1 rule 3).
//!
//! Review threads, inline comments, review summaries and issue comments are
//! already modelled in `snapshot.rs` (`GithubThread`, `ImportedReview`,
//! `IssueComment`) and `review.rs` (`ReviewThread`); they are referenced here,
//! not duplicated. TODO(#18): extend those with the remaining §10.4 fields
//! (resolved_by, viewer permissions, source revision) when the importer
//! persists them. The types below cover native feedback with no existing model:
//! check runs, check annotations and commit statuses.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::finding::SourceKind;

/// Identity of a native record.
///
/// `upstream` is an ID GitHub gave us. `content_fingerprint` is a weaker key
/// Aside computed (for example for check annotations without an upstream ID)
/// and must never be presented as a GitHub ID (§10.4).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
#[ts(export)]
pub enum NativeSourceKey {
    Upstream(String),
    ContentFingerprint(String),
}

impl NativeSourceKey {
    pub fn parts(&self) -> (&'static str, &str) {
        match self {
            Self::Upstream(v) => ("upstream", v),
            Self::ContentFingerprint(v) => ("content_fingerprint", v),
        }
    }

    pub fn is_upstream(&self) -> bool {
        matches!(self, Self::Upstream(_))
    }
}

/// Pointer from a finding to the native record it projects.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct NativeSourceRef {
    pub host: String,
    /// Provider's immutable repository ID, or `owner/repo` when unknown.
    pub repository_id: String,
    pub source_kind: SourceKind,
    pub key: NativeSourceKey,
    pub external_url: Option<String>,
}

/// Annotation level as GitHub reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AnnotationLevel {
    Notice,
    Warning,
    Failure,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImportedCheckRun {
    #[ts(type = "number")]
    pub github_check_run_id: u64,
    pub app_slug: Option<String>,
    pub name: String,
    /// GitHub's raw `status` (`queued`, `in_progress`, `completed`, ...).
    pub status: String,
    /// GitHub's raw `conclusion`; absent until completed.
    pub conclusion: Option<String>,
    pub head_sha: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub details_url: Option<String>,
    pub summary: Option<String>,
    pub annotations_count: u32,
    pub observed_at: String,
}

/// File-scoped CI information. Not a replyable thread (§19.5 scenario 7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImportedCheckAnnotation {
    #[ts(type = "number")]
    pub check_run_id: u64,
    /// Upstream key when available, else a content fingerprint.
    pub key: NativeSourceKey,
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub start_column: Option<u32>,
    pub end_column: Option<u32>,
    pub annotation_level: AnnotationLevel,
    pub title: Option<String>,
    pub message: String,
    pub raw_details: Option<String>,
    pub head_sha: String,
    pub observed_at: String,
}

impl ImportedCheckAnnotation {
    /// Weaker identity for annotations without an upstream ID: stable digest
    /// over check run, path, range, message and occurrence ordinal.
    pub fn content_fingerprint(
        check_run_id: u64,
        path: &str,
        start_line: u32,
        end_line: u32,
        message: &str,
        ordinal: u32,
    ) -> NativeSourceKey {
        let fields = [
            check_run_id.to_string(),
            path.to_string(),
            start_line.to_string(),
            end_line.to_string(),
            message.to_string(),
            ordinal.to_string(),
        ];
        let refs: Vec<Option<&str>> = fields.iter().map(|s| Some(s.as_str())).collect();
        NativeSourceKey::ContentFingerprint(super::analysis::stable_digest(
            "aside-check-annotation/v1",
            &refs,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImportedCommitStatus {
    #[ts(type = "number")]
    pub github_status_id: u64,
    pub context: String,
    /// GitHub's raw state (`error`, `failure`, `pending`, `success`).
    pub state: String,
    pub description: Option<String>,
    pub target_url: Option<String>,
    pub sha: String,
    pub updated_at: Option<String>,
    pub observed_at: String,
}
