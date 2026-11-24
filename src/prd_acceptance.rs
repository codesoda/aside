//! Backend acceptance cases from PRD §9 that need a file-backed session.
//!
//! Rule matching, diff parsing, publication guards, SSE, and the agent skill
//! already have their own tests. See `docs/plans/prd-acceptance.md`.

use std::{
    sync::{Arc, Barrier, mpsc},
    thread,
    time::Duration,
};

use crate::{
    demo,
    model::{Acknowledgement, Actor, FileStatus, PrSnapshot, RuleAction, RuleKind},
    session::Session,
    store::{RuleUpdate, Store},
    util::{new_id, now_rfc3339},
};

fn open_session(path: &std::path::Path, snapshot: PrSnapshot) -> Arc<Session> {
    Session::new(snapshot, Store::open(path).unwrap(), None, None).unwrap()
}

fn file_state(session: &Session, path: &str) -> crate::model::FileState {
    session
        .view(Actor::Human)
        .unwrap()
        .file_states
        .into_iter()
        .find(|file| file.path == path)
        .unwrap_or_else(|| panic!("missing file {path}"))
}

fn acknowledge(session: &Session, path: &str) {
    let snapshot = session.snapshot.read().unwrap().clone();
    let file = snapshot
        .files
        .iter()
        .find(|file| file.path == path)
        .unwrap_or_else(|| panic!("missing file {path}"));
    let ack = Acknowledgement {
        id: new_id("ack"),
        path: path.to_owned(),
        kind: "reviewed".into(),
        fingerprint_version: file.fingerprint_version,
        diff_digest: file.diff_digest.clone(),
        content_digest: file.content_digest.clone(),
        head_sha: snapshot.pr.head_sha.clone(),
        base_sha: snapshot.pr.base_sha.clone(),
        actor: Actor::Human,
        created_at: now_rfc3339(),
    };
    session
        .store
        .lock()
        .unwrap()
        .insert_acknowledgement(&session.pr_id, &ack)
        .unwrap();
}

fn directory_with_two_files(snapshot: &PrSnapshot) -> (String, String, String) {
    let mut groups: std::collections::BTreeMap<&str, Vec<&str>> = std::collections::BTreeMap::new();
    for file in &snapshot.files {
        if let Some((dir, _)) = file.path.split_once('/') {
            groups.entry(dir).or_default().push(file.path.as_str());
        }
    }
    let (dir, paths) = groups
        .into_iter()
        .find(|(_, paths)| paths.len() >= 2)
        .expect("demo fixture has a directory with two changed files");
    (dir.to_owned(), paths[0].to_owned(), paths[1].to_owned())
}

fn assert_partition(session: &Session) {
    let progress = session.view(Actor::Human).unwrap().progress;
    assert_eq!(
        progress.reviewed
            + progress.changed_since_reviewed
            + progress.unreviewed
            + progress.ignored,
        progress.total
    );
    assert_eq!(progress.in_scope + progress.ignored, progress.total);
}

