//! GitHub access through the user's authenticated `gh` CLI.
//!
//! `gh` is always run with an argv array, never through a shell. All calls go
//! through [`GhRunner`] so tests can substitute a fake.

mod git_fallback;
mod review;
mod runner;
mod snapshot;
mod threads;
#[cfg(test)]
pub use threads::DeniedVia;
pub use threads::{LegacyKind, ThreadImportProblem};

pub use review::{
    NewReviewComment, create_review, fetch_revision, reply_to_comment, review_comments,
};

#[allow(unused_imports)]
pub use review::ReviewCommentRecord;
pub use runner::{GhRunner, PrRef, SystemGh, check_gh, parse_pr_url};
pub use snapshot::fetch_snapshot;

#[cfg(test)]
pub mod fake {
    //! Scripted `gh` for tests. Never touches the network.
    use super::GhRunner;
    use anyhow::{Result, anyhow};
    use std::sync::Mutex;

    type ScriptedResponse = (Vec<String>, Result<Vec<u8>, String>);

    #[derive(Default)]
    pub struct FakeGh {
        /// (argv prefix, response). First matching prefix wins.
        pub responses: Mutex<Vec<ScriptedResponse>>,
        pub calls: Mutex<Vec<(Vec<String>, Option<String>)>>,
    }

    impl FakeGh {
        pub fn on(&self, prefix: &[&str], response: Result<&str, &str>) {
            self.responses.lock().unwrap().push((
                prefix.iter().map(|s| s.to_string()).collect(),
                response
                    .map(|s| s.as_bytes().to_vec())
                    .map_err(str::to_string),
            ));
        }

