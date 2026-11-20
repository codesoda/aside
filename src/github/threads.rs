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

use std::collections::HashSet;

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
    let mut seen_threads = HashSet::new();
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
        fetched_any_page = true;
        for node in page["nodes"].as_array().into_iter().flatten() {
            if node.is_null() {
                problems.push("a review thread could not be read (null node)".into());
                continue;
            }
            let mut thread = parse_thread(node);
            if !seen_threads.insert(thread.node_id.clone()) {
                continue;
            }
            let mut seen = HashSet::new();
            let first = collect_comments(&node["comments"], &mut thread.comments, &mut seen);
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
            Ok(Some(next)) => cursor = Some(next),
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
    seen: &mut HashSet<String>,
    mut cursor: String,
    problems: &mut Vec<String>,
) {
    let query = format!("query={}", replies_query());
    let id = format!("id={}", thread.node_id);
    for _ in 0..MAX_PAGES {
        let cursor_arg = format!("cursor={cursor}");
        let args = ["api", "graphql", "-f", &query, "-f", &id, "-f", &cursor_arg];
        let what = format!("replies of thread {}", thread.node_id);
        let Some(v) = fetch_page(gh, &args, problems, &what) else {
            return;
        };
        let conn = &v["data"]["node"]["comments"];
        if !conn.is_object() {
            problems.push(format!("{what}: page had no comments connection"));
            return;
        }
        match collect_comments(conn, &mut thread.comments, seen) {
            Ok(None) => return,
            Ok(Some(next)) if next == cursor => {
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
    seen: &mut HashSet<String>,
) -> Result<Option<String>, String> {
    for c in conn["nodes"].as_array().into_iter().flatten() {
        let comment = parse_comment(c);
        if comment.node_id.is_empty() {
            return Err("a reply could not be read (missing ID)".into());
        }
        if seen.insert(comment.node_id.clone()) {
            out.push(comment);
        }
    }
    next_cursor(&conn["pageInfo"], None).map_err(|r| format!("replies: {r}"))
}

/// `Ok(None)` when there are no more pages. Missing `pageInfo` is an error:
/// without it we cannot claim to have every item.
fn next_cursor(page_info: &Value, previous: Option<&str>) -> Result<Option<String>, String> {
    match page_info["hasNextPage"].as_bool() {
        Some(false) => Ok(None),
        Some(true) => match page_info["endCursor"].as_str() {
            Some(c) if Some(c) == previous => Err("cursor did not advance".into()),
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
    }
}

#[cfg(test)]
#[path = "threads_tests.rs"]
mod tests;
