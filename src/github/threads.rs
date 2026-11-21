use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

use super::{GhRunner, PrRef, runner::run_json};
use crate::model::{GithubComment, GithubThread, Side};

const THREADS_QUERY: &str =
    "query($owner: String!, $repo: String!, $number: Int!, $cursor: String) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      reviewThreads(first: 100, after: $cursor) {
        pageInfo { hasNextPage endCursor }
        nodes {
          id isResolved isOutdated path line originalLine startLine diffSide
          comments(first: 100) {
            pageInfo { hasNextPage }
            nodes { id databaseId body createdAt updatedAt author { login } }
          }
        }
      }
    }
  }
}";

/// Returns merged threads and whether *both* sources were fully enumerated.
pub(super) fn fetch_threads(
    gh: &dyn GhRunner,
    pr_ref: &PrRef,
) -> Result<(Vec<GithubThread>, Vec<String>, bool)> {
    let graph = fetch_graphql(gh, pr_ref);
    let rest = fetch_rest(gh, pr_ref);
    if let (Err(g), Err(r)) = (&graph, &rest) {
        bail!("review threads unavailable: GraphQL: {g}; REST: {r}");
    }
    let complete = graph.as_ref().is_ok_and(|(_, complete)| *complete) && rest.is_ok();
    let mut warnings = Vec::new();
    if let Err(e) = &graph {
        warnings.push(format!("GraphQL review threads incomplete: {e}"));
    }
    if let Err(e) = &rest {
        warnings.push(format!("REST review comments incomplete: {e}"));
    }
    let mut threads = graph.map(|(threads, _)| threads).unwrap_or_default();
    if let Ok(comments) = rest {
        merge_rest(&mut threads, comments);
    }
    for thread in &mut threads {
        thread.enumeration_complete = complete;
    }
    Ok((threads, warnings, complete))
}

fn fetch_graphql(gh: &dyn GhRunner, pr_ref: &PrRef) -> Result<(Vec<GithubThread>, bool)> {
    let mut threads = Vec::new();
    let mut complete = true;
    let mut cursor: Option<String> = None;
    let mut seen = HashSet::new();
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
        if !v["errors"].is_null() {
            bail!("GraphQL returned errors");
        }
        let page = &v["data"]["repository"]["pullRequest"]["reviewThreads"];
        let nodes = page["nodes"]
            .as_array()
            .context("missing GraphQL review thread nodes")?;
        for node in nodes {
            let (t, comments_complete) = parse_thread(node)?;
            complete &= comments_complete;
            threads.push(t);
        }
        match page["pageInfo"]["hasNextPage"].as_bool() {
            Some(false) => return Ok((threads, complete)),
            Some(true) => {
                let next = page["pageInfo"]["endCursor"]
                    .as_str()
                    .context("missing review thread cursor")?;
                if !seen.insert(next.to_owned()) {
                    bail!("repeated review thread cursor");
                }
                cursor = Some(next.to_owned());
            }
            None => bail!("missing review thread pagination state"),
        }
    }
    bail!("too many review thread pages")
}

fn parse_thread(node: &Value) -> Result<(GithubThread, bool)> {
    let num = |k: &str| node[k].as_u64().and_then(|n| u32::try_from(n).ok());
    let comments = &node["comments"];
    // Missing pagination state is not proof that the comment set is complete.
    let comments_complete = comments["pageInfo"]["hasNextPage"].as_bool() == Some(false);
    let nodes = comments["nodes"]
        .as_array()
        .context("missing GraphQL comments")?;
    Ok((
        GithubThread {
            node_id: node["id"].as_str().context("missing thread ID")?.into(),
            path: node["path"].as_str().context("missing thread path")?.into(),
            side: node["diffSide"].as_str().and_then(Side::parse),
            line: num("line"),
            start_line: num("startLine"),
            original_line: num("originalLine"),
            is_outdated: node["isOutdated"]
                .as_bool()
                .context("missing outdated state")?,
            is_resolved: node["isResolved"]
                .as_bool()
                .context("missing resolution state")?,
            comments: nodes
                .iter()
                .map(|c| {
                    Ok(GithubComment {
                        node_id: c["id"].as_str().context("missing comment ID")?.into(),
                        database_id: c["databaseId"].as_i64(),
                        author: c["author"]["login"].as_str().unwrap_or("ghost").into(),
                        body: c["body"].as_str().context("missing comment body")?.into(),
                        created_at: c["createdAt"]
                            .as_str()
                            .context("missing comment date")?
                            .into(),
                        updated_at: c["updatedAt"].as_str().map(str::to_owned),
                        in_reply_to_id: None,
                    })
                })
                .collect::<Result<Vec<_>>>()?,
            rest_only: false,
            enumeration_complete: false,
        },
        comments_complete,
    ))
}

