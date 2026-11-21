use super::{
    GhRunner, PrRef,
    feedback::{fetch_comments, fetch_reviews},
    runner::output_stdout,
};
use crate::model::Completeness;
use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::{process::Command, sync::Mutex};

struct Pages {
    reviews: Mutex<Vec<Result<Value, String>>>,
    comments: Mutex<Vec<Result<Value, String>>>,
    calls: Mutex<Vec<Vec<String>>>,
}
impl Pages {
    fn new(reviews: Vec<Result<Value, String>>, comments: Vec<Result<Value, String>>) -> Self {
        Self {
            reviews: Mutex::new(reviews),
            comments: Mutex::new(comments),
            calls: Mutex::new(Vec::new()),
        }
    }
}
impl GhRunner for Pages {
    fn run(&self, args: &[&str], _: Option<&[u8]>) -> Result<Vec<u8>> {
        self.calls
            .lock()
            .unwrap()
            .push(args.iter().map(|a| (*a).to_owned()).collect());
        let pages = if args.iter().any(|a| a.contains("reviews(first: 100")) {
            &self.reviews
        } else {
            &self.comments
        };
        let page = pages.lock().unwrap().remove(0);
        page.map(|v| v.to_string().into_bytes())
            .map_err(|e| anyhow!(e))
    }
}
fn pr() -> PrRef {
    PrRef {
        owner: "o".into(),
        repo: "r".into(),
        number: 7,
    }
}
fn node(n: usize) -> Value {
    json!({"id":format!("N{n}"),"author":{"login":"a","id":"U1","__typename":"User"},"body":format!("body {n}"),"state":"COMMENTED","databaseId":n,"url":"https://github.com/o/r","createdAt":"2026-01-01T00:00:00Z"})
}
fn page(kind: &str, nodes: Vec<Value>, next: bool, cursor: Option<&str>) -> Result<Value, String> {
    Ok(
        json!({"data":{"repository":{"pullRequest":{kind:{"nodes":nodes,"pageInfo":{"hasNextPage":next,"endCursor":cursor}}}}}}),
    )
}
#[test]
fn paginates_over_hundred_reviews_and_comments_preserving_order_and_deduping_overlap() {
    let first: Vec<Value> = (0..100).map(node).collect();
    let second: Vec<Value> = (99..151).map(node).collect();
    let pages = || {
        vec![
            page("reviews", first.clone(), true, Some("next")),
            page("reviews", second.clone(), false, None),
        ]
    };
    let comments = vec![
        page("comments", (0..100).map(node).collect(), true, Some("next")),
        page("comments", (99..151).map(node).collect(), false, None),
    ];
    let gh = Pages::new(pages(), comments);
    let reviews = fetch_reviews(&gh, &pr());
    let comments = fetch_comments(&gh, &pr());
    assert_eq!(reviews.completeness, Completeness::Complete);
    assert_eq!(comments.completeness, Completeness::Complete);
    assert_eq!(reviews.records.len(), 151);
    assert_eq!(comments.records.len(), 151);
    assert_eq!(reviews.records[100].node_id.as_deref(), Some("N100"));
    assert_eq!(comments.records[100].node_id.as_deref(), Some("N100"));
    assert_eq!(reviews.records[0].author_id.as_deref(), Some("U1"));
    let calls = gh.calls.lock().unwrap();
    assert_eq!(calls.len(), 4);
    for call in [&calls[0], &calls[2]] {
        assert!(!call.iter().any(|arg| arg.starts_with("cursor=")));
    }
    for call in [&calls[1], &calls[3]] {
        assert!(call.windows(2).any(|args| args == ["-f", "cursor=next"]));
    }
}
#[test]
fn empty_terminal_page_is_complete() {
    let gh = Pages::new(
        vec![page("reviews", vec![], false, None)],
        vec![page("comments", vec![], false, None)],
    );
    assert_eq!(
        fetch_reviews(&gh, &pr()).completeness,
        Completeness::Complete
    );
    assert_eq!(
        fetch_comments(&gh, &pr()).completeness,
        Completeness::Complete
    );
}
#[test]
fn partial_errors_and_failed_pages_keep_usable_records_independently() {
    let mut partial = page("reviews", vec![node(1)], false, None).unwrap();
    partial["errors"] = json!([{"message":"forbidden"}]);
    let gh = Pages::new(
        vec![Ok(partial)],
        vec![
            page("comments", vec![node(2)], true, Some("next")),
            Err("offline".into()),
        ],
    );
    let reviews = fetch_reviews(&gh, &pr());
    let comments = fetch_comments(&gh, &pr());
    assert_eq!(reviews.completeness, Completeness::Partial);
    assert_eq!(comments.completeness, Completeness::Partial);
    assert_eq!(reviews.records.len(), 1);
    assert_eq!(comments.records.len(), 1);
    assert!(reviews.problems[0].contains("forbidden"));
    assert!(comments.problems[0].contains("offline"));
}
#[test]
fn importer_keeps_graphql_partial_stdout_from_nonzero_cli() {
    struct NonzeroGh;
    impl GhRunner for NonzeroGh {
        fn run(&self, _: &[&str], _: Option<&[u8]>) -> Result<Vec<u8>> {
            Err(anyhow!("ordinary gh failure"))
        }

        fn run_graphql(&self, args: &[&str]) -> Result<Vec<u8>> {
            assert!(args.starts_with(&["api", "graphql"]));
            let mut out = Command::new("sh").args(["-c", "exit 1"]).output()?;
            out.stdout = json!({"data":{"repository":{"pullRequest":{"reviews":{"nodes":[node(1)],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}},"errors":[{"message":"partial access"}]}).to_string().into_bytes();
            out.stderr = b"GraphQL: partial access".to_vec();
            output_stdout(out, true)
        }
    }
    let reviews = fetch_reviews(&NonzeroGh, &pr());
    assert_eq!(reviews.completeness, Completeness::Partial);
    assert_eq!(reviews.records.len(), 1);
    assert_eq!(reviews.records[0].node_id.as_deref(), Some("N1"));
    assert!(reviews.problems[0].contains("partial access"));
}

#[test]
fn malformed_and_cyclic_pages_fail_closed() {
    let gh = Pages::new(
        vec![
            page("reviews", vec![node(1)], true, Some("same")),
            page("reviews", vec![node(2)], true, Some("same")),
        ],
        vec![Ok(
            json!({"data":{"repository":{"pullRequest":{"comments":{"nodes":[{"id":"N3","body":"retained"}]}}}}}),
        )],
    );
    let reviews = fetch_reviews(&gh, &pr());
    let comments = fetch_comments(&gh, &pr());
    assert_eq!(reviews.completeness, Completeness::Partial);
    assert_eq!(reviews.records.len(), 2);
    assert!(reviews.problems[0].contains("cyclic"));
    assert_eq!(comments.completeness, Completeness::Partial);
    assert_eq!(comments.records.len(), 1);
}
#[test]
fn malformed_required_fields_keep_valid_nodes_and_mark_partial_or_failed() {
    for kind in ["reviews", "comments"] {
        for field in if kind == "reviews" {
            vec!["id", "state", "body"]
        } else {
            vec!["id", "body"]
        } {
            for malformed in [false, true] {
                let mut bad = node(2);
                if malformed {
                    bad[field] = json!(42);
                } else {
                    bad.as_object_mut().unwrap().remove(field);
                }
                let run = |nodes| {
                    let response = page(kind, nodes, false, None);
                    let gh = if kind == "reviews" {
                        Pages::new(vec![response], vec![])
                    } else {
                        Pages::new(vec![], vec![response])
                    };
                    if kind == "reviews" {
                        let result = fetch_reviews(&gh, &pr());
                        (result.records.len(), result.completeness, result.problems)
                    } else {
                        let result = fetch_comments(&gh, &pr());
                        (result.records.len(), result.completeness, result.problems)
                    }
                };
                let (count, completeness, problems) = run(vec![node(1), bad.clone()]);
                assert_eq!(count, 1, "{kind} {field} malformed={malformed}");
                assert_eq!(completeness, Completeness::Partial);
                assert!(!problems.is_empty());
                let (count, completeness, problems) = run(vec![bad]);
                assert_eq!(count, 0);
                assert_eq!(completeness, Completeness::Failed);
                assert!(!problems.is_empty());
            }
        }
    }
}

#[test]
fn empty_body_approval_and_comment_with_null_optional_metadata_are_complete() {
    let mut approval = node(1);
    approval["state"] = json!("APPROVED");
    approval["body"] = json!("");
    approval["author"] = Value::Null;
    approval["submittedAt"] = Value::Null;
    let mut comment = node(2);
    comment["body"] = json!("");
    comment["author"] = Value::Null;
    comment["updatedAt"] = Value::Null;
    let gh = Pages::new(
        vec![page("reviews", vec![approval], false, None)],
        vec![page("comments", vec![comment], false, None)],
    );
    let reviews = fetch_reviews(&gh, &pr());
    let comments = fetch_comments(&gh, &pr());
    assert_eq!(reviews.completeness, Completeness::Complete);
    assert_eq!(reviews.records[0].state, "APPROVED");
    assert!(reviews.records[0].body.is_empty());
    assert_eq!(comments.completeness, Completeness::Complete);
    assert!(comments.records[0].body.is_empty());
}

#[test]
fn fifty_page_cap_is_partial() {
    let pages = (0..50)
        .map(|n| page("reviews", vec![node(n)], true, Some(&format!("cursor-{n}"))))
        .collect();
    let gh = Pages::new(pages, vec![]);
    let result = fetch_reviews(&gh, &pr());
    assert_eq!(result.records.len(), 50);
    assert_eq!(result.completeness, Completeness::Partial);
    assert!(result.problems[0].contains("50 pages"));
}
