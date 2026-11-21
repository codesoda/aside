//! Deterministic offline replay of cli/cli#14921. The first revision is an
//! unmodified public PR capture; the second is a synthetic refresh exercise.

use crate::{
    diff::{self, Limits},
    model::{PrSnapshot, PullRequest, SCHEMA_VERSION},
};

const PR_JSON: &str = include_str!("../fixtures/cli-cli-pr-14921.json");
const PR_DIFF: &str = include_str!("../fixtures/cli-cli-pr-14921.diff");
const REFRESH_HEAD: &str = "a123f7736e7aaabe56b4662de888cc2bff1338a7";
const ORIGINAL_LINE: &str =
    "+\t\t\t\tcmd.Env = append(cmd.Env, \"GH_IDENTITY_SSH_VARIANT=\"+value)";
const REFRESH_LINE: &str =
    "+\t\t\t\tcmd.Env = append(cmd.Env, \"GH_IDENTITY_SSH_VARIANT=\"+strings.TrimSpace(value))";

/// Return the captured PR or a synthetic revision with only `git/client.go`
/// changed. Both variants run through the same diff parser as live reviews.
pub fn snapshot(version: usize) -> PrSnapshot {
    let revision = version % 2;
    let mut pr: PullRequest = serde_json::from_str(PR_JSON).expect("bundled PR metadata is valid");
    let diff_text = if revision == 0 {
        PR_DIFF.to_owned()
    } else {
        pr.head_sha = REFRESH_HEAD.into();
        assert_eq!(
            PR_DIFF.matches(ORIGINAL_LINE).count(),
            1,
            "refresh target occurs once"
        );
        PR_DIFF.replacen(ORIGINAL_LINE, REFRESH_LINE, 1)
    };
    let parsed = diff::parse_unified(&diff_text, &pr.head_sha, &Limits::default());

    PrSnapshot {
        schema_version: SCHEMA_VERSION,
        pr,
        provider_repository_id: Some("212613049".into()),
        files: parsed.files,
        github_threads: Vec::new(),
        github_threads_complete: false,
        reviews: Vec::new(),
        issue_comments: Vec::new(),
        warnings: parsed.warnings,
        complete: true,
        fetched_at: "2026-11-20T11:40:19Z".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::LineKind, util::file_id};

    #[test]
    fn fixture_is_the_public_pr_with_all_changed_files() {
        let snap = snapshot(0);
        assert_eq!(
            (
                snap.pr.owner.as_str(),
                snap.pr.repo.as_str(),
                snap.pr.number
            ),
            ("cli", "cli", 14921)
        );
        assert_eq!(snap.pr.url, "https://github.com/cli/cli/pull/14921");
        assert_eq!(snap.pr.title, "Add repository and worktree identities");
        assert_eq!(snap.pr.author, "williammartin");
        assert_eq!(snap.pr.state, "OPEN");
        assert!(snap.pr.is_draft);
        assert_eq!(snap.pr.base_sha, "2cbd5515878542a5125ec412045d2b3e365659b6");
        assert_eq!(snap.pr.head_sha, "a123f7736e7aaabe56b4662de888cc2bff1338a6");
        assert!(
            snap.pr
                .body
                .contains("A one-command override still leaves account selection")
        );
        assert_eq!(snap.files.len(), 33);
        assert!(snap.complete);
        assert!(snap.warnings.is_empty(), "{:?}", snap.warnings);
        assert!(snap.files.iter().all(|file| file.patch_complete));
        assert!(
            snap.files
                .iter()
                .all(|file| file.id == file_id(&file.path) && !file.diff_digest.is_empty())
        );
        let paths: Vec<_> = snap.files.iter().map(|file| file.path.as_str()).collect();
        assert!(paths.contains(&"internal/identity/store.go"));
        assert!(paths.contains(&"pkg/cmd/auth/use/use.go"));
        assert!(paths.contains(&"script/multiaccount-review/02-repository.sh"));
        assert!(
            snap.files
                .iter()
                .any(|file| file.hunks.iter().flat_map(|h| &h.lines).any(|line| {
                    line.kind == LineKind::Addition
                        && line.text.contains("GH_IDENTITY_SSH_COMMAND=")
                }))
        );
    }

    #[test]
    fn refresh_changes_only_one_patch_and_head() {
        let old = snapshot(0);
        let new = snapshot(1);
        assert_ne!(old.pr.head_sha, new.pr.head_sha);
        assert_eq!(old.files.len(), new.files.len());
        let changed: Vec<_> = old
            .files
            .iter()
            .zip(&new.files)
            .filter(|(before, after)| before.diff_digest != after.diff_digest)
            .map(|(_, after)| after.path.as_str())
            .collect();
        assert_eq!(changed, ["git/client.go"]);
        assert!(
            new.files
                .iter()
                .find(|file| file.path == "git/client.go")
                .unwrap()
                .hunks
                .iter()
                .flat_map(|h| &h.lines)
                .any(|line| {
                    line.kind == LineKind::Addition
                        && line.text.contains("strings.TrimSpace(value)")
                })
        );
        assert_eq!(snapshot(2), old);
    }
}
