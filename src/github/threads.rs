use anyhow::{Result, bail};
use serde_json::Value;

use crate::model::{GithubComment, GithubThread, Side};

use super::{GhRunner, PrRef, runner::run_json};

const THREADS_QUERY: &str =
    "query($owner: String!, $repo: String!, $number: Int!, $cursor: String) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      reviewThreads(first: 100, after: $cursor) {
        pageInfo { hasNextPage endCursor }
        nodes {
          id isResolved isOutdated path line originalLine startLine diffSide
          comments(first: 100) {
            nodes { id databaseId body createdAt author { login } }
          }
        }
      }
    }
  }
}";

pub(super) fn fetch_threads(gh: &dyn GhRunner, pr_ref: &PrRef) -> Result<Vec<GithubThread>> {
    let mut threads = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..50 {
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
        for node in page["nodes"].as_array().into_iter().flatten() {
            threads.push(parse_thread(node));
        }
        if page["pageInfo"]["hasNextPage"].as_bool() != Some(true) {
            return Ok(threads);
        }
        cursor = page["pageInfo"]["endCursor"].as_str().map(str::to_string);
    }
    bail!("too many review thread pages")
}

fn parse_thread(node: &Value) -> GithubThread {
    let num = |k: &str| node[k].as_u64().map(|n| n as u32);
    GithubThread {
        node_id: node["id"].as_str().unwrap_or_default().into(),
        path: node["path"].as_str().unwrap_or_default().into(),
        side: node["diffSide"].as_str().and_then(Side::parse),
        line: num("line"),
        start_line: num("startLine"),
        original_line: num("originalLine"),
        is_outdated: node["isOutdated"].as_bool().unwrap_or(false),
        is_resolved: node["isResolved"].as_bool().unwrap_or(false),
        comments: node["comments"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| GithubComment {
                node_id: c["id"].as_str().unwrap_or_default().into(),
                database_id: c["databaseId"].as_i64(),
                author: c["author"]["login"].as_str().unwrap_or("ghost").into(),
                body: c["body"].as_str().unwrap_or_default().into(),
                created_at: c["createdAt"].as_str().unwrap_or_default().into(),
            })
            .collect(),
    }
}