#[test]
fn exclusions_and_digests_survive_restart_and_stay_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("aside.db");
    let original = demo::snapshot(0);
    let (folder, included, hidden) = directory_with_two_files(&original);
    let stable = original
        .files
        .iter()
        .find(|file| file.path != "git/client.go" && !file.path.starts_with(&format!("{folder}/")))
        .expect("a file outside the excluded directory")
        .path
        .clone();

    {
        let session = open_session(&path, original.clone());
        let mut store = session.store.lock().unwrap();
        store
            .create_rule(
                &session.repository.id,
                RuleAction::Exclude,
                RuleKind::Directory,
                &folder,
                None,
                Actor::Human,
            )
            .unwrap();
        store
            .create_rule(
                &session.repository.id,
                RuleAction::Include,
                RuleKind::File,
                &included,
                None,
                Actor::Human,
            )
            .unwrap();
        drop(store);
        acknowledge(&session, "git/client.go");
        acknowledge(&session, &stable);
        let included_state = file_state(&session, &included);
        let hidden_state = file_state(&session, &hidden);
        assert_eq!(included_state.status, FileStatus::Unreviewed);
        assert!(!included_state.scope.ignored);
        assert_eq!(hidden_state.status, FileStatus::Ignored);
        assert_eq!(hidden_state.review_status, FileStatus::Unreviewed);
        assert_eq!(
            file_state(&session, "git/client.go").status,
            FileStatus::ReviewedCurrent
        );
        assert_partition(&session);
    }

    {
        let session = open_session(&path, original.clone());
        assert_eq!(
            file_state(&session, "git/client.go").status,
            FileStatus::ReviewedCurrent
        );
        assert_eq!(
            file_state(&session, &stable).status,
            FileStatus::ReviewedCurrent
        );
        assert_eq!(file_state(&session, &hidden).status, FileStatus::Ignored);
        assert!(!file_state(&session, &included).scope.ignored);
        assert_eq!(
            session
                .store
                .lock()
                .unwrap()
                .list_acknowledgements(&session.pr_id)
                .unwrap()
                .len(),
            2
        );
    }

    {
        let mut later = original.clone();
        later.pr.number = 57;
        later.pr.url = "https://github.com/cli/cli/pull/57".into();
        let session = open_session(&path, later);
        assert_eq!(file_state(&session, &hidden).status, FileStatus::Ignored);
        assert!(!file_state(&session, &included).scope.ignored);
        assert_eq!(
            file_state(&session, "git/client.go").status,
            FileStatus::Unreviewed,
            "acknowledgements belong to the pull request, not the repository"
        );
        assert!(
            session
                .store
                .lock()
                .unwrap()
                .list_acknowledgements(&session.pr_id)
                .unwrap()
                .is_empty()
        );
    }

    {
        let mut other = original.clone();
        other.provider_repository_id = Some("acceptance-other-repo".into());
        other.pr.owner = "other".into();
        other.pr.repo = "widget".into();
        other.pr.number = 1;
        other.pr.url = "https://github.com/other/widget/pull/1".into();
        let session = open_session(&path, other);
        assert_eq!(
            file_state(&session, &hidden).status,
            FileStatus::Unreviewed,
            "repository exclusions must not leak"
        );
    }

    {
        let session = open_session(&path, demo::snapshot(1));
        assert_eq!(
            file_state(&session, "git/client.go").status,
            FileStatus::ChangedSinceReviewed
        );
        assert_eq!(
            file_state(&session, &stable).status,
            FileStatus::ReviewedCurrent
        );
        assert_eq!(
            file_state(&session, "git/client.go").review_status,
            FileStatus::ChangedSinceReviewed
        );
    }
}

#[test]
fn ignoring_and_reincluding_keeps_acknowledgements() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("aside.db");
    let session = open_session(&path, demo::snapshot(0));
    acknowledge(&session, "git/client.go");
    let rule = session
        .store
        .lock()
        .unwrap()
        .create_rule(
            &session.repository.id,
            RuleAction::Exclude,
            RuleKind::File,
            "git/client.go",
            None,
            Actor::Human,
        )
        .unwrap();
    let ignored = file_state(&session, "git/client.go");
    assert_eq!(ignored.status, FileStatus::Ignored);
    assert_eq!(ignored.review_status, FileStatus::ReviewedCurrent);
    assert_eq!(
        session
            .store
            .lock()
            .unwrap()
            .list_acknowledgements(&session.pr_id)
            .unwrap()
            .len(),
        1
    );

    session
        .store
        .lock()
        .unwrap()
        .update_rule(
            &session.repository.id,
            &rule.id,
            &RuleUpdate {
                enabled: Some(false),
                ..RuleUpdate::default()
            },
        )
        .unwrap();
    assert_eq!(
        file_state(&session, "git/client.go").status,
        FileStatus::ReviewedCurrent
    );
    session
        .store
        .lock()
        .unwrap()
        .update_rule(
            &session.repository.id,
            &rule.id,
            &RuleUpdate {
                enabled: Some(true),
                ..RuleUpdate::default()
            },
        )
        .unwrap();
    assert_eq!(
        file_state(&session, "git/client.go").status,
        FileStatus::Ignored
    );
    session
        .store
        .lock()
        .unwrap()
        .delete_rule(&session.repository.id, &rule.id)
        .unwrap();
    assert_eq!(
        file_state(&session, "git/client.go").status,
        FileStatus::ReviewedCurrent
    );
    drop(session);

    let session = open_session(&path, demo::snapshot(0));
    assert_eq!(
        file_state(&session, "git/client.go").status,
        FileStatus::ReviewedCurrent
    );
    let acks = session
        .store
        .lock()
        .unwrap()
        .list_acknowledgements(&session.pr_id)
        .unwrap();
    assert_eq!(acks.len(), 1);
    assert_eq!(acks[0].kind, "reviewed");
    assert_eq!(acks[0].path, "git/client.go");
}

