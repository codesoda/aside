//! Review-thread import over `gh api graphql`.
//!
//! Both connections are cursor-paginated: top-level `reviewThreads`, and each
//! thread's nested `comments`. `pageInfo` is requested on both. Replies are
//! de-duplicated by node ID so overlapping pages never import a reply twice.
//!
//! Anything short of a full import (GraphQL `errors`, a failed or malformed
//! page, a stalled cursor, the page cap) is reported as
//! [`Completeness::Partial`] or [`Completeness::Failed`] with a reason, never
//! as a successful complete import.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::model::{Completeness, GithubComment, GithubThread, Side};

use super::{GhRunner, PrRef, runner::run_json};

/// Safety cap per connection (100 items per page).
const MAX_PAGES: usize = 50;

const COMMENT_FIELDS: &str = "pageInfo { hasNextPage endCursor }
        nodes {
          id databaseId body createdAt updatedAt url outdated line originalLine
          commit { oid } originalCommit { oid }
          author { __typename login ... on User { id } ... on Bot { id } ... on Mannequin { id } ... on Organization { id } ... on EnterpriseUserAccount { id } }
        }";

fn threads_query() -> String {
    format!(
        "query($owner: String!, $repo: String!, $number: Int!, $cursor: String) {{
  repository(owner: $owner, name: $repo) {{
    pullRequest(number: $number) {{
      reviewThreads(first: 100, after: $cursor) {{
        pageInfo {{ hasNextPage endCursor }}
        nodes {{
          id isResolved isOutdated path line originalLine startLine originalStartLine
          diffSide startDiffSide subjectType
          viewerCanReply viewerCanResolve viewerCanUnresolve
          resolvedBy {{ login }}
          comments(first: 100) {{ {COMMENT_FIELDS} }}
        }}
      }}
    }}
  }}
}}"
    )
}

fn replies_query() -> String {
    format!(
        "query($id: ID!, $cursor: String) {{
  node(id: $id) {{
    ... on PullRequestReviewThread {{
      id
      comments(first: 100, after: $cursor) {{ {COMMENT_FIELDS} }}
    }}
  }}
}}"
    )
}

/// Result of a thread import. `threads` holds whatever was fetched, even when
/// the import is incomplete.
#[derive(Debug)]
pub(super) struct ThreadImport {
    pub threads: Vec<GithubThread>,
    pub completeness: Completeness,
    /// Why the import is not complete. Empty when `completeness` is complete.
    pub problems: Vec<String>,
}

