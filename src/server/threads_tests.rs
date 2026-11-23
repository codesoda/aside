use std::sync::Arc;

use axum::{
    Router,
    http::{Method, StatusCode, header},
};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::{
    demo,
    model::Side,
    server::{AppState, router},
    session::Session,
    store::Store,
};

fn fixture() -> (Arc<Session>, Router, String, String, u32) {
    let session = Session::new(
        demo::snapshot(0),
        Store::open_in_memory().unwrap(),
        None,
        None,
    )
    .unwrap();
    let (file, digest, line) = {
        let snap = session.snapshot.read().unwrap();
        let file = snap
            .files
            .iter()
            .find(|f| {
                f.patch_complete
                    && f.hunks
                        .iter()
                        .flat_map(|h| &h.lines)
                        .any(|l| l.new_line.is_some())
            })
            .unwrap();
        let line = file
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .find_map(|l| l.new_line)
            .unwrap();
        (file.id.clone(), file.diff_digest.clone(), line)
    };
    let app = router(AppState::new(
        Arc::clone(&session),
        "ui".into(),
        "agent".into(),
    ));
    (session, app, file, digest, line)
}

async fn call(
    app: &Router,
    method: Method,
    path: &str,
    body: Value,
    agent: bool,
) -> (StatusCode, Value) {
    let mut req = axum::http::Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "127.0.0.1:1234")
        .header(header::CONTENT_TYPE, "application/json");
    req = if agent {
        req.header(header::AUTHORIZATION, "Bearer agent")
    } else {
        req.header(header::COOKIE, "aside_ui=ui")
            .header(header::ORIGIN, "http://127.0.0.1:1234")
    };
    let response = app
        .clone()
        .oneshot(req.body(axum::body::Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1_048_576)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn file_conversation_is_private_and_agent_cannot_publish_or_resolve() {
    let (_session, app, file, digest, _) = fixture();
    let (status, created) = call(&app, Method::POST, "/api/v1/threads", json!({"fileId":file,"diffDigest":digest,"side":Side::Right,"subject":"file","body":"Please inspect"}), false).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let id = created["thread"]["id"].as_str().unwrap();
    assert_eq!(created["thread"]["line"], Value::Null);
    assert_eq!(created["thread"]["comments"][0]["publishState"], "private");
    let request = created["thread"]["comments"][0]["id"].as_str().unwrap();
    let path = format!("/api/v1/threads/{id}/replies");
    let view = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/v1/session")
                .header(header::HOST, "127.0.0.1:1234")
                .header(header::COOKIE, "aside_ui=ui")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let data = axum::body::to_bytes(view.into_body(), 1_048_576)
        .await
        .unwrap();
    let view: Value = serde_json::from_slice(&data).unwrap();
    assert_eq!(view["agentPresence"]["connected"], false);
    assert_eq!(
        view["threads"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == id)
            .unwrap()["agent"]["state"],
        "no_agent"
    );
    let (_, reply) = call(
        &app,
        Method::POST,
        &path,
        json!({"body":"Response","inReplyTo":request}),
        true,
    )
    .await;
    assert_eq!(reply["comment"]["publishState"], "private");
    let (status, _) = call(
        &app,
        Method::POST,
        &path,
        json!({"body":"Duplicate","inReplyTo":request}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = call(
        &app,
        Method::PUT,
        &format!("/api/v1/threads/{id}/review-comment"),
        json!({"body":"Publish me"}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &app,
        Method::PATCH,
        &format!("/api/v1/threads/{id}"),
        json!({"resolved":true}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn repeated_human_replies_remain_private() {
    let (session, app, file, digest, line) = fixture();
    let (_, created) = call(
        &app,
        Method::POST,
        "/api/v1/threads",
        json!({"fileId":file,"diffDigest":digest,"side":"RIGHT","line":line,"body":"First"}),
        false,
    )
    .await;
    let id = created["thread"]["id"].as_str().unwrap();
    let path = format!("/api/v1/threads/{id}/replies");
    let request = created["thread"]["comments"][0]["id"].as_str().unwrap();
    let (status, _) = call(&app, Method::POST, &path, json!({"body":"Too soon"}), false).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = call(
        &app,
        Method::POST,
        &path,
        json!({"body":"Agent reply","inReplyTo":request}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, response) = call(
        &app,
        Method::POST,
        &path,
        json!({"body":"Follow up"}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["comment"]["publishState"], "private");
    let thread = session
        .store
        .lock()
        .unwrap()
        .get_thread(&session.pr_id, id)
        .unwrap();
    assert_eq!(thread.comments.len(), 3);
    assert!(thread.review_comment.is_none());
}

#[tokio::test]
async fn agent_presence_distinguishes_recent_request_from_live_event_stream() {
    use tower::ServiceExt;
    let (_session, app, _, _, _) = fixture();
    let get = |path, agent| {
        let req = axum::http::Request::builder()
            .uri(path)
            .header(header::HOST, "127.0.0.1:1234");
        let req = if agent {
            req.header(header::AUTHORIZATION, "Bearer agent")
        } else {
            req.header(header::COOKIE, "aside_ui=ui")
        };
        req.body(axum::body::Body::empty()).unwrap()
    };
    let stream = app
        .clone()
        .oneshot(get("/api/v1/events", true))
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    let view = app
        .clone()
        .oneshot(get("/api/v1/session", false))
        .await
        .unwrap();
    let data = axum::body::to_bytes(view.into_body(), 1_048_576)
        .await
        .unwrap();
    let view: Value = serde_json::from_slice(&data).unwrap();
    assert_eq!(view["agentPresence"]["connected"], true);
    assert_eq!(view["agentPresence"]["activeStreams"], 1);
    assert_eq!(view["agentPresence"]["recentlySeen"], true);
    drop(stream);
    let view = app.oneshot(get("/api/v1/session", false)).await.unwrap();
    let data = axum::body::to_bytes(view.into_body(), 1_048_576)
        .await
        .unwrap();
    let view: Value = serde_json::from_slice(&data).unwrap();
    assert_eq!(view["agentPresence"]["connected"], false);
    assert_eq!(view["agentPresence"]["recentlySeen"], true);
}

#[tokio::test]
async fn insight_open_is_idempotent_without_fabricated_message_and_rejects_foreign_file() {
    use crate::analysis::{
        ProviderKind, SchedulerConfig,
        test_doubles::{Behaviour, DIFF, Double},
    };
    let session = Session::with_analysis(
        demo::snapshot(0),
        Store::open_in_memory().unwrap(),
        None,
        None,
        vec![Double::new(DIFF, ProviderKind::Diff, Behaviour::Fixture)],
        SchedulerConfig::default(),
    )
    .unwrap();
    for handle in session.start_analysis().unwrap() {
        handle.wait().await;
    }
    let finding = session
        .store
        .lock()
        .unwrap()
        .list_findings(&session.pr_id, true)
        .unwrap()
        .into_iter()
        .find(|f| f.finding.value().anchor.current_path.is_some())
        .unwrap();
    let anchor = &finding.finding.value().anchor;
    let (file_id, digest, other) = {
        let snapshot = session.snapshot.read().unwrap();
        let file = snapshot
            .files
            .iter()
            .find(|file| Some(file.path.as_str()) == anchor.current_path.as_deref())
            .unwrap();
        let other = snapshot
            .files
            .iter()
            .find(|f| f.id != file.id)
            .map(|f| (f.id.clone(), f.diff_digest.clone()));
        (file.id.clone(), file.diff_digest.clone(), other)
    };
    let app = router(AppState::new(
        Arc::clone(&session),
        "ui".into(),
        "agent".into(),
    ));
    let request = json!({"fileId":file_id,"diffDigest":digest,"side":"RIGHT","subject":"file","findingId":finding.finding.value().id.as_str()});
    let (status, first) = call(
        &app,
        Method::POST,
        "/api/v1/threads",
        request.clone(),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["thread"]["comments"].as_array().unwrap().len(), 0);
    let (status, second) = call(
        &app,
        Method::POST,
        "/api/v1/threads",
        request.clone(),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(first["thread"]["id"], second["thread"]["id"]);
    if let Some((id, digest)) = other {
        let mut foreign = request;
        foreign["fileId"] = json!(id);
        foreign["diffDigest"] = json!(digest);
        let (status, _) = call(&app, Method::POST, "/api/v1/threads", foreign, false).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn review_comment_requires_explicit_human_action_and_is_separate_from_messages() {
    let (session, app, file, digest, _) = fixture();
    let (_, created) = call(
        &app,
        Method::POST,
        "/api/v1/threads",
        json!({"fileId":file,"diffDigest":digest,"side":"RIGHT","subject":"file","body":"Private"}),
        false,
    )
    .await;
    let id = created["thread"]["id"].as_str().unwrap();
    let path = format!("/api/v1/threads/{id}/review-comment");
    let (status, response) =
        call(&app, Method::PUT, &path, json!({"body":"Outgoing"}), false).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["reviewComment"]["body"], "Outgoing");
    let thread = session
        .store
        .lock()
        .unwrap()
        .get_thread(&session.pr_id, id)
        .unwrap();
    assert_eq!(thread.comments.len(), 1);
    assert_eq!(
        thread.comments[0].publish_state,
        crate::model::PublishState::Private
    );
    let (status, _) = call(&app, Method::DELETE, &path, json!({}), false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        session
            .store
            .lock()
            .unwrap()
            .get_thread(&session.pr_id, id)
            .unwrap()
            .review_comment
            .is_none()
    );
}

#[tokio::test]
async fn failed_request_retries_only_by_human_and_stale_agent_claim_conflicts() {
    let (_session, app, file, digest, _) = fixture();
    let (_, created) = call(
        &app,
        Method::POST,
        "/api/v1/threads",
        json!({"fileId":file,"diffDigest":digest,"side":"RIGHT","subject":"file","body":"Help"}),
        false,
    )
    .await;
    let id = created["thread"]["id"].as_str().unwrap();
    let request = created["thread"]["comments"][0]["id"].as_str().unwrap();
    let response_path = format!("/api/v1/threads/{id}/agent-response");
    let (status, _) = call(
        &app,
        Method::POST,
        &response_path,
        json!({"requestId":request,"state":"failed","error":"unavailable"}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &app,
        Method::POST,
        &response_path,
        json!({"requestId":request,"state":"responding"}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let retry_path = format!("/api/v1/threads/{id}/agent-retry");
    let (status, _) = call(
        &app,
        Method::POST,
        &retry_path,
        json!({"requestId":request}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &app,
        Method::POST,
        &retry_path,
        json!({"requestId":request}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}
