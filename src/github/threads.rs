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

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{Completeness, GithubComment, GithubThread, Side};

use super::{GhRunner, PrRef, runner::run_json};

/// Safety cap per connection (100 items per page).
const MAX_PAGES: usize = 50;

/// One reason a thread import is not complete. This typed value is the source
/// of truth: `session` maps it to reason codes with an exhaustive `match`, and
/// the human-readable message is derived from it via [`Display`](std::fmt::Display).
///
/// `scope` is the rendered prefix naming what was being fetched, e.g.
/// `"review threads"`, `"thread T1: replies"` or `"replies of thread T1"`.
///
/// Persisted on `PrSnapshot`. Serialised as a `kind`-tagged object; snapshots
/// stored before this type existed held plain strings, which decode as
/// [`ThreadImportProblem::Legacy`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ThreadImportProblem {
    /// GitHub refused access: a GraphQL error of type `FORBIDDEN`, or an
    /// error whose text reports a permission denial.
    PermissionDenied {
        scope: String,
        detail: String,
        via: DeniedVia,
    },
    /// A GraphQL error that is not a permission denial.
    GraphqlError { scope: String, message: String },
    /// The `gh` call for a page failed outright.
    PageFetchFailed { scope: String, error: String },
    /// The response had neither `data` nor `errors`.
    NoData { scope: String },
    /// A review-threads page had no `reviewThreads` connection.
    MissingThreadsConnection,
    /// A replies page had no `comments` connection.
    MissingCommentsConnection { scope: String },
    /// A review thread node had no ID.
    UnreadableThread,
    /// A reply had no node ID.
    UnreadableReply { scope: String },
    /// `pageInfo` was missing, so completeness cannot be confirmed.
    MissingPageInfo { scope: String },
    /// `hasNextPage` was true but no `endCursor` was given.
    MissingCursor { scope: String },
    /// The next cursor equalled the previous one.
    StalledCursor { scope: String },
    /// Stopped after `max_pages` pages with more remaining.
    PageLimitReached { scope: String, max_pages: usize },
    /// A plain-string problem from a snapshot stored before this type
    /// existed. `legacy_kind` was classified once, at decode time.
    Legacy {
        message: String,
        legacy_kind: LegacyKind,
    },
}

/// How a permission denial was reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeniedVia {
    GraphqlError,
    FailedRequest,
}

/// What a legacy plain-string problem was understood to mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyKind {
    PermissionDenied,
    PageLimitReached,
    Other,
}

impl ThreadImportProblem {
    /// Compatibility for older tests that inspect the rendered diagnostic text.
    #[cfg(test)]
    pub fn contains(&self, needle: &str) -> bool {
        self.to_string().contains(needle)
    }

    /// Wraps a pre-typed stored message. The wording checks here are frozen:
    /// they describe text that older builds already wrote, not live messages,
    /// so rewording a [`Display`](std::fmt::Display) arm cannot affect them.
    pub fn legacy(message: String) -> Self {
        let kind = if mentions_permission_denial(&message) {
            LegacyKind::PermissionDenied
        } else if message
            .to_ascii_lowercase()
            .contains("pages; stopped early")
        {
            LegacyKind::PageLimitReached
        } else {
            LegacyKind::Other
        };
        Self::Legacy {
            message,
            legacy_kind: kind,
        }
    }
}