pub(super) fn fetch_threads(gh: &dyn GhRunner, pr_ref: &PrRef) -> ThreadImport {
    let mut threads: Vec<GithubThread> = Vec::new();
    let mut seen_threads: HashMap<String, Value> = HashMap::new();
    let mut seen_cursors = HashSet::new();
    let mut problems = Vec::new();
    let mut fetched_any_page = false;
    let query = format!("query={}", threads_query());
    let owner = format!("owner={}", pr_ref.owner);
    let repo = format!("repo={}", pr_ref.repo);
    let number = format!("number={}", pr_ref.number);
    let mut cursor: Option<String> = None;
    let mut finished = false;
    for _ in 0..MAX_PAGES {
        let mut args = vec![
            "api", "graphql", "-f", &query, "-f", &owner, "-f", &repo, "-F", &number,
        ];
        let cursor_arg = cursor.as_ref().map(|c| format!("cursor={c}"));
        if let Some(c) = &cursor_arg {
            args.extend(["-f", c]);
        }
        let v = match fetch_page(gh, &args, &mut problems, "review threads") {
            Some(v) => v,
            None => break,
        };
        let page = &v["data"]["repository"]["pullRequest"]["reviewThreads"];
        if !page.is_object() {
            problems.push("review threads page had no reviewThreads connection".into());
            break;
        }
        let Some(nodes) = page["nodes"].as_array() else {
            problems.push("review threads: missing nodes array".into());
            break;
        };
        fetched_any_page = true;
        for (index, node) in nodes.iter().enumerate() {
            let Some(id) = valid_id(node) else {
                problems.push("a review thread could not be read (missing ID)".into());
                continue;
            };
            if let Some(previous) = seen_threads.get(id) {
                if previous != node {
                    problems.push(format!("conflicting duplicate thread ID {id}"));
                }
                continue;
            }
            let mut thread = parse_thread(node);
            seen_threads.insert(id.to_owned(), node.clone());
            let mut seen = HashMap::new();
            let first = collect_comments(&node["comments"], &mut thread.comments, &mut seen);
            mark_errors(
                &v,
                &["repository", "pullRequest", "reviewThreads", "nodes"],
                index,
                &node["comments"],
                &mut thread,
            );
            match first {
                Err(reason) => problems.push(format!("thread {}: {reason}", thread.node_id)),
                Ok(Some(next)) => {
                    fetch_remaining_replies(gh, &mut thread, &mut seen, next, &mut problems)
                }
                Ok(None) => {}
            }
            thread.url = thread.comments.first().and_then(|c| c.url.clone());
            threads.push(thread);
        }
        match next_cursor(&page["pageInfo"], cursor.as_deref()) {
            Ok(None) => {
                finished = true;
                break;
            }
            Ok(Some(next)) if seen_cursors.insert(next.clone()) => cursor = Some(next),
            Ok(Some(_)) => {
                problems.push("review threads: cyclic cursor".into());
                break;
            }
            Err(reason) => {
                problems.push(format!("review threads: {reason}"));
                break;
            }
        }
    }
    if !finished && problems.is_empty() {
        problems.push(format!(
            "review threads exceed {MAX_PAGES} pages; stopped early"
        ));
    }
    let completeness = if problems.is_empty() {
        Completeness::Complete
    } else if fetched_any_page {
        Completeness::Partial
    } else {
        Completeness::Failed
    };
    ThreadImport {
        threads,
        completeness,
        problems,
    }
}

/// Page the rest of one thread's `comments` connection.
fn fetch_remaining_replies(
    gh: &dyn GhRunner,
    thread: &mut GithubThread,
    seen: &mut HashMap<String, Value>,
    mut cursor: String,
    problems: &mut Vec<String>,
) {
    let query = format!("query={}", replies_query());
    let id = format!("id={}", thread.node_id);
    let mut seen_cursors = HashSet::from([cursor.clone()]);
    for _ in 1..MAX_PAGES {
        let cursor_arg = format!("cursor={cursor}");
        let args = ["api", "graphql", "-f", &query, "-f", &id, "-f", &cursor_arg];
        let what = format!("replies of thread {}", thread.node_id);
        let Some(v) = fetch_page(gh, &args, problems, &what) else {
            return;
        };
        if valid_id(&v["data"]["node"]) != Some(thread.node_id.as_str()) {
            problems.push(format!("{what}: comments returned wrong thread ID"));
            return;
        }
        let conn = &v["data"]["node"]["comments"];
        if !conn.is_object() {
            problems.push(format!("{what}: page had no comments connection"));
            return;
        }
        let collected = collect_comments(conn, &mut thread.comments, seen);
        mark_reply_errors(&v, conn, thread);
        match collected {
            Ok(None) => return,
            Ok(Some(next)) if !seen_cursors.insert(next.clone()) => {
                problems.push(format!("{what}: cursor did not advance"));
                return;
            }
            Ok(Some(next)) => cursor = next,
            Err(reason) => {
                problems.push(format!("{what}: {reason}"));
                return;
            }
        }
    }
    problems.push(format!(
        "replies of thread {} exceed {MAX_PAGES} pages; stopped early",
        thread.node_id
    ));
}

