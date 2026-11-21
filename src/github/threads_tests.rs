//! Offline tests for review-thread pagination. Never runs `gh`.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use super::*;
use crate::github::fake::FakeGh;

/// Answers GraphQL calls by (thread id or "threads", cursor).
type Script = HashMap<(String, Option<String>), Result<Value, String>>;

#[derive(Default)]
struct PagedGh {
    script: Script,
    calls: Mutex<Vec<(String, Option<String>)>>,
}

impl PagedGh {
    fn on(mut self, target: &str, cursor: Option<&str>, resp: Result<Value, &str>) -> Self {
        self.script.insert(
            (target.into(), cursor.map(str::to_string)),
            resp.map_err(str::to_string),
        );
        self
    }
}

impl GhRunner for PagedGh {
    fn run(&self, args: &[&str], _stdin: Option<&[u8]>) -> Result<Vec<u8>> {
        if args.get(1).is_some_and(|arg| arg.starts_with("repos/")) {
            return Ok(b"[]".to_vec());
        }
        let arg = |k: &str| {
            args.iter()
                .find_map(|a| a.strip_prefix(&format!("{k}=")))
                .map(str::to_string)
        };
        let target = arg("id").unwrap_or_else(|| "threads".into());
        let key = (target, arg("cursor"));
        self.calls.lock().unwrap().push(key.clone());
        match self.script.get(&key) {
            Some(Ok(v)) => Ok(v.to_string().into_bytes()),
            Some(Err(e)) => Err(anyhow!("{e}")),
            None => Err(anyhow!("no scripted response for {key:?}")),
        }
    }
}

