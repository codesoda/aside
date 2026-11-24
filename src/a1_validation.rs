//! Measured checks for issue #43. Broader scenario ownership is recorded in
//! `docs/plans/a1-validation.md`.

use axum::{
    body::to_bytes,
    http::{HeaderMap, Method, Request, StatusCode, header},
};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::{
    demo,
    diff::{self, Limits},
    model::Actor,
    server::{AppState, router},
    session::Session,
    store::Store,
};

fn app() -> axum::Router {
    let session = Session::new(
        demo::snapshot(0),
        Store::open_in_memory().unwrap(),
        None,
        None,
    )
    .unwrap();
    router(AppState::new(
        session,
        "ui-secret".into(),
        "agent-secret".into(),
    ))
}

async fn send(
    method: Method,
    path: &str,
    origin: Option<&str>,
    body: Option<Value>,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "127.0.0.1:1234")
        .header(header::COOKIE, "aside_ui=ui-secret");
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    let bytes = if let Some(body) = body {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
        body.to_string()
    } else {
        String::new()
    };
    app()
        .oneshot(builder.body(axum::body::Body::from(bytes)).unwrap())
        .await
        .unwrap()
}

fn no_cors(headers: &HeaderMap) {
    assert!(headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
    assert!(
        headers
            .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
            .is_none()
    );
}

#[tokio::test]
async fn cross_origin_writes_fail_and_responses_are_not_shared() {
    let read = send(
        Method::GET,
        "/api/v1/session",
        Some("http://evil.example"),
        None,
    )
    .await;
    assert_eq!(read.status(), StatusCode::OK);
    no_cors(read.headers());

    let write = send(
        Method::POST,
        "/api/v1/repositories/current/exclusions",
        Some("http://evil.example"),
        Some(json!({"action":"exclude","kind":"file","expression":"git/client.go"})),
    )
    .await;
    assert_eq!(write.status(), StatusCode::FORBIDDEN);
    no_cors(write.headers());
    let body: Value =
        serde_json::from_slice(&to_bytes(write.into_body(), 100_000).await.unwrap()).unwrap();
    assert_eq!(body["error"], "invalid_origin");

    let session = send(
        Method::GET,
        "/api/v1/session",
        Some("http://127.0.0.1:1234"),
        None,
    )
    .await;
    let view: Value =
        serde_json::from_slice(&to_bytes(session.into_body(), 2_000_000).await.unwrap()).unwrap();
    assert_eq!(view["rules"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn untrusted_comment_and_path_stay_data() {
    let snapshot = demo::snapshot(0);
    let file = snapshot
        .files
        .iter()
        .find(|file| file.path == "git/client.go")
        .unwrap();
    let payload = "<script>alert(1)</script>\n[click](javascript:alert(1))";
    let created = send(
        Method::POST,
        "/api/v1/threads",
        Some("http://127.0.0.1:1234"),
        Some(json!({
            "fileId": file.id,
            "side": "RIGHT",
            "subject": "file",
            "diffDigest": file.diff_digest,
            "body": payload,
        })),
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);
    let created_body = to_bytes(created.into_body(), 2_000_000).await.unwrap();
    let created_json: Value = serde_json::from_slice(&created_body).unwrap();
    assert_eq!(created_json["thread"]["comments"][0]["body"], payload);
    assert!(!created_body.windows(5).any(|bytes| bytes == b"root:"));

    let rejected = send(
        Method::POST,
        "/api/v1/repositories/current/exclusions",
        Some("http://127.0.0.1:1234"),
        Some(json!({"action":"exclude","kind":"file","expression":"../etc/passwd"})),
    )
    .await;
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);

    let hostile = "diff --git a/../etc/passwd b/../etc/passwd\n--- a/../etc/passwd\n+++ b/../etc/passwd\n@@ -1 +1 @@\n-a\n+b\n";
    let parsed = diff::parse_unified(hostile, "head", &Limits::default());
    assert_eq!(parsed.files.len(), 1);
    assert_eq!(parsed.files[0].path, "../etc/passwd");
    assert!(parsed.warnings.is_empty());
}

#[test]
fn oversized_diff_keeps_a_counted_prefix() {
    assert_eq!(Limits::default().max_files, 3_000);
    let text = (0..3)
        .map(|index| {
            format!(
                "diff --git a/f{index}.txt b/f{index}.txt\n--- a/f{index}.txt\n+++ b/f{index}.txt\n@@ -1 +1 @@\n-a\n+b\n"
            )
        })
        .collect::<String>();
    let parsed = diff::parse_unified(
        &text,
        "head",
        &Limits {
            max_files: 2,
            max_lines_per_file: 20_000,
        },
    );
    assert_eq!(parsed.files.len(), 2);
    assert_eq!(parsed.warnings.len(), 1);
    assert!(parsed.warnings[0].contains("more than 2 files"));
    assert!(parsed.files.iter().all(|file| file.path != "f2.txt"));
}

#[test]
fn demo_session_has_no_github_runner() {
    let session = Session::new(
        demo::snapshot(0),
        Store::open_in_memory().unwrap(),
        None,
        None,
    )
    .unwrap();
    assert!(session.demo);
    assert!(session.gh.is_none());
    let view = session.view(Actor::Human).unwrap();
    assert!(view.complete);
    assert!(view.warnings.is_empty());
}