fn fetch_rest(gh: &dyn GhRunner, pr_ref: &PrRef) -> Result<Vec<Value>> {
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
    let mut by_database = HashMap::new();
    let mut by_node = HashMap::new();
    for (ti, t) in threads.iter().enumerate() {
        for (ci, c) in t.comments.iter().enumerate() {
            if let Some(id) = c.database_id {
                by_database.insert(id, (ti, ci));
            }
            by_node.insert(c.node_id.clone(), (ti, ci));
        }
    }
    let mut roots = HashMap::new();
    for row in &rows {
        let id = row["id"].as_i64().unwrap();
        let root = row["in_reply_to_id"].as_i64().unwrap_or(id);
        roots.insert(id, root);
    }
    for row in rows {
        let id = row["id"].as_i64().unwrap();
        let node = row["node_id"].as_str().unwrap_or_default();
        let parent = row["in_reply_to_id"].as_i64();
        let root = parent
            .map(|p| roots.get(&p).copied().unwrap_or(p))
            .unwrap_or(id);
        let location = by_database
            .get(&id)
            .copied()
            .or_else(|| by_node.get(node).copied());
        let ti = location
            .map(|(ti, _)| ti)
            .or_else(|| by_database.get(&root).map(|(ti, _)| *ti))
            .or_else(|| {
                threads
                    .iter()
                    .position(|t| t.node_id == format!("rest:{root}"))
            })
            .unwrap_or_else(|| {
                threads.push(GithubThread {
                    node_id: format!("rest:{root}"),
                    path: row["path"].as_str().unwrap().into(),
                    side: row["side"].as_str().and_then(Side::parse),
                    line: None,
                    start_line: None,
                    original_line: None,
                    is_outdated: false,
                    is_resolved: false,
                    comments: Vec::new(),
                    rest_only: true,
                    enumeration_complete: false,
                });
                threads.len() - 1
            });
        let comment = GithubComment {
            node_id: node.into(),
            database_id: Some(id),
            author: row["user"]["login"].as_str().unwrap_or("ghost").into(),
            body: row["body"].as_str().unwrap().into(),
            created_at: row["created_at"].as_str().unwrap().into(),
            updated_at: row["updated_at"].as_str().map(str::to_owned),
            in_reply_to_id: parent,
        };
        if let Some((old_ti, ci)) = location {
            let old = &mut threads[old_ti].comments[ci];
            old.database_id = Some(id);
            old.in_reply_to_id = parent;
            old.body = comment.body;
            old.author = comment.author;
            old.updated_at = comment.updated_at;
        } else {
            let ci = threads[ti].comments.len();
            threads[ti].comments.push(comment);
            by_database.insert(id, (ti, ci));
        }
    }
    for t in threads {
        t.comments
            .sort_by_key(|c| (c.created_at.clone(), c.database_id.unwrap_or_default()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::fake::FakeGh;
    use serde_json::json;

    #[test]
    fn paginates_rest_and_preserves_partial_graph_when_later_page_fails() {
        let gh = FakeGh::default();
        let pr = PrRef {
            owner: "o".into(),
            repo: "r".into(),
            number: 1,
        };
        gh.on(
            &["api", "graphql"],
            Ok(
                &json!({"data":{"repository":{"pullRequest":{"reviewThreads":{
                    "pageInfo":{"hasNextPage":false},"nodes":[]
                }}}}})
                .to_string(),
            ),
        );
        let rows: Vec<_> = (1..=100)
            .map(|id| {
                json!({
                    "id":id,"node_id":format!("N{id}"),"path":"a.rs","body":"same",
                    "created_at":"2026-01-01T00:00:00Z","user":{"login":"alice"}
                })
            })
            .collect();
        gh.on(
            &["api", "repos/o/r/pulls/1/comments?per_page=100&page=1"],
            Ok(&serde_json::to_string(&rows).unwrap()),
        );
        gh.on(
            &["api", "repos/o/r/pulls/1/comments?per_page=100&page=2"],
            Err("page failed"),
        );
        let (threads, warnings, complete) = fetch_threads(&gh, &pr).unwrap();
        assert!(!complete);
        assert!(
            threads.is_empty(),
            "failed page must not import partial results"
        );
        assert_eq!(warnings.len(), 1);

        let ok = FakeGh::default();
        ok.on(&["api", "graphql"], Err("unavailable"));
        ok.on(
            &["api", "repos/o/r/pulls/1/comments?per_page=100&page=1"],
            Ok(&serde_json::to_string(&rows).unwrap()),
        );
        let last = json!([{"id":101,"node_id":"N101","path":"a.rs","body":"same",
            "created_at":"2026-01-01T00:00:00Z","in_reply_to_id":1,"user":{"login":"bob"}}]);
        ok.on(
            &["api", "repos/o/r/pulls/1/comments?per_page=100&page=2"],
            Ok(&last.to_string()),
        );
        let (threads, warnings, complete) = fetch_threads(&ok, &pr).unwrap();
        assert!(!complete);
        assert_eq!(warnings.len(), 1);
        assert_eq!(threads.iter().map(|t| t.comments.len()).sum::<usize>(), 101);
        assert!(!threads[0].enumeration_complete);
    }

    #[test]
    fn complete_empty_enumeration_is_explicit() {
        let gh = FakeGh::default();
        let pr = PrRef {
            owner: "o".into(),
            repo: "r".into(),
            number: 1,
        };
        gh.on(
            &["api", "graphql"],
            Ok(
                &json!({"data":{"repository":{"pullRequest":{"reviewThreads":{
                    "pageInfo":{"hasNextPage":false},"nodes":[]
                }}}}})
                .to_string(),
            ),
        );
        gh.on(
            &["api", "repos/o/r/pulls/1/comments?per_page=100&page=1"],
            Ok("[]"),
        );
        let (threads, warnings, complete) = fetch_threads(&gh, &pr).unwrap();
        assert!(threads.is_empty());
        assert!(warnings.is_empty());
        assert!(complete);
    }

    #[test]
    fn nested_pagination_retains_graphql_metadata_and_supplements_rest() {
        for page_info in [json!({"hasNextPage":true}), json!({})] {
            let gh = FakeGh::default();
            let pr = PrRef {
                owner: "o".into(),
                repo: "r".into(),
                number: 1,
            };
            let graph = json!({"data":{"repository":{"pullRequest":{"reviewThreads":{
                "pageInfo":{"hasNextPage":false},"nodes":[{
                    "id":"T","path":"a.rs","diffSide":"LEFT","line":8,
                    "isResolved":true,"isOutdated":false,
                    "comments":{"pageInfo":page_info,"nodes":[{
                        "id":"N1","databaseId":1,"body":"first","createdAt":"now"
                    }]}
                }]
            }}}}});
            gh.on(&["api", "graphql"], Ok(&graph.to_string()));
            let rest = json!([{"id":1,"node_id":"N1","path":"a.rs","body":"first","created_at":"now"},
                {"id":101,"node_id":"N101","path":"a.rs","body":"reply","created_at":"later","in_reply_to_id":1}]);
            gh.on(
                &["api", "repos/o/r/pulls/1/comments?per_page=100&page=1"],
                Ok(&rest.to_string()),
            );
            let (threads, _, complete) = fetch_threads(&gh, &pr).unwrap();
            assert!(!complete);
            assert_eq!(threads.len(), 1);
            assert_eq!(threads[0].node_id, "T");
            assert_eq!(threads[0].side, Some(Side::Left));
            assert!(threads[0].is_resolved);
            assert_eq!(threads[0].comments.len(), 2);
            assert_eq!(
                threads[0]
                    .comments
                    .iter()
                    .find(|c| c.database_id == Some(101))
                    .unwrap()
                    .in_reply_to_id,
                Some(1)
            );
        }
    }

    #[test]
    fn missing_rest_parent_groups_sibling_replies() {
        let mut threads = Vec::new();
        merge_rest(
            &mut threads,
            vec![
                json!({"id":3,"node_id":"N3","path":"a","body":"third","created_at":"2026-01-03T00:00:00Z","in_reply_to_id":1}),
                json!({"id":2,"node_id":"N2","path":"a","body":"second","created_at":"2026-01-02T00:00:00Z","in_reply_to_id":1}),
            ],
        );
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].node_id, "rest:1");
        assert_eq!(
            threads[0]
                .comments
                .iter()
                .map(|c| c.database_id)
                .collect::<Vec<_>>(),
            vec![Some(2), Some(3)]
        );
    }

    #[test]
    fn rest_alias_and_distinct_authors() {
        let mut threads = vec![GithubThread {
            node_id: "T".into(),
            path: "a".into(),
            side: None,
            line: Some(1),
            start_line: None,
            original_line: None,
            is_outdated: false,
            is_resolved: false,
            rest_only: false,
            enumeration_complete: false,
            comments: vec![GithubComment {
                node_id: "N".into(),
                database_id: Some(1),
                author: "first".into(),
                body: "same".into(),
                created_at: "now".into(),
                updated_at: None,
                in_reply_to_id: None,
            }],
        }];
        merge_rest(
            &mut threads,
            vec![
                json!({"id":1,"node_id":"N","path":"a","user":{"login":"first"},"body":"same","created_at":"now"}),
                json!({"id":2,"node_id":"M","path":"a","user":{"login":"second"},"body":"same","created_at":"now","in_reply_to_id":1}),
            ],
        );
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].comments.len(), 2);
        assert_eq!(threads[0].comments[1].author, "second");
        assert_eq!(threads[0].comments[1].in_reply_to_id, Some(1));
    }
}