#[test]
fn rest_fallback_groups_replies_without_claiming_graphql_anchor() {
    let gh = FakeGh::default();
    gh.on(&["api", "graphql"], Err("unavailable"));
    gh.on(&["api", "repos/o/r/pulls/1/comments?per_page=100&page=1"],
        Ok(r#"[{"id":1,"node_id":"N1","path":"a.rs","body":"root","created_at":"now"},{"id":2,"node_id":"N2","path":"a.rs","body":"reply","created_at":"later","in_reply_to_id":1}]"#));
    let result = fetch_threads(&gh, &pr());
    assert_eq!(result.threads.len(), 1);
    assert!(result.threads[0].rest_only);
    assert_eq!(result.completeness, Completeness::Partial);
    assert!(!result.problems.is_empty());
    assert!(!result.threads[0].enumeration_complete);
    assert!(
        result.threads[0]
            .comments
            .iter()
            .any(|c| c.in_reply_to_id == Some(1))
    );
}

fn pr() -> PrRef {
    PrRef {
        owner: "o".into(),
        repo: "r".into(),
        number: 1,
    }
}

fn comment(i: usize) -> Value {
    json!({
        "id": format!("C{i}"), "databaseId": i, "body": format!("reply {i}"),
        "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z",
        "url": format!("https://github.com/o/r/pull/1#discussion_r{i}"),
        "outdated": false, "line": 10, "originalLine": 8,
        "commit": {"oid": "head1"}, "originalCommit": {"oid": "old1"},
        "author": if i.is_multiple_of(2) {
            json!({"__typename": "Bot", "login": "coderabbitai", "id": "BOT_1"})
        } else {
            json!({"__typename": "User", "login": "alice", "id": "U_1"})
        }
    })
}

fn conn(range: std::ops::Range<usize>, next: Option<&str>) -> Value {
    json!({
        "pageInfo": {"hasNextPage": next.is_some(), "endCursor": next},
        "nodes": range.map(comment).collect::<Vec<_>>()
    })
}

fn thread(id: &str, comments: Value) -> Value {
    json!({
        "id": id, "isResolved": true, "isOutdated": false, "path": "a.rs",
        "line": 10, "originalLine": 8, "startLine": null, "originalStartLine": null,
        "diffSide": "RIGHT", "startDiffSide": null, "subjectType": "LINE",
        "viewerCanReply": true, "viewerCanResolve": false, "viewerCanUnresolve": true,
        "resolvedBy": {"login": "bob"}, "comments": comments
    })
}

fn threads_page(nodes: Vec<Value>, next: Option<&str>) -> Value {
    json!({"data": {"repository": {"pullRequest": {"reviewThreads": {
        "pageInfo": {"hasNextPage": next.is_some(), "endCursor": next},
        "nodes": nodes
    }}}}})
}

fn replies_page(range: std::ops::Range<usize>, next: Option<&str>) -> Value {
    json!({"data": {"node": {"id": "T1", "comments": conn(range, next)}}})
}

fn assert_each_once(t: &GithubThread, n: usize) {
    assert_eq!(t.comments.len(), n);
    let mut ids: Vec<&str> = t.comments.iter().map(|c| c.node_id.as_str()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), n, "duplicate reply IDs");
    for i in 0..n {
        assert!(ids.contains(&format!("C{i}").as_str()), "missing C{i}");
    }
}

#[test]
fn thread_with_over_100_replies_imports_every_reply_exactly_once() {
    // 250 replies over three pages; the third page overlaps the second.
    let gh = PagedGh::default()
        .on(
            "threads",
            None,
            Ok(threads_page(
                vec![thread("T1", conn(0..100, Some("c1")))],
                None,
            )),
        )
        .on("T1", Some("c1"), Ok(replies_page(100..200, Some("c2"))))
        .on("T1", Some("c2"), Ok(replies_page(150..250, None)));
    let import = fetch_threads(&gh, &pr());
    assert_eq!(
        import.completeness,
        Completeness::Complete,
        "{:?}",
        import.problems
    );
    assert!(import.problems.is_empty());
    assert_eq!(import.threads.len(), 1);
    let t = &import.threads[0];
    assert_each_once(t, 250);
    // Order follows GitHub's order.
    assert_eq!(t.comments[0].node_id, "C0");
    assert_eq!(t.comments[249].node_id, "C249");
}

#[test]
fn pages_top_level_threads_and_nested_replies_independently() {
    let gh = PagedGh::default()
        .on(
            "threads",
            None,
            Ok(threads_page(
                vec![thread("T1", conn(0..100, Some("a")))],
                Some("p2"),
            )),
        )
        .on(
            "threads",
            Some("p2"),
            // Overlapping thread page repeats T1; it must not be imported twice.
            Ok(threads_page(
                vec![
                    thread("T1", conn(0..100, Some("a"))),
                    thread("T2", conn(0..3, None)),
                ],
                None,
            )),
        )
        .on("T1", Some("a"), Ok(replies_page(100..101, None)));
    let import = fetch_threads(&gh, &pr());
    assert_eq!(
        import.completeness,
        Completeness::Complete,
        "{:?}",
        import.problems
    );
    let ids: Vec<_> = import.threads.iter().map(|t| t.node_id.as_str()).collect();
    assert_eq!(ids, ["T1", "T2"]);
    assert_each_once(&import.threads[0], 101);
    assert_each_once(&import.threads[1], 3);
    assert_eq!(
        gh.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(t, _)| t == "T1")
            .count(),
        1
    );
}

#[test]
fn preserves_thread_and_comment_fidelity() {
    let gh = PagedGh::default().on(
        "threads",
        None,
        Ok(threads_page(vec![thread("T1", conn(0..2, None))], None)),
    );
    let t = &fetch_threads(&gh, &pr()).threads[0];
    assert_eq!((t.line, t.original_line), (Some(10), Some(8)));
    assert!(t.is_resolved && !t.is_outdated);
    assert_eq!(t.resolved_by.as_deref(), Some("bob"));
    assert_eq!(t.subject_type.as_deref(), Some("LINE"));
    assert_eq!(
        (
            t.viewer_can_reply,
            t.viewer_can_resolve,
            t.viewer_can_unresolve
        ),
        (Some(true), Some(false), Some(true))
    );
    assert_eq!(
        t.url.as_deref(),
        Some("https://github.com/o/r/pull/1#discussion_r0")
    );
    let (bot, human) = (&t.comments[0], &t.comments[1]);
    assert_eq!(bot.author_type.as_deref(), Some("Bot"));
    assert_eq!(bot.author_id.as_deref(), Some("BOT_1"));
    assert_eq!(human.author_type.as_deref(), Some("User"));
    assert_eq!(human.author_id.as_deref(), Some("U_1"));
    assert_eq!(bot.updated_at.as_deref(), Some("2026-01-02T00:00:00Z"));
    assert_eq!((bot.line, bot.original_line), (Some(10), Some(8)));
    assert_eq!(bot.commit_oid.as_deref(), Some("head1"));
    assert_eq!(bot.original_commit_oid.as_deref(), Some("old1"));
    assert_eq!(bot.outdated, Some(false));
    assert_eq!(t.start_side, None);
    assert_eq!(t.original_start_line, None);
}

