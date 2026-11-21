//! Read-only analysis API (#33). Sessions replay the #17 offline fixture.

use std::sync::Arc;

use anyhow::Result;
use axum::{
    Router,
    http::{Method, StatusCode, header},
};
use futures_util::{FutureExt, future::BoxFuture};
use serde_json::Value;

use super::{MAX_PAGE_SIZE, page_size};
use crate::{
    analysis::{
        AnalysisProvider, ProviderContext, ProviderDescriptor, ProviderKind, ProviderOutput,
        SchedulerConfig,
        test_doubles::{Behaviour, DIFF, Double, fixture_output},
    },
    demo,
    model::{
        Actor, AnalysisCoverage, AnalysisMode, Completeness, FindingId, RuleAction, RuleKind,
        RunStatus,
    },
    server::{AppState, router},
    session::Session,
    store::{AnalysisBatch, Raw, Store},
};

const AGENT: &str = "Bearer agent-secret";
const BROWSER: &str = "aside_ui=ui-secret";

/// A provider that returns a fixed status and coverage with no findings.
struct Empty {
    descriptor: ProviderDescriptor,
    status: RunStatus,
    completeness: Completeness,
}

impl Empty {
    fn new(status: RunStatus, completeness: Completeness) -> Arc<Self> {
        Arc::new(Self {
            descriptor: ProviderDescriptor {
                id: DIFF.into(),
                kind: ProviderKind::Diff,
                engine_version: "test-1".into(),
                configuration_digest: "cfg".into(),
                mode: AnalysisMode::Static,
                limits: None,
            },
            status,
            completeness,
        })
    }
}

impl AnalysisProvider for Empty {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn run(&self, ctx: ProviderContext) -> BoxFuture<'static, Result<ProviderOutput>> {
        let (status, completeness) = (self.status, self.completeness);
        async move {
            let mut out = fixture_output(DIFF, &ctx)?;
            out.findings.clear();
            out.evidence.clear();
            out.status = status;
            out.coverage = AnalysisCoverage {
                completeness,
                ..out.coverage
            };
            Ok(out)
        }
        .boxed()
    }
}

async fn session(providers: Vec<Arc<dyn AnalysisProvider>>) -> Arc<Session> {
    let session = Session::with_analysis(
        demo::snapshot(0),
        Store::open_in_memory().unwrap(),
        None,
        None,
        providers,
        SchedulerConfig::default(),
    )
    .unwrap();
    for handle in session.start_analysis().unwrap() {
        handle.wait().await;
    }
    session
}

async fn fixture_session() -> Arc<Session> {
    session(vec![Double::new(
        DIFF,
        ProviderKind::Diff,
        Behaviour::Fixture,
    )])
    .await
}

fn app(session: &Arc<Session>) -> Router {
    router(AppState::new(
        Arc::clone(session),
        "ui-secret".into(),
        "agent-secret".into(),
    ))
}

