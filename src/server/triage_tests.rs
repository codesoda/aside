//! HTTP contract tests for local finding triage and promotion (#26).
use std::sync::Arc;

use axum::{
    Router,
    http::{Method, StatusCode, header},
};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::super::{AppState, router};
use crate::{
    analysis::{
        ProviderKind, SchedulerConfig,
        test_doubles::{Behaviour, DIFF, Double, FEEDBACK},
    },
    demo,
    github::{PrRef, fake::FakeGh},
    model::{Actor, RuleAction, RuleKind},
    session::Session,
    store::Store,
};

async fn fixture() -> (Arc<Session>, Router, Arc<FakeGh>) {
    fixture_on_store(Store::open_in_memory().unwrap()).await
}

async fn fixture_on_store(store: Store) -> (Arc<Session>, Router, Arc<FakeGh>) {
    let snapshot = demo::snapshot(0);
    let gh = Arc::new(FakeGh::default());
    let pr_ref = PrRef {
        owner: snapshot.pr.owner.clone(),
        repo: snapshot.pr.repo.clone(),
        number: snapshot.pr.number,
    };
    let session = Session::with_analysis(
        snapshot,
        store,
        Some(gh.clone()),
        Some(pr_ref),
        vec![
            Double::new(DIFF, ProviderKind::Diff, Behaviour::Fixture),
            Double::new(FEEDBACK, ProviderKind::Feedback, Behaviour::Fixture),
        ],
        SchedulerConfig::default(),
    )
    .unwrap();
    for run in session.start_analysis().unwrap() {
        run.wait().await;
    }
    let app = router(AppState::new(
        session.clone(),
        "ui-secret".into(),
        "agent-secret".into(),
    ));
    (session, app, gh)
}

