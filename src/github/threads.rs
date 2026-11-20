use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::model::{GithubComment, GithubThread, Side};

use super::{GhRunner, PrRef, runner::run_json};

const PAGE_LIMIT: usize = 50;
const THREADS_QUERY: &str =
    "query($owner: String!, $repo: String!, $number: Int!, $cursor: String) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      reviewThreads(first: 100, after: $cursor) {
        pageInfo { hasNextPage endCursor }
        nodes {
          id isResolved isOutdated path line originalLine startLine originalStartLine diffSide startDiffSide viewerCanResolve viewerCanUnresolve
          comments(first: 100) {
            pageInfo { hasNextPage endCursor }
            nodes { id databaseId body createdAt updatedAt author { login __typename ... on Node { id } } }
          }
        }
      }
    }
  }
}";
const COMMENTS_QUERY: &str = "query($threadId: ID!, $cursor: String) {
  node(id: $threadId) {
    ... on PullRequestReviewThread {
      id
      comments(first: 100, after: $cursor) {
        pageInfo { hasNextPage endCursor }
        nodes { id databaseId body createdAt updatedAt author { login __typename ... on Node { id } } }
      }
    }
  }
}";

fn checked_page<'a>(
    response: &'a Value,
    page: &'a Value,
    label: &str,
) -> Result<(&'a [Value], bool, Option<&'a str>)> {
    if response
        .get("errors")
        .is_some_and(|e| !e.is_null() && e.as_array().is_none_or(|a| !a.is_empty()))
    {
        bail!(
            "GraphQL errors while fetching {label}: {}",
            response["errors"]
        );
    }
    let nodes = page["nodes"]
        .as_array()
        .with_context(|| format!("missing {label} nodes"))?;
    let has_next = page["pageInfo"]["hasNextPage"]
        .as_bool()
        .with_context(|| format!("missing {label} hasNextPage"))?;
    if page["pageInfo"].get("endCursor").is_none() {
        bail!("missing {label} endCursor field");
    }
    let end = page["pageInfo"]["endCursor"].as_str();
    if !page["pageInfo"]["endCursor"].is_null() && end.is_none() {
        bail!("invalid {label} endCursor");
    }
    if has_next && end.is_none_or(str::is_empty) {
        bail!("missing {label} endCursor");
    }
    Ok((nodes, has_next, end))
}

fn node_id<'a>(node: &'a Value, label: &str) -> Result<&'a str> {
    node["id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("missing {label} ID"))
}

fn advance_cursor(
    cursor: &mut Option<String>,
    seen: &mut HashSet<String>,
    end: Option<&str>,
    label: &str,
) -> Result<()> {
    let next = end.context("missing pagination cursor")?;
    if !seen.insert(next.to_owned()) || cursor.as_deref() == Some(next) {
        bail!("non-progressing {label} cursor");
    }
    *cursor = Some(next.to_owned());
    Ok(())
}

pub(super) fn fetch_threads(gh: &dyn GhRunner, pr_ref: &PrRef) -> Result<Vec<GithubThread>> {
    let mut threads = Vec::new();
    let mut ids: HashMap<String, Value> = HashMap::new();
    let mut cursor: Option<String> = None;
    let mut seen = HashSet::new();
    for page_number in 0..PAGE_LIMIT {
        let query = format!("query={THREADS_QUERY}");
        let owner = format!("owner={}", pr_ref.owner);
        let repo = format!("repo={}", pr_ref.repo);
        let number = format!("number={}", pr_ref.number);
        let mut args = vec![
            "api", "graphql", "-f", &query, "-f", &owner, "-f", &repo, "-F", &number,
        ];
        let cursor_arg = cursor.as_ref().map(|c| format!("cursor={c}"));
        if let Some(c) = &cursor_arg {
            args.extend(["-f", c]);
        }
        let v: Value = run_json(gh, &args)?;
        let page = &v["data"]["repository"]["pullRequest"]["reviewThreads"];
        let (nodes, has_next, end) = checked_page(&v, page, "review threads")?;
        for node in nodes {
            let id = node_id(node, "thread")?.to_owned();
            if let Some(previous) = ids.get(&id) {
                if previous != node {
                    bail!("conflicting duplicate thread ID {id}");
                }
                continue;
            }
            let mut thread = parse_thread(node)?;
            paginate_comments(gh, &id, &node["comments"], &mut thread.comments)?;
            ids.insert(id, node.clone());
            threads.push(thread);
        }
        if !has_next {
            return Ok(threads);
        }
        if page_number + 1 == PAGE_LIMIT {
            bail!("too many review thread pages");
        }
        advance_cursor(&mut cursor, &mut seen, end, "review threads")?;
    }
    unreachable!()
}

