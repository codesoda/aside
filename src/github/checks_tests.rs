use super::PrRef;
use super::checks::fetch;
use super::fake::FakeGh;
use crate::model::Completeness;
use serde_json::json;

fn pr() -> PrRef {
    PrRef {
        owner: "o".into(),
        repo: "r".into(),
        number: 1,
    }
}
fn run(id: u64, count: usize) -> serde_json::Value {
    json!({"id":id,"name":"build","status":"completed","conclusion":"success","head_sha":"head","output":{"annotations_count":count}})
}
#[test]
fn imports_nested_pages_and_statuses_for_exact_sha() {
    let gh = FakeGh::default();
    let mut first = vec![run(1, 101)];
    first.extend((2..=100).map(|id| run(id, 0)));
    gh.on(
        &[
            "api",
            "repos/o/r/commits/head/check-runs?per_page=100&page=1",
        ],
        Ok(&json!({"total_count":101,"check_runs":first}).to_string()),
    );
    gh.on(
        &[
            "api",
            "repos/o/r/commits/head/check-runs?per_page=100&page=2",
        ],
        Ok(&json!({"check_runs":[run(101,0)]}).to_string()),
    );
    let annotations = (0..100).map(|n| json!({"path":"src/a.rs","start_line":n+1,"end_line":n+1,"message":format!("message {n}"),"annotation_level":"warning"})).collect::<Vec<_>>();
    gh.on(
        &[
            "api",
            "repos/o/r/check-runs/1/annotations?per_page=100&page=1",
        ],
        Ok(&json!(annotations).to_string()),
    );
    gh.on(
        &[
            "api",
            "repos/o/r/check-runs/1/annotations?per_page=100&page=2",
        ],
        Ok(
            &json!([{"path":"src/a.rs","start_line":101,"end_line":102,"message":"last"}])
                .to_string(),
        ),
    );
    gh.on(
        &["api", "repos/o/r/commits/head/status?per_page=100&page=1"],
        Ok(&json!({"statuses":[{"id":7,"context":"ci","state":"success"}]}).to_string()),
    );
    let imported = fetch(&gh, &pr(), "head");
    assert_eq!(imported.runs.len(), 101);
    assert_eq!(imported.runs[0].annotations_count, 101);
    assert_eq!(imported.annotations.len(), 101);
    assert_eq!(imported.statuses.len(), 1);
    assert_eq!(imported.runs_coverage.completeness, Completeness::Complete);
    assert_eq!(
        imported.annotation_coverage[0].1.completeness,
        Completeness::Complete
    );
    assert_eq!(imported.annotations[100].end_line, 102);
    assert!(
        gh.calls_matching(&["api"])
            .iter()
            .all(|(args, _)| !args.join(" ").contains("reviews"))
    );
}
#[test]
fn empty_and_forbidden_are_distinct() {
    let gh = FakeGh::default();
    gh.on(
        &[
            "api",
            "repos/o/r/commits/head/check-runs?per_page=100&page=1",
        ],
        Ok(r#"{"total_count":0,"check_runs":[]}"#),
    );
    gh.on(
        &["api", "repos/o/r/commits/head/status?per_page=100&page=1"],
        Err("403 forbidden"),
    );
    let imported = fetch(&gh, &pr(), "head");
    assert_eq!(imported.runs_coverage.completeness, Completeness::Complete);
    assert_eq!(
        imported.statuses_coverage.completeness,
        Completeness::Failed
    );
    assert_eq!(imported.statuses_coverage.reasons, ["forbidden"]);
}
#[test]
fn aggregate_limit_is_explicit_and_keeps_partial_records() {
    let gh = FakeGh::default();
    let many = (0..100).map(|id| run(id, 1)).collect::<Vec<_>>();
    gh.on(
        &[
            "api",
            "repos/o/r/commits/head/check-runs?per_page=100&page=1",
        ],
        Ok(&json!({"total_count":100,"check_runs":many}).to_string()),
    );
    gh.on(
        &[
            "api",
            "repos/o/r/check-runs/0/annotations?per_page=100&page=1",
        ],
        Ok(r#"[{"path":"p","start_line":1,"end_line":1,"message":"m"}]"#),
    );
    let imported = fetch(&gh, &pr(), "head");
    assert_eq!(imported.runs.len(), 100);
    assert!(imported.annotation_coverage.iter().any(|(_, c)| {
        c.reasons
            .contains(&"aggregate_request_limit_reached".into())
    }));
    assert!(gh.calls_matching(&["api"]).len() <= 20);
}

#[test]
fn count_mismatch_and_invalid_runs_keep_independent_reasons_and_statuses() {
    let gh = FakeGh::default();
    gh.on(
        &[
            "api",
            "repos/o/r/commits/head/check-runs?per_page=100&page=1",
        ],
        Ok(&json!({"total_count":4,"check_runs":[run(1,0),run(2,0),run(3,0)]}).to_string()),
    );
    gh.on(
        &["api", "repos/o/r/commits/head/status?per_page=100&page=1"],
        Ok(r#"{"sha":"head","statuses":[{"id":8,"state":"success"}]}"#),
    );
    let imported = fetch(&gh, &pr(), "head");
    assert_eq!(imported.runs.len(), 3);
    assert_eq!(imported.runs_coverage.reasons, ["count_mismatch"]);
    assert_eq!(imported.statuses.len(), 1);
    assert_eq!(
        imported.statuses_coverage.completeness,
        Completeness::Complete
    );
}

#[test]
fn multiple_invalid_runs_and_partial_annotations_preserve_every_reason() {
    let gh = FakeGh::default();
    let mut wrong = run(2, 0);
    wrong["head_sha"] = json!("other");
    gh.on(
        &[
            "api",
            "repos/o/r/commits/head/check-runs?per_page=100&page=1",
        ],
        Ok(
            &json!({"total_count":4,"check_runs":[run(1,2),wrong,{"name":"bad"},run(4,0)]})
                .to_string(),
        ),
    );
    gh.on(
        &["api", "repos/o/r/commits/head/status?per_page=100&page=1"],
        Ok(r#"{"statuses":[]}"#),
    );
    gh.on(
        &[
            "api",
            "repos/o/r/check-runs/1/annotations?per_page=100&page=1",
        ],
        Ok(r#"[{"path":"p","start_line":1,"end_line":1,"message":"good"},{"path":"p"}]"#),
    );
    let imported = fetch(&gh, &pr(), "head");
    assert_eq!(imported.runs.len(), 2);
    assert_eq!(
        imported.runs_coverage.reasons,
        ["sha_mismatch", "missing_run_id"]
    );
    assert_eq!(imported.annotations.len(), 1);
    assert_eq!(
        imported.annotation_coverage[0].1.reasons,
        ["malformed_annotation", "annotation_count_mismatch"]
    );
    assert_eq!(
        imported.annotation_coverage[0].1.completeness,
        Completeness::Partial
    );
}

#[test]
fn invalid_first_and_last_runs_finalize_partial_from_accepted_records() {
    for invalid_first in [true, false] {
        let gh = FakeGh::default();
        let runs = if invalid_first {
            vec![json!({"name":"bad"}), run(1, 0)]
        } else {
            vec![run(1, 0), json!({"name":"bad"})]
        };
        gh.on(
            &[
                "api",
                "repos/o/r/commits/head/check-runs?per_page=100&page=1",
            ],
            Ok(&json!({"total_count":2,"check_runs":runs}).to_string()),
        );
        gh.on(
            &["api", "repos/o/r/commits/head/status?per_page=100&page=1"],
            Ok(r#"{"statuses":[]}"#),
        );
        let imported = fetch(&gh, &pr(), "head");
        assert_eq!(imported.runs.len(), 1);
        assert_eq!(imported.runs_coverage.reasons, ["missing_run_id"]);
        assert_eq!(imported.runs_coverage.completeness, Completeness::Partial);
    }
}

#[test]
fn invalid_first_and_last_annotations_finalize_partial_from_accepted_records() {
    for invalid_first in [true, false] {
        let gh = FakeGh::default();
        gh.on(
            &[
                "api",
                "repos/o/r/commits/head/check-runs?per_page=100&page=1",
            ],
            Ok(&json!({"total_count":1,"check_runs":[run(1, 2)]}).to_string()),
        );
        gh.on(
            &["api", "repos/o/r/commits/head/status?per_page=100&page=1"],
            Ok(r#"{"statuses":[]}"#),
        );
        let valid = json!({"path":"p","start_line":1,"end_line":1,"message":"good"});
        let items = if invalid_first {
            vec![json!({"path":"p"}), valid]
        } else {
            vec![valid, json!({"path":"p"})]
        };
        gh.on(
            &[
                "api",
                "repos/o/r/check-runs/1/annotations?per_page=100&page=1",
            ],
            Ok(&json!(items).to_string()),
        );
        let imported = fetch(&gh, &pr(), "head");
        assert_eq!(imported.annotations.len(), 1);
        assert_eq!(
            imported.annotation_coverage[0].1.completeness,
            Completeness::Partial
        );
    }
}

#[test]
fn excess_short_page_records_are_count_mismatches_for_runs_and_annotations() {
    let gh = FakeGh::default();
    gh.on(
        &[
            "api",
            "repos/o/r/commits/head/check-runs?per_page=100&page=1",
        ],
        Ok(&json!({"total_count":1,"check_runs":[run(1, 1), run(2, 0)]}).to_string()),
    );
    gh.on(
        &["api", "repos/o/r/commits/head/status?per_page=100&page=1"],
        Ok(r#"{"statuses":[]}"#),
    );
    gh.on(
        &["api", "repos/o/r/check-runs/1/annotations?per_page=100&page=1"],
        Ok(r#"[{"path":"p","start_line":1,"end_line":1,"message":"one"},{"path":"p","start_line":2,"end_line":2,"message":"two"}]"#),
    );
    let imported = fetch(&gh, &pr(), "head");
    assert_eq!(imported.runs.len(), 2);
    assert_eq!(imported.annotations.len(), 2);
    assert_eq!(imported.runs_coverage.reasons, ["count_mismatch"]);
    assert_eq!(imported.runs_coverage.completeness, Completeness::Partial);
    assert_eq!(
        imported.annotation_coverage[0].1.reasons,
        ["count_mismatch", "annotation_count_mismatch"]
    );
    assert_eq!(
        imported.annotation_coverage[0].1.completeness,
        Completeness::Partial
    );
}

#[test]
fn status_pages_keep_earlier_records_on_forbidden_next_page() {
    let gh = FakeGh::default();
    gh.on(
        &[
            "api",
            "repos/o/r/commits/head/check-runs?per_page=100&page=1",
        ],
        Ok(r#"{"total_count":0,"check_runs":[]}"#),
    );
    let first = (0..100)
        .map(|id| json!({"id":id,"state":"success"}))
        .collect::<Vec<_>>();
    gh.on(
        &["api", "repos/o/r/commits/head/status?per_page=100&page=1"],
        Ok(&json!({"statuses":first}).to_string()),
    );
    gh.on(
        &["api", "repos/o/r/commits/head/status?per_page=100&page=2"],
        Err("403 forbidden"),
    );
    let imported = fetch(&gh, &pr(), "head");
    assert_eq!(imported.statuses.len(), 100);
    assert_eq!(
        imported.statuses_coverage.completeness,
        Completeness::Partial
    );
    assert_eq!(imported.statuses_coverage.reasons, ["forbidden"]);
}

#[test]
fn status_second_page_is_imported() {
    let gh = FakeGh::default();
    gh.on(
        &[
            "api",
            "repos/o/r/commits/head/check-runs?per_page=100&page=1",
        ],
        Ok(r#"{"total_count":0,"check_runs":[]}"#),
    );
    let first = (0..100)
        .map(|id| json!({"id":id,"state":"success"}))
        .collect::<Vec<_>>();
    gh.on(
        &["api", "repos/o/r/commits/head/status?per_page=100&page=1"],
        Ok(&json!({"statuses":first}).to_string()),
    );
    gh.on(
        &["api", "repos/o/r/commits/head/status?per_page=100&page=2"],
        Ok(r#"{"statuses":[{"id":100,"state":"pending"}]}"#),
    );
    let imported = fetch(&gh, &pr(), "head");
    assert_eq!(imported.statuses.len(), 101);
    assert_eq!(imported.statuses[100].state, "pending");
    assert_eq!(
        imported.statuses_coverage.completeness,
        Completeness::Complete
    );
}

#[test]
fn refresh_is_sha_bound_and_does_not_reuse_old_records() {
    let gh = FakeGh::default();
    gh.on(
        &[
            "api",
            "repos/o/r/commits/head/check-runs?per_page=100&page=1",
        ],
        Ok(&json!({"total_count":1,"check_runs":[run(1,0)]}).to_string()),
    );
    gh.on(
        &["api", "repos/o/r/commits/head/status?per_page=100&page=1"],
        Ok(r#"{"sha":"other","statuses":[{"id":1}]}"#),
    );
    gh.on(
        &[
            "api",
            "repos/o/r/commits/new/check-runs?per_page=100&page=1",
        ],
        Ok(r#"{"total_count":0,"check_runs":[]}"#),
    );
    gh.on(
        &["api", "repos/o/r/commits/new/status?per_page=100&page=1"],
        Ok(r#"{"sha":"new","statuses":[]}"#),
    );
    let old = fetch(&gh, &pr(), "head");
    let new = fetch(&gh, &pr(), "new");
    assert_eq!(old.runs.len(), 1);
    assert!(old.statuses.is_empty());
    assert_eq!(old.statuses_coverage.reasons, ["sha_mismatch"]);
    assert!(new.runs.is_empty());
    assert!(new.statuses.is_empty());
    assert_eq!(new.sha, "new");
}