impl std::fmt::Display for ThreadImportProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PermissionDenied {
                scope,
                detail,
                via: DeniedVia::GraphqlError,
            }
            | Self::GraphqlError {
                scope,
                message: detail,
            } => write!(f, "{scope}: GraphQL error: {detail}"),
            Self::PermissionDenied {
                scope,
                detail,
                via: DeniedVia::FailedRequest,
            }
            | Self::PageFetchFailed {
                scope,
                error: detail,
            } => write!(f, "{scope}: {detail}"),
            Self::NoData { scope } => write!(f, "{scope}: response had no data"),
            Self::MissingThreadsConnection => {
                f.write_str("review threads page had no reviewThreads connection")
            }
            Self::MissingCommentsConnection { scope } => {
                write!(f, "{scope}: page had no comments connection")
            }
            Self::UnreadableThread => f.write_str("a review thread could not be read (missing ID)"),
            Self::UnreadableReply { scope } => {
                write!(f, "{scope}: a reply could not be read (missing ID)")
            }
            Self::MissingPageInfo { scope } => {
                write!(
                    f,
                    "{scope}: pageInfo missing; cannot confirm every item was fetched"
                )
            }
            Self::MissingCursor { scope } => {
                write!(f, "{scope}: next page reported without a cursor")
            }
            Self::StalledCursor { scope } => write!(f, "{scope}: cursor did not advance"),
            Self::PageLimitReached { scope, max_pages } => {
                write!(f, "{scope} exceed {max_pages} pages; stopped early")
            }
            Self::Legacy { message, .. } => f.write_str(message),
        }
    }
}

impl<'de> Deserialize<'de> for ThreadImportProblem {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(
            tag = "kind",
            rename_all = "snake_case",
            rename_all_fields = "camelCase"
        )]
        enum Typed {
            PermissionDenied {
                scope: String,
                detail: String,
                via: DeniedVia,
            },
            GraphqlError {
                scope: String,
                message: String,
            },
            PageFetchFailed {
                scope: String,
                error: String,
            },
            NoData {
                scope: String,
            },
            MissingThreadsConnection,
            MissingCommentsConnection {
                scope: String,
            },
            UnreadableThread,
            UnreadableReply {
                scope: String,
            },
            MissingPageInfo {
                scope: String,
            },
            MissingCursor {
                scope: String,
            },
            StalledCursor {
                scope: String,
            },
            PageLimitReached {
                scope: String,
                max_pages: usize,
            },
            Legacy {
                message: String,
                legacy_kind: LegacyKind,
            },
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Stored {
            Plain(String),
            Typed(Typed),
        }
        Ok(match Stored::deserialize(d)? {
            Stored::Plain(message) => Self::legacy(message),
            Stored::Typed(t) => match t {
                Typed::PermissionDenied { scope, detail, via } => {
                    Self::PermissionDenied { scope, detail, via }
                }
                Typed::GraphqlError { scope, message } => Self::GraphqlError { scope, message },
                Typed::PageFetchFailed { scope, error } => Self::PageFetchFailed { scope, error },
                Typed::NoData { scope } => Self::NoData { scope },
                Typed::MissingThreadsConnection => Self::MissingThreadsConnection,
                Typed::MissingCommentsConnection { scope } => {
                    Self::MissingCommentsConnection { scope }
                }
                Typed::UnreadableThread => Self::UnreadableThread,
                Typed::UnreadableReply { scope } => Self::UnreadableReply { scope },
                Typed::MissingPageInfo { scope } => Self::MissingPageInfo { scope },
                Typed::MissingCursor { scope } => Self::MissingCursor { scope },
                Typed::StalledCursor { scope } => Self::StalledCursor { scope },
                Typed::PageLimitReached { scope, max_pages } => {
                    Self::PageLimitReached { scope, max_pages }
                }
                Typed::Legacy {
                    message,
                    legacy_kind,
                } => Self::Legacy {
                    message,
                    legacy_kind,
                },
            },
        })
    }
}

/// Whether GitHub's own error text reports a permission denial. Applied at
/// detection to text GitHub produced, never to our rendered messages.
fn mentions_permission_denial(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("forbidden") || lower.contains("permission") || lower.contains("not accessible")
}

/// A pagination problem before it is attached to a scope.
#[derive(Debug)]
enum CursorProblem {
    Stalled,
    MissingCursor,
    MissingPageInfo,
    InvalidCursor,
}

impl CursorProblem {
    fn at(self, scope: String) -> ThreadImportProblem {
        match self {
            Self::Stalled => ThreadImportProblem::StalledCursor { scope },
            Self::MissingCursor => ThreadImportProblem::MissingCursor { scope },
            Self::MissingPageInfo => ThreadImportProblem::MissingPageInfo { scope },
            Self::InvalidCursor => ThreadImportProblem::PageFetchFailed {
                scope,
                error: "missing or invalid endCursor field".into(),
            },
        }
    }
}

