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
    json!({"data": {"node": {"comments": conn(range, next)}}})
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
        import.problems.iter().any(|p| p.contains("T1")),
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
    assert!(import.problems[0].contains("not accessible"));
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
