//! Publication plans and guarded GitHub submission.

use super::*;
use crate::{
    diff,
    github::{PrRef, fake::FakeGh},
    model::{
        Actor, AnchorState, Comment, PrSnapshot, PublishState, PullRequest, ReviewThread,
        SCHEMA_VERSION, Side, ThreadSource, Verdict,
    },
    store::{NewThread, Store},
};

fn snapshot() -> PrSnapshot {
    PrSnapshot {
        schema_version: SCHEMA_VERSION,
        pr: PullRequest {
            owner: "o".into(),
            repo: "r".into(),
            number: 1,
            title: "t".into(),
            body: String::new(),
            author: "a".into(),
            url: "u".into(),
            state: "OPEN".into(),
            is_draft: false,
            base_ref: "main".into(),
            base_sha: "base".into(),
            head_ref: "feature".into(),
            head_sha: "head".into(),
        },
        provider_repository_id: None,
        files: vec![],
        github_threads: vec![],
        reviews: vec![],
        issue_comments: vec![],
        warnings: vec![],
        complete: true,
        fetched_at: String::new(),
    }
}

#[test]
fn plan_id_binds_head_and_text() {
    let a = build_plan(
        &snapshot(),
        Verdict::Approve,
        "looks good",
        &[],
        &Selection::default(),
    )
    .unwrap();
    assert_eq!(
        a.plan_id,
        build_plan(
            &snapshot(),
            Verdict::Approve,
            "looks good",
            &[],
            &Selection::default()
        )
        .unwrap()
        .plan_id
    );
    let mut changed = snapshot();
    changed.pr.head_sha = "later".into();
    assert_ne!(
        a.plan_id,
        build_plan(
            &changed,
            Verdict::Approve,
            "looks good",
            &[],
            &Selection::default()
        )
        .unwrap()
        .plan_id
    );
    assert_ne!(
        a.plan_id,
        build_plan(
            &snapshot(),
            Verdict::Approve,
            "different",
            &[],
            &Selection::default()
        )
        .unwrap()
        .plan_id
    );
}

