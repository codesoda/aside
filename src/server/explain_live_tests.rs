//! Explicitly opt-in live inference. Never runs in the default test suite.
//! Only the synthetic snapshot below is sent; user/global Aside config is not loaded.
use super::*;
use crate::{config::AiConfig, diff, explain::Explanations, model::PrSnapshot};
use axum::{
    body::Body,
    http::{Request, header},
};
use tower::ServiceExt;

fn synthetic_snapshot() -> PrSnapshot {
    let mut snapshot: PrSnapshot = serde_json::from_value(json!({
        "schemaVersion": crate::model::SCHEMA_VERSION,
        "pr": {
            "owner": "synthetic", "repo": "explanation-smoke", "number": 1,
            "title": "Synthetic retry limit change", "body": "", "author": "synthetic-user",
            "url": "https://example.invalid/synthetic/explanation-smoke/pull/1",
            "state": "OPEN", "isDraft": true,
            "baseRef": "main", "baseSha": "1111111111111111111111111111111111111111",
            "headRef": "synthetic-change", "headSha": "2222222222222222222222222222222222222222"
        },
        "providerRepositoryId": null, "files": [], "githubThreads": [], "reviews": [],
        "issueComments": [], "warnings": [], "complete": true,
        "fetchedAt": "2026-01-01T00:00:00Z"
    }))
    .unwrap();
    snapshot.files = diff::parse_unified(
        "diff --git a/example.rs b/example.rs\n--- a/example.rs\n+++ b/example.rs\n@@ -1,3 +1,3 @@\n fn retry_limit() -> u32 {\n-    5\n+    10\n }\n",
        &snapshot.pr.head_sha,
        &diff::Limits::default(),
    ).files;
    snapshot
}

async fn ask_live(app: Router, body: Value) -> Value {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/explain/questions")
                .header(header::HOST, "127.0.0.1:1234")
                .header(header::ORIGIN, "http://127.0.0.1:1234")
                .header(header::COOKIE, "aside_ui=synthetic-ui")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let payload: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 100_000)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(status, StatusCode::OK, "Live provider failed: {payload}");
    assert_eq!(payload["requestId"], body["requestId"]);
    assert_eq!(payload["selection"], body["selection"]);
    assert_eq!(payload["provider"], "claude");
    assert!(!payload["answer"].as_str().unwrap().trim().is_empty());
    payload
}

#[tokio::test]
#[ignore = "Live Claude inference uses existing authentication and may incur charges; explicit opt-in required"]
async fn claude_synthetic_smoke() {
    assert_eq!(
        std::env::var("ASIDE_LIVE_CLAUDE_SMOKE").as_deref(),
        Ok("1"),
        "Obtain operator approval before setting ASIDE_LIVE_CLAUDE_SMOKE=1",
    );
    let snapshot = synthetic_snapshot();
    let file = &snapshot.files[0];
    let mut selection = json!({
        "sessionId": "", "baseSha": snapshot.pr.base_sha, "headSha": snapshot.pr.head_sha,
        "fileId": file.id, "diffDigest": file.diff_digest,
        "hunkId": file.hunks[0].id, "lineId": file.hunks[0].lines[1].id,
    });
    let session = Session::new(
        snapshot,
        crate::store::Store::open_in_memory().unwrap(),
        None,
        None,
    )
    .unwrap();
    selection["sessionId"] = json!(session.id);
    let before = serde_json::to_value(session.view(crate::model::Actor::Human).unwrap()).unwrap();
    let mut state = AppState::new(
        session.clone(),
        "synthetic-ui".into(),
        "synthetic-agent".into(),
    );
    // In-memory opt-in only: no user configuration or credential files are read by the test.
    // The actual Claude adapter still delegates authentication to the installed CLI.
    state.explanations = Arc::new(Explanations::new(AiConfig {
        claude_enabled: true,
        claude_model: "sonnet".into(),
        ..Default::default()
    }));
    let app = router(state);
    let question = "Explain this change in plain English. Refer to the numeric values and old/new line numbers. Keep the answer under 180 words.";
    let first = ask_live(
        app.clone(),
        json!({
            "requestId": "live-smoke-first", "provider": "claude", "selection": selection,
            "question": question, "history": [],
        }),
    )
    .await;
    println!(
        "LIVE CLAUDE — INITIAL ANSWER\n{}",
        first["answer"].as_str().unwrap()
    );
    let followup = ask_live(app, json!({
        "requestId": "live-smoke-followup", "provider": "claude", "selection": selection,
        "question": "What two values were we discussing? Does the red highlighting itself prove a bug, and can you tell whether callers will be affected? Keep it under 100 words.",
        "history": [{"question": question, "answer": first["answer"]}],
    })).await;
    println!(
        "LIVE CLAUDE — FOLLOW-UP ANSWER\n{}",
        followup["answer"].as_str().unwrap()
    );
    let after = serde_json::to_value(session.view(crate::model::Actor::Human).unwrap()).unwrap();
    assert_eq!(before, after, "Live Q&A changed review state");
    // Semantic correctness is inspected by the operator; do not call mere text generation safety.
    println!("LIVE SMOKE: two real Claude answers; matching selection; review state unchanged.");
}
