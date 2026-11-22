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
        "/api/v1/github-feedback/records".into(),
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
async fn native_records_cover_every_non_thread_source_across_pages_without_mutations() {
    use crate::github::checks::Coverage;
    use crate::model::{ImportedCheckAnnotation, ImportedCommitStatus, NativeSourceKey};
    use std::collections::HashSet;

    let mut snapshot = demo::snapshot(0);
    let head = snapshot.pr.head_sha.clone();
    let review = snapshot.reviews[0].clone();
    snapshot.reviews = (0..205)
        .map(|i| {
            let mut r = review.clone();
            r.node_id = Some(format!("review-{i}"));
            if i == 203 {
                r.node_id = None;
                r.database_id = Some(987654321);
            }
            if i == 204 {
                r.node_id = None;
                r.database_id = None;
            }
            r
        })
        .collect();
    snapshot.checks.sha = head.clone();
    snapshot.checks.runs_coverage.completeness = Completeness::Complete;
    snapshot.checks.statuses_coverage.completeness = Completeness::Complete;
    snapshot.checks.runs.push(crate::model::ImportedCheckRun {
        github_check_run_id: 42,
        app_slug: Some("ci".into()),
        name: "CI".into(),
        status: "completed".into(),
        conclusion: Some("failure".into()),
        head_sha: head.clone(),
        started_at: None,
        completed_at: None,
        details_url: Some("https://example.test/check".into()),
        summary: None,
        annotations_count: 1,
        observed_at: "now".into(),
    });
    snapshot.checks.annotations.push(ImportedCheckAnnotation {
        check_run_id: 42,
        key: NativeSourceKey::ContentFingerprint("annotation-1".into()),
        path: "src/a.rs".into(),
        start_line: 1,
        end_line: 1,
        start_column: None,
        end_column: None,
        annotation_level: serde_json::from_str("\"warning\"").unwrap(),
        title: None,
        message: "native annotation".into(),
        raw_details: None,
        head_sha: head.clone(),
        observed_at: "now".into(),
    });
    snapshot.checks.annotation_coverage.push((
        42,
        Coverage {
            completeness: Completeness::Partial,
            reasons: vec!["page_limit".into()],
        },
    ));
    snapshot.checks.statuses.push(ImportedCommitStatus {
        github_status_id: 7,
        context: "ci".into(),
        state: "success".into(),
        description: None,
        target_url: None,
        sha: head,
        updated_at: None,
        observed_at: "now".into(),
    });
    let session = Session::new(snapshot, Store::open_in_memory().unwrap(), None, None).unwrap();
    session.start_analysis().unwrap();
    let app = app(&session);
    let before = ok(&app, "/api/v1/session").await;
    let findings_before = ok(&app, "/api/v1/analysis/findings").await;
    let mut cursor: Option<String> = None;
    let mut all = Vec::new();
    loop {
        let path = match &cursor {
            Some(c) => format!("/api/v1/github-feedback/records?limit=37&cursor={c}"),
            None => "/api/v1/github-feedback/records?limit=37".into(),
        };
        let page = ok(&app, &path).await;
        assert_eq!(page["coverage"]["sources"].as_array().unwrap().len(), 5);
        assert!(!page["coverage"]["complete"].as_bool().unwrap());
        all.extend(page["items"].as_array().unwrap().iter().cloned());
        cursor = page["nextCursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    let kinds: HashSet<_> = all
        .iter()
        .filter_map(|r| r["resourceKind"].as_str())
        .map(|s| s.split(':').next().unwrap())
        .collect();
    assert_eq!(
        kinds,
        HashSet::from([
            "reviews",
            "issue_comments",
            "check_runs",
            "check_annotations",
            "commit_statuses"
        ])
    );
    assert_eq!(
        all.len(),
        205 + before["issueComments"].as_array().unwrap().len() + 3
    );
    let ids: HashSet<_> = all
        .iter()
        .filter_map(|r| r["native"]["key"]["value"].as_str())
        .collect();
    assert_eq!(
        ids.len(),
        all.iter().filter(|r| !r["native"].is_null()).count()
    );
    assert!(
        all.iter()
            .filter(|r| r["native"].is_null())
            .all(|r| r["nodeId"].is_null() && r["databaseId"].is_null())
    );
    assert!(
        all.iter()
            .any(|r| r["native"]["key"]["value"] == "987654321")
    );
    let run = all
        .iter()
        .find(|r| {
            r["resourceKind"]
                .as_str()
                .unwrap_or("")
                .starts_with("check_runs:")
        })
        .unwrap();
    assert_eq!(run["annotationCoverage"]["completeness"], "partial");
    assert_eq!(run["currentHead"], true);
    assert_eq!(run["native"]["externalUrl"], "https://example.test/check");
    assert_eq!(ok(&app, "/api/v1/session").await, before);
    assert_eq!(ok(&app, "/api/v1/analysis/findings").await, findings_before);
    let checks = ok(&app, "/api/v1/github-feedback/checks").await;
    assert_eq!(checks["total"], 3);
}

#[tokio::test]
async fn checks_are_persisted_and_served_separately_from_review_threads() {
    let mut snapshot = demo::snapshot(0);
    snapshot.checks.sha = snapshot.pr.head_sha.clone();
    snapshot.checks.runs_coverage.completeness = Completeness::Complete;
    snapshot.checks.runs_coverage.reasons.clear();
    snapshot.checks.statuses_coverage.completeness = Completeness::Complete;
    snapshot.checks.statuses_coverage.reasons.clear();
    snapshot.checks.runs.push(crate::model::ImportedCheckRun {
        github_check_run_id: 42,
        app_slug: None,
        name: "CI".into(),
        status: "completed".into(),
        conclusion: Some("failure".into()),
        head_sha: snapshot.pr.head_sha.clone(),
        started_at: None,
        completed_at: None,
        details_url: None,
        summary: None,
        annotations_count: 0,
        observed_at: "now".into(),
    });
    let session = Session::new(snapshot, Store::open_in_memory().unwrap(), None, None).unwrap();
    session.start_analysis().unwrap();
    let app = app(&session);
    let checks = ok(&app, "/api/v1/github-feedback/checks").await;
    assert_eq!(checks["items"].as_array().unwrap().len(), 1);
    assert_eq!(checks["items"][0]["githubCheckRunId"], 42);
    assert_eq!(checks["items"][0]["currentHead"], true);
    assert_eq!(
        checks["items"][0]["native"]["key"],
        serde_json::json!({"kind":"upstream","value":"42"})
    );
    let threads = ok(&app, "/api/v1/github-feedback/threads").await;
    assert!(
        threads["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item.get("githubCheckRunId").is_none())
    );
    assert_eq!(
        session
            .store
            .lock()
            .unwrap()
            .list_imported_sources(&session.pr_id, None)
            .unwrap()
            .len(),
        1
    );
    session.refresh().await.unwrap();
    let stale = ok(&app, "/api/v1/github-feedback/checks").await;
    assert_eq!(stale["items"][0]["currentHead"], false);
}

#[tokio::test]
async fn feedback_joins_native_metadata_and_rejects_changed_payload_cursor() {
    let mut snapshot = demo::snapshot(0);
    assert!(snapshot.github_threads.len() >= 3);
    let first_id = snapshot.github_threads[0].node_id.clone();
    let rest_id = snapshot.github_threads[1].node_id.clone();
    let missing_id = snapshot.github_threads[2].node_id.clone();
    let native = &mut snapshot.github_threads[0];
    native.url = Some("https://github.com/acme/pr/pull/1#discussion_r1".into());
    native.comments[0].url = Some("https://github.com/acme/pr/pull/1#discussion_r1".into());
    native.comments[0].author_type = Some("Bot".into());
    native.comments[0].outdated = Some(true);
    native.comments[0].original_line = Some(19);
    snapshot.github_threads[1].rest_only = true;
    let session = Session::new(snapshot, Store::open_in_memory().unwrap(), None, None).unwrap();
    let app = app(&session);
    let before = ok(&app, "/api/v1/session").await;
    // The stored thread remains, but current native metadata is missing.
    session
        .snapshot
        .write()
        .unwrap()
        .github_threads
        .retain(|t| t.node_id != missing_id);
    let page = ok(
        &app,
        "/api/v1/github-feedback/threads?limit=1&includeIgnored=true",
    )
    .await;
    let cursor = page["nextCursor"].as_str().unwrap();
    let cursor_thread_id = page["items"][0]["githubThreadId"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut all = page["items"].as_array().unwrap().clone();
    let mut next = Some(cursor.to_string());
    while let Some(cursor) = next {
        let page = ok(
            &app,
            &format!("/api/v1/github-feedback/threads?limit=1&includeIgnored=true&cursor={cursor}"),
        )
        .await;
        all.extend(page["items"].as_array().unwrap().iter().cloned());
        next = page["nextCursor"].as_str().map(str::to_owned);
    }
    let find = |id: &str| all.iter().find(|t| t["githubThreadId"] == id).unwrap();
    let first = find(&first_id);
    assert_eq!(
        first["github"]["url"],
        "https://github.com/acme/pr/pull/1#discussion_r1"
    );
    assert_eq!(first["github"]["comments"][0]["authorType"], "Bot");
    assert_eq!(first["github"]["comments"][0]["outdated"], true);
    assert_eq!(first["github"]["comments"][0]["originalLine"], 19);
    assert_eq!(first["resolutionKnown"], true);
    assert_eq!(find(&rest_id)["resolutionKnown"], false);
    assert_eq!(find(&missing_id)["github"], Value::Null);
    assert_eq!(find(&missing_id)["resolutionKnown"], false);
    // A field-level GraphQL error invalidates the resolution assertion.
    session.snapshot.write().unwrap().github_threads[0]
        .unavailable_fields
        .push("isResolved".into());
    let after = ok(&app, "/api/v1/github-feedback/threads?includeIgnored=true").await;
    let changed = after["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["githubThreadId"] == first_id)
        .unwrap();
    assert_eq!(changed["resolutionKnown"], false);
    let cursor_native = session
        .snapshot
        .read()
        .unwrap()
        .github_threads
        .iter()
        .find(|t| t.node_id == cursor_thread_id)
        .cloned()
        .unwrap();
    session
        .snapshot
        .write()
        .unwrap()
        .github_threads
        .iter_mut()
        .find(|t| t.node_id == cursor_thread_id)
        .unwrap()
        .comments[0]
        .author_type = Some(format!(
        "changed-from-{:?}",
        cursor_native.comments[0].author_type
    ));
    let (status, body) = get(
        &app,
        &format!("/api/v1/github-feedback/threads?limit=1&includeIgnored=true&cursor={cursor}"),
        Some(AGENT),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_recoverable_conflict(status, &body, "results_changed");
    assert_eq!(
        before["threads"],
        ok(&app, "/api/v1/session").await["threads"]
    );
}

#[tokio::test]
async fn feedback_serves_persisted_node_id_for_comment_without_database_id() {
    let mut snapshot = demo::snapshot(0);
    let comment = &mut snapshot.github_threads[0].comments[0];
    comment.database_id = None;
    comment.node_id = "node-only-comment".into();
    comment.url = Some("https://github.com/acme/pr/pull/1#discussion_node".into());
    comment.author_type = Some("Bot".into());
    comment.original_line = Some(19);
    let session = Session::new(snapshot, Store::open_in_memory().unwrap(), None, None).unwrap();
    let page = ok(
        &app(&session),
        "/api/v1/github-feedback/threads?includeIgnored=true",
    )
    .await;
    let thread = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| {
            item["github"]["comments"]
                .as_array()
                .is_some_and(|comments| {
                    comments
                        .iter()
                        .any(|comment| comment["nodeId"] == "node-only-comment")
                })
        })
        .unwrap();
    let native = &thread["github"]["comments"][0];
    assert_eq!(native["databaseId"], Value::Null);
    assert_eq!(
        native["url"],
        "https://github.com/acme/pr/pull/1#discussion_node"
    );
    assert_eq!(native["authorType"], "Bot");
    assert_eq!(native["originalLine"], 19);
    assert_eq!(thread["comments"][0]["githubCommentId"], Value::Null);
    assert_eq!(thread["comments"][0]["githubNodeId"], "node-only-comment");
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