fn paginate_comments(
    gh: &dyn GhRunner,
    id: &str,
    initial: &Value,
    comments: &mut Vec<GithubComment>,
) -> Result<()> {
    let mut cursor = None;
    let mut seen_cursors = HashSet::new();
    let mut seen_nodes: HashMap<String, Value> = HashMap::new();
    for page_number in 0..PAGE_LIMIT {
        let response = if page_number == 0 {
            None
        } else {
            let query = format!("query={COMMENTS_QUERY}");
            let thread_id = format!("threadId={id}");
            let after = format!("cursor={}", cursor.as_deref().unwrap_or_default());
            Some(run_json::<Value>(
                gh,
                &[
                    "api", "graphql", "-f", &query, "-f", &thread_id, "-f", &after,
                ],
            )?)
        };
        let response_ref = response.as_ref().unwrap_or(&Value::Null);
        if let Some(response) = &response {
            if response
                .get("errors")
                .is_some_and(|e| !e.is_null() && e.as_array().is_none_or(|a| !a.is_empty()))
            {
                bail!(
                    "GraphQL errors while fetching comments: {}",
                    response["errors"]
                );
            }
            if node_id(&response["data"]["node"], "thread")? != id {
                bail!("comments returned wrong thread ID");
            }
        }
        let page = response
            .as_ref()
            .map_or(initial, |v| &v["data"]["node"]["comments"]);
        let (nodes, has_next, end) = checked_page(response_ref, page, "comments")?;
        for node in nodes {
            let comment_id = node_id(node, "comment")?.to_owned();
            if let Some(previous) = seen_nodes.get(&comment_id) {
                if previous != node {
                    bail!("conflicting duplicate comment ID {comment_id}");
                }
                continue;
            }
            comments.push(parse_comment(node));
            seen_nodes.insert(comment_id, node.clone());
        }
        if !has_next {
            return Ok(());
        }
        if page_number + 1 == PAGE_LIMIT {
            bail!("too many comment pages for thread {id}");
        }
        advance_cursor(&mut cursor, &mut seen_cursors, end, "comments")?;
    }
    unreachable!()
}

fn parse_comment(c: &Value) -> GithubComment {
    GithubComment {
        node_id: c["id"].as_str().unwrap_or_default().into(),
        database_id: c["databaseId"].as_i64(),
        author: c["author"]["login"].as_str().unwrap_or("ghost").into(),
        author_node_id: c["author"]["id"].as_str().map(str::to_owned),
        author_type: c["author"]["__typename"].as_str().map(str::to_owned),
        updated_at: c["updatedAt"].as_str().map(str::to_owned),
        body: c["body"].as_str().unwrap_or_default().into(),
        created_at: c["createdAt"].as_str().unwrap_or_default().into(),
    }
}