#[tokio::test]
async fn fake_publication_marks_each_comment_and_blocks_duplicate() {
    let mut snapshot = snapshot();
    snapshot.files = diff::parse_unified(
        "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        "head",
        &diff::Limits::default(),
    )
    .files;
    let fake = Arc::new(FakeGh::default());
    fake.on(&["pr", "view"], Ok(r#"{"headRefOid":"head"}"#));
    fake.on(
        &["api", "--method", "POST", "repos/o/r/pulls/1/reviews"],
        Ok(r#"{"id":7}"#),
    );
    fake.on(
        &[
            "api",
            "--paginate",
            "--slurp",
            "repos/o/r/pulls/1/reviews/7/comments?per_page=100",
        ],
        Ok(r#"[[{"id":70,"path":"a.rs","line":1,"body":"fix this"}]]"#),
    );
    fake.on(
        &[
            "api",
            "--method",
            "POST",
            "repos/o/r/pulls/1/comments/70/replies",
        ],
        Ok(r#"{"id":71}"#),
    );
    let session = Session::new(
        snapshot,
        Store::open_in_memory().unwrap(),
        Some(fake.clone()),
        Some(PrRef {
            owner: "o".into(),
            repo: "r".into(),
            number: 1,
        }),
    )
    .unwrap();
    {
        let mut store = session.store.lock().unwrap();
        store
            .insert_thread(
                &session.pr_id,
                &NewThread {
                    id: "t".into(),
                    source: ThreadSource::Local,
                    github_thread_id: None,
                    path: "a.rs".into(),
                    side: Side::Right,
                    line: Some(1),
                    start_line: None,
                    line_text: Some("new".into()),
                    diff_digest: None,
                    head_sha: "head".into(),
                    anchor_state: AnchorState::Current,
                    anchor_reason: None,
                    resolved: false,
                    created_at: None,
                },
            )
            .unwrap();
        for (id, body) in [("c1", "fix this"), ("c2", "follow up")] {
            store
                .insert_comment(
                    &Comment {
                        id: id.into(),
                        thread_id: "t".into(),
                        author_kind: Actor::Human,
                        author: "me".into(),
                        body: body.into(),
                        created_at: id.into(),
                        updated_at: id.into(),
                        publish_state: PublishState::Draft,
                        github_comment_id: None,
                    },
                    None,
                )
                .unwrap();
        }
        store
            .finish_review(&session.pr_id, Verdict::Comment, "review body")
            .unwrap();
    }
    let plan = preview(&session).unwrap();
    assert_eq!(plan.inline.len(), 1);
    assert_eq!(plan.replies.len(), 1);
    let result = publish(&session, &plan.plan_id, true).await.unwrap();
    assert_eq!(result.status, "succeeded");
    assert_eq!(result.published_comment_ids, ["c1", "c2"]);
    let stored = session
        .store
        .lock()
        .unwrap()
        .list_threads(&session.pr_id)
        .unwrap();
    assert_eq!(stored[0].comments[0].github_comment_id, Some(70));
    assert_eq!(stored[0].comments[1].github_comment_id, Some(71));
    assert_eq!(fake.calls_matching(&["api", "--method", "POST"]).len(), 2);
    assert!(publish(&session, &plan.plan_id, true).await.is_err());
    assert_eq!(fake.calls_matching(&["api", "--method", "POST"]).len(), 2);
}

#[tokio::test]
async fn unknown_remote_outcome_is_durable_and_never_retried() {
    let fake = Arc::new(FakeGh::default());
    fake.on(&["pr", "view"], Ok(r#"{"headRefOid":"head"}"#));
    fake.on(
        &["api", "--method", "POST"],
        Err("connection lost after submit"),
    );
    let session = Session::new(
        snapshot(),
        Store::open_in_memory().unwrap(),
        Some(fake.clone()),
        Some(PrRef {
            owner: "o".into(),
            repo: "r".into(),
            number: 1,
        }),
    )
    .unwrap();
    session
        .store
        .lock()
        .unwrap()
        .finish_review(&session.pr_id, Verdict::Approve, "okay")
        .unwrap();
    let plan = preview(&session).unwrap();
    let first = publish(&session, &plan.plan_id, true).await.unwrap();
    assert_eq!(first.status, "partial");
    assert!(first.error.unwrap().contains("do not retry"));
    assert!(publish(&session, &plan.plan_id, true).await.is_err());
    session
        .store
        .lock()
        .unwrap()
        .finish_review(&session.pr_id, Verdict::Approve, "edited summary")
        .unwrap();
    let changed = preview(&session).unwrap();
    assert_ne!(plan.plan_id, changed.plan_id);
    assert!(publish(&session, &changed.plan_id, true).await.is_err());
    assert_eq!(fake.calls_matching(&["api", "--method", "POST"]).len(), 1);
}

#[test]
fn stale_draft_is_omitted_without_moving_its_anchor() {
    let t = ReviewThread {
        id: "t".into(),
        source: ThreadSource::Local,
        github_thread_id: None,
        path: "lost.rs".into(),
        file_id: None,
        side: Side::Right,
        line: Some(4),
        start_line: None,
        line_text: Some("x".into()),
        anchor_state: AnchorState::Current,
        anchor_reason: None,
        resolved: false,
        head_sha: "head".into(),
        created_at: String::new(),
        comments: vec![Comment {
            id: "c".into(),
            thread_id: "t".into(),
            author_kind: Actor::Human,
            author: "a".into(),
            body: "fix".into(),
            created_at: String::new(),
            updated_at: String::new(),
            publish_state: PublishState::Draft,
            github_comment_id: None,
        }],
    };
    let plan = build_plan(
        &snapshot(),
        Verdict::Approve,
        "",
        &[t],
        &Selection::default(),
    )
    .unwrap();
    assert!(plan.inline.is_empty());
    assert_eq!(plan.omitted[0].comment_id, "c");
}