/// Run one GraphQL page. Records a problem and returns `None` when the page
/// failed outright; records GraphQL `errors` (field-level permission denials
/// and the like) but still returns the partial data.
fn fetch_page(
    gh: &dyn GhRunner,
    args: &[&str],
    problems: &mut Vec<String>,
    what: &str,
) -> Option<Value> {
    let v: Value = match run_json(gh, args) {
        Ok(v) => v,
        Err(e) => {
            problems.push(format!("{what}: {e}"));
            return None;
        }
    };
    for err in v["errors"].as_array().into_iter().flatten() {
        let msg = err["message"].as_str().unwrap_or("unknown GraphQL error");
        problems.push(format!("{what}: GraphQL error: {msg}"));
    }
    if v["data"].is_null() {
        if v["errors"].as_array().is_none_or(|e| e.is_empty()) {
            problems.push(format!("{what}: response had no data"));
        }
        return None;
    }
    Some(v)
}

/// Append unseen comments from one connection page. Returns the cursor of the
/// next page, if any.
fn collect_comments(
    conn: &Value,
    out: &mut Vec<GithubComment>,
    seen: &mut HashMap<String, Value>,
) -> Result<Option<String>, String> {
    let nodes = conn["nodes"]
        .as_array()
        .ok_or("missing replies nodes array")?;
    for c in nodes {
        let id = valid_id(c).ok_or("a reply could not be read (missing ID)")?;
        if let Some(previous) = seen.get(id) {
            if previous != c {
                return Err(format!("conflicting duplicate comment ID {id}"));
            }
            continue;
        }
        out.push(parse_comment(c));
        seen.insert(id.to_owned(), c.clone());
    }
    next_cursor(&conn["pageInfo"], None).map_err(|r| format!("replies: {r}"))
}

/// Attach only errors targeting this record. A missing error path is treated
/// conservatively as unavailable metadata on every returned record.
fn retained_comment<'a>(
    thread: &'a mut GithubThread,
    conn: &Value,
    node_index: usize,
) -> Option<&'a mut GithubComment> {
    let id = valid_id(&conn["nodes"][node_index])?;
    thread
        .comments
        .iter_mut()
        .find(|comment| comment.node_id == id)
}

fn mark_errors(v: &Value, prefix: &[&str], index: usize, conn: &Value, thread: &mut GithubThread) {
    for error in v["errors"].as_array().into_iter().flatten() {
        let Some(path) = error["path"].as_array() else {
            thread.unavailable_fields.push("*".into());
            for comment in &mut thread.comments {
                comment.unavailable_fields.push("*".into());
            }
            continue;
        };
        if !prefix
            .iter()
            .enumerate()
            .all(|(i, part)| path.get(i).and_then(Value::as_str) == Some(*part))
            || path.get(prefix.len()).and_then(Value::as_u64) != Some(index as u64)
        {
            continue;
        }
        let rest = &path[prefix.len() + 1..];
        if rest.first().and_then(Value::as_str) == Some("comments") {
            if let Some(comment_index) = rest.get(2).and_then(Value::as_u64)
                && rest.get(1).and_then(Value::as_str) == Some("nodes")
                && let Some(comment) = retained_comment(thread, conn, comment_index as usize)
            {
                comment
                    .unavailable_fields
                    .push(rest.get(3).and_then(Value::as_str).unwrap_or("*").into());
            }
        } else {
            thread
                .unavailable_fields
                .push(rest.first().and_then(Value::as_str).unwrap_or("*").into());
        }
    }
}