async fn get(app: &Router, path: &str, credential: Option<&str>) -> (StatusCode, Value) {
    use tower::ServiceExt;
    let mut builder = axum::http::Request::builder()
        .method(Method::GET)
        .uri(path)
        .header(header::HOST, "127.0.0.1:1234");
    if let Some(value) = credential {
        builder = builder.header(
            if value.starts_with("Bearer ") {
                header::AUTHORIZATION
            } else {
                header::COOKIE
            },
            value,
        );
    }
    let response = app
        .clone()
        .oneshot(builder.body(axum::body::Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 10_000_000)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

async fn ok(app: &Router, path: &str) -> Value {
    let (status, body) = get(app, path, Some(AGENT)).await;
    assert_eq!(status, StatusCode::OK, "{path}: {body}");
    body
}

/// Some finding ID of the fixture session, with its path.
async fn some_finding(app: &Router) -> (String, String) {
    let page = ok(app, "/api/v1/analysis/findings?limit=200").await;
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|i| {
            let path = i["finding"]["anchor"]["currentPath"].as_str()?;
            Some((i["finding"]["id"].as_str()?.to_string(), path.to_string()))
        })
        .expect("a fixture finding with a path")
}

async fn routes(app: &Router) -> Vec<String> {
    let (id, _) = some_finding(app).await;
    vec![
        "/api/v1/analysis/summary".into(),
        "/api/v1/analysis/queue".into(),
        "/api/v1/analysis/runs".into(),
        "/api/v1/analysis/findings".into(),
        format!("/api/v1/analysis/findings/{id}/evidence"),
        "/api/v1/github-feedback/threads".into(),
    ]
}

#[tokio::test]
async fn every_route_rejects_missing_and_wrong_credentials() {
    let session = fixture_session().await;
    let app = app(&session);
    for path in routes(&app).await {
        for credential in [
            None,
            Some("Bearer wrong"),
            Some("aside_ui=wrong"),
            // The UI token is not an agent bearer token.
            Some("Bearer ui-secret"),
        ] {
            let (status, body) = get(&app, &path, credential).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{path} {credential:?}");
            assert_eq!(body["error"], "unauthorized");
            assert!(body.get("items").is_none());
        }
    }
}

#[tokio::test]
async fn agent_and_browser_get_identical_responses() {
    let session = fixture_session().await;
    let app = app(&session);
    for path in routes(&app).await {
        for path in [path.clone(), format!("{path}?limit=1")] {
            let (a, agent) = get(&app, &path, Some(AGENT)).await;
            let (b, browser) = get(&app, &path, Some(BROWSER)).await;
            assert_eq!((a, b), (StatusCode::OK, StatusCode::OK), "{path}");
            assert_eq!(agent, browser, "{path}");
        }
    }
}

/// Walks every page of `path` with the given page size.
async fn walk(app: &Router, path: &str, limit: usize) -> (Vec<Value>, usize) {
    let sep = if path.contains('?') { '&' } else { '?' };
    let mut url = format!("{path}{sep}limit={limit}");
    let mut items = Vec::new();
    let mut pages = 0;
    loop {
        let page = ok(app, &url).await;
        pages += 1;
        let batch = page["items"].as_array().unwrap();
        assert!(batch.len() <= limit);
        items.extend(batch.iter().cloned());
        match page["nextCursor"].as_str() {
            Some(cursor) => {
                assert_eq!(page["hasMore"], true);
                url = format!("{path}{sep}limit={limit}&cursor={cursor}");
            }
            None => {
                assert_eq!(page["hasMore"], false);
                assert_eq!(page["total"].as_u64().unwrap() as usize, items.len());
                return (items, pages);
            }
        }
    }
}

#[tokio::test]
async fn pagination_returns_every_item_exactly_once() {
    let session = fixture_session().await;
    let app = app(&session);
    for (path, id) in [
        ("/api/v1/analysis/findings", "/finding/id"),
        (
            "/api/v1/analysis/findings?includeHistorical=true",
            "/finding/id",
        ),
        ("/api/v1/analysis/runs", "/id"),
        ("/api/v1/github-feedback/threads", "/id"),
    ] {
        let (all, _) = walk(&app, path, MAX_PAGE_SIZE).await;
        assert!(all.len() >= 3, "{path}: need several pages to test");
        let expected: Vec<&str> = all
            .iter()
            .map(|i| i.pointer(id).unwrap().as_str().unwrap())
            .collect();
        let mut unique = expected.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), expected.len(), "{path}: duplicate ids");
        for limit in [1, 2] {
            let (paged, pages) = walk(&app, path, limit).await;
            assert!(pages >= 2, "{path}");
            let got: Vec<&str> = paged
                .iter()
                .map(|i| i.pointer(id).unwrap().as_str().unwrap())
                .collect();
            assert_eq!(got, expected, "{path} limit={limit}");
        }
    }
}

#[test]
fn absurd_page_sizes_are_clamped() {
    assert_eq!(page_size(None).unwrap(), super::DEFAULT_PAGE_SIZE);
    assert_eq!(page_size(Some("0")).unwrap(), 1);
    assert_eq!(page_size(Some("1000000")).unwrap(), MAX_PAGE_SIZE);
    assert_eq!(
        page_size(Some("99999999999999999999999999999")).unwrap(),
        MAX_PAGE_SIZE
    );
    assert!(page_size(Some("-1")).is_err());
    assert!(page_size(Some("ten")).is_err());
}

#[tokio::test]
async fn absurd_limit_over_http_is_clamped_not_trusted() {
    let session = fixture_session().await;
    let app = app(&session);
    let page = ok(
        &app,
        "/api/v1/analysis/findings?limit=99999999999999999999999",
    )
    .await;
    assert_eq!(page["limit"], MAX_PAGE_SIZE);
    let (status, body) = get(&app, "/api/v1/analysis/findings?limit=-5", Some(AGENT)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid_request");
}

fn assert_recoverable_conflict(status: StatusCode, body: &Value, reason: &str) {
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "cursor_stale");
    assert_eq!(body["details"]["retryable"], true);
    assert_eq!(body["details"]["reason"], reason);
    assert_eq!(body["details"]["resync"]["reload"], "/api/v1/session");
    assert!(body["details"]["snapshotId"].is_string());
    assert!(body["details"]["expectedSnapshotId"].is_string());
}

