//! Reconciling an unresolved publication so its remaining drafts can be sent (#13).

use super::selection_tests::{POST_REVIEW, github, inline_ids, select, session, session_with};
use super::*;
use crate::{
    github::fake::FakeGh,
    model::{PublishState, Verdict},
    store::{NotFound, PublishingElsewhere, Store},
};

const REPLY_TO_70: &[&str] = &[
    "api",
    "--method",
    "POST",
    "repos/o/r/pulls/1/comments/70/replies",
];

/// A GitHub whose every POST fails after it may have been received.
fn lost_connection() -> Arc<FakeGh> {
    let fake = Arc::new(FakeGh::default());
    fake.on(&["pr", "view"], Ok(r#"{"headRefOid":"head"}"#));
    fake.on(
        &["api", "--method", "POST"],
        Err("connection lost after submit"),
    );
    fake
}

/// Replace the scripted GitHub with one that accepts reviews and replies.
fn recover(fake: &FakeGh) {
    let healthy = github();
    let responses = std::mem::take(&mut *healthy.responses.lock().unwrap());
    *fake.responses.lock().unwrap() = responses;
}

fn state(session: &Session, id: &str) -> PublishState {
    lock_store(session)
        .unwrap()
        .get_comment(id)
        .unwrap()
        .publish_state
}

#[tokio::test]
async fn reconciling_a_failed_send_lets_its_drafts_and_held_back_drafts_publish() {
    let fake = lost_connection();
    let s = session(&fake);
    select(&s, &["c3"], false).unwrap();
    let plan = preview(&s).unwrap();
    let outcome = publish(&s, &plan.plan_id, true).await.unwrap();
    assert_eq!(outcome.status, "partial");

    let unresolved = lock_store(&s)
        .unwrap()
        .unresolved_publication(&s.pr_id)
        .unwrap()
        .unwrap();
    assert_eq!(unresolved.attempt_id, outcome.attempt_id);
    assert_eq!(unresolved.remaining_comment_ids, ["c1", "c2"]);

    // The human checked GitHub and found nothing from the attempt.
    reconcile(&s, &outcome.attempt_id, &[]).await.unwrap();
    {
        let mut store = lock_store(&s).unwrap();
        assert!(store.unresolved_publication(&s.pr_id).unwrap().is_none());
        store.update_comment_body("c2", "follow up").unwrap();
        store
            .set_publication_selection(&s.pr_id, &["c1".into()], true)
            .unwrap();
    }
    recover(&fake);
    let again = preview(&s).unwrap();
    assert_eq!(
        again.plan_id, plan.plan_id,
        "the same drafts, held back the same"
    );
    let outcome = publish(&s, &again.plan_id, true).await.unwrap();
    assert_eq!(outcome.status, "succeeded");
    assert_eq!(outcome.published_comment_ids, ["c1", "c2"]);
    assert_eq!(
        state(&s, "c3"),
        PublishState::Draft,
        "c3 is still held back"
    );

    select(&s, &["c3"], true).unwrap();
    {
        let mut store = lock_store(&s).unwrap();
        store.update_comment_body("c3", "and this, please").unwrap();
        store
            .finish_review(&s.pr_id, Verdict::Comment, "second pass")
            .unwrap();
    }
    let last = preview(&s).unwrap();
    assert_eq!(inline_ids(&last), ["c3"]);
    let outcome = publish(&s, &last.plan_id, true).await.unwrap();
    assert_eq!(outcome.status, "succeeded", "the claim was released");
    assert_eq!(outcome.published_comment_ids, ["c3"]);
}

#[tokio::test]
async fn a_reply_is_sent_only_once_its_parent_is_confirmed_on_github() {
    let fake = Arc::new(FakeGh::default());
    fake.on(&["pr", "view"], Ok(r#"{"headRefOid":"head"}"#));
    fake.on(POST_REVIEW, Ok(r#"{"id":7}"#));
    let s = session(&fake);
    select(&s, &["c3"], false).unwrap();
    let plan = preview(&s).unwrap();
    // The review exists, but listing its comments fails.
    let outcome = publish(&s, &plan.plan_id, true).await.unwrap();
    assert_eq!(outcome.status, "partial");
    assert_eq!(outcome.github_review_id, Some(7));

    reconcile(&s, &outcome.attempt_id, &[("c1".into(), Some(70))])
        .await
        .unwrap();
    assert_eq!(state(&s, "c1"), PublishState::Published);
    assert_eq!(state(&s, "c2"), PublishState::Draft);
    lock_store(&s)
        .unwrap()
        .finish_review(&s.pr_id, Verdict::Comment, "the reply")
        .unwrap();
    recover(&fake);
    let next = preview(&s).unwrap();
    assert!(next.inline.is_empty());
    assert_eq!(next.replies[0].comment_id, "c2");
    assert_eq!(next.replies[0].github_parent_id, Some(70));
    let outcome = publish(&s, &next.plan_id, true).await.unwrap();
    assert_eq!(outcome.status, "succeeded");
    assert_eq!(fake.calls_matching(REPLY_TO_70).len(), 1);
}

#[tokio::test]
async fn reconciling_needs_the_unresolved_attempt_and_its_own_drafts() {
    let s = session(&lost_connection());
    assert!(
        reconcile(&s, "pub_none", &[])
            .await
            .unwrap_err()
            .downcast_ref::<NotFound>()
            .is_some()
    );
    select(&s, &["c3"], false).unwrap();
    let plan = preview(&s).unwrap();
    let outcome = publish(&s, &plan.plan_id, true).await.unwrap();
    for (attempt, comment) in [("pub_other", "c1"), (outcome.attempt_id.as_str(), "c3")] {
        let error = reconcile(&s, attempt, &[(comment.into(), Some(70))])
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<NotFound>().is_some());
    }
    let store = lock_store(&s).unwrap();
    assert_eq!(
        store.get_comment("c3").unwrap().publish_state,
        PublishState::Draft
    );
    let unresolved = store.unresolved_publication(&s.pr_id).unwrap().unwrap();
    assert_eq!(
        unresolved.remaining_comment_ids,
        ["c1", "c2"],
        "a refused reconcile changes nothing"
    );
}

#[tokio::test]
async fn another_process_publishing_the_pr_holds_off_reconcile_and_publish() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("aside.db")).unwrap();
    let fake = lost_connection();
    let s = session_with(fake.clone(), store);
    select(&s, &["c3"], false).unwrap();
    let plan = preview(&s).unwrap();
    let outcome = publish(&s, &plan.plan_id, true).await.unwrap();
    assert_eq!(outcome.status, "partial");

    // Another Aside process sharing this database is sending this PR's review.
    let path = lock_store(&s)
        .unwrap()
        .publication_lock_path(&s.pr_id)
        .unwrap();
    let other = std::fs::File::open(&path).unwrap();
    other.try_lock().unwrap();
    let error = reconcile(&s, &outcome.attempt_id, &[]).await.unwrap_err();
    assert!(error.downcast_ref::<PublishingElsewhere>().is_some());
    let unresolved = lock_store(&s)
        .unwrap()
        .unresolved_publication(&s.pr_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        unresolved.remaining_comment_ids,
        ["c1", "c2"],
        "a refused reconcile changes nothing"
    );

    // When that process ends, the OS releases its lock.
    drop(other);
    reconcile(&s, &outcome.attempt_id, &[]).await.unwrap();
    recover(&fake);
    let held = std::fs::File::open(&path).unwrap();
    held.try_lock().unwrap();
    let error = publish(&s, &plan.plan_id, true).await.unwrap_err();
    assert!(error.downcast_ref::<PublishingElsewhere>().is_some());
    drop(held);
    let outcome = publish(&s, &plan.plan_id, true).await.unwrap();
    assert_eq!(outcome.status, "succeeded", "{:?}", outcome.error);
}

/// A GitHub that, during each POST, records whether another process could take the
/// PR's publication lock.
struct LockProbe {
    gh: Arc<FakeGh>,
    lock: std::sync::OnceLock<std::path::PathBuf>,
    free_during_post: std::sync::Mutex<Vec<bool>>,
}

impl GhRunner for LockProbe {
    fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>> {
        if args.contains(&"POST") {
            let other = std::fs::File::open(self.lock.get().unwrap())?;
            let free = other.try_lock().is_ok();
            self.free_during_post.lock().unwrap().push(free);
        }
        self.gh.run(args, stdin)
    }
}

#[tokio::test]
async fn a_send_holds_the_lock_until_it_ends() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("aside.db")).unwrap();
    let probe = Arc::new(LockProbe {
        gh: github(),
        lock: std::sync::OnceLock::new(),
        free_during_post: std::sync::Mutex::new(Vec::new()),
    });
    let s = session_with(probe.clone(), store);
    let path = lock_store(&s)
        .unwrap()
        .publication_lock_path(&s.pr_id)
        .unwrap();
    probe.lock.set(path.clone()).unwrap();
    select(&s, &["c3"], false).unwrap();
    let plan = preview(&s).unwrap();
    let outcome = publish(&s, &plan.plan_id, true).await.unwrap();
    assert_eq!(outcome.status, "succeeded", "{:?}", outcome.error);
    let free = probe.free_during_post.lock().unwrap().clone();
    assert_eq!(
        free,
        [false, false],
        "the review and the reply were sent locked"
    );
    let after = std::fs::File::open(&path).unwrap();
    after.try_lock().expect("released once the send ends");
}