#[test]
fn failed_reply_page_is_partial_not_complete() {
    let gh = PagedGh::default()
        .on(
            "threads",
            None,
            Ok(threads_page(
                vec![thread("T1", conn(0..100, Some("c1")))],
                None,
            )),
        )
        .on("T1", Some("c1"), Err("gh failed: HTTP 502"));
    let import = fetch_threads(&gh, &pr());
    assert_eq!(import.completeness, Completeness::Partial);
    assert!(
        import.problems.iter().any(|p| p.to_string().contains("T1")),
        "{:?}",
        import.problems
    );
    assert_eq!(import.threads[0].comments.len(), 100);
}

#[test]
fn graphql_permission_error_is_partial() {
    let mut page = threads_page(vec![thread("T1", conn(0..1, None))], None);
    page["errors"] =
        json!([{"type": "FORBIDDEN", "message": "Resource not accessible by integration"}]);
    let gh = PagedGh::default().on("threads", None, Ok(page));
    let import = fetch_threads(&gh, &pr());
    assert_eq!(import.completeness, Completeness::Partial);
    assert!(import.problems[0].to_string().contains("not accessible"));
    assert!(matches!(
        import.problems[0],
        ThreadImportProblem::PermissionDenied {
            via: DeniedVia::GraphqlError,
            ..
        }
    ));
    assert_eq!(
        import.problems[0].to_string(),
        "review threads: GraphQL error: Resource not accessible by integration"
    );
    assert_eq!(import.threads.len(), 1);
}

#[test]
fn aborted_top_level_page_is_partial_and_first_page_failure_is_failed() {
    let gh = PagedGh::default().on(
        "threads",
        None,
        Ok(threads_page(
            vec![thread("T1", conn(0..1, None))],
            Some("p2"),
        )),
    );
    // No response for p2: the second page aborts.
    let import = fetch_threads(&gh, &pr());
    assert_eq!(import.completeness, Completeness::Partial);
    assert_eq!(import.threads.len(), 1);

    let gh = FakeGh::default();
    gh.on(&["api", "graphql"], Err("boom"));
    let import = fetch_threads(&gh, &pr());
    assert_eq!(import.completeness, Completeness::Failed);
    assert!(import.threads.is_empty());

    let gh = PagedGh::default().on(
        "threads",
        None,
        Ok(json!({"data": null, "errors": [{"message": "denied"}]})),
    );
    assert_eq!(fetch_threads(&gh, &pr()).completeness, Completeness::Failed);
}

#[test]
fn missing_page_info_or_stalled_cursor_is_not_complete() {
    let mut no_info = conn(0..1, None);
    no_info.as_object_mut().unwrap().remove("pageInfo");
    let gh = PagedGh::default().on(
        "threads",
        None,
        Ok(threads_page(vec![thread("T1", no_info)], None)),
    );
    assert_eq!(
        fetch_threads(&gh, &pr()).completeness,
        Completeness::Partial
    );

    let gh = PagedGh::default()
        .on(
            "threads",
            None,
            Ok(threads_page(
                vec![thread("T1", conn(0..1, Some("same")))],
                None,
            )),
        )
        .on("T1", Some("same"), Ok(replies_page(1..2, Some("same"))));
    let import = fetch_threads(&gh, &pr());
    assert_eq!(import.completeness, Completeness::Partial);
    assert_each_once(&import.threads[0], 2);
}