async fn call(
    app: &Router,
    method: Method,
    path: &str,
    body: Option<Value>,
    browser: bool,
) -> (StatusCode, Value) {
    let mut request = axum::http::Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "127.0.0.1:1234");
    if browser {
        request = request
            .header(header::COOKIE, "aside_ui=ui-secret")
            .header(header::ORIGIN, "http://127.0.0.1:1234");
    } else {
        request = request.header(header::AUTHORIZATION, "Bearer agent-secret");
    }
    let bytes = body.map(|v| v.to_string()).unwrap_or_default();
    let response = app
        .clone()
        .oneshot(
            request
                .header(header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 10_000_000)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn findings(app: &Router, historical: bool) -> Vec<Value> {
    let path = if historical {
        "/api/v1/analysis/findings?includeHistorical=true&includeIgnored=true&limit=200"
    } else {
        "/api/v1/analysis/findings?limit=200"
    };
    let (status, page) = call(app, Method::GET, path, None, true).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    page["items"].as_array().unwrap().clone()
}

fn promotable(items: &[Value]) -> &Value {
    items
        .iter()
        .find(|item| item["canPromote"] == true)
        .expect("fixture has a promotable current finding")
}
fn id(item: &Value) -> &str {
    item["finding"]["id"].as_str().unwrap()
}
fn route(id: &str, action: &str) -> String {
    format!("/api/v1/analysis/findings/{id}/{action}")
}

#[tokio::test]
async fn browser_only_triage_and_stale_revision_return_authoritative_state() {
    let (session, app, gh) = fixture().await;
    let item = promotable(&findings(&app, false).await).clone();
    let path = route(id(&item), "state");
    let before = session.view(Actor::Human).unwrap();
    let input = json!({"expectedRevision":0,"disposition":"investigating"});
    let (status, _) = call(&app, Method::PATCH, &path, Some(input.clone()), false).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, response) = call(&app, Method::PATCH, &path, Some(input), true).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["state"]["disposition"], "investigating");
    assert_eq!(response["state"]["revision"], 1);
    assert!(response["sequence"].as_u64().is_some());
    let updated = findings(&app, false)
        .await
        .into_iter()
        .find(|i| id(i) == id(&item))
        .unwrap();
    assert_eq!(updated["userState"], response["state"]);
    assert_eq!(updated["disposition"], "investigating");
    let (status, stale) = call(
        &app,
        Method::PATCH,
        &path,
        Some(json!({"expectedRevision":0,"disposition":"dismissed_local"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    assert_eq!(stale["details"]["current"], response["state"]);
    assert_eq!(stale["details"]["currentRevision"], 1);
    let after = session.view(Actor::Human).unwrap();
    assert_eq!(after.progress, before.progress);
    assert_eq!(after.review, before.review);
    assert_eq!(after.threads, before.threads);
    assert!(gh.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn promotion_uses_server_anchor_and_retry_preserves_edited_draft_and_selection() {
    let (session, app, gh) = fixture().await;
    let item = promotable(&findings(&app, false).await).clone();
    let before = session.view(Actor::Human).unwrap();
    let path = route(id(&item), "promote");
    let input = json!({"expectedRevision":0,"operationId":"same-operation"});
    let injected = json!({"expectedRevision":0,"operationId":"injected", "anchor":{"kind":"pr","currentPath":"evil.rs"}});
    let (status, _) = call(&app, Method::POST, &path, Some(injected), true).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = call(&app, Method::POST, &path, Some(input.clone()), false).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, first) = call(&app, Method::POST, &path, Some(input.clone()), true).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["state"]["disposition"], "promoted_to_draft");
    assert_eq!(first["thread"]["findingId"], id(&item));
    assert_eq!(
        first["thread"]["path"],
        item["finding"]["anchor"]["currentPath"]
    );
    assert_eq!(
        first["thread"]["fileId"],
        item["finding"]["anchor"]["fileId"]
    );
    assert_eq!(first["thread"]["line"], item["finding"]["anchor"]["line"]);
    assert_eq!(first["thread"]["side"], item["finding"]["anchor"]["side"]);
    assert_eq!(first["draft"]["body"], item["finding"]["body"]);
    assert_eq!(first["draft"]["selected"], false);
    let (status, preview) = call(
        &app,
        Method::POST,
        "/api/v1/review/publish/preview",
        Some(json!({"verdict":"COMMENT","body":"Review summary"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert!(
        !preview["plan"]["inline"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line["commentId"] == first["draft"]["id"])
    );
    assert!(first["sequence"].as_u64().is_some());
    let draft_id = first["draft"]["id"].as_str().unwrap();
    let thread_id = first["thread"]["id"].as_str().unwrap();
    let (status, edited) = call(
        &app,
        Method::PUT,
        &format!("/api/v1/threads/{thread_id}/review-comment"),
        Some(json!({"body":"Human-edited exact body"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    let (status, selection) = call(
        &app,
        Method::POST,
        "/api/v1/review/publish/selection",
        Some(json!({"commentIds":[draft_id],"selected":true})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{selection}");
    let (status, preview) = call(
        &app,
        Method::POST,
        "/api/v1/review/publish/preview",
        Some(json!({"verdict":"COMMENT","body":"Review summary"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(
        preview["plan"]["inline"]
            .as_array()
            .unwrap()
            .iter()
            .find(|line| line["commentId"] == draft_id)
            .unwrap()["body"],
        "Human-edited exact body"
    );
    for confirmation in [
        json!({"planId":preview["plan"]["planId"],"verdict":"COMMENT","body":"Review summary","confirm":false}),
        json!({"planId":preview["plan"]["planId"],"verdict":"COMMENT","body":"Review summary"}),
    ] {
        let (status, _) = call(
            &app,
            Method::POST,
            "/api/v1/review/publish",
            Some(confirmation),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    assert!(gh.calls.lock().unwrap().is_empty());
    let (status, retry) = call(&app, Method::POST, &path, Some(input.clone()), true).await;
    assert_eq!(status, StatusCode::OK, "{retry}");
    assert_eq!(retry["draft"]["id"], draft_id);
    assert_eq!(retry["draft"]["body"], "Human-edited exact body");
    assert_eq!(retry["draft"]["selected"], true);
    let after = session.view(Actor::Human).unwrap();
    assert_eq!(after.progress, before.progress);
    assert_eq!(after.review.verdict, before.review.verdict);
    assert_eq!(after.review.summary, before.review.summary);
    session
        .store
        .lock()
        .unwrap()
        .create_rule(
            &session.repository.id,
            RuleAction::Exclude,
            RuleKind::File,
            item["finding"]["anchor"]["currentPath"].as_str().unwrap(),
            None,
            Actor::Human,
        )
        .unwrap();
    let (status, ignored_retry) = call(&app, Method::POST, &path, Some(input), true).await;
    assert_eq!(status, StatusCode::OK, "{ignored_retry}");
    assert_eq!(ignored_retry["draft"]["id"], draft_id);
    assert_eq!(ignored_retry["draft"]["body"], "Human-edited exact body");
    let (status, _) = call(
        &app,
        Method::POST,
        &path,
        Some(json!({"expectedRevision":1,"operationId":"never-seen"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(gh.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn promotion_rejects_ignored_native_and_invalid_anchors() {
    let (session, app, gh) = fixture().await;
    let all = findings(&app, true).await;
    let native = all
        .iter()
        .find(|i| {
            i["finding"]["source"]["kind"]
                .as_str()
                .is_some_and(|s| s.starts_with("github_"))
        })
        .unwrap();
    assert_eq!(native["canPromote"], false);
    let historical = all
        .iter()
        .find(|i| i["label"] == "historical")
        .expect("fixture contains outdated findings");
    assert_eq!(historical["canPromote"], false);
    let (status, _) = call(
        &app,
        Method::POST,
        &route(id(historical), "promote"),
        Some(json!({"expectedRevision":0,"operationId":"historical"})),
        true,
    )
    .await;
    assert_ne!(status, StatusCode::OK);
    let invalid = all.iter().find(|i| {
        i["finding"]["source"]["kind"] == "aside_static" && i["finding"]["anchor"]["kind"] == "file"
    });
    if let Some(invalid) = invalid {
        assert_eq!(invalid["canPromote"], false);
        let (status, _) = call(
            &app,
            Method::POST,
            &route(id(invalid), "promote"),
            Some(json!({"expectedRevision":0,"operationId":"invalid-anchor"})),
            true,
        )
        .await;
        assert_ne!(status, StatusCode::OK);
    }
    let (status, _) = call(
        &app,
        Method::POST,
        &route(id(native), "promote"),
        Some(json!({"expectedRevision":0,"operationId":"native"})),
        true,
    )
    .await;
    assert_ne!(status, StatusCode::OK);
    let item = promotable(&all);
    let path = item["finding"]["anchor"]["currentPath"].as_str().unwrap();
    session
        .store
        .lock()
        .unwrap()
        .create_rule(
            &session.repository.id,
            RuleAction::Exclude,
            RuleKind::File,
            path,
            None,
            Actor::Human,
        )
        .unwrap();
    let ignored = findings(&app, true)
        .await
        .into_iter()
        .find(|i| id(i) == id(item))
        .unwrap();
    assert_eq!(ignored["canPromote"], false);
    let (status, _) = call(
        &app,
        Method::POST,
        &route(id(item), "promote"),
        Some(json!({"expectedRevision":0,"operationId":"ignored"})),
        true,
    )
    .await;
    assert_ne!(status, StatusCode::OK);
    assert!(gh.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn other_pr_cannot_read_or_promote_a_real_finding() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("cross-pr.db");
    let (_, first_app, _) = fixture_on_store(Store::open(&db).unwrap()).await;
    let first = promotable(&findings(&first_app, false).await).clone();
    let foreign_id = id(&first);

    let mut snapshot = demo::snapshot(0);
    snapshot.pr.number += 1;
    snapshot.pr.url = format!(
        "https://github.com/{}/{}/pull/{}",
        snapshot.pr.owner, snapshot.pr.repo, snapshot.pr.number
    );
    let second = Session::with_analysis(
        snapshot,
        Store::open(&db).unwrap(),
        None,
        None,
        vec![],
        SchedulerConfig::default(),
    )
    .unwrap();
    let second_app = router(AppState::new(
        second,
        "ui-secret".into(),
        "agent-secret".into(),
    ));
    assert!(
        !findings(&second_app, true)
            .await
            .iter()
            .any(|item| id(item) == foreign_id)
    );
    for action in ["state", "promote"] {
        let body = if action == "state" {
            json!({"expectedRevision":0,"disposition":"investigating"})
        } else {
            json!({"expectedRevision":0,"operationId":"foreign-pr"})
        };
        let method = if action == "state" {
            Method::PATCH
        } else {
            Method::POST
        };
        let (status, response) = call(
            &second_app,
            method,
            &route(foreign_id, action),
            Some(body),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{action}: {response}");
    }
}