        pub fn calls_matching(&self, prefix: &[&str]) -> Vec<(Vec<String>, Option<String>)> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(a, _)| {
                    a.starts_with(&prefix.iter().map(|s| s.to_string()).collect::<Vec<_>>())
                })
                .cloned()
                .collect()
        }
    }

    impl GhRunner for FakeGh {
        fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>> {
            let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
            self.calls.lock().unwrap().push((
                args.clone(),
                stdin.map(|s| String::from_utf8_lossy(s).into_owned()),
            ));
            let responses = self.responses.lock().unwrap();
            let found = responses
                .iter()
                .find(|(prefix, _)| args.starts_with(prefix));
            match found {
                Some((_, Ok(out))) => Ok(out.clone()),
                Some((_, Err(e))) => Err(anyhow!("{e}")),
                None => Err(anyhow!("fake gh: no response for {args:?}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeGh;
    use super::runner::classify_error;
    use super::snapshot::{
        GhApiFile, PR_FIELDS, apply_recovery, needs_recovery, validated_aggregate,
    };
    use super::*;
    use crate::diff::Limits;
    use anyhow::Result;
    use serde_json::{Value, json};

    #[test]
    fn parses_pr_url() {
        let pr = parse_pr_url("https://github.com/acme/widget/pull/42/files").unwrap();
        assert_eq!(
            pr,
            PrRef {
                owner: "acme".into(),
                repo: "widget".into(),
                number: 42
            }
        );
    }

    #[test]
    fn rejects_other_hosts_and_paths() {
        assert!(parse_pr_url("https://gitlab.com/a/b/pull/1").is_err());
        assert!(parse_pr_url("https://github.com/a/b/issues/1").is_err());
        assert!(parse_pr_url("https://github.com/a/b/pull/x").is_err());
        assert!(parse_pr_url("https://github.com/a;rm/b/pull/1").is_err());
    }

    #[test]
    fn classifies_errors_without_echoing_secrets() {
        assert!(
            classify_error("To get started with GitHub CLI, please run:  gh auth login")
                .to_string()
                .contains("authentication")
        );
        assert!(
            classify_error("API rate limit exceeded")
                .to_string()
                .contains("rate limit")
        );
        assert!(
            classify_error("GraphQL: Could not resolve to a PullRequest")
                .to_string()
                .contains("not found")
        );
    }

    fn pr_json() -> String {
        json!({
            "number": 7, "title": "T", "body": "B", "url": "https://github.com/o/r/pull/7", "state": "OPEN",
            "isDraft": false, "author": {"login": "alice"}, "baseRefName": "main", "baseRefOid": "base1",
            "headRefName": "feat", "headRefOid": "head1", "changedFiles": 1,
            "reviews": [{"author": {"login": "bob"}, "state": "COMMENTED", "body": "hi", "submittedAt": "2026-01-01T00:00:00Z"}],
            "comments": [{"author": {"login": "carol"}, "body": "c", "createdAt": "2026-01-01T00:00:00Z"}]
        })
        .to_string()
    }

    fn threads_json() -> String {
        json!({"data": {"repository": {"pullRequest": {"reviewThreads": {
            "pageInfo": {"hasNextPage": false, "endCursor": null},
            "nodes": [{"id": "T1", "isResolved": false, "isOutdated": false, "path": "a.rs", "line": 1,
                "originalLine": 1, "startLine": null, "diffSide": "RIGHT",
                "comments": {"pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": [{"id": "C1", "databaseId": 11, "body": "why?", "createdAt": "2026-01-01T00:00:00Z", "author": {"login": "bob"}}]}}]
        }}}}})
        .to_string()
    }

    fn snapshot_gh(threads: Result<&str, &str>) -> FakeGh {
        let gh = FakeGh::default();
        gh.on(&["pr", "view"], Ok(&pr_json()));
        gh.on(&["repo", "view"], Ok(r#"{"id":"R_1"}"#));
        gh.on(
            &["pr", "diff"],
            Ok("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n"),
        );
        gh.on(&["api", "--paginate"], Ok(r#"[[{"filename":"a.rs","status":"modified","patch":null,"additions":1,"deletions":1}]]"#));
        gh.on(&["api", "graphql"], threads);
        gh
    }

    fn test_pr_ref() -> PrRef {
        PrRef {
            owner: "o".into(),
            repo: "r".into(),
            number: 7,
        }
    }

    #[tokio::test]
    async fn graphql_error_during_refresh_preserves_imported_threads() {
        use crate::{session::Session, store::Store};
        use std::sync::Arc;

        let pr = test_pr_ref();
        let initial =
            fetch_snapshot(&snapshot_gh(Ok(&threads_json())), &pr, &Limits::default()).unwrap();
        assert_eq!(initial.github_threads.len(), 1);
        let failing = Arc::new(snapshot_gh(Ok(
            r#"{"errors":[{"message":"Resource not accessible by integration"}],"data":{"repository":null}}"#,
        )));
        let session = Session::new(
            initial,
            Store::open_in_memory().unwrap(),
            Some(failing),
            Some(pr),
        )
        .unwrap();
        let before = session
            .store
            .lock()
            .unwrap()
            .list_threads(&session.pr_id)
            .unwrap();
        assert_eq!(before.len(), 1);
        let refreshed = session.refresh().await.unwrap();
        let snapshot = session.snapshot.read().unwrap();
        assert_eq!(
            snapshot.github_threads_completeness,
            crate::model::Completeness::Failed
        );
        assert!(snapshot.github_threads.is_empty());
        assert!(
            snapshot
                .github_threads_problems
                .iter()
                .any(|p| p.contains("Resource not accessible"))
        );
        assert!(
            refreshed
                .warnings
                .iter()
                .any(|w| w.contains("review threads incomplete"))
        );
        assert_eq!(refreshed.threads.len(), 1);
        assert_eq!(refreshed.threads[0].id, before[0].id);
        drop(snapshot);
        let after = session
            .store
            .lock()
            .unwrap()
            .list_threads(&session.pr_id)
            .unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].id, before[0].id);
    }

    #[test]
    fn graphql_command_failure_marks_threads_failed() {
        let snapshot = fetch_snapshot(
            &snapshot_gh(Err("GraphQL: forbidden")),
            &test_pr_ref(),
            &Limits::default(),
        )
        .unwrap();
        assert!(snapshot.complete);
        assert_eq!(snapshot.files.len(), 1);
        assert!(snapshot.github_threads.is_empty());
        assert_eq!(
            snapshot.github_threads_completeness,
            crate::model::Completeness::Failed
        );
        assert!(
            snapshot
                .github_threads_problems
                .iter()
                .any(|p| p.contains("forbidden"))
        );
        assert!(
            snapshot
                .warnings
                .iter()
                .any(|w| w.contains("review threads incomplete"))
        );
    }

    #[test]
    fn fetches_snapshot_through_gh() {
        let gh = FakeGh::default();
        gh.on(&["pr", "view"], Ok(&pr_json()));
        gh.on(&["repo", "view"], Ok(r#"{"id":"R_1"}"#));
        gh.on(
            &["pr", "diff"],
            Ok("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n"),
        );
        gh.on(&["api", "--paginate"], Ok(r#"[[{"filename":"a.rs","status":"modified","patch":null,"additions":1,"deletions":1}]]"#));
        gh.on(&["api", "graphql"], Ok(&threads_json()));
        let pr = PrRef {
            owner: "o".into(),
            repo: "r".into(),
            number: 7,
        };
        let snap = fetch_snapshot(&gh, &pr, &Limits::default()).unwrap();
        assert_eq!(snap.pr.head_sha, "head1");
        assert_eq!(snap.provider_repository_id.as_deref(), Some("R_1"));
        assert_eq!(snap.files.len(), 1);
        assert!(snap.complete);
        assert_eq!(snap.github_threads[0].comments[0].database_id, Some(11));
        assert_eq!(snap.reviews[0].author, "bob");
        assert!(snap.warnings.is_empty());
        assert_eq!(
            snap.github_threads_completeness,
            crate::model::Completeness::Complete
        );
    }

    // A force push can happen after the metadata request, while the diff and
    // thread requests still return successful, same-sized responses.
    struct MovingPrGh {
        fake: FakeGh,
        views: std::sync::atomic::AtomicUsize,
        changed: String,
    }

    impl GhRunner for MovingPrGh {
        fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>> {
            if args.starts_with(&["pr", "view"])
                && self.views.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0
            {
                return Ok(self.changed.as_bytes().to_vec());
            }
            self.fake.run(args, stdin)
        }
    }

    #[test]
    fn rejects_force_push_between_metadata_and_diff_even_with_same_file_count() {
        let fake = FakeGh::default();
        fake.on(&["api", "--paginate"], Ok(r#"[[{"filename":"a.rs","status":"modified","patch":null,"additions":1,"deletions":1}]]"#));
        fake.on(&["pr", "view"], Ok(&pr_json()));
        fake.on(&["repo", "view"], Ok(r#"{"id":"R_1"}"#));
        fake.on(
            &["pr", "diff"],
            Ok("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n"),
        );
        fake.on(&["api", "graphql"], Ok(&threads_json()));
        let mut changed: Value = serde_json::from_str(&pr_json()).unwrap();
        changed["headRefOid"] = json!("head2");
        let gh = MovingPrGh {
            fake,
            views: std::sync::atomic::AtomicUsize::new(0),
            changed: changed.to_string(),
        };
        let pr = PrRef {
            owner: "o".into(),
            repo: "r".into(),
            number: 7,
        };
        assert!(fetch_snapshot(&gh, &pr, &Limits::default()).is_err());
    }

    #[test]
    fn rejects_base_change_during_import() {
        let fake = FakeGh::default();
        fake.on(&["api", "--paginate"], Ok(r#"[[{"filename":"a.rs","status":"modified","patch":null,"additions":1,"deletions":1}]]"#));
        fake.on(&["pr", "view"], Ok(&pr_json()));
        fake.on(&["repo", "view"], Ok(r#"{"id":"R_1"}"#));
        fake.on(
            &["pr", "diff"],
            Ok("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n"),
        );
        fake.on(&["api", "graphql"], Ok(&threads_json()));
        let mut changed: Value = serde_json::from_str(&pr_json()).unwrap();
        changed["baseRefOid"] = json!("base2");
        let gh = MovingPrGh {
            fake,
            views: std::sync::atomic::AtomicUsize::new(0),
            changed: changed.to_string(),
        };
        let pr = PrRef {
            owner: "o".into(),
            repo: "r".into(),
            number: 7,
        };
        assert!(
            fetch_snapshot(&gh, &pr, &Limits::default())
                .unwrap_err()
                .to_string()
                .contains("base or head changed")
        );
    }

    #[test]
    fn rejects_force_push_during_paginated_files_fallback() {
        let fake = FakeGh::default();
        fake.on(&["pr", "view"], Ok(&pr_json()));
        fake.on(&["repo", "view"], Ok(r#"{"id":"R_1"}"#));
        fake.on(&["pr", "diff"], Err("diff unavailable"));
        fake.on(&["api", "--paginate"], Ok(r#"[[{"filename":"a.rs","status":"modified","patch":"@@ -1 +1 @@\n-x\n+y","sha":"s1","additions":1,"deletions":1}]]"#));
        fake.on(&["api", "graphql"], Ok(&threads_json()));
        let mut changed: Value = serde_json::from_str(&pr_json()).unwrap();
        changed["headRefOid"] = json!("head2");
        let gh = MovingPrGh {
            fake,
            views: std::sync::atomic::AtomicUsize::new(0),
            changed: changed.to_string(),
        };
        let pr = PrRef {
            owner: "o".into(),
            repo: "r".into(),
            number: 7,
        };
        assert!(
            fetch_snapshot(&gh, &pr, &Limits::default())
                .unwrap_err()
                .to_string()
                .contains("base or head changed")
        );
        assert_eq!(gh.fake.calls_matching(&["api", "--paginate"]).len(), 1);
    }

    #[test]
    fn fails_closed_if_revision_cannot_be_rechecked() {
        let fake = FakeGh::default();
        fake.on(&["api", "--paginate"], Ok(r#"[[{"filename":"a.rs","status":"modified","patch":null,"additions":1,"deletions":1}]]"#));
        fake.on(
            &[
                "pr",
                "view",
                "https://github.com/o/r/pull/7",
                "--json",
                PR_FIELDS,
            ],
            Ok(&pr_json()),
        );
        fake.on(&["repo", "view"], Ok(r#"{"id":"R_1"}"#));
        fake.on(
            &["pr", "diff"],
            Ok("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n"),
        );
        fake.on(&["api", "graphql"], Ok(&threads_json()));
        let pr = PrRef {
            owner: "o".into(),
            repo: "r".into(),
            number: 7,
        };
        assert!(
            fetch_snapshot(&fake, &pr, &Limits::default())
                .unwrap_err()
                .to_string()
                .contains("could not verify PR revision")
        );
    }

    fn snapshot_for_diff(
        diff: &str,
        manifest: Result<&str, &str>,
    ) -> Result<crate::model::PrSnapshot> {
        let gh = FakeGh::default();
        gh.on(&["pr", "view"], Ok(&pr_json()));
        gh.on(&["repo", "view"], Ok(r#"{"id":"R_1"}"#));
        gh.on(&["pr", "diff"], Ok(diff));
        gh.on(&["api", "--paginate"], manifest);
        gh.on(&["api", "graphql"], Ok(&threads_json()));
        fetch_snapshot(
            &gh,
            &PrRef {
                owner: "o".into(),
                repo: "r".into(),
                number: 7,
            },
            &Limits::default(),
        )
    }

    #[test]
    fn validated_diff_survives_null_api_patch() {
        let snap = snapshot_for_diff("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n", Ok(r#"[[{"filename":"a.rs","status":"modified","patch":null,"additions":1,"deletions":1}]]"#)).unwrap();
        assert!(snap.complete);
        assert_eq!(snap.files[0].hunks.len(), 1);
    }

    #[test]
    fn incomplete_hunk_uses_api_patch() {
        let snap = snapshot_for_diff("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n-x\n+y\n", Ok(r#"[[{"filename":"a.rs","status":"modified","patch":"@@ -1 +1 @@\n-x\n+y","additions":1,"deletions":1}]]"#)).unwrap();
        assert_eq!(snap.files[0].hunks[0].old_count, 1);
        assert!(snap.warnings.iter().any(|w| w.contains("did not match")));
    }

    #[test]
    fn equal_count_wrong_path_uses_api_patch() {
        let snap = snapshot_for_diff("diff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-x\n+y\n", Ok(r#"[[{"filename":"a.rs","status":"modified","patch":"@@ -1 +1 @@\n-x\n+y","additions":1,"deletions":1}]]"#)).unwrap();
        assert_eq!(snap.files[0].path, "a.rs");
        assert!(snap.warnings.iter().any(|w| w.contains("did not match")));
    }

    #[test]
    fn duplicate_aggregate_path_cannot_hide_missing_manifest_file() {
        let snap = snapshot_for_diff(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\ndiff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n",
            Ok(r#"[[{"filename":"a.rs","status":"modified","patch":"@@ -1 +1 @@\n-x\n+y","additions":1,"deletions":1},{"filename":"b.rs","status":"modified","patch":"@@ -1 +1 @@\n-x\n+y","additions":1,"deletions":1}]]"#),
        )
        .unwrap();
        assert_eq!(
            snap.files
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            ["a.rs", "b.rs"]
        );
        assert!(snap.warnings.iter().any(|w| w.contains("did not match")));
    }

    #[test]
    fn aggregate_rename_must_match_previous_filename() {
        let snap = snapshot_for_diff(
            "diff --git a/wrong.rs b/new.rs\nsimilarity index 50%\nrename from wrong.rs\nrename to new.rs\n--- a/wrong.rs\n+++ b/new.rs\n@@ -1 +1 @@\n-x\n+y\n",
            Ok(r#"[[{"filename":"new.rs","previous_filename":"old.rs","status":"renamed","patch":"@@ -1 +1 @@\n-x\n+y","additions":1,"deletions":1}]]"#),
        )
        .unwrap();
        assert_eq!(snap.files[0].previous_path.as_deref(), Some("old.rs"));
        assert!(snap.warnings.iter().any(|w| w.contains("did not match")));
    }

    #[test]
    fn aggregate_change_kind_must_match_manifest_status() {
        let snap = snapshot_for_diff(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n",
            Ok(r#"[[{"filename":"a.rs","status":"added","patch":"@@ -0,0 +1 @@\n+y","additions":1,"deletions":1}]]"#),
        )
        .unwrap();
        assert_eq!(snap.files[0].change_kind, crate::model::ChangeKind::Added);
        assert!(snap.warnings.iter().any(|w| w.contains("did not match")));
    }

    #[test]
    fn api_omission_prevents_complete_snapshot() {
        let snap = snapshot_for_diff("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n", Ok(r#"[[{"filename":"a.rs","status":"modified","patch":null,"additions":2,"deletions":1}]]"#)).unwrap();
        assert!(!snap.complete);
        assert!(snap.warnings.iter().any(|w| w.contains("did not match")));
    }

    #[test]
    fn api_omitted_file_cannot_validate_aggregate() {
        let snap = snapshot_for_diff(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n",
            Ok("[[]]"),
        )
        .unwrap();
        assert!(snap.files.is_empty());
        assert!(!snap.complete);
        assert!(snap.warnings.iter().any(|w| w.contains("did not match")));
    }

    #[test]
    fn missing_manifest_entry_requests_recovery_even_with_complete_api_patch() {
        let snap = snapshot_for_diff(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n",
            Ok(r#"[[{"filename":"a.rs","status":"modified","patch":"@@ -1 +1 @@\n-x\n+y","additions":1,"deletions":1}]]"#),
        )
        .unwrap();
        assert!(snap.files[0].patch_complete);
        assert!(needs_recovery(&snap.files, 1, Some(2)));
        assert!(!needs_recovery(&snap.files, 1, Some(1)));
    }

    #[test]
    fn recovered_extras_require_expected_count_and_exact_manifest_entries() {
        let manifest: Vec<GhApiFile> = serde_json::from_str(
            r#"[{"filename":"a.rs","status":"modified","additions":1,"deletions":1}]"#,
        )
        .unwrap();
        let diff = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let files = crate::diff::parse_unified(diff, "head1", &Limits::default()).files;
        assert!(validated_aggregate(&files, &manifest, Some(2), true));
        assert!(!validated_aggregate(&files, &manifest, Some(2), false));
        assert!(!validated_aggregate(&files, &manifest, Some(3), true));
        assert!(!validated_aggregate(&files, &manifest, None, true));
        let wrong_manifest: Vec<GhApiFile> = serde_json::from_str(
            r#"[{"filename":"a.rs","status":"added","additions":1,"deletions":1}]"#,
        )
        .unwrap();
        assert!(!validated_aggregate(&files, &wrong_manifest, Some(2), true));
        assert!(!validated_aggregate(&files[1..], &manifest, Some(1), true));
        assert!(!validated_aggregate(
            &[files[0].clone(), files[0].clone()],
            &manifest,
            Some(2),
            true
        ));
    }

    #[test]
    fn recovery_accepted_when_truncation_zeroes_manifest_counts() {
        // Aggregate truncation: GitHub drops the patch AND zeroes the counts.
        let manifest: Vec<GhApiFile> = serde_json::from_str(
            r#"[{"filename":"a.rs","status":"modified","additions":0,"deletions":0}]"#,
        )
        .unwrap();
        let diff = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n";
        let files = crate::diff::parse_unified(diff, "head1", &Limits::default()).files;
        assert_eq!((files[0].additions, files[0].deletions), (1, 1));
        assert!(validated_aggregate(&files, &manifest, Some(1), true));
        assert!(validated_aggregate(&files, &manifest, None, true));
    }

    #[test]
    fn recovery_accepted_with_real_counts_and_omitted_patch() {
        // Single oversized file: GitHub drops only the patch; counts stay real.
        let manifest: Vec<GhApiFile> = serde_json::from_str(
            r#"[{"filename":"a.rs","status":"modified","additions":5,"deletions":2}]"#,
        )
        .unwrap();
        let diff = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,5 @@\n-x\n-y\n+z1\n+z2\n+z3\n+z4\n+z5\n";
        let files = crate::diff::parse_unified(diff, "head1", &Limits::default()).files;
        assert_eq!((files[0].additions, files[0].deletions), (5, 2));
        assert!(validated_aggregate(&files, &manifest, Some(1), true));
    }

    #[test]
    fn recovery_rejected_on_count_mismatch_for_patch_backed_entry() {
        // A patch-backed entry keeps strict counts; recovery must not bypass it.
        let manifest: Vec<GhApiFile> = serde_json::from_str(
            r#"[{"filename":"a.rs","status":"modified","patch":"@@ -1 +1 @@\n-x\n+y","additions":3,"deletions":1}]"#,
        )
        .unwrap();
        let diff = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n";
        let files = crate::diff::parse_unified(diff, "head1", &Limits::default()).files;
        assert_eq!((files[0].additions, files[0].deletions), (1, 1));
        assert!(!validated_aggregate(&files, &manifest, Some(1), true));
    }

    #[test]
    fn recovery_rejected_on_previous_filename_mismatch() {
        let manifest: Vec<GhApiFile> = serde_json::from_str(
            r#"[{"filename":"new.rs","previous_filename":"old.rs","status":"renamed","additions":0,"deletions":0}]"#,
        )
        .unwrap();
        let diff = "diff --git a/other.rs b/new.rs\nsimilarity index 100%\nrename from other.rs\nrename to new.rs\n";
        let files = crate::diff::parse_unified(diff, "head1", &Limits::default()).files;
        assert!(!validated_aggregate(&files, &manifest, Some(1), true));
    }

    fn truncated_api_file() -> crate::model::ChangedFile {
        crate::diff::file_from_api(
            &crate::diff::ApiFile {
                path: "a.rs",
                previous_path: None,
                status: "modified",
                patch: None,
                blob_sha: None,
                additions: 0,
                deletions: 0,
            },
            "head1",
            &Limits::default(),
            &mut Vec::new(),
        )
    }

    #[test]
    fn apply_recovery_accepts_valid_recovered_diff_for_truncated_manifest() {
        let manifest: Vec<GhApiFile> = serde_json::from_str(
            r#"[{"filename":"a.rs","status":"modified","additions":0,"deletions":0}]"#,
        )
        .unwrap();
        let api_files = vec![truncated_api_file()];
        assert!(!api_files[0].patch_complete);
        let recovered = crate::diff::parse_unified(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n",
            "head1",
            &Limits::default(),
        );
        let mut warnings = Vec::new();
        let files = apply_recovery(
            &api_files,
            &manifest,
            Some(1),
            Some(recovered),
            &mut warnings,
        )
        .expect("valid recovery must supersede the API fallback");
        assert!(files.iter().all(|f| f.patch_complete));
        assert_eq!((files[0].additions, files[0].deletions), (1, 1));
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("recovered complete diff"))
        );
    }

    #[test]
    fn apply_recovery_rejects_invalid_recovered_diff() {
        let manifest: Vec<GhApiFile> = serde_json::from_str(
            r#"[{"filename":"a.rs","status":"modified","patch":"@@ -1 +1 @@\n-x\n+y","additions":3,"deletions":1}]"#,
        )
        .unwrap();
        let api_files = vec![truncated_api_file()];
        let recovered = crate::diff::parse_unified(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n",
            "head1",
            &Limits::default(),
        );
        let mut warnings = Vec::new();
        let result = apply_recovery(
            &api_files,
            &manifest,
            Some(1),
            Some(recovered),
            &mut warnings,
        );
        assert!(result.is_none());
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("did not match PR files metadata; patch remains incomplete"))
        );
    }

    #[test]
    fn apply_recovery_without_a_recovery_keeps_incomplete_api_files() {
        let manifest: Vec<GhApiFile> = serde_json::from_str(
            r#"[{"filename":"a.rs","status":"modified","additions":0,"deletions":0}]"#,
        )
        .unwrap();
        let api_files = vec![truncated_api_file()];
        let mut warnings = Vec::new();
        let result = apply_recovery(&api_files, &manifest, Some(1), None, &mut warnings);
        assert!(result.is_none());
        assert!(warnings.is_empty());
    }

    #[test]
    fn apply_recovery_skipped_when_api_patch_is_complete() {
        let manifest: Vec<GhApiFile> = serde_json::from_str(
            r#"[{"filename":"a.rs","status":"modified","patch":"@@ -1 +1 @@\n-x\n+y","additions":1,"deletions":1}]"#,
        )
        .unwrap();
        let api_files = vec![crate::diff::file_from_api(
            &crate::diff::ApiFile {
                path: "a.rs",
                previous_path: None,
                status: "modified",
                patch: Some("@@ -1 +1 @@\n-x\n+y"),
                blob_sha: None,
                additions: 1,
                deletions: 1,
            },
            "head1",
            &Limits::default(),
            &mut Vec::new(),
        )];
        assert!(api_files[0].patch_complete);
        let recovered = crate::diff::parse_unified(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+z\n",
            "head1",
            &Limits::default(),
        );
        let mut warnings = Vec::new();
        let result = apply_recovery(
            &api_files,
            &manifest,
            Some(1),
            Some(recovered),
            &mut warnings,
        );
        assert!(result.is_none());
        assert!(warnings.is_empty());
    }

    #[test]
    fn manifest_failure_fails_closed() {
        let err = snapshot_for_diff(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n",
            Err("API unavailable"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("manifest"));
    }

    #[test]
    fn missing_diff_uses_api_patch() {
        let snap = snapshot_for_diff("", Ok(r#"[[{"filename":"a.rs","status":"modified","patch":"@@ -1 +1 @@\n-x\n+y","additions":1,"deletions":1}]]"#)).unwrap();
        assert_eq!(snap.files[0].hunks.len(), 1);
        assert!(snap.warnings.iter().any(|w| w.contains("did not match")));
    }

    #[test]
    fn falls_back_to_files_api_and_flags_missing_files() {
        let gh = FakeGh::default();
        let mut pr = serde_json::from_str::<Value>(&pr_json()).unwrap();
        pr["changedFiles"] = json!(2);
        gh.on(&["pr", "view"], Ok(&pr.to_string()));
        gh.on(&["repo", "view"], Ok(r#"{"id":"R_1"}"#));
        gh.on(&["pr", "diff"], Err("HTTP 406: diff too large"));
        gh.on(
            &["api", "--paginate"],
            Ok(r#"[[{"filename":"a.rs","status":"modified","patch":"@@ -1 +1 @@\n-x\n+y","sha":"s1","additions":1,"deletions":1}]]"#),
        );
        gh.on(&["api", "graphql"], Ok(&threads_json()));
        let pr = PrRef {
            owner: "o".into(),
            repo: "r".into(),
            number: 7,
        };
        let snap = fetch_snapshot(&gh, &pr, &Limits::default()).unwrap();
        assert_eq!(snap.files.len(), 1);
        assert!(
            !snap.complete,
            "1 of 2 files must mark the snapshot incomplete"
        );
        assert!(snap.warnings.iter().any(|w| w.contains("changed files")));
        assert_eq!(snap.github_threads.len(), 1);
        assert_eq!(snap.github_threads[0].node_id, "T1");
        assert_eq!(
            snap.github_threads_completeness,
            crate::model::Completeness::Complete
        );
        assert!(!snap.warnings.iter().any(|w| w.contains("review threads")));
    }
}
