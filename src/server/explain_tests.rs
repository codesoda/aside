//! Authenticated API tests with synthetic model responses, never live inference.
use super::*;
use crate::{config::AiConfig, explain::Explanations, model::LineKind};
use axum::{
    body::Body,
    http::{Method, Request, header},
};
use tower::ServiceExt;

fn fixture() -> (AppState, Value) {
    let session = Session::new(
        crate::demo::snapshot(0),
        crate::store::Store::open_in_memory().unwrap(),
        None,
        None,
    )
    .unwrap();
    let selection = {
        let snapshot = session.snapshot.read().unwrap();
        let file = snapshot
            .files
            .iter()
            .find(|f| f.patch_complete && !f.hunks.is_empty())
            .unwrap();
        let hunk = file
            .hunks
            .iter()
            .find(|h| {
                h.lines
                    .iter()
                    .any(|l| matches!(l.kind, LineKind::Addition | LineKind::Deletion))
            })
            .unwrap();
        let line = hunk
            .lines
            .iter()
            .find(|l| matches!(l.kind, LineKind::Addition | LineKind::Deletion))
            .unwrap();
        json!({"sessionId": session.id, "baseSha": snapshot.pr.base_sha, "headSha": snapshot.pr.head_sha,
            "fileId": file.id, "diffDigest": file.diff_digest, "hunkId": hunk.id, "lineId": line.id})
    };
    let state = AppState::new(session, "ui-test".into(), "agent-test".into());
    let body = json!({"requestId":"test-request-123", "provider":"local", "selection":selection, "question":"What changed?", "history":[]});
    (state, body)
}