#[tokio::test]
async fn stale_cursor_after_snapshot_change_is_a_recoverable_conflict() {
    let session = fixture_session().await;
    let app = app(&session);
    let first = ok(&app, "/api/v1/analysis/findings?limit=1").await;
    let cursor = first["nextCursor"].as_str().unwrap().to_string();
    let pinned = first["snapshotId"].as_str().unwrap().to_string();
    session.refresh().await.unwrap();
    let (status, body) = get(
        &app,
        &format!("/api/v1/analysis/findings?limit=1&cursor={cursor}"),
        Some(AGENT),
    )
    .await;
    assert_recoverable_conflict(status, &body, "snapshot_changed");
    assert_eq!(body["details"]["expectedSnapshotId"], pinned);
    assert_ne!(body["details"]["snapshotId"], pinned);
    // An explicit snapshot pin behaves the same.
    let (status, body) = get(
        &app,
        &format!("/api/v1/analysis/runs?snapshotId={pinned}"),
        Some(AGENT),
    )
    .await;
    assert_recoverable_conflict(status, &body, "snapshot_changed");
    // Resync recovery: restarting without a cursor succeeds.
    ok(&app, "/api/v1/analysis/findings?limit=1").await;
}

#[tokio::test]
async fn cursor_over_a_changed_result_set_is_a_recoverable_conflict() {
    let session = fixture_session().await;
    let app = app(&session);
    let (_, path) = some_finding(&app).await;
    let first = ok(&app, "/api/v1/analysis/findings?limit=1").await;
    let cursor = first["nextCursor"].as_str().unwrap().to_string();
    // Same snapshot, different visible set: an exclusion removes items.
    session
        .store
        .lock()
        .unwrap()
        .create_rule(
            &session.repository.id,
            RuleAction::Exclude,
            RuleKind::File,
            &path,
            None,
            Actor::Human,
        )
        .unwrap();
    let (status, body) = get(
        &app,
        &format!("/api/v1/analysis/findings?limit=1&cursor={cursor}"),
        Some(AGENT),
    )
    .await;
    assert_recoverable_conflict(status, &body, "results_changed");
}