#[test]
fn queries_request_page_info_on_every_paged_connection() {
    assert_eq!(threads_query().matches("pageInfo").count(), 2);
    assert!(replies_query().contains("pageInfo"));
    assert!(replies_query().contains("after: $cursor"));
}

#[test]
fn malformed_nodes_cursors_and_wrong_thread_retain_imported_data() {
    let mut malformed = threads_page(vec![thread("T1", conn(0..1, None))], Some("next"));
    malformed["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"] = Value::Null;
    assert_eq!(
        fetch_threads(
            &PagedGh::default().on("threads", None, Ok(malformed)),
            &pr()
        )
        .completeness,
        Completeness::Failed
    );

    let first = threads_page(vec![thread("T1", conn(0..1, Some("c1")))], None);
    let mut wrong = replies_page(1..2, None);
    wrong["data"]["node"]["id"] = json!("other");
    let import = fetch_threads(
        &PagedGh::default()
            .on("threads", None, Ok(first))
            .on("T1", Some("c1"), Ok(wrong)),
        &pr(),
    );
    assert_eq!(import.completeness, Completeness::Partial);
    assert_each_once(&import.threads[0], 1);
    assert!(
        import
            .problems
            .iter()
            .any(|p| p.to_string().contains("wrong thread ID"))
    );

    let gh = PagedGh::default()
        .on(
            "threads",
            None,
            Ok(threads_page(
                vec![thread("T1", conn(0..1, None))],
                Some("a"),
            )),
        )
        .on("threads", Some("a"), Ok(threads_page(vec![], Some("b"))))
        .on("threads", Some("b"), Ok(threads_page(vec![], Some("a"))));
    let import = fetch_threads(&gh, &pr());
    assert_eq!(import.completeness, Completeness::Partial);
    assert_eq!(import.threads.len(), 1);
    assert!(
        import
            .problems
            .iter()
            .any(|p| p.to_string().contains("cyclic cursor"))
    );
}

#[test]
fn conflicting_duplicates_and_invalid_cursor_are_partial() {
    let first = threads_page(vec![thread("T1", conn(0..1, None))], Some("a"));
    let mut changed = thread("T1", conn(0..1, None));
    changed["path"] = json!("different.rs");
    let import = fetch_threads(
        &PagedGh::default().on("threads", None, Ok(first)).on(
            "threads",
            Some("a"),
            Ok(threads_page(vec![changed], None)),
        ),
        &pr(),
    );
    assert_eq!(import.completeness, Completeness::Partial);
    assert_eq!(import.threads[0].path, "a.rs");
    let mut invalid = threads_page(vec![], Some("a"));
    invalid["data"]["repository"]["pullRequest"]["reviewThreads"]["pageInfo"]["endCursor"] =
        json!(123);
    assert_eq!(
        fetch_threads(&PagedGh::default().on("threads", None, Ok(invalid)), &pr()).completeness,
        Completeness::Partial
    );
}

#[test]
fn raw_metadata_and_legacy_author_alias_roundtrip() {
    let mut t = thread("T1", conn(0..1, None));
    t["startDiffSide"] = json!("FUTURE_SIDE");
    t["originalStartLine"] = json!(4);
    let import = fetch_threads(
        &PagedGh::default().on("threads", None, Ok(threads_page(vec![t], None))),
        &pr(),
    );
    assert_eq!(import.completeness, Completeness::Complete);
    assert_eq!(import.threads[0].start_side.as_deref(), Some("FUTURE_SIDE"));
    assert_eq!(import.threads[0].original_start_line, Some(4));
    let mut legacy = serde_json::to_value(&import.threads[0].comments[0]).unwrap();
    let id = legacy.as_object_mut().unwrap().remove("authorId").unwrap();
    legacy["authorNodeId"] = id;
    let comment: GithubComment = serde_json::from_value(legacy).unwrap();
    assert_eq!(comment.author_id.as_deref(), Some("BOT_1"));
}

