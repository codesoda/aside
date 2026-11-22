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
