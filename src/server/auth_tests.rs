use super::*;
use crate::{
    github::{PrRef, fake::FakeGh},
    model::{AnchorState, Side, ThreadSource},
    store::{NewThread, Store},
};
use axum::http::{Method, header};
use std::sync::atomic::Ordering;
use tower::ServiceExt;

const AGENT: &str = "Bearer agent-secret";

fn live() -> (Arc<Session>, Arc<FakeGh>, Router) {
    let snapshot = crate::demo::snapshot(0);
    let reference = PrRef {
        owner: snapshot.pr.owner.clone(),
        repo: snapshot.pr.repo.clone(),
        number: snapshot.pr.number,
    };
    let gh = Arc::new(FakeGh::default());
    let session = Session::new(
        snapshot,
        Store::open_in_memory().unwrap(),
        Some(gh.clone()),
        Some(reference),
    )
    .unwrap();
    let app = router(AppState::new(
        session.clone(),
        "ui-secret".into(),
        "agent-secret".into(),
    ));
    (session, gh, app)
}

async fn send(
    app: &Router,
    method: Method,
    path: &str,
    credential: &str,
    body: Value,
) -> (StatusCode, Value) {
    let mut builder = axum::http::Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "127.0.0.1:1234")
        .header(
            if credential.starts_with("Bearer ") {
                header::AUTHORIZATION
            } else {
                header::COOKIE
            },
            credential,
        )
        .header(header::CONTENT_TYPE, "application/json");
    if credential.starts_with("aside_ui=") {
        builder = builder.header(header::ORIGIN, "http://127.0.0.1:1234");
    }
    let request = builder
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1_000_000)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn draft_body(session: &Session) -> Value {
    let snapshot = session.snapshot.read().unwrap();
    let file = snapshot
        .files
        .iter()
        .find(|f| {
            f.patch_complete
                && f.hunks
                    .iter()
                    .flat_map(|h| &h.lines)
                    .any(|l| l.new_line.is_some() && l.kind != crate::model::LineKind::Meta)
        })
        .unwrap();
    let line = file
        .hunks
        .iter()
        .flat_map(|h| &h.lines)
        .find_map(|l| {
            (l.kind != crate::model::LineKind::Meta)
                .then_some(l.new_line)
                .flatten()
        })
        .unwrap();
    json!({"fileId":file.id,"diffDigest":file.diff_digest,"side":"RIGHT","line":line,"body":"Agent note"})
}