/// Every participant waits exactly once. A failure before the wait still
/// releases the other participants instead of hanging the suite.
fn after_barrier<T, R>(
    barrier: &Barrier,
    prepare: impl FnOnce() -> Result<T, String>,
    write: impl FnOnce(T) -> Result<R, String>,
) -> Result<R, String> {
    let mut waited = false;
    let result = (|| {
        let prepared = prepare()?;
        barrier.wait();
        waited = true;
        write(prepared)
    })();
    if !waited {
        barrier.wait();
    }
    result
}

#[test]
fn barrier_releases_when_preparation_fails() {
    let barrier = Arc::new(Barrier::new(2));
    let (tx, rx) = mpsc::channel();
    for fail in [true, false] {
        let barrier = Arc::clone(&barrier);
        let tx = tx.clone();
        thread::spawn(move || {
            let result = after_barrier(
                &barrier,
                || {
                    if fail {
                        Err("open failed".into())
                    } else {
                        Ok(())
                    }
                },
                |()| Ok(()),
            );
            tx.send(result).unwrap();
        });
    }
    drop(tx);
    let mut results = Vec::new();
    for _ in 0..2 {
        results.push(
            rx.recv_timeout(Duration::from_secs(2))
                .expect("worker did not release the barrier"),
        );
    }
    let failures = results.iter().filter(|result| result.is_err()).count();
    assert_eq!(failures, 1);
}

#[test]
fn concurrent_rule_and_acknowledgement_writes_commit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("aside.db");
    let (repo_id, pr_id) = {
        let mut store = Store::open(&path).unwrap();
        let repo = store
            .resolve_repository("github.com", "acme", "widget", Some("R-accept"))
            .unwrap();
        let pr = store
            .upsert_pull_request(
                &repo.id,
                42,
                "Acceptance",
                "https://github.com/acme/widget/pull/42",
            )
            .unwrap();
        (repo.id, pr)
    };
    let barrier = Arc::new(Barrier::new(8));
    let mut tasks = Vec::new();
    for index in 0..4 {
        let path = path.clone();
        let repo_id = repo_id.clone();
        let barrier = Arc::clone(&barrier);
        tasks.push(thread::spawn(move || -> Result<(), String> {
            after_barrier(
                &barrier,
                || Store::open(&path).map_err(|error| format!("{error:#}")),
                |mut store| {
                    store
                        .create_rule(
                            &repo_id,
                            RuleAction::Exclude,
                            RuleKind::Directory,
                            &format!("concurrent/dir-{index}"),
                            None,
                            Actor::Human,
                        )
                        .map(|_| ())
                        .map_err(|error| format!("{error:#}"))
                },
            )
        }));
    }
    for index in 0..4 {
        let path = path.clone();
        let pr_id = pr_id.clone();
        let barrier = Arc::clone(&barrier);
        tasks.push(thread::spawn(move || -> Result<(), String> {
            after_barrier(
                &barrier,
                || Store::open(&path).map_err(|error| format!("{error:#}")),
                |mut store| {
                    let ack = Acknowledgement {
                        id: new_id("ack"),
                        path: format!("concurrent/{index}.rs"),
                        kind: "reviewed".into(),
                        fingerprint_version: 1,
                        diff_digest: format!("digest-{index}"),
                        content_digest: None,
                        head_sha: "head".into(),
                        base_sha: "base".into(),
                        actor: Actor::Human,
                        created_at: now_rfc3339(),
                    };
                    store
                        .insert_acknowledgement(&pr_id, &ack)
                        .map(|_| ())
                        .map_err(|error| format!("{error:#}"))
                },
            )
        }));
    }
    for task in tasks {
        task.join().unwrap().unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(store.list_rules(&repo_id).unwrap().len(), 4);
    assert_eq!(store.list_acknowledgements(&pr_id).unwrap().len(), 4);
}