#[test]
fn error_paths_follow_comment_ids_after_duplicates_on_both_pages() {
    let mut initial = conn(0..1, Some("c1"));
    initial["nodes"]
        .as_array_mut()
        .unwrap()
        .extend([comment(0), comment(1)]);
    initial["nodes"][2]["author"] = Value::Null;
    let mut first = threads_page(vec![thread("T1", initial)], None);
    first["errors"] = json!([{"message":"denied", "path":["repository","pullRequest","reviewThreads","nodes",0,"comments","nodes",2,"author"]}]);
    let mut second = replies_page(1..2, None);
    second["data"]["node"]["comments"]["nodes"] = json!([comment(0), comment(1), comment(2)]);
    second["data"]["node"]["comments"]["nodes"][1]["author"] = Value::Null;
    second["data"]["node"]["comments"]["nodes"][2]["author"] = Value::Null;
    second["errors"] = json!([{"message":"denied", "path":["node","comments","nodes",2,"author"]}]);
    let import = fetch_threads(
        &PagedGh::default()
            .on("threads", None, Ok(first))
            .on("T1", Some("c1"), Ok(second)),
        &pr(),
    );
    assert_eq!(import.completeness, Completeness::Partial);
    let comments = &import.threads[0].comments;
    assert_eq!(
        comments
            .iter()
            .map(|c| c.node_id.as_str())
            .collect::<Vec<_>>(),
        ["C0", "C1", "C2"]
    );
    assert!(comments[0].unavailable_fields.is_empty());
    assert_eq!(comments[1].unavailable_fields, ["author"]);
    assert_eq!(comments[2].unavailable_fields, ["author"]);
}

#[test]
fn initial_reply_error_marker_survives_later_malformed_node_or_page_info() {
    for failure in ["node", "pageInfo"] {
        let mut comments = conn(0..1, None);
        comments["nodes"][0]["author"] = Value::Null;
        if failure == "node" {
            comments["nodes"].as_array_mut().unwrap().push(Value::Null);
        } else {
            comments.as_object_mut().unwrap().remove("pageInfo");
        }
        let mut page = threads_page(vec![thread("T1", comments)], None);
        page["errors"] = json!([{"message":"denied", "path":["repository","pullRequest","reviewThreads","nodes",0,"comments","nodes",0,"author"]}]);
        let import = fetch_threads(&PagedGh::default().on("threads", None, Ok(page)), &pr());
        assert_eq!(import.completeness, Completeness::Partial, "{failure}");
        assert_eq!(
            import.threads[0].comments[0].unavailable_fields,
            ["author"],
            "{failure}"
        );
    }
}

#[test]
fn reply_error_markers_survive_malformed_node_page_info_and_cyclic_cursor() {
    for failure in ["node", "pageInfo", "cursor"] {
        let first = threads_page(vec![thread("T1", conn(0..1, Some("c1")))], None);
        let mut second = replies_page(
            1..2,
            if failure == "cursor" {
                Some("c1")
            } else {
                None
            },
        );
        second["data"]["node"]["comments"]["nodes"][0]["author"] = Value::Null;
        if failure == "node" {
            second["data"]["node"]["comments"]["nodes"]
                .as_array_mut()
                .unwrap()
                .push(Value::Null);
        } else if failure == "pageInfo" {
            second["data"]["node"]["comments"]
                .as_object_mut()
                .unwrap()
                .remove("pageInfo");
        }
        second["errors"] =
            json!([{"message":"denied", "path":["node","comments","nodes",0,"author"]}]);
        let import = fetch_threads(
            &PagedGh::default()
                .on("threads", None, Ok(first))
                .on("T1", Some("c1"), Ok(second)),
            &pr(),
        );
        assert_eq!(import.completeness, Completeness::Partial, "{failure}");
        assert_eq!(
            import.threads[0].comments[1].unavailable_fields,
            ["author"],
            "{failure}"
        );
    }
}