/// A problem reading one comments page, before it is attached to a scope.
#[derive(Debug)]
enum PageProblem {
    UnreadableReply,
    Malformed(String),
    Replies(CursorProblem),
}

impl PageProblem {
    fn at(self, scope: String) -> ThreadImportProblem {
        match self {
            Self::UnreadableReply => ThreadImportProblem::UnreadableReply { scope },
            Self::Malformed(error) => ThreadImportProblem::PageFetchFailed { scope, error },
            Self::Replies(c) => c.at(format!("{scope}: replies")),
        }
    }
}

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
    pub problems: Vec<ThreadImportProblem>,
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
            problems.push(ThreadImportProblem::MissingThreadsConnection);
            break;
        }
        let Some(nodes) = page["nodes"].as_array() else {
            problems.push(ThreadImportProblem::PageFetchFailed {
                scope: "review threads".into(),
                error: "missing nodes array".into(),
            });
            break;
        };
        fetched_any_page = true;
        for (index, node) in nodes.iter().enumerate() {
            let Some(id) = valid_id(node) else {
                problems.push(ThreadImportProblem::UnreadableThread);
                continue;
            };
            if let Some(previous) = seen_threads.get(id) {
                if previous != node {
                    problems.push(ThreadImportProblem::PageFetchFailed {
                        scope: "review threads".into(),
                        error: format!("conflicting duplicate thread ID {id}"),
                    });
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
                Err(reason) => problems.push(reason.at(format!("thread {}", thread.node_id))),
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
                problems.push(ThreadImportProblem::PageFetchFailed {
                    scope: "review threads".into(),
                    error: "cyclic cursor".into(),
                });
                break;
            }
            Err(reason) => {
                problems.push(reason.at("review threads".into()));
                break;
            }
        }
    }
    if !finished && problems.is_empty() {
        problems.push(ThreadImportProblem::PageLimitReached {
            scope: "review threads".into(),
            max_pages: MAX_PAGES,
        });
    }
    let completeness = if problems.is_empty() {
        Completeness::Complete
    } else if fetched_any_page {
        Completeness::Partial
    } else {
        Completeness::Failed
    };
    // REST review comments supplement GraphQL's thread connection and provide
    // stable database IDs even when the GraphQL connection is inaccessible.
    let rest = fetch_rest(gh, pr_ref);
    let mut completeness = completeness;
    match rest {
        Ok(rows) => merge_rest(&mut threads, rows),
        Err(error) => {
            problems.push(ThreadImportProblem::PageFetchFailed {
                scope: "REST review comments".into(),
                error: error.to_string(),
            });
            completeness = if threads.is_empty() {
                Completeness::Failed
            } else {
                Completeness::Partial
            };
        }
    }
    if !problems.is_empty() && !threads.is_empty() {
        completeness = Completeness::Partial;
    }
    let complete = completeness == Completeness::Complete;
    for thread in &mut threads {
        thread.enumeration_complete = complete;
    }
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
    problems: &mut Vec<ThreadImportProblem>,
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
            problems.push(ThreadImportProblem::PageFetchFailed {
                scope: what,
                error: "comments returned wrong thread ID".into(),
            });
            return;
        }
        let conn = &v["data"]["node"]["comments"];
        if !conn.is_object() {
            problems.push(ThreadImportProblem::MissingCommentsConnection { scope: what });
            return;
        }
        let collected = collect_comments(conn, &mut thread.comments, seen);
        mark_reply_errors(&v, conn, thread);
        match collected {
            Ok(None) => return,
            Ok(Some(next)) if !seen_cursors.insert(next.clone()) => {
                problems.push(ThreadImportProblem::StalledCursor { scope: what });
                return;
            }
            Ok(Some(next)) => cursor = next,
            Err(reason) => {
                problems.push(reason.at(what));
                return;
            }
        }
    }
    problems.push(ThreadImportProblem::PageLimitReached {
        scope: format!("replies of thread {}", thread.node_id),
        max_pages: MAX_PAGES,
    });
}

