//! Typed domain model shared by the API, persistence and the frontend.
//!
//! Every type here derives `ts_rs::TS`; `cargo test` writes matching
//! TypeScript declarations into `app/src/generated/`.

mod review;
mod rules;
mod snapshot;

pub use review::{
    Acknowledgement, Actor, AnchorState, Comment, EventEnvelope, FileScope, FileState, FileStatus,
    Progress, PublishState, ReviewState, ReviewThread, SessionView, ThreadSource, Verdict,
};
pub use rules::{ExclusionRule, Repository, RuleAction, RuleKind};
pub use snapshot::{
    ChangeKind, ChangedFile, DiffHunk, DiffLine, GithubComment, GithubThread, ImportedReview,
    IssueComment, LineKind, PrSnapshot, PullRequest, Side,
};

/// Version of the JSON contract served to the browser and agents.
pub const SCHEMA_VERSION: u32 = 1;