#[test]
fn complete_import_then_field_error_refresh_preserves_only_unavailable_metadata() {
    use crate::store::Store;
    let complete = fetch_threads(
        &PagedGh::default().on(
            "threads",
            None,
            Ok(threads_page(vec![thread("T1", conn(0..1, None))], None)),
        ),
        &pr(),
    );
    assert_eq!(complete.completeness, Completeness::Complete);
    let mut partial_page = threads_page(vec![thread("T1", conn(0..1, None))], None);
    let node = &mut partial_page["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"][0];
    node["originalLine"] = Value::Null;
    node["viewerCanResolve"] = Value::Null;
    node["startDiffSide"] = json!("LEFT");
    node["isResolved"] = json!(false);
    node["comments"]["nodes"][0]["author"] = Value::Null;
    node["comments"]["nodes"][0]["body"] = json!("edited");
    partial_page["errors"] = json!([
        {"message":"denied", "path":["repository","pullRequest","reviewThreads","nodes",0,"originalLine"]},
        {"message":"denied", "path":["repository","pullRequest","reviewThreads","nodes",0,"viewerCanResolve"]},
        {"message":"denied", "path":["repository","pullRequest","reviewThreads","nodes",0,"comments","nodes",0,"author"]}
    ]);
    let partial = fetch_threads(
        &PagedGh::default().on("threads", None, Ok(partial_page)),
        &pr(),
    );
    assert_eq!(partial.completeness, Completeness::Partial);
    let mut store = Store::open_in_memory().unwrap();
    let repo = store
        .resolve_repository("github.com", "o", "r", None)
        .unwrap();
    let pr_id = store
        .upsert_pull_request(&repo.id, 1, "title", "url")
        .unwrap();
    store
        .sync_github_threads(&pr_id, "head", &complete.threads, true)
        .unwrap();
    store
        .sync_github_threads(&pr_id, "head", &partial.threads, false)
        .unwrap();
    let threads = store.list_threads(&pr_id).unwrap();
    assert_eq!(threads.len(), 1);
    assert!(!threads[0].resolved);
    assert_eq!(threads[0].comments[0].author, "coderabbitai");
    assert_eq!(threads[0].comments[0].body, "edited");
    assert!(
        partial.threads[0]
            .unavailable_fields
            .contains(&"originalLine".into())
    );
    assert_eq!(partial.threads[0].start_side.as_deref(), Some("LEFT"));
}

#[test]
fn typed_problems_round_trip_and_legacy_strings_still_decode() {
    let typed = vec![
        ThreadImportProblem::PageLimitReached {
            scope: "review threads".into(),
            max_pages: MAX_PAGES,
        },
        ThreadImportProblem::StalledCursor {
            scope: "thread T1: replies".into(),
        },
    ];
    let json = serde_json::to_value(&typed).unwrap();
    assert_eq!(json[0]["kind"], "page_limit_reached");
    assert_eq!(json[0]["maxPages"], 50);
    let back: Vec<ThreadImportProblem> = serde_json::from_value(json).unwrap();
    assert_eq!(back, typed);
    assert_eq!(
        typed[0].to_string(),
        "review threads exceed 50 pages; stopped early"
    );
    assert_eq!(
        typed[1].to_string(),
        "thread T1: replies: cursor did not advance"
    );

    let legacy: Vec<ThreadImportProblem> = serde_json::from_value(json!([
        "review threads: FORBIDDEN: Resource not accessible by integration",
        "review threads exceed 50 pages; stopped early",
        "thread T: cursor did not advance",
    ]))
    .unwrap();
    let kinds: Vec<_> = legacy
        .iter()
        .map(|p| match p {
            ThreadImportProblem::Legacy { legacy_kind, .. } => *legacy_kind,
            other => panic!("expected legacy, got {other:?}"),
        })
        .collect();
    assert_eq!(
        kinds,
        [
            LegacyKind::PermissionDenied,
            LegacyKind::PageLimitReached,
            LegacyKind::Other
        ]
    );
    assert_eq!(legacy[2].to_string(), "thread T: cursor did not advance");
}
