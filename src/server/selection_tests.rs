//! `PUT /api/v1/review/publish/selection` (#13).

use super::*;
use crate::{
    model::{AnchorState, Comment, PublishState, Side, ThreadSource},
    store::{NewThread, Store},
};
use axum::http::{Method, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

const PATH: &str = "/api/v1/review/publish/selection";
const HUMAN: &str = "aside_ui=ui-secret";
const ORIGIN: &str = "http://127.0.0.1:1234";

/// The demo session plus one local draft, `c`, on its first commentable line.
fn app() -> (Router, Arc<Session>) {
    let session = Session::new(
        crate::demo::snapshot(0),
        Store::open_in_memory().unwrap(),
        None,
        None,
    )
    .unwrap();
    {
        let snapshot = session.snapshot.read().unwrap();
        let file = snapshot
            .files
            .iter()
            .find(|f| f.patch_complete && !f.hunks.is_empty())
            .unwrap();
        let line = file.hunks[0]
            .lines
            .iter()
            .find(|l| l.new_line.is_some())
            .unwrap();
        let mut store = session.store.lock().unwrap();
        let thread = NewThread {
            id: "t".into(),
            source: ThreadSource::Local,
            github_thread_id: None,
            path: file.path.clone(),
            side: Side::Right,
            line: line.new_line,
            start_line: None,
            line_text: Some(line.text.clone()),
            diff_digest: None,
            head_sha: snapshot.pr.head_sha.clone(),
            anchor_state: AnchorState::Current,
            anchor_reason: None,
            resolved: false,
            created_at: None,
        };
        store.insert_thread(&session.pr_id, &thread).unwrap();
        let draft = Comment {
            id: "c".into(),
            thread_id: "t".into(),
            author_kind: Actor::Human,
            author: "me".into(),
            body: "look here".into(),
            created_at: "c".into(),
            updated_at: "c".into(),
            publish_state: PublishState::Draft,
            github_comment_id: None,
        };
        store.insert_comment(&draft, None).unwrap();
    }
    let state = AppState::new(session.clone(), "ui-secret".into(), "agent-secret".into());
    (router(state), session)
}

async fn put(
    app: Router,
    credential: &str,
    origin: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut builder = axum::http::Request::builder()
        .method(Method::PUT)
        .uri(PATH)
        .header(header::HOST, "127.0.0.1:1234")
        .header(header::CONTENT_TYPE, "application/json");
    builder = if let Some(token) = credential.strip_prefix("Bearer ") {
        builder.header(header::AUTHORIZATION, format!("Bearer {token}"))
    } else {
        builder.header(header::COOKIE, credential)
    };
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    let response = app
        .oneshot(
            builder
                .body(axum::body::Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn the_human_can_hold_a_draft_back_and_select_it_again() {
    let (app, session) = app();
    let off = json!({"commentIds":["c"],"selected":false});
    let (status, body) = put(app.clone(), HUMAN, Some(ORIGIN), off).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["deselectedCommentIds"], json!(["c"]));
    let on = json!({"commentIds":["c"],"selected":true});
    let (status, body) = put(app, HUMAN, Some(ORIGIN), on).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["deselectedCommentIds"], json!([]));
    let store = session.store.lock().unwrap();
    assert!(
        store
            .deselected_comment_ids(&session.pr_id)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn agents_and_cross_origin_requests_cannot_change_the_selection() {
    let (app, session) = app();
    let off = json!({"commentIds":["c"],"selected":false});
    let (status, _) = put(app.clone(), "Bearer agent-secret", None, off.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = put(app, HUMAN, None, off).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let store = session.store.lock().unwrap();
    assert!(
        store
            .deselected_comment_ids(&session.pr_id)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn bad_selections_are_refused() {
    let (app, _) = app();
    let empty = json!({"commentIds":[],"selected":false});
    let (status, _) = put(app.clone(), HUMAN, Some(ORIGIN), empty).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let unknown = json!({"commentIds":["c","nope"],"selected":false});
    let (status, body) = put(app, HUMAN, Some(ORIGIN), unknown).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not_found");
}
