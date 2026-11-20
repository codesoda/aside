//! Selecting individual local drafts for publication (#13).

use super::{plan::OmitCode, *};
use crate::{
    diff,
    github::{PrRef, fake::FakeGh},
    model::{
        Actor, AnchorState, Comment, PrSnapshot, PublishState, PullRequest, SCHEMA_VERSION, Side,
        ThreadSource, Verdict,
    },
    store::{NewThread, NotADraft, NotFound, ReservedDraft, Store},
};

const DIFF: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n\
                    diff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-was\n+now\n";
pub(super) const POST_REVIEW: &[&str] = &["api", "--method", "POST", "repos/o/r/pulls/1/reviews"];

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
        files: diff::parse_unified(DIFF, "head", &diff::Limits::default()).files,
        github_threads: vec![],
        reviews: vec![],
        issue_comments: vec![],
        warnings: vec![],
        complete: true,
        fetched_at: String::new(),
    }
}

/// A GitHub that accepts reviews and replies and lists the inline comments it created.
pub(super) fn github() -> Arc<FakeGh> {
    let fake = Arc::new(FakeGh::default());
    fake.on(&["pr", "view"], Ok(r#"{"headRefOid":"head"}"#));
    fake.on(POST_REVIEW, Ok(r#"{"id":7}"#));
    fake.on(
        &[
            "api",
            "--paginate",
            "--slurp",
            "repos/o/r/pulls/1/reviews/7/comments?per_page=100",
        ],
        Ok(r#"[[{"id":70,"path":"a.rs","line":1,"body":"fix this"},
                {"id":72,"path":"b.rs","line":1,"body":"and this, please"}]]"#),
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
    fake
}

fn thread(id: &str, path: &str, text: &str, source: ThreadSource) -> NewThread {
    NewThread {
        id: id.into(),
        source,
        github_thread_id: None,
        path: path.into(),
        side: Side::Right,
        line: Some(1),
        start_line: None,
        line_text: Some(text.into()),
        diff_digest: None,
        head_sha: "head".into(),
        anchor_state: AnchorState::Current,
        anchor_reason: None,
        resolved: false,
        created_at: None,
    }
}

fn comment(
    id: &str,
    thread_id: &str,
    body: &str,
    author_kind: Actor,
    state: PublishState,
) -> Comment {
    Comment {
        id: id.into(),
        thread_id: thread_id.into(),
        author_kind,
        author: "me".into(),
        body: body.into(),
        created_at: id.into(),
        updated_at: id.into(),
        publish_state: state,
        github_comment_id: None,
    }
}

/// Thread t1 on a.rs: draft c1 and its draft reply c2. Thread t2 on b.rs: draft c3.
pub(super) fn session(fake: &Arc<FakeGh>) -> Arc<Session> {
    let pr = PrRef {
        owner: "o".into(),
        repo: "r".into(),
        number: 1,
    };
    let session = Session::new(
        snapshot(),
        Store::open_in_memory().unwrap(),
        Some(fake.clone()),
        Some(pr),
    )
    .unwrap();
    {
        let mut store = session.store.lock().unwrap();
        let pr_id = session.pr_id.clone();
        store
            .insert_thread(&pr_id, &thread("t1", "a.rs", "new", ThreadSource::Local))
            .unwrap();
        store
            .insert_thread(&pr_id, &thread("t2", "b.rs", "now", ThreadSource::Local))
            .unwrap();
        for (id, t, body) in [
            ("c1", "t1", "fix this"),
            ("c2", "t1", "follow up"),
            ("c3", "t2", "and this"),
        ] {
            let draft = comment(id, t, body, Actor::Human, PublishState::Draft);
            store.insert_comment(&draft, None).unwrap();
        }
        store
            .finish_review(&pr_id, Verdict::Comment, "review body")
            .unwrap();
    }
    session
}

pub(super) fn select(session: &Session, ids: &[&str], selected: bool) -> Result<()> {
    let ids: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
    session
        .store
        .lock()
        .unwrap()
        .set_publication_selection(&session.pr_id, &ids, selected)
}

pub(super) fn inline_ids(plan: &PublishPlan) -> Vec<&str> {
    plan.inline.iter().map(|c| c.comment_id.as_str()).collect()
}

pub(super) fn omitted(plan: &PublishPlan) -> Vec<(&str, OmitCode)> {
    plan.omitted
        .iter()
        .map(|o| (o.comment_id.as_str(), o.code))
        .collect()
}

#[test]
fn every_eligible_draft_is_selected_by_default() {
    let plan = preview(&session(&github())).unwrap();
    assert_eq!(inline_ids(&plan), ["c1", "c3"]);
    assert_eq!(plan.replies[0].comment_id, "c2");
    assert!(plan.omitted.is_empty());
}

#[test]
fn a_deselected_first_comment_leaves_its_replies_local() {
    let s = session(&github());
    select(&s, &["c1"], false).unwrap();
    let plan = preview(&s).unwrap();
    assert_eq!(inline_ids(&plan), ["c3"]);
    assert!(plan.replies.is_empty());
    assert_eq!(
        omitted(&plan),
        [
            ("c1", OmitCode::NotSelected),
            ("c2", OmitCode::ParentNotSelected)
        ]
    );
    assert_eq!(
        plan.omitted[1].reason,
        "its thread's first comment is not selected"
    );

    select(&s, &["c1"], true).unwrap();
    select(&s, &["c2"], false).unwrap();
    let plan = preview(&s).unwrap();
    assert_eq!(inline_ids(&plan), ["c1", "c3"]);
    assert!(
        plan.replies.is_empty(),
        "a deselected reply affects nothing else"
    );
    assert_eq!(omitted(&plan), [("c2", OmitCode::NotSelected)]);
}

#[tokio::test]
async fn confirm_sends_only_selected_drafts_and_a_later_review_resumes() {
    let fake = github();
    let s = session(&fake);
    select(&s, &["c3"], false).unwrap();
    let plan = preview(&s).unwrap();
    let outcome = publish(&s, &plan.plan_id, true).await.unwrap();
    assert_eq!(outcome.status, "succeeded");
    assert_eq!(outcome.published_comment_ids, ["c1", "c2"]);
    let calls = fake.calls_matching(POST_REVIEW);
    let sent: serde_json::Value = serde_json::from_str(calls[0].1.as_deref().unwrap()).unwrap();
    assert_eq!(sent["comments"].as_array().unwrap().len(), 1);
    assert_eq!(sent["comments"][0]["body"], "fix this");

    {
        let mut store = s.store.lock().unwrap();
        let held = store.get_comment("c3").unwrap();
        assert_eq!(held.publish_state, PublishState::Draft);
        assert_eq!(held.github_comment_id, None);
        store.update_comment_body("c3", "and this, please").unwrap();
        assert!(
            store
                .deselected_comment_ids(&s.pr_id)
                .unwrap()
                .contains("c3")
        );
        store
            .finish_review(&s.pr_id, Verdict::Comment, "second pass")
            .unwrap();
    }
    assert_eq!(
        omitted(&preview(&s).unwrap()),
        [("c3", OmitCode::NotSelected)],
        "the choice is durable"
    );
    select(&s, &["c3"], true).unwrap();
    let next = preview(&s).unwrap();
    assert_eq!(inline_ids(&next), ["c3"]);
    let outcome = publish(&s, &next.plan_id, true).await.unwrap();
    assert_eq!(outcome.status, "succeeded");
    assert_eq!(outcome.published_comment_ids, ["c3"]);
}

#[tokio::test]
async fn a_plan_previewed_before_a_selection_change_is_refused() {
    let fake = github();
    let s = session(&fake);
    let plan = preview(&s).unwrap();
    select(&s, &["c2"], false).unwrap();
    assert!(publish(&s, &plan.plan_id, true).await.is_err());
    assert!(fake.calls_matching(&["api", "--method", "POST"]).is_empty());
}

#[test]
fn reserving_a_draft_deselected_after_the_preview_is_refused() {
    let s = session(&github());
    select(&s, &["c3"], false).unwrap();
    let error = s
        .store
        .lock()
        .unwrap()
        .start_publication(&s.pr_id, "plan", "{}", "head", &[("c3", "and this")])
        .unwrap_err();
    assert!(error.to_string().contains("preview again"));
}

#[tokio::test]
async fn a_failed_send_keeps_unsent_and_held_back_drafts() {
    let fake = Arc::new(FakeGh::default());
    fake.on(&["pr", "view"], Ok(r#"{"headRefOid":"head"}"#));
    fake.on(
        &["api", "--method", "POST"],
        Err("connection lost after submit"),
    );
    let s = session(&fake);
    select(&s, &["c3"], false).unwrap();
    let plan = preview(&s).unwrap();
    let outcome = publish(&s, &plan.plan_id, true).await.unwrap();
    assert_eq!(outcome.status, "partial");
    assert_eq!(outcome.remaining_comment_ids, ["c1", "c2"]);

    let mut store = s.store.lock().unwrap();
    for id in ["c1", "c2", "c3"] {
        assert_eq!(
            store.get_comment(id).unwrap().publish_state,
            PublishState::Draft
        );
    }
    let locked = store.update_comment_body("c1", "edited").unwrap_err();
    assert!(locked.downcast_ref::<ReservedDraft>().is_some());
    let reselect = store
        .set_publication_selection(&s.pr_id, &["c1".into()], false)
        .unwrap_err();
    assert!(reselect.downcast_ref::<ReservedDraft>().is_some());
    store.update_comment_body("c3", "still mine").unwrap();
    store
        .set_publication_selection(&s.pr_id, &["c3".into()], true)
        .unwrap();
}

#[test]
fn only_local_drafts_of_this_pr_can_be_selected() {
    let s = session(&github());
    {
        let mut store = s.store.lock().unwrap();
        store
            .insert_thread(&s.pr_id, &thread("g", "a.rs", "new", ThreadSource::Github))
            .unwrap();
        let imported = comment(
            "g1",
            "g",
            "from GitHub",
            Actor::Github,
            PublishState::Imported,
        );
        store.insert_comment(&imported, None).unwrap();
    }
    let error = select(&s, &["g1"], false).unwrap_err();
    assert!(error.downcast_ref::<NotADraft>().is_some());
    let error = select(&s, &["c1", "missing"], false).unwrap_err();
    assert!(error.downcast_ref::<NotFound>().is_some());
    let store = s.store.lock().unwrap();
    assert!(
        store.deselected_comment_ids(&s.pr_id).unwrap().is_empty(),
        "a refused change leaves every draft as it was"
    );
}

#[test]
fn deleting_a_thread_s_first_draft_leaves_its_replies_local() {
    let s = session(&github());
    s.store.lock().unwrap().delete_comment("c1").unwrap();
    let plan = preview(&s).unwrap();
    assert_eq!(
        inline_ids(&plan),
        ["c3"],
        "c2 is a reply, not a new comment"
    );
    assert!(plan.replies.is_empty());
    assert_eq!(omitted(&plan), [("c2", OmitCode::ParentNotPublishable)]);

    let s = session(&github());
    s.store.lock().unwrap().delete_comment("c2").unwrap();
    let plan = preview(&s).unwrap();
    assert_eq!(inline_ids(&plan), ["c1", "c3"]);
    assert!(plan.omitted.is_empty(), "deleting a reply leaves the root");
}

#[test]
fn deleting_a_held_back_draft_forgets_its_selection() {
    let s = session(&github());
    select(&s, &["c3"], false).unwrap();
    let mut store = s.store.lock().unwrap();
    store.delete_comment("c3").unwrap();
    assert!(store.deselected_comment_ids(&s.pr_id).unwrap().is_empty());
}
