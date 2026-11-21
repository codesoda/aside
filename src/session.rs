//! Runtime session: durable PR snapshots plus derived scope and review state.

use std::sync::{
    Arc, Mutex, RwLock,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::sync::{Mutex as AsyncMutex, broadcast};

use crate::{
    demo,
    diff::{self, Limits},
    github::{self, GhRunner, PrRef},
    model::{
        Acknowledgement, Actor, AnchorState, ChangedFile, EventEnvelope, FileState, FileStatus,
        PrSnapshot, Progress, Repository, ReviewThread, SCHEMA_VERSION, SessionView, ThreadSource,
    },
    rules,
    store::Store,
    util::{new_id, now_rfc3339},
};

pub struct Session {
    pub id: String,
    pub demo: bool,
    pub pr_ref: Option<PrRef>,
    pub gh: Option<Arc<dyn GhRunner>>,
    pub store: Mutex<Store>,
    pub snapshot: RwLock<PrSnapshot>,
    pub repository: Repository,
    pub pr_id: String,
    pub sequence: AtomicU64,
    pub events: broadcast::Sender<EventEnvelope>,
    refresh_lock: AsyncMutex<()>,
    demo_version: AtomicUsize,
}

impl Session {
    /// Persist the initial snapshot and imported threads before exposing the session.
    /// Demo mode is selected by passing neither `gh` nor `pr_ref`.
    pub fn new(
        snapshot: PrSnapshot,
        mut store: Store,
        gh: Option<Arc<dyn GhRunner>>,
        pr_ref: Option<PrRef>,
    ) -> Result<Arc<Self>> {
        if gh.is_some() != pr_ref.is_some() {
            bail!("GitHub runner and PR reference must be provided together");
        }
        if let Some(reference) = &pr_ref
            && (reference.owner != snapshot.pr.owner
                || reference.repo != snapshot.pr.repo
                || reference.number != snapshot.pr.number)
        {
            bail!("PR reference does not match the snapshot");
        }
        let repository = store.resolve_repository(
            "github.com",
            &snapshot.pr.owner,
            &snapshot.pr.repo,
            snapshot.provider_repository_id.as_deref(),
        )?;
        let pr_id = store.upsert_pull_request(
            &repository.id,
            snapshot.pr.number,
            &snapshot.pr.title,
            &snapshot.pr.url,
        )?;
        store.save_snapshot(&pr_id, &snapshot)?;
        store.sync_github_threads(
            &pr_id,
            &snapshot.pr.head_sha,
            &snapshot.github_threads,
            snapshot.github_threads_complete,
        )?;
        revalidate_anchors(&mut store, &pr_id, &snapshot)?;
        let demo_version =
            if pr_ref.is_none() && snapshot.pr.head_sha == demo::snapshot(1).pr.head_sha {
                1
            } else {
                0
            };
        let (events, _) = broadcast::channel(256);
        Ok(Arc::new(Self {
            id: new_id("session"),
            demo: pr_ref.is_none(),
            pr_ref,
            gh,
            store: Mutex::new(store),
            snapshot: RwLock::new(snapshot),
            repository,
            pr_id,
            sequence: AtomicU64::new(0),
            events,
            refresh_lock: AsyncMutex::new(()),
            demo_version: AtomicUsize::new(demo_version),
        }))
    }

    pub fn view(&self, actor: Actor) -> Result<SessionView> {
        let snapshot = self
            .snapshot
            .read()
            .expect("snapshot lock poisoned")
            .clone();
        let store = self.store.lock().expect("store lock poisoned");
        let rules = store.list_rules(&self.repository.id)?;
        let acknowledgements = store.list_acknowledgements(&self.pr_id)?;
        let mut threads = store.list_threads(&self.pr_id)?;
        attach_files(&mut threads, &snapshot.files);
        let file_states = snapshot
            .files
            .iter()
            .map(|file| {
                let scope = rules::evaluate(&rules, &file.path);
                let (review_status, last_reviewed_at) = review_status(file, &acknowledgements);
                FileState {
                    file_id: file.id.clone(),
                    path: file.path.clone(),
                    status: if scope.ignored {
                        FileStatus::Ignored
                    } else {
                        review_status
                    },
                    review_status,
                    scope,
                    last_reviewed_at,
                }
            })
            .collect::<Vec<_>>();
        let progress = progress(&file_states);
        Ok(SessionView {
            schema_version: SCHEMA_VERSION,
            session_id: self.id.clone(),
            demo: self.demo,
            sequence: self.sequence.load(Ordering::SeqCst),
            actor,
            repository: self.repository.clone(),
            pr: snapshot.pr,
            files: snapshot.files,
            file_states,
            progress,
            rules,
            threads,
            reviews: snapshot.reviews,
            issue_comments: snapshot.issue_comments,
            review: store.review_state(&self.pr_id)?,
            warnings: snapshot.warnings,
            complete: snapshot.complete,
            fetched_at: snapshot.fetched_at,
        })
    }

    /// Emit an event only after the corresponding mutation has committed.
    pub fn notify(&self, event_type: &str, payload: Value) -> EventEnvelope {
        let event = EventEnvelope {
            schema_version: SCHEMA_VERSION,
            session_id: self.id.clone(),
            sequence: self.sequence.fetch_add(1, Ordering::SeqCst) + 1,
            event_type: event_type.to_owned(),
            timestamp: now_rfc3339(),
            payload,
        };
        let _ = self.events.send(event.clone());
        event
    }

    /// Fetch outside SQLite locks, then persist before replacing the in-memory snapshot.
    /// On failure the previous snapshot stays available and no refresh event is emitted.
    /// Demo mode alternates deterministic versions 0 and 1.
    pub async fn refresh(&self) -> Result<SessionView> {
        let _refresh = self.refresh_lock.lock().await;
        let (snapshot, next_demo_version) = if self.demo {
            let next = (self.demo_version.load(Ordering::SeqCst) + 1) % 2;
            (demo::snapshot(next), Some(next))
        } else {
            let gh = Arc::clone(self.gh.as_ref().context("GitHub runner unavailable")?);
            let reference = self.pr_ref.clone().context("PR reference unavailable")?;
            let snapshot = tokio::task::spawn_blocking(move || {
                github::fetch_snapshot(gh.as_ref(), &reference, &Limits::default())
            })
            .await
            .context("GitHub refresh task failed")??;
            (snapshot, None)
        };
        let previous_head = self
            .snapshot
            .read()
            .expect("snapshot lock poisoned")
            .pr
            .head_sha
            .clone();
        {
            let mut store = self.store.lock().expect("store lock poisoned");
            store.upsert_pull_request(
                &self.repository.id,
                snapshot.pr.number,
                &snapshot.pr.title,
                &snapshot.pr.url,
            )?;
            store.save_snapshot(&self.pr_id, &snapshot)?;
            store.sync_github_threads(
                &self.pr_id,
                &snapshot.pr.head_sha,
                &snapshot.github_threads,
                snapshot.github_threads_complete,
            )?;
            revalidate_anchors(&mut store, &self.pr_id, &snapshot)?;
            if previous_head != snapshot.pr.head_sha {
                store.reopen_review(&self.pr_id)?;
            }
        }
        *self.snapshot.write().expect("snapshot lock poisoned") = snapshot;
        if let Some(next) = next_demo_version {
            self.demo_version.store(next, Ordering::SeqCst);
        }
        self.notify("pr.refreshed", serde_json::json!({"headSha": self.snapshot.read().expect("snapshot lock poisoned").pr.head_sha}));
        self.view(Actor::Human)
    }
}

fn review_status(file: &ChangedFile, history: &[Acknowledgement]) -> (FileStatus, Option<String>) {
    let latest = history.iter().rev().find(|ack| ack.path == file.path);
    let previous = file
        .previous_path
        .as_deref()
        .and_then(|path| history.iter().rev().find(|ack| ack.path == path));
    let last_reviewed_at = history
        .iter()
        .rev()
        .find(|ack| {
            ack.kind == "reviewed"
                && (ack.path == file.path
                    || Some(ack.path.as_str()) == file.previous_path.as_deref())
        })
        .map(|ack| ack.created_at.clone());
    if latest.is_some_and(|ack| ack.kind == "cleared") {
        return (FileStatus::Unreviewed, last_reviewed_at);
    }
    if latest.is_some_and(|ack| {
        file.patch_complete
            && ack.kind == "reviewed"
            && ack.fingerprint_version == file.fingerprint_version
            && ack.diff_digest == file.diff_digest
    }) {
        return (FileStatus::ReviewedCurrent, last_reviewed_at);
    }
    if latest.is_some_and(|ack| ack.kind == "reviewed")
        || previous.is_some_and(|ack| ack.kind == "reviewed")
    {
        (FileStatus::ChangedSinceReviewed, last_reviewed_at)
    } else {
        (FileStatus::Unreviewed, last_reviewed_at)
    }
}

fn progress(states: &[FileState]) -> Progress {
    let mut result = Progress {
        total: states.len() as u32,
        ..Default::default()
    };
    for state in states {
        match state.status {
            FileStatus::Ignored => result.ignored += 1,
            FileStatus::ReviewedCurrent => result.reviewed += 1,
            FileStatus::ChangedSinceReviewed => result.changed_since_reviewed += 1,
            FileStatus::Unreviewed => result.unreviewed += 1,
        }
    }
    result.in_scope = result.total - result.ignored;
    result
}

fn attach_files(threads: &mut [ReviewThread], files: &[ChangedFile]) {
    for thread in threads {
        thread.file_id = files
            .iter()
            .find(|file| file.path == thread.path)
            .map(|file| file.id.clone());
    }
}

fn revalidate_anchors(store: &mut Store, pr_id: &str, snapshot: &PrSnapshot) -> Result<()> {
    for thread in store.list_threads(pr_id)? {
        let file = snapshot.files.iter().find(|file| file.path == thread.path);
        let (state, reason) = match file {
            None => (AnchorState::Orphaned, Some("File is no longer in the PR")),
            Some(_) if thread.source == ThreadSource::Github => {
                let outdated = snapshot
                    .github_threads
                    .iter()
                    .find(|t| Some(t.node_id.as_str()) == thread.github_thread_id.as_deref())
                    .is_some_and(|t| t.is_outdated);
                if outdated {
                    (
                        AnchorState::Outdated,
                        Some("GitHub marks this thread as outdated"),
                    )
                } else if thread.anchor_state == AnchorState::Outdated {
                    (
                        AnchorState::Outdated,
                        Some("GitHub anchor cannot be verified locally"),
                    )
                } else {
                    (AnchorState::Current, None)
                }
            }
            Some(file) => match (thread.line, thread.line_text.as_deref()) {
                (Some(line), Some(text))
                    if diff::find_line(file, thread.side, line)
                        .is_some_and(|found| found.text == text) =>
                {
                    (AnchorState::Current, None)
                }
                _ => (
                    AnchorState::Outdated,
                    Some("Anchored line no longer matches the diff"),
                ),
            },
        };
        store.set_thread_anchor(&thread.id, state, reason)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::{RuleAction, RuleKind, Side, ThreadSource},
        store::NewThread,
        util::now_rfc3339,
    };

    fn session() -> Arc<Session> {
        Session::new(
            demo::snapshot(0),
            Store::open_in_memory().unwrap(),
            None,
            None,
        )
        .unwrap()
    }

    fn acknowledge(session: &Session, path: &str, kind: &str) {
        let snapshot = session.snapshot.read().unwrap();
        let file = snapshot.files.iter().find(|f| f.path == path).unwrap();
        let ack = Acknowledgement {
            id: new_id("ack"),
            path: path.into(),
            kind: kind.into(),
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

    #[test]
    fn scope_progress_and_cleared_acknowledgements() {
        let session = session();
        let initial = session.view(Actor::Agent).unwrap();
        assert_eq!(initial.actor, Actor::Agent);
        assert_eq!(initial.progress.unreviewed, initial.progress.total);
        let path = "git/client.go";
        acknowledge(&session, path, "reviewed");
        let mut store = session.store.lock().unwrap();
        store
            .create_rule(
                &session.repository.id,
                RuleAction::Exclude,
                RuleKind::File,
                path,
                None,
                Actor::Human,
            )
            .unwrap();
        drop(store);
        let ignored = session.view(Actor::Human).unwrap();
        let state = ignored.file_states.iter().find(|s| s.path == path).unwrap();
        assert_eq!(state.status, FileStatus::Ignored);
        assert_eq!(state.review_status, FileStatus::ReviewedCurrent);
        assert_eq!(ignored.progress.ignored, 1);
        assert_eq!(
            ignored.progress.in_scope + ignored.progress.ignored,
            ignored.progress.total
        );
        session
            .store
            .lock()
            .unwrap()
            .delete_rule(&session.repository.id, &ignored.rules[0].id)
            .unwrap();
        acknowledge(&session, path, "cleared");
        assert_eq!(
            session
                .view(Actor::Human)
                .unwrap()
                .file_states
                .iter()
                .find(|s| s.path == path)
                .unwrap()
                .status,
            FileStatus::Unreviewed
        );
    }

    #[test]
    fn incomplete_patch_never_derives_reviewed_current_from_stored_acknowledgement() {
        let session = session();
        let path = "git/client.go";
        acknowledge(&session, path, "reviewed");
        let current = session.view(Actor::Human).unwrap();
        assert_eq!(
            current
                .file_states
                .iter()
                .find(|s| s.path == path)
                .unwrap()
                .review_status,
            FileStatus::ReviewedCurrent
        );

        session
            .snapshot
            .write()
            .unwrap()
            .files
            .iter_mut()
            .find(|f| f.path == path)
            .unwrap()
            .patch_complete = false;
        let incomplete = session.view(Actor::Human).unwrap();
        let state = incomplete
            .file_states
            .iter()
            .find(|s| s.path == path)
            .unwrap();
        assert_eq!(state.review_status, FileStatus::ChangedSinceReviewed);
        assert_eq!(state.status, FileStatus::ChangedSinceReviewed);
        assert_eq!(incomplete.progress.reviewed, 0);
        assert_eq!(incomplete.progress.changed_since_reviewed, 1);
    }

    #[tokio::test]
    async fn refresh_keeps_unchanged_reviews_and_flags_changed_diffs() {
        let session = session();
        acknowledge(&session, "git/client.go", "reviewed");
        acknowledge(&session, "internal/identity/ssh.go", "reviewed");
        let mut receiver = session.events.subscribe();
        let view = session.refresh().await.unwrap();
        assert_eq!(
            view.file_states
                .iter()
                .find(|s| s.path == "git/client.go")
                .unwrap()
                .status,
            FileStatus::ChangedSinceReviewed
        );
        assert_eq!(
            view.file_states
                .iter()
                .find(|s| s.path == "internal/identity/ssh.go")
                .unwrap()
                .status,
            FileStatus::ReviewedCurrent
        );
        assert_eq!(view.sequence, 1);
        assert_eq!(receiver.try_recv().unwrap().sequence, 1);
        assert_eq!(session.refresh().await.unwrap().sequence, 2);
    }

    #[tokio::test]
    async fn refresh_revalidates_local_anchors_without_moving_them() {
        let session = session();
        let initial = session.view(Actor::Human).unwrap();
        let file = initial
            .files
            .iter()
            .find(|f| f.path == "git/client.go")
            .unwrap();
        let line = file
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .find(|l| l.text.contains("GH_IDENTITY_SSH_VARIANT="))
            .unwrap();
        session
            .store
            .lock()
            .unwrap()
            .insert_thread(
                &session.pr_id,
                &NewThread {
                    id: "anchor".into(),
                    source: ThreadSource::Local,
                    github_thread_id: None,
                    path: file.path.clone(),
                    side: Side::Right,
                    line: line.new_line,
                    start_line: None,
                    line_text: Some(line.text.clone()),
                    diff_digest: Some(file.diff_digest.clone()),
                    head_sha: initial.pr.head_sha,
                    anchor_state: AnchorState::Current,
                    anchor_reason: None,
                    resolved: false,
                    created_at: None,
                },
            )
            .unwrap();
        let view = session.refresh().await.unwrap();
        let thread = view.threads.iter().find(|t| t.id == "anchor").unwrap();
        assert_eq!(thread.anchor_state, AnchorState::Outdated);
        assert_eq!(thread.line, line.new_line);
        assert_eq!(thread.path, file.path);
    }

    #[tokio::test]
    async fn failed_live_refresh_retains_snapshot_and_emits_no_event() {
        let snapshot = demo::snapshot(0);
        let reference = PrRef {
            owner: snapshot.pr.owner.clone(),
            repo: snapshot.pr.repo.clone(),
            number: snapshot.pr.number,
        };
        let gh = Arc::new(github::fake::FakeGh::default());
        let session = Session::new(
            snapshot,
            Store::open_in_memory().unwrap(),
            Some(gh.clone()),
            Some(reference),
        )
        .unwrap();
        let before = session.view(Actor::Agent).unwrap();
        let mut receiver = session.events.subscribe();
        assert!(session.refresh().await.is_err());
        assert_eq!(
            session.view(Actor::Agent).unwrap().pr.head_sha,
            before.pr.head_sha
        );
        assert_eq!(session.sequence.load(Ordering::SeqCst), 0);
        assert!(receiver.try_recv().is_err());
        assert_eq!(gh.calls_matching(&["pr", "view"]).len(), 1);
    }

    #[test]
    fn absent_file_orphans_anchor() {
        let session = session();
        let original = session.snapshot.read().unwrap().clone();
        session
            .store
            .lock()
            .unwrap()
            .insert_thread(
                &session.pr_id,
                &NewThread {
                    id: "missing".into(),
                    source: ThreadSource::Local,
                    github_thread_id: None,
                    path: "git/client.go".into(),
                    side: Side::Right,
                    line: Some(1),
                    start_line: None,
                    line_text: Some("old".into()),
                    diff_digest: None,
                    head_sha: original.pr.head_sha.clone(),
                    anchor_state: AnchorState::Current,
                    anchor_reason: None,
                    resolved: false,
                    created_at: None,
                },
            )
            .unwrap();
        let mut without_file = original;
        without_file.files.retain(|f| f.path != "git/client.go");
        revalidate_anchors(
            &mut session.store.lock().unwrap(),
            &session.pr_id,
            &without_file,
        )
        .unwrap();
        let thread = session
            .store
            .lock()
            .unwrap()
            .get_thread(&session.pr_id, "missing")
            .unwrap();
        assert_eq!(thread.anchor_state, AnchorState::Orphaned);
        assert_eq!(thread.path, "git/client.go");
    }

    #[test]
    fn mismatched_live_configuration_is_rejected() {
        assert!(
            Session::new(
                demo::snapshot(0),
                Store::open_in_memory().unwrap(),
                None,
                Some(PrRef {
                    owner: "a".into(),
                    repo: "b".into(),
                    number: 1
                })
            )
            .is_err()
        );
    }
}