#[tokio::test]
async fn cursors_are_bound_to_their_query_and_tamper_is_rejected() {
    let session = fixture_session().await;
    let app = app(&session);
    let first = ok(&app, "/api/v1/analysis/findings?limit=1").await;
    let cursor = first["nextCursor"].as_str().unwrap();
    for path in [
        format!("/api/v1/analysis/runs?cursor={cursor}"),
        format!("/api/v1/analysis/findings?includeHistorical=true&cursor={cursor}"),
        "/api/v1/analysis/findings?cursor=zz".to_string(),
        "/api/v1/analysis/findings?cursor=7b7d".to_string(),
    ] {
        let (status, body) = get(&app, &path, Some(AGENT)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(body["error"], "invalid_cursor", "{path}");
    }
}

/// Appends one finding through the store — the same write path a provider's
/// scheduler commit uses — to simulate a live run landing mid-walk (#64).
fn append_finding(session: &Arc<Session>, suffix: &str) {
    let mut store = session.store.lock().unwrap();
    let run = store.list_analysis_runs(&session.pr_id).unwrap().remove(0);
    let mut all = store.list_findings(&session.pr_id, true).unwrap();
    let mut clone = all.remove(0).finding.value().clone();
    clone.id = FindingId::new(format!("append_{suffix}"));
    // Later than every fixture finding, so the append lands at the tail.
    clone.first_seen_at = "2999-01-01T00:00:00Z".into();
    clone.last_seen_at = clone.first_seen_at.clone();
    clone.source.run_id = Some(run.value().id.clone());
    let raw = Raw::new(clone).unwrap();
    store
        .commit_analysis_batch(&AnalysisBatch {
            run,
            findings: vec![raw],
            evidence: Vec::new(),
            relations: Vec::new(),
        })
        .unwrap();
}

#[tokio::test]
async fn appended_findings_do_not_invalidate_cursors_and_are_served_later() {
    let session = fixture_session().await;
    let app = app(&session);
    let before = ok(&app, "/api/v1/analysis/findings?limit=200").await;
    let total_before = before["total"].as_u64().unwrap();
    let first = ok(&app, "/api/v1/analysis/findings?limit=1").await;
    let cursor = first["nextCursor"].as_str().unwrap().to_string();
    let first_id = first["items"][0]["finding"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // A live run commits one finding between the two pages.
    append_finding(&session, "mid_walk");

    // The outstanding cursor survives the append: no 409, and the page
    // continues in stable order instead of restarting or shifting.
    let second = ok(
        &app,
        &format!("/api/v1/analysis/findings?limit=1&cursor={cursor}"),
    )
    .await;
    let second_id = second["items"][0]["finding"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(second_id, first_id, "page two must not repeat page one");
    let ids_before: Vec<&str> = before["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["finding"]["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids_before.iter().position(|id| *id == second_id),
        Some(1),
        "the second page continues the pre-append order"
    );

    // Walking to the end picks up the appended finding exactly once: no
    // duplicates, no drops, under concurrent insertion.
    let mut seen = vec![first_id, second_id];
    let mut cursor = second["nextCursor"].as_str().unwrap().to_string();
    loop {
        let page = ok(
            &app,
            &format!("/api/v1/analysis/findings?limit=1&cursor={cursor}"),
        )
        .await;
        for item in page["items"].as_array().unwrap() {
            seen.push(item["finding"]["id"].as_str().unwrap().to_string());
        }
        match page["nextCursor"].as_str() {
            Some(next) => cursor = next.to_string(),
            None => break,
        }
    }
    let after = ok(&app, "/api/v1/analysis/findings?limit=200").await;
    assert_eq!(after["total"].as_u64().unwrap(), total_before + 1);
    let expected: Vec<String> = after["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["finding"]["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(seen, expected, "the walk returned every item exactly once");
}

#[tokio::test]
async fn cursor_issued_before_an_append_picks_up_the_new_items() {
    let session = fixture_session().await;
    let app = app(&session);
    let before = ok(&app, "/api/v1/analysis/findings?limit=200").await;
    let total = before["total"].as_u64().unwrap();
    // A cursor is only issued when a page stops short of the end, so the
    // deepest cursor resumes at the one pre-append item it had not served.
    // The append must land after that, not invalidate anything.
    let page = ok(
        &app,
        &format!("/api/v1/analysis/findings?limit={}", total - 1),
    )
    .await;
    assert!(page["hasMore"].as_bool().unwrap());
    let unserved = before["items"].as_array().unwrap().last().unwrap()["finding"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let cursor = page["nextCursor"].as_str().unwrap().to_string();

    append_finding(&session, "tail");

    let next = ok(
        &app,
        &format!("/api/v1/analysis/findings?limit=50&cursor={cursor}"),
    )
    .await;
    assert!(!next["hasMore"].as_bool().unwrap());
    let served: Vec<&str> = next["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["finding"]["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        served,
        vec![unserved.as_str(), "append_tail"],
        "the deep cursor serves the one item it owed and then the append"
    );
}

#[tokio::test]
async fn a_cursor_whose_key_vanished_is_a_recoverable_conflict() {
    let session = fixture_session().await;
    let app = app(&session);
    let first = ok(&app, "/api/v1/analysis/findings?limit=1").await;
    let cursor = first["nextCursor"].as_str().unwrap().to_string();
    // A structurally valid cursor (right version, query and snapshot) whose
    // key is not in the list: the paged region itself changed.
    let mut decoded: serde_json::Value =
        serde_json::from_slice(&hex::decode(cursor).unwrap()).unwrap();
    decoded["last"] = "not-a-key-anywhere".into();
    let forged = hex::encode(serde_json::to_vec(&decoded).unwrap());
    let (status, body) = get(
        &app,
        &format!("/api/v1/analysis/findings?limit=1&cursor={forged}"),
        Some(AGENT),
    )
    .await;
    assert_recoverable_conflict(status, &body, "results_changed");
}

#[tokio::test]
async fn out_of_scope_findings_are_filtered_on_the_server() {
    let session = fixture_session().await;
    let app = app(&session);
    let (id, path) = some_finding(&app).await;
    let before = ok(&app, "/api/v1/analysis/findings?limit=200").await;
    session
        .store
        .lock()
        .unwrap()
        .create_rule(
            &session.repository.id,
            RuleAction::Exclude,
            RuleKind::File,
            &path,
            None,
            Actor::Human,
        )
        .unwrap();
    // Omitting the filter is the filtered view, not "everything".
    let after = ok(&app, "/api/v1/analysis/findings?limit=200").await;
    let paths = |page: &Value| -> Vec<String> {
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|i| {
                i["finding"]["anchor"]["currentPath"]
                    .as_str()
                    .map(String::from)
            })
            .collect()
    };
    assert!(!paths(&after).contains(&path));
    let excluded = after["excludedByScope"].as_u64().unwrap();
    assert!(excluded >= 1);
    assert_eq!(
        after["total"].as_u64().unwrap() + excluded,
        before["total"].as_u64().unwrap()
    );
    // Evidence of an excluded finding is not reachable without opting in.
    let (status, _) = get(
        &app,
        &format!("/api/v1/analysis/findings/{id}/evidence"),
        Some(AGENT),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Explicit opt-in shows them, and says so.
    let all = ok(
        &app,
        "/api/v1/analysis/findings?limit=200&includeIgnored=true",
    )
    .await;
    assert_eq!(all["includeIgnored"], true);
    assert_eq!(all["total"], before["total"]);
    ok(
        &app,
        &format!("/api/v1/analysis/findings/{id}/evidence?includeIgnored=true"),
    )
    .await;
}

#[tokio::test]
async fn evidence_of_an_unknown_finding_is_not_found() {
    let session = fixture_session().await;
    let app = app(&session);
    let (status, body) = get(
        &app,
        "/api/v1/analysis/findings/fnd_nope/evidence",
        Some(AGENT),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not_found");
}

#[tokio::test]
async fn empty_page_from_a_partial_run_is_distinguishable_from_a_clean_one() {
    let partial = session(vec![Empty::new(RunStatus::Partial, Completeness::Partial)]).await;
    let clean = session(vec![Empty::new(
        RunStatus::Succeeded,
        Completeness::Complete,
    )])
    .await;
    let partial = ok(&app(&partial), "/api/v1/analysis/findings").await;
    let clean = ok(&app(&clean), "/api/v1/analysis/findings").await;
    for page in [&partial, &clean] {
        assert_eq!(page["items"].as_array().unwrap().len(), 0);
        assert_eq!(page["total"], 0);
    }
    assert_eq!(partial["coverage"]["complete"], false);
    assert_eq!(partial["coverage"]["sources"][0]["state"], "incomplete");
    assert_eq!(partial["coverage"]["sources"][0]["sourceId"], DIFF);
    assert_eq!(clean["coverage"]["complete"], true);
    assert_eq!(clean["coverage"]["sources"][0]["state"], "clean");
}

#[tokio::test]
async fn feedback_reports_thread_import_coverage() {
    let mut snapshot = demo::snapshot(0);
    snapshot.github_threads_completeness = Completeness::Partial;
    let session = Session::new(snapshot, Store::open_in_memory().unwrap(), None, None).unwrap();
    let page = ok(&app(&session), "/api/v1/github-feedback/threads").await;
    assert_eq!(page["coverage"]["complete"], false);
    let sources = page["coverage"]["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0]["sourceId"], "github.review_threads");
    assert_eq!(sources[0]["state"], "incomplete");
    for item in page["items"].as_array().unwrap() {
        assert_eq!(item["source"], "github");
    }
}

#[tokio::test]
async fn summary_matches_the_session_view() {
    let session = fixture_session().await;
    let app = app(&session);
    let summary = ok(&app, "/api/v1/analysis/summary").await;
    let view = serde_json::to_value(session.view(Actor::Agent).unwrap()).unwrap();
    for key in [
        "snapshotId",
        "overall",
        "sources",
        "currentFindings",
        "eventVersion",
    ] {
        assert_eq!(summary[key], view["analysis"][key], "{key}");
    }
}

#[tokio::test]
async fn feedback_coverage_survives_refresh_and_api_reload() {
    let mut snapshot = demo::snapshot(0);
    snapshot.reviews_completeness = Completeness::Complete;
    snapshot.issue_comments_completeness = Completeness::Partial;
    let session = Session::with_analysis(
        snapshot,
        Store::open_in_memory().unwrap(),
        None,
        None,
        vec![],
        SchedulerConfig::default(),
    )
    .unwrap();
    session.start_analysis().unwrap();
    let app = app(&session);
    let before = ok(&app, "/api/v1/analysis/summary").await;
    let feedback = |summary: &Value| {
        summary["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["sourceId"] == "github.reviews_and_comments")
            .unwrap()
            .clone()
    };
    let initial = feedback(&before);
    assert_eq!(initial["completeness"], "partial");
    assert_eq!(initial["status"], "partial");
    assert_eq!(initial["state"], "incomplete");
    assert!(!initial["limitations"].as_array().unwrap().is_empty());

    session.refresh().await.unwrap();
    let refreshed = ok(&app, "/api/v1/analysis/summary").await;
    assert_ne!(before["snapshotId"], refreshed["snapshotId"]);
    assert_eq!(feedback(&refreshed)["snapshotId"], refreshed["snapshotId"]);
    assert_eq!(
        refreshed["sources"],
        ok(&app, "/api/v1/analysis/summary").await["sources"]
    );
    assert_eq!(feedback(&refreshed)["completeness"], "complete");
    assert_eq!(feedback(&refreshed)["status"], "succeeded");
}

#[test]
fn revision_conflict_surfaces_as_a_recoverable_409() {
    let error = super::super::db_error(crate::store::RevisionConflict { current: None }.into());
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.code, "revision_conflict");
    let details = error.details.unwrap();
    assert_eq!(details["retryable"], true);
    assert_eq!(details["currentRevision"], 0);
}

// Review queue (#30 exposed for the Review screen, #38).

fn queue_paths(queue: &Value) -> Vec<(String, String)> {
    queue["queue"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["path"].as_str().unwrap().to_string(),
                i["bucket"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn queue_lists_every_file_once_with_reasons_and_never_acknowledges() {
    let session = fixture_session().await;
    let app = app(&session);
    let before = serde_json::to_value(session.view(Actor::Agent).unwrap()).unwrap();
    let queue = ok(&app, "/api/v1/analysis/queue").await;
    let items = queue["queue"]["items"].as_array().unwrap();
    assert_eq!(items.len(), before["files"].as_array().unwrap().len());
    for item in items {
        assert!(!item["reasons"].as_array().unwrap().is_empty(), "{item}");
        // Analysis never acknowledges: nothing was reviewed by a human.
        assert_ne!(item["acknowledgement"], "reviewed-current");
    }
    assert_eq!(queue["snapshotId"], before["analysis"]["snapshotId"]);
    assert_eq!(queue["counts"]["files"]["total"], items.len());
    // Reading the queue writes nothing.
    let after = serde_json::to_value(session.view(Actor::Agent).unwrap()).unwrap();
    assert_eq!(before["fileStates"], after["fileStates"]);
    assert_eq!(before["progress"], after["progress"]);
}

#[tokio::test]
async fn excluding_and_reincluding_reranks_without_new_runs() {
    let session = fixture_session().await;
    let app = app(&session);
    let (_, path) = some_finding(&app).await;
    let runs = ok(&app, "/api/v1/analysis/runs?limit=200").await["total"].clone();
    let original = ok(&app, "/api/v1/analysis/queue").await;
    let rule = session
        .store
        .lock()
        .unwrap()
        .create_rule(
            &session.repository.id,
            RuleAction::Exclude,
            RuleKind::File,
            &path,
            None,
            Actor::Human,
        )
        .unwrap();
    let excluded = ok(&app, "/api/v1/analysis/queue").await;
    assert!(queue_paths(&excluded).contains(&(path.clone(), "ignored".into())));
    assert_eq!(excluded["counts"]["files"]["ignored"], 1);
    session
        .store
        .lock()
        .unwrap()
        .delete_rule(&session.repository.id, &rule.id)
        .unwrap();
    let restored = ok(&app, "/api/v1/analysis/queue").await;
    assert_eq!(restored["queue"], original["queue"]);
    assert_eq!(
        ok(&app, "/api/v1/analysis/runs?limit=200").await["total"],
        runs
    );
}

#[tokio::test]
async fn empty_queue_from_a_partial_run_is_not_all_clear() {
    let partial = session(vec![Empty::new(RunStatus::Partial, Completeness::Partial)]).await;
    let queue = ok(&app(&partial), "/api/v1/analysis/queue").await;
    assert_eq!(queue["queue"]["coverage"], "incomplete");
    assert_ne!(queue["queue"]["verdict"]["kind"], "all_clear");
}