fn parse_thread(node: &Value) -> Result<GithubThread> {
    let num = |k: &str| node[k].as_u64().map(|n| n as u32);
    Ok(GithubThread {
        node_id: node_id(node, "thread")?.into(),
        path: node["path"].as_str().unwrap_or_default().into(),
        side: node["diffSide"].as_str().and_then(Side::parse),
        line: num("line"),
        start_line: num("startLine"),
        original_line: num("originalLine"),
        original_start_line: num("originalStartLine"),
        start_side: node["startDiffSide"].as_str().and_then(Side::parse),
        viewer_can_resolve: node["viewerCanResolve"].as_bool(),
        viewer_can_unresolve: node["viewerCanUnresolve"].as_bool(),
        is_outdated: node["isOutdated"].as_bool().unwrap_or(false),
        is_resolved: node["isResolved"].as_bool().unwrap_or(false),
        comments: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use serde_json::json;
    use std::sync::Mutex;

    struct Pages(Mutex<Vec<Value>>);
    impl GhRunner for Pages {
        fn run(&self, _args: &[&str], _: Option<&[u8]>) -> Result<Vec<u8>> {
            let mut pages = self.0.lock().unwrap();
            if pages.is_empty() {
                return Err(anyhow!("unexpected request"));
            }
            Ok(pages.remove(0).to_string().into_bytes())
        }
    }
    fn comment(i: usize) -> Value {
        json!({"id": format!("C{i}"), "body": format!("body{i}"), "createdAt":"today", "author":{"login":"a"}})
    }
    fn connection(nodes: Vec<Value>, next: bool, cursor: Option<&str>) -> Value {
        json!({"nodes":nodes,"pageInfo":{"hasNextPage":next,"endCursor":cursor}})
    }
    fn thread(comments: Value) -> Value {
        json!({"id":"T", "path":"a", "comments":comments})
    }
    fn outer(nodes: Vec<Value>, next: bool, cursor: Option<&str>) -> Value {
        json!({"data":{"repository":{"pullRequest":{"reviewThreads":connection(nodes,next,cursor)}}}})
    }
    fn replies(nodes: Vec<Value>, next: bool, cursor: Option<&str>) -> Value {
        json!({"data":{"node":{"id":"T","comments":connection(nodes,next,cursor)}}})
    }
    fn fetch(pages: Vec<Value>) -> Result<Vec<GithubThread>> {
        fetch_threads(
            &Pages(Mutex::new(pages)),
            &PrRef {
                owner: "o".into(),
                repo: "r".into(),
                number: 1,
            },
        )
    }
    #[test]
    fn nested_pages_keep_order_and_deduplicate_overlap() {
        let first = (0..100).map(comment).collect();
        let second = (99..201).map(comment).collect();
        let threads = fetch(vec![
            outer(
                vec![thread(connection(first, true, Some("c1")))],
                false,
                None,
            ),
            replies(second, true, Some("c2")),
            replies(vec![comment(200), comment(201)], false, None),
        ])
        .unwrap();
        assert_eq!(threads[0].comments.len(), 202);
        assert_eq!(threads[0].comments[101].node_id, "C101");
    }
    #[test]
    fn rejects_partial_errors_and_invalid_pages() {
        let valid = outer(vec![], false, None);
        let mut error = valid.clone();
        error["errors"] = json!([{"message":"forbidden"}]);
        let mut malformed = valid.clone();
        malformed["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"] = Value::Null;
        for page in [
            error,
            malformed,
            outer(
                vec![thread(connection(
                    vec![json!({"body":"no id"})],
                    false,
                    None,
                ))],
                false,
                None,
            ),
            outer(vec![], true, None),
        ] {
            assert!(fetch(vec![page]).is_err());
        }
    }
    #[test]
    fn rejects_conflicting_duplicates_and_cyclic_cursors() {
        let first = thread(connection(vec![comment(0)], true, Some("a")));
        assert!(
            fetch(vec![
                outer(vec![first], false, None),
                replies(vec![json!({"id":"C0","body":"changed"})], false, None)
            ])
            .is_err()
        );
        assert!(
            fetch(vec![
                outer(vec![], true, Some("a")),
                outer(vec![], true, Some("a"))
            ])
            .is_err()
        );
    }
    #[test]
    fn independent_outer_and_nested_pages_preserve_metadata() {
        let mut first = thread(connection(vec![comment(0)], true, Some("reply1")));
        first["originalStartLine"] = json!(4);
        first["startDiffSide"] = json!("LEFT");
        first["viewerCanResolve"] = json!(true);
        first["viewerCanUnresolve"] = json!(false);
        first["comments"]["nodes"][0]["updatedAt"] = json!("later");
        first["comments"]["nodes"][0]["author"]["id"] = json!("U1");
        first["comments"]["nodes"][0]["author"]["__typename"] = json!("User");
        let mut second = thread(connection(vec![comment(2)], false, None));
        second["id"] = json!("T2");
        let threads = fetch(vec![
            outer(vec![first.clone()], true, Some("outer1")),
            replies(vec![comment(1)], false, None),
            outer(vec![first, second], false, None),
        ])
        .unwrap();
        assert_eq!(threads.len(), 2);
        assert_eq!(
            threads[0]
                .comments
                .iter()
                .map(|c| c.node_id.as_str())
                .collect::<Vec<_>>(),
            vec!["C0", "C1"]
        );
        assert_eq!(threads[0].original_start_line, Some(4));
        assert_eq!(threads[0].start_side, Some(Side::Left));
        assert_eq!(threads[0].viewer_can_resolve, Some(true));
        assert_eq!(threads[0].viewer_can_unresolve, Some(false));
        assert_eq!(threads[0].comments[0].author_node_id.as_deref(), Some("U1"));
        assert_eq!(threads[0].comments[0].author_type.as_deref(), Some("User"));
        assert_eq!(threads[0].comments[0].updated_at.as_deref(), Some("later"));
    }
    #[test]
    fn rejects_nested_partial_errors_and_wrong_thread() {
        let first = outer(
            vec![thread(connection(vec![], true, Some("a")))],
            false,
            None,
        );
        let mut error = replies(vec![], false, None);
        error["errors"] = json!([{"message":"forbidden"}]);
        assert!(fetch(vec![first.clone(), error]).is_err());
        let mut wrong = replies(vec![], false, None);
        wrong["data"]["node"]["id"] = json!("other");
        assert!(fetch(vec![first, wrong]).is_err());
    }
    #[test]
    fn nested_limit_terminal_succeeds_and_continuation_fails() {
        for continuation in [false, true] {
            let mut pages = vec![outer(
                vec![thread(connection(vec![], true, Some("c0")))],
                false,
                None,
            )];
            pages.extend((1..PAGE_LIMIT).map(|i| {
                replies(
                    vec![],
                    i + 1 < PAGE_LIMIT || continuation,
                    Some(&format!("c{i}")),
                )
            }));
            assert_eq!(fetch(pages).is_err(), continuation);
        }
    }
    #[test]
    fn terminal_page_at_limit_succeeds_and_continuation_fails() {
        for continuation in [false, true] {
            let pages = (0..PAGE_LIMIT)
                .map(|i| {
                    outer(
                        vec![],
                        i + 1 < PAGE_LIMIT || continuation,
                        Some(&format!("c{i}")),
                    )
                })
                .collect();
            assert_eq!(fetch(pages).is_err(), continuation);
        }
    }
}
