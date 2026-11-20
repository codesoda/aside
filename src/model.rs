//! Typed snapshot served to the frontend. Mirror of `web/src/api.ts`.
use serde::Serialize;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    Added,
    Modified,
    Removed,
    Renamed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedFile {
    pub id: String,
    pub path: String,
    pub previous_path: Option<String>,
    pub change_kind: ChangeKind,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequest {
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub author: String,
    pub url: String,
    pub base_ref: String,
    pub head_ref: String,
    pub head_sha: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub schema_version: u32,
    pub session_id: String,
    pub demo: bool,
    pub pr: PullRequest,
    pub files: Vec<ChangedFile>,
}
