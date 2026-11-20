//! Bundled fixture data for `aside demo`. Never touches GitHub.
use crate::model::{ChangeKind, ChangedFile, PullRequest};

pub fn pull_request() -> PullRequest {
    PullRequest {
        owner: "acme".into(),
        repo: "widget".into(),
        number: 42,
        title: "Add widget export pipeline".into(),
        author: "octocat".into(),
        url: "https://github.com/acme/widget/pull/42".into(),
        base_ref: "main".into(),
        head_ref: "feature/export".into(),
        head_sha: "0123456789abcdef0123456789abcdef01234567".into(),
    }
}

pub fn files() -> Vec<ChangedFile> {
    let f = |id: &str, path: &str, kind, add, del| ChangedFile {
        id: id.into(),
        path: path.into(),
        previous_path: None,
        change_kind: kind,
        additions: add,
        deletions: del,
    };
    vec![
        f("f1", "src/export/mod.rs", ChangeKind::Added, 120, 0),
        f("f2", "src/api.rs", ChangeKind::Modified, 14, 3),
        f(
            "f3",
            "src/generated/types.rs",
            ChangeKind::Modified,
            410,
            380,
        ),
        f("f4", "src/generated/manual.rs", ChangeKind::Modified, 8, 2),
        f("f5", "tests/output.snap", ChangeKind::Modified, 60, 58),
        f("f6", "web/src/Export.svelte", ChangeKind::Added, 75, 0),
        f("f7", "Cargo.lock", ChangeKind::Modified, 42, 11),
        f("f8", "docs/old-export.md", ChangeKind::Removed, 0, 31),
    ]
}