async fn request(
    app: Router,
    path: &str,
    credential: &str,
    origin: Option<&str>,
    body: Value,
) -> Response {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(header::HOST, "127.0.0.1:1234")
        .header(header::CONTENT_TYPE, "application/json");
    builder = if credential.starts_with("Bearer") {
        builder.header(header::AUTHORIZATION, credential)
    } else {
        builder.header(header::COOKIE, credential)
    };
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    app.oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}
async fn payload(response: Response) -> Value {
    serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 100_000)
            .await
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn explanations_require_human_and_matching_origin_and_never_mutate_review() {
    let (state, body) = fixture();
    let before =
        serde_json::to_value(state.session.view(crate::model::Actor::Human).unwrap()).unwrap();
    let app = router(state.clone());
    for (credential, origin, expected) in [
        ("", None, StatusCode::UNAUTHORIZED),
        ("Bearer agent-test", None, StatusCode::FORBIDDEN),
        ("aside_ui=ui-test", None, StatusCode::FORBIDDEN),
        (
            "aside_ui=ui-test",
            Some("https://other.example"),
            StatusCode::FORBIDDEN,
        ),
    ] {
        assert_eq!(
            request(
                app.clone(),
                "/api/v1/explain/questions",
                credential,
                origin,
                body.clone()
            )
            .await
            .status(),
            expected
        );
        assert_eq!(
            request(
                app.clone(),
                "/api/v1/explain/questions/test-request-123/cancel",
                credential,
                origin,
                json!({})
            )
            .await
            .status(),
            expected
        );
    }
    let mut stale = body.clone();
    stale["selection"]["headSha"] = json!("stale");
    let response = request(
        app.clone(),
        "/api/v1/explain/questions",
        "aside_ui=ui-test",
        Some("http://127.0.0.1:1234"),
        stale,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(payload(response).await["error"], "stale_selection");
    let response = request(
        app,
        "/api/v1/explain/questions",
        "aside_ui=ui-test",
        Some("http://127.0.0.1:1234"),
        body,
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "unconfigured provider must fail without dispatch"
    );
    let after =
        serde_json::to_value(state.session.view(crate::model::Actor::Human).unwrap()).unwrap();
    assert_eq!(
        before, after,
        "Q&A cannot change review, draft, progress or analysis state"
    );
}

/// Each child gets a missing config and a PATH containing only a counting fake CLI.
/// Default configuration rejects all three provider requests; the fake CLI
/// call count stays zero. This does not establish endpoint reachability.
#[cfg(unix)]
#[tokio::test]
async fn default_explain_paths_do_not_invoke_cli_with_or_without_credentials() {
    const CHILD: &str = "ASIDE_EXPLAIN_DEFAULT_CHILD";
    const TEST: &str = "server::explain_tests::default_explain_paths_do_not_invoke_cli_with_or_without_credentials";
    if let Ok(expected) = std::env::var(CHILD) {
        let configured = crate::config::load().unwrap();
        assert_eq!(configured.ai, AiConfig::default());
        assert_eq!(
            std::env::var_os("ANTHROPIC_API_KEY").is_some(),
            expected == "present"
        );
        assert_eq!(
            std::env::var_os("OPENAI_API_KEY").is_some(),
            expected == "present"
        );
        let (state, body) = fixture();
        assert_eq!(state.explanations.config, configured.ai);
        let app = router(state);
        let discovery = Request::builder()
            .uri("/api/v1/explain/providers")
            .header(header::HOST, "127.0.0.1:1234")
            .header(header::COOKIE, "aside_ui=ui-test")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(discovery).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let listed = payload(response).await;
        let providers = listed["providers"].as_array().unwrap();
        assert_eq!(providers.len(), 3);
        for (id, available) in [("local", false), ("claude", false), ("codex", false)] {
            let provider = providers.iter().find(|p| p["id"] == id).unwrap();
            assert_eq!(provider["available"], available, "{id}");
        }
        for provider in ["local", "claude", "codex"] {
            let mut question = body.clone();
            question["provider"] = json!(provider);
            let response = request(
                app.clone(),
                "/api/v1/explain/questions",
                "aside_ui=ui-test",
                Some("http://127.0.0.1:1234"),
                question,
            )
            .await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{provider}");
            assert_eq!(
                payload(response).await["error"],
                "invalid_explanation",
                "{provider}"
            );
        }
        return;
    }

    use std::{os::unix::fs::PermissionsExt, process::Command};
    for credentials in ["absent", "present"] {
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join("claude");
        let calls = dir.path().join("cli-calls");
        std::fs::write(
            &cli,
            "#!/bin/sh\nprintf 'called\\n' >> \"$ASIDE_FAKE_CLI_CALLS\"\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .arg("--exact")
            .arg(TEST)
            .arg("--nocapture")
            .env_clear()
            .env("PATH", dir.path())
            .env("HOME", dir.path())
            .env("ASIDE_CONFIG_DIR", dir.path())
            .env("ASIDE_FAKE_CLI_CALLS", &calls)
            .env(CHILD, credentials);
        if credentials == "present" {
            child
                .env("ANTHROPIC_API_KEY", "synthetic-not-a-secret")
                .env("OPENAI_API_KEY", "synthetic-not-a-secret");
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{credentials}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "child test was not selected: {credentials}"
        );
        assert!(
            !calls.exists(),
            "default Explain invoked the fake CLI with {credentials} credentials"
        );
    }
}

#[tokio::test]
async fn complete_http_flow_and_refresh_during_inference_are_revision_bound() {
    use axum::routing::post;
    use std::sync::atomic::{AtomicBool, Ordering};
    let (mut state, body) = fixture();
    let change_revision = Arc::new(AtomicBool::new(false));
    let change = change_revision.clone();
    let session = state.session.clone();
    let captures = Arc::new(std::sync::Mutex::new(vec![]));
    let recorded = captures.clone();
    let provider = Router::new()
        .route("/api/show", post(|| async { Json(json!({"model_info":{"general.architecture":"llama"}})) }))
        .route("/api/chat", post(move |Json(body): Json<Value>| async move {
            recorded.lock().unwrap().push(body);
            if change.load(Ordering::SeqCst) { session.snapshot.write().unwrap().pr.head_sha = "new-head".into(); }
            Json(json!({"done":true,"message":{"content":"A synthetic explanation, not a safety verdict."}}))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    state.explanations = Arc::new(Explanations::new(AiConfig {
        local_url: format!("http://{}", listener.local_addr().unwrap()),
        local_model: "test-model".into(),
        ..Default::default()
    }));
    let task = tokio::spawn(async { axum::serve(listener, provider).await.unwrap() });
    let app = router(state);
    let response = request(
        app.clone(),
        "/api/v1/explain/questions",
        "aside_ui=ui-test",
        Some("http://127.0.0.1:1234"),
        body.clone(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let answer = payload(response).await;
    assert_eq!(answer["selection"], body["selection"]);
    assert_eq!(answer["requestId"], body["requestId"]);
    assert_eq!(answer["provider"], "local");
    let mut followup = body.clone();
    followup["requestId"] = json!("test-request-456");
    followup["question"] = json!("What should I check?");
    followup["history"] = json!([{"question":body["question"],"answer":answer["answer"]}]);
    let response = request(
        app.clone(),
        "/api/v1/explain/questions",
        "aside_ui=ui-test",
        Some("http://127.0.0.1:1234"),
        followup,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    {
        let captures = captures.lock().unwrap();
        let first: Value =
            serde_json::from_str(captures[0]["messages"][1]["content"].as_str().unwrap()).unwrap();
        let second: Value =
            serde_json::from_str(captures[1]["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(first["context"], second["context"]);
        assert_eq!(second["conversation"][0]["answer"], answer["answer"]);
    }
    change_revision.store(true, Ordering::SeqCst);
    let response = request(
        app,
        "/api/v1/explain/questions",
        "aside_ui=ui-test",
        Some("http://127.0.0.1:1234"),
        body,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(payload(response).await["error"], "stale_selection");
    task.abort();
}