fn mark_reply_errors(v: &Value, conn: &Value, thread: &mut GithubThread) {
    for error in v["errors"].as_array().into_iter().flatten() {
        let path = error["path"].as_array();
        let index = path.and_then(|p| {
            if p.first().and_then(Value::as_str) == Some("node")
                && p.get(1).and_then(Value::as_str) == Some("comments")
                && p.get(2).and_then(Value::as_str) == Some("nodes")
            {
                p.get(3).and_then(Value::as_u64)
            } else {
                None
            }
        });
        if let Some(index) = index {
            if let Some(comment) = retained_comment(thread, conn, index as usize) {
                comment.unavailable_fields.push(
                    path.and_then(|p| p.get(4))
                        .and_then(Value::as_str)
                        .unwrap_or("*")
                        .into(),
                );
            }
        } else if path.is_none()
            && let Some(nodes) = conn["nodes"].as_array()
        {
            for index in 0..nodes.len() {
                if let Some(comment) = retained_comment(thread, conn, index) {
                    comment.unavailable_fields.push("*".into());
                }
            }
        }
    }
}

fn valid_id(node: &Value) -> Option<&str> {
    node["id"].as_str().filter(|id| !id.is_empty())
}

/// `Ok(None)` when there are no more pages. Missing `pageInfo` is an error:
/// without it we cannot claim to have every item.
fn next_cursor(page_info: &Value, previous: Option<&str>) -> Result<Option<String>, String> {
    if page_info.get("endCursor").is_none()
        || (!page_info["endCursor"].is_null() && !page_info["endCursor"].is_string())
    {
        return Err("missing or invalid endCursor field".into());
    }
    match page_info["hasNextPage"].as_bool() {
        Some(false) => Ok(None),
        Some(true) => match page_info["endCursor"].as_str() {
            Some(c) if c.is_empty() || Some(c) == previous => Err("cursor did not advance".into()),
            Some(c) => Ok(Some(c.to_string())),
            None => Err("next page reported without a cursor".into()),
        },
        None => Err("pageInfo missing; cannot confirm every item was fetched".into()),
    }
}

fn num(v: &Value, k: &str) -> Option<u32> {
    v[k].as_u64().map(|n| n as u32)
}

fn string(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

fn parse_thread(node: &Value) -> GithubThread {
    GithubThread {
        node_id: node["id"].as_str().unwrap_or_default().into(),
        path: node["path"].as_str().unwrap_or_default().into(),
        side: node["diffSide"].as_str().and_then(Side::parse),
        line: num(node, "line"),
        start_line: num(node, "startLine"),
        original_line: num(node, "originalLine"),
        is_outdated: node["isOutdated"].as_bool().unwrap_or(false),
        is_resolved: node["isResolved"].as_bool().unwrap_or(false),
        comments: Vec::new(),
        url: None,
        original_start_line: num(node, "originalStartLine"),
        start_side: string(&node["startDiffSide"]),
        subject_type: string(&node["subjectType"]),
        resolved_by: string(&node["resolvedBy"]["login"]),
        viewer_can_reply: node["viewerCanReply"].as_bool(),
        viewer_can_resolve: node["viewerCanResolve"].as_bool(),
        viewer_can_unresolve: node["viewerCanUnresolve"].as_bool(),
        unavailable_fields: Vec::new(),
    }
}

fn parse_comment(c: &Value) -> GithubComment {
    GithubComment {
        node_id: c["id"].as_str().unwrap_or_default().into(),
        database_id: c["databaseId"].as_i64(),
        author: c["author"]["login"].as_str().unwrap_or("ghost").into(),
        body: c["body"].as_str().unwrap_or_default().into(),
        created_at: c["createdAt"].as_str().unwrap_or_default().into(),
        author_id: string(&c["author"]["id"]),
        author_type: string(&c["author"]["__typename"]),
        updated_at: string(&c["updatedAt"]),
        url: string(&c["url"]),
        line: num(c, "line"),
        original_line: num(c, "originalLine"),
        commit_oid: string(&c["commit"]["oid"]),
        original_commit_oid: string(&c["originalCommit"]["oid"]),
        outdated: c["outdated"].as_bool(),
        unavailable_fields: Vec::new(),
    }
}

#[cfg(test)]
#[path = "threads_tests.rs"]
mod tests;