/// Run one GraphQL page. Records a problem and returns `None` when the page
/// failed outright; records GraphQL `errors` (field-level permission denials
/// and the like) but still returns the partial data.
fn fetch_page(
    gh: &dyn GhRunner,
    args: &[&str],
    problems: &mut Vec<ThreadImportProblem>,
    what: &str,
) -> Option<Value> {
    let v: Value = match run_json(gh, args) {
        Ok(v) => v,
        Err(e) => {
            let error = e.to_string();
            problems.push(if mentions_permission_denial(&error) {
                ThreadImportProblem::PermissionDenied {
                    scope: what.into(),
                    detail: error,
                    via: DeniedVia::FailedRequest,
                }
            } else {
                ThreadImportProblem::PageFetchFailed {
                    scope: what.into(),
                    error,
                }
            });
            return None;
        }
    };
    for err in v["errors"].as_array().into_iter().flatten() {
        let msg = err["message"].as_str().unwrap_or("unknown GraphQL error");
        let forbidden = err["type"].as_str() == Some("FORBIDDEN");
        problems.push(if forbidden || mentions_permission_denial(msg) {
            ThreadImportProblem::PermissionDenied {
                scope: what.into(),
                detail: msg.into(),
                via: DeniedVia::GraphqlError,
            }
        } else {
            ThreadImportProblem::GraphqlError {
                scope: what.into(),
                message: msg.into(),
            }
        });
    }
    if v["data"].is_null() {
        if v["errors"].as_array().is_none_or(|e| e.is_empty()) {
            problems.push(ThreadImportProblem::NoData { scope: what.into() });
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
) -> Result<Option<String>, PageProblem> {
    let nodes = conn["nodes"]
        .as_array()
        .ok_or(PageProblem::Malformed("missing replies nodes array".into()))?;
    let mut problem = None;
    for c in nodes {
        let Some(id) = valid_id(c) else {
            problem.get_or_insert(PageProblem::UnreadableReply);
            continue;
        };
        if let Some(previous) = seen.get(id) {
            if previous != c {
                problem.get_or_insert(PageProblem::Malformed(format!(
                    "conflicting duplicate comment ID {id}"
                )));
            }
            continue;
        }
        out.push(parse_comment(c));
        seen.insert(id.to_owned(), c.clone());
    }
    if let Some(problem) = problem {
        return Err(problem);
    }
    next_cursor(&conn["pageInfo"], None).map_err(PageProblem::Replies)
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
fn next_cursor(page_info: &Value, previous: Option<&str>) -> Result<Option<String>, CursorProblem> {
    if page_info.get("endCursor").is_none()
        || (!page_info["endCursor"].is_null() && !page_info["endCursor"].is_string())
    {
        return Err(CursorProblem::InvalidCursor);
    }
    match page_info["hasNextPage"].as_bool() {
        Some(false) => Ok(None),
        Some(true) => match page_info["endCursor"].as_str() {
            Some(c) if c.is_empty() || Some(c) == previous => Err(CursorProblem::Stalled),
            Some(c) => Ok(Some(c.to_string())),
            None => Err(CursorProblem::MissingCursor),
        },
        None => Err(CursorProblem::MissingPageInfo),
    }
}

fn num(v: &Value, k: &str) -> Option<u32> {
    v[k].as_u64().map(|n| n as u32)
}

fn string(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

fn fetch_rest(gh: &dyn GhRunner, pr_ref: &PrRef) -> anyhow::Result<Vec<Value>> {
    use anyhow::bail;
    let mut all = Vec::new();
    for page in 1..=500 {
        let endpoint = format!("{}/comments?per_page=100&page={page}", pr_ref.api_base());
        let rows: Vec<Value> = run_json(gh, &["api", &endpoint])?;
        let count = rows.len();
        for row in &rows {
            if row["id"].as_i64().is_none()
                || row["body"].as_str().is_none()
                || row["path"].as_str().is_none()
                || row["created_at"].as_str().is_none()
            {
                bail!("malformed REST review comment on page {page}");
            }
        }
        all.extend(rows);
        if count < 100 {
            return Ok(all);
        }
    }
    bail!("too many REST review comment pages")
}

fn merge_rest(threads: &mut Vec<GithubThread>, rows: Vec<Value>) {
    let mut locations = HashMap::new();
    let mut nodes = HashMap::new();
    for (ti, thread) in threads.iter().enumerate() {
        for (ci, comment) in thread.comments.iter().enumerate() {
            if let Some(id) = comment.database_id {
                locations.insert(id, (ti, ci));
            }
            nodes.insert(comment.node_id.clone(), (ti, ci));
        }
    }
    let roots: HashMap<i64, i64> = rows
        .iter()
        .filter_map(|r| {
            let id = r["id"].as_i64()?;
            Some((id, r["in_reply_to_id"].as_i64().unwrap_or(id)))
        })
        .collect();
    for row in rows {
        let (Some(id), Some(path), Some(body), Some(created_at)) = (
            row["id"].as_i64(),
            row["path"].as_str(),
            row["body"].as_str(),
            row["created_at"].as_str(),
        ) else {
            continue;
        };
        let node = row["node_id"].as_str().unwrap_or_default();
        let parent = row["in_reply_to_id"].as_i64();
        let root = parent
            .map(|p| roots.get(&p).copied().unwrap_or(p))
            .unwrap_or(id);
        let found = locations
            .get(&id)
            .copied()
            .or_else(|| nodes.get(node).copied());
        let ti = found
            .map(|(ti, _)| ti)
            .or_else(|| locations.get(&root).map(|(ti, _)| *ti))
            .or_else(|| {
                threads
                    .iter()
                    .position(|t| t.node_id == format!("rest:{root}"))
            })
            .unwrap_or_else(|| {
                threads.push(GithubThread {
                    node_id: format!("rest:{root}"),
                    path: path.into(),
                    side: row["side"].as_str().and_then(Side::parse),
                    line: None,
                    start_line: None,
                    original_line: None,
                    is_outdated: false,
                    is_resolved: false,
                    comments: Vec::new(),
                    rest_only: true,
                    enumeration_complete: false,
                    url: None,
                    original_start_line: None,
                    start_side: None,
                    subject_type: None,
                    resolved_by: None,
                    viewer_can_reply: None,
                    viewer_can_resolve: None,
                    viewer_can_unresolve: None,
                    unavailable_fields: Vec::new(),
                });
                threads.len() - 1
            });
        let comment = GithubComment {
            node_id: node.into(),
            database_id: Some(id),
            author: row["user"]["login"].as_str().unwrap_or("ghost").into(),
            body: body.into(),
            created_at: created_at.into(),
            in_reply_to_id: parent,
            author_id: None,
            author_type: None,
            updated_at: row["updated_at"].as_str().map(str::to_owned),
            url: row["html_url"].as_str().map(str::to_owned),
            line: None,
            original_line: None,
            commit_oid: None,
            original_commit_oid: None,
            outdated: None,
            unavailable_fields: Vec::new(),
        };
        if let Some((old_ti, ci)) = found {
            let old = &mut threads[old_ti].comments[ci];
            old.database_id = Some(id);
            old.in_reply_to_id = parent;
            // A field-level GraphQL error must not overwrite a valid REST value.
            if unavailable_field(&old.unavailable_fields, "body") {
                old.body = comment.body;
            }
            if unavailable_field(&old.unavailable_fields, "author") {
                old.author = comment.author;
            }
            if unavailable_field(&old.unavailable_fields, "updatedAt") {
                old.updated_at = comment.updated_at;
            }
        } else {
            let ci = threads[ti].comments.len();
            threads[ti].comments.push(comment);
            locations.insert(id, (ti, ci));
        }
    }
    for thread in threads {
        thread
            .comments
            .sort_by_key(|c| (c.created_at.clone(), c.database_id.unwrap_or_default()));
        if thread.url.is_none() {
            thread.url = thread.comments.first().and_then(|c| c.url.clone());
        }
    }
}

fn unavailable_field(fields: &[String], name: &str) -> bool {
    fields.iter().any(|field| field == "*" || field == name)
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
        rest_only: false,
        enumeration_complete: false,
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
        in_reply_to_id: None,
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
