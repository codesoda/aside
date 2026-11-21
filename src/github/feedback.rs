//! Independent, bounded imports of PR reviews and issue comments.
use std::collections::HashSet;

use serde_json::Value;

use crate::model::{Completeness, ImportedReview, IssueComment};

use super::{GhRunner, PrRef, runner::run_graphql_json};

const MAX_PAGES: usize = 50;

#[derive(Debug)]
pub(super) struct FeedbackImport<T> {
    pub records: Vec<T>,
    pub completeness: Completeness,
    pub problems: Vec<String>,
}

const AUTHOR: &str = "author { __typename login ... on User { id } ... on Bot { id } ... on Mannequin { id } ... on Organization { id } ... on EnterpriseUserAccount { id } }";

fn query(kind: &str) -> String {
    let fields = if kind == "reviews" {
        "id databaseId url body state createdAt updatedAt lastEditedAt submittedAt"
    } else {
        "id databaseId url body createdAt updatedAt lastEditedAt"
    };
    format!(
        "query($owner: String!, $repo: String!, $number: Int!, $cursor: String) {{ repository(owner: $owner, name: $repo) {{ pullRequest(number: $number) {{ {kind}(first: 100, after: $cursor) {{ pageInfo {{ hasNextPage endCursor }} nodes {{ {fields} {AUTHOR} }} }} }} }} }}"
    )
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn author(v: &Value) -> String {
    v.get("author")
        .and_then(|a| text(a, "login"))
        .unwrap_or_else(|| "ghost".into())
}

fn import<T>(
    gh: &dyn GhRunner,
    pr: &PrRef,
    kind: &str,
    convert: impl Fn(&Value) -> T,
) -> FeedbackImport<T> {
    let mut records = Vec::new();
    let mut problems = Vec::new();
    let mut ids = HashSet::new();
    let mut cursors = HashSet::new();
    let mut cursor: Option<String> = None;
    let q = format!("query={}", query(kind));
    let owner = format!("owner={}", pr.owner);
    let repo = format!("repo={}", pr.repo);
    let number = format!("number={}", pr.number);
    let mut finished = false;
    let mut fetched = false;
    let mut invalid_nodes = false;
    for page_number in 0..MAX_PAGES {
        let cursor_arg = cursor.as_ref().map(|c| format!("cursor={c}"));
        let mut args = vec![
            "api", "graphql", "-f", &q, "-f", &owner, "-f", &repo, "-F", &number,
        ];
        if let Some(ref arg) = cursor_arg {
            args.extend(["-f", arg]);
        }
        let response: Value = match run_graphql_json(gh, &args) {
            Ok(v) => v,
            Err(e) => {
                problems.push(format!("{kind}: page fetch failed: {e}"));
                break;
            }
        };
        if let Some(errors) = response.get("errors") {
            if let Some(errors) = errors.as_array() {
                for error in errors {
                    problems.push(format!(
                        "{kind}: GraphQL error: {}",
                        error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown error")
                    ));
                }
            } else {
                problems.push(format!("{kind}: malformed GraphQL errors"));
            }
        }
        let Some(connection) = response.pointer(&format!("/data/repository/pullRequest/{kind}"))
        else {
            problems.push(format!("{kind}: missing connection"));
            break;
        };
        let Some(nodes) = connection.get("nodes").and_then(Value::as_array) else {
            problems.push(format!("{kind}: missing nodes array"));
            break;
        };
        fetched = true;
        for node in nodes {
            let Some(id) = node
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
            else {
                invalid_nodes = true;
                problems.push(format!("{kind}: node missing ID"));
                continue;
            };
            let required = if kind == "reviews" {
                &["state", "body"][..]
            } else {
                &["body"][..]
            };
            if let Some(field) = required.iter().find(|field| text(node, field).is_none()) {
                invalid_nodes = true;
                problems.push(format!("{kind}: node {id} missing or malformed {field}"));
                continue;
            }
            if ids.insert(id.to_owned()) {
                records.push(convert(node));
            }
        }
        let Some(info) = connection.get("pageInfo") else {
            problems.push(format!("{kind}: missing pageInfo"));
            break;
        };
        let Some(has_next) = info.get("hasNextPage").and_then(Value::as_bool) else {
            problems.push(format!("{kind}: malformed pageInfo"));
            break;
        };
        if !has_next {
            finished = true;
            break;
        }
        if page_number + 1 == MAX_PAGES {
            problems.push(format!("{kind}: exceeded {MAX_PAGES} pages; stopped early"));
            break;
        }
        let Some(next) = info
            .get("endCursor")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            problems.push(format!("{kind}: next page missing cursor"));
            break;
        };
        if !cursors.insert(next.to_owned()) || cursor.as_deref() == Some(next) {
            problems.push(format!("{kind}: cyclic cursor"));
            break;
        }
        cursor = Some(next.to_owned());
    }
    let completeness = if finished && problems.is_empty() {
        Completeness::Complete
    } else if !records.is_empty() || (fetched && !invalid_nodes) {
        Completeness::Partial
    } else {
        Completeness::Failed
    };
    FeedbackImport {
        records,
        completeness,
        problems,
    }
}

pub(super) fn fetch_reviews(gh: &dyn GhRunner, pr: &PrRef) -> FeedbackImport<ImportedReview> {
    import(gh, pr, "reviews", |v| ImportedReview {
        node_id: text(v, "id"),
        database_id: v.get("databaseId").and_then(Value::as_u64),
        url: text(v, "url"),
        author_id: v.get("author").and_then(|a| text(a, "id")),
        author_type: v.get("author").and_then(|a| text(a, "__typename")),
        created_at: text(v, "createdAt"),
        updated_at: text(v, "updatedAt"),
        last_edited_at: text(v, "lastEditedAt"),
        submitted_at: text(v, "submittedAt"),
        author: author(v),
        state: text(v, "state").unwrap_or_default(),
        body: text(v, "body").unwrap_or_default(),
    })
}

pub(super) fn fetch_comments(gh: &dyn GhRunner, pr: &PrRef) -> FeedbackImport<IssueComment> {
    import(gh, pr, "comments", |v| IssueComment {
        node_id: text(v, "id"),
        database_id: v.get("databaseId").and_then(Value::as_u64),
        url: text(v, "url"),
        author_id: v.get("author").and_then(|a| text(a, "id")),
        author_type: v.get("author").and_then(|a| text(a, "__typename")),
        created_at: text(v, "createdAt"),
        updated_at: text(v, "updatedAt"),
        last_edited_at: text(v, "lastEditedAt"),
        author: author(v),
        body: text(v, "body").unwrap_or_default(),
    })
}