#[tokio::test]
async fn agent_mutations_are_exact_and_denied_before_side_effects() {
    let (session, gh, app) = live();
    let draft = draft_body(&session);
    let attempts = [
        (
            Method::POST,
            "/api/v1/repositories/current/exclusions",
            json!({"action":"exclude","kind":"glob","expression":"*.rs"}),
        ),
        (
            Method::POST,
            "/api/v1/repositories/current/exclusions/preview",
            json!({"action":"exclude","kind":"glob","expression":"*.rs"}),
        ),
        (
            Method::PATCH,
            "/api/v1/repositories/current/exclusions/rule",
            json!({"expression":"*.rs"}),
        ),
        (
            Method::DELETE,
            "/api/v1/repositories/current/exclusions/rule",
            json!({}),
        ),
        (
            Method::POST,
            "/api/v1/files/file/reviewed",
            json!({"diffDigest":"valid"}),
        ),
        (
            Method::DELETE,
            "/api/v1/files/file/reviewed",
            json!({"diffDigest":"valid"}),
        ),
        (
            Method::PATCH,
            "/api/v1/threads/thread",
            json!({"resolved":true}),
        ),
        (
            Method::POST,
            "/api/v1/review/finish",
            json!({"verdict":"approve","summary":"Approved"}),
        ),
        (Method::POST, "/api/v1/review/publish/preview", json!({})),
        (
            Method::POST,
            "/api/v1/review/publish",
            json!({"planId":"plan","confirm":true}),
        ),
        (Method::POST, "/api/v1/analysis/runs", json!({})),
        (
            Method::POST,
            "/api/v1/model/consent",
            json!({"consent":true}),
        ),
        (Method::PATCH, "/api/v1/config", json!({"enabled":true})),
        (Method::POST, "/api/v2/threads", draft.clone()),
        (Method::POST, "/api/v1/threads/extra", draft),
        (Method::PUT, "/api/v1/threads", json!({})),
    ];
    let before = serde_json::to_value(session.view(Actor::Human).unwrap()).unwrap();
    for (method, path, payload) in attempts {
        let (status, response) = send(&app, method, path, AGENT, payload).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {response}");
        assert_eq!(response["error"], "forbidden");
        assert_eq!(
            serde_json::to_value(session.view(Actor::Human).unwrap()).unwrap(),
            before,
            "{path} changed state"
        );
        assert!(gh.calls.lock().unwrap().is_empty(), "{path} called GitHub");
    }
    assert_eq!(session.sequence.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn agent_drafts_replies_and_own_edits_stay_local() {
    let (session, gh, app) = live();
    let before_review = session.view(Actor::Human).unwrap().review;
    let (status, created) = send(
        &app,
        Method::POST,
        "/api/v1/threads",
        AGENT,
        draft_body(&session),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let thread = &created["thread"];
    assert_eq!(thread["source"], "local");
    assert_eq!(thread["comments"][0]["authorKind"], "agent");
    assert_eq!(thread["comments"][0]["publishState"], "private");
    let id = thread["id"].as_str().unwrap();
    let comment_id = thread["comments"][0]["id"].as_str().unwrap();
    let (status, _) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/comments/{comment_id}"),
        AGENT,
        json!({"body":"Updated note"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, reply) = send(
        &app,
        Method::POST,
        &format!("/api/v1/threads/{id}/replies"),
        AGENT,
        json!({"body":"Follow up"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(reply["error"], "invalid_request");
    let (status, _) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/comments/{comment_id}"),
        "aside_ui=ui-secret",
        json!({"body":"Steal"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN); // ownership is still checked
    let (status, human_thread) = send(
        &app,
        Method::POST,
        "/api/v1/threads",
        "aside_ui=ui-secret",
        draft_body(&session),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let human_comment = human_thread["thread"]["comments"][0]["id"]
        .as_str()
        .unwrap();
    for method in [Method::PATCH, Method::DELETE] {
        let (status, _) = send(
            &app,
            method,
            &format!("/api/v1/comments/{human_comment}"),
            AGENT,
            json!({"body":"Steal"}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    let (status, _) = send(
        &app,
        Method::DELETE,
        &format!("/api/v1/comments/{comment_id}"),
        AGENT,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(session.view(Actor::Human).unwrap().review, before_review);
    assert!(gh.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn imported_thread_reply_is_a_private_draft_and_credentials_are_separate() {
    let (session, gh, app) = live();
    {
        let snapshot = session.snapshot.read().unwrap();
        let file = &snapshot.files[0];
        session
            .store
            .lock()
            .unwrap()
            .insert_thread(
                &session.pr_id,
                &NewThread {
                    id: "imported".into(),
                    source: ThreadSource::Github,
                    github_thread_id: Some("GH_1".into()),
                    subject: crate::model::ThreadSubject::Line,
                    finding_id: None,
                    path: file.path.clone(),
                    side: Side::Right,
                    line: Some(1),
                    start_line: None,
                    line_text: None,
                    diff_digest: Some(file.diff_digest.clone()),
                    head_sha: snapshot.pr.head_sha.clone(),
                    anchor_state: AnchorState::Current,
                    anchor_reason: None,
                    resolved: false,
                    created_at: None,
                },
            )
            .unwrap();
    }
    let (status, reply) = send(
        &app,
        Method::POST,
        "/api/v1/threads/imported/replies",
        AGENT,
        json!({"body":"Private response"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(reply["error"], "invalid_request");
    assert!(gh.calls.lock().unwrap().is_empty());
    for (credential, status) in [
        ("Bearer ui-secret", StatusCode::UNAUTHORIZED),
        ("aside_ui=agent-secret", StatusCode::UNAUTHORIZED),
    ] {
        assert_eq!(
            send(&app, Method::GET, "/api/v1/session", credential, json!({}))
                .await
                .0,
            status
        );
    }
    let (status, _) = send(
        &app,
        Method::GET,
        "/auth?token=agent-secret",
        AGENT,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[test]
fn pinned_agent_route_policy() {
    use super::auth::agent_mutation_allowed;
    for (method, route) in [
        (Method::POST, "/api/v1/threads"),
        (Method::POST, "/api/v1/threads/abc/replies"),
        (Method::POST, "/api/v1/threads/abc/agent-response"),
        (Method::PATCH, "/api/v1/comments/abc"),
        (Method::DELETE, "/api/v1/comments/abc"),
        (Method::POST, "/api/v1/refresh"),
    ] {
        assert!(agent_mutation_allowed(&method, route), "{route}");
    }
    for route in [
        "/api/v1/threads/",
        "/api/v1/threads//replies",
        "/api/v1/comments/",
        "/api/v1/refresh/",
        "/api/v2/refresh",
        "/api/v1/model/consent",
    ] {
        assert!(!agent_mutation_allowed(&Method::POST, route), "{route}");
    }
}

#[tokio::test]
async fn successful_refresh_updates_snapshot_without_publication() {
    let (session, gh, app) = live();
    let before = session.view(Actor::Agent).unwrap();
    let review = serde_json::to_value(&before.review).unwrap();
    let url = format!(
        "https://github.com/{}/{}/pull/{}",
        before.pr.owner, before.pr.repo, before.pr.number
    );
    // The revision check must be scripted before the broader PR metadata prefix.
    gh.on(
        &["pr", "view", &url, "--json", "baseRefOid,headRefOid"],
        Ok(r#"{"baseRefOid":"base-live","headRefOid":"head-live"}"#),
    );
    let metadata = json!({
        "number": before.pr.number, "title": "Refreshed live title", "body": "Updated body", "url": url,
        "state": "OPEN", "isDraft": false, "author": {"login":"alice"},
        "baseRefName": "main", "baseRefOid": "base-live", "headRefName": "feature",
        "headRefOid": "head-live", "changedFiles": 1,
        "reviews": [], "comments": []
    }).to_string();
    gh.on(&["pr", "view"], Ok(&metadata));
    gh.on(&["repo", "view"], Ok(r#"{"id":"R_live"}"#));
    gh.on(
        &["pr", "diff"],
        Ok("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n"),
    );
    gh.on(&["api", "--paginate"], Ok(r#"[[{"filename":"a.rs","status":"modified","patch":null,"additions":1,"deletions":1}]]"#));
    gh.on(&["api", "graphql"], Ok(r#"{"data":{"repository":{"pullRequest":{"reviewThreads":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]}}}}}"#));

    let (status, refreshed) = send(&app, Method::POST, "/api/v1/refresh", AGENT, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{refreshed}");
    let after = session.view(Actor::Agent).unwrap();
    assert_eq!(after.pr.head_sha, "head-live");
    assert_eq!(after.pr.title, "Refreshed live title");
    assert_eq!(session.snapshot.read().unwrap().files[0].path, "a.rs");
    assert!(refreshed["sequence"].as_u64().unwrap() > before.sequence);
    assert_eq!(serde_json::to_value(after.review).unwrap(), review);
    let calls = gh.calls.lock().unwrap();
    assert!(
        calls
            .iter()
            .any(|(args, _)| args.starts_with(&["pr".into(), "diff".into()]))
    );
    assert!(
        calls
            .iter()
            .any(|(args, _)| args.starts_with(&["api".into(), "graphql".into()]))
    );
    assert!(calls.iter().all(|(args, stdin)| {
        stdin.is_none()
            && !args
                .iter()
                .any(|part| matches!(part.as_str(), "POST" | "PATCH" | "DELETE" | "PUT"))
    }));
}

#[tokio::test]
async fn refresh_reads_but_never_publishes() {
    let (session, gh, app) = live();
    let before = serde_json::to_value(session.view(Actor::Agent).unwrap()).unwrap();
    let (status, _) = send(&app, Method::POST, "/api/v1/refresh", AGENT, json!({})).await;
    // Unscripted FakeGh fails the read; authorization still reached the refresh handler.
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        serde_json::to_value(session.view(Actor::Agent).unwrap()).unwrap(),
        before
    );
    let calls = gh.calls.lock().unwrap();
    assert!(
        calls
            .iter()
            .any(|(args, _)| args.starts_with(&["pr".into(), "view".into()]))
    );
    assert!(calls.iter().all(|(args, _)| {
        !args
            .iter()
            .any(|part| matches!(part.as_str(), "POST" | "PATCH" | "DELETE"))
    }));
}
