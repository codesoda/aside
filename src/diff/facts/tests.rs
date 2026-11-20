use super::*;
use crate::diff::{Limits, find_line, parse_unified};
use crate::model::{AnchorKind, SnapshotId};

const OBSERVED: &str = "2026-01-01T00:00:00Z";

fn run(snapshot: &PrSnapshot, config: &DiffFactsConfig) -> DiffFacts {
    let snapshot_id = SnapshotId::derive(
        "test/repo",
        snapshot.pr.number,
        &snapshot.pr.base_sha,
        &snapshot.pr.head_sha,
    );
    scan(
        &DiffFactsInput {
            snapshot,
            snapshot_id: &snapshot_id,
            canonical_repo_id: "test/repo",
            run_id: None,
            observed_at: OBSERVED,
        },
        config,
    )
}

fn json(facts: &DiffFacts) -> String {
    serde_json::to_string_pretty(facts).unwrap()
}

fn synthetic(diff: &str) -> PrSnapshot {
    let mut snap = crate::demo::snapshot(0);
    let parsed = parse_unified(diff, &snap.pr.head_sha, &Limits::default());
    assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
    snap.files = parsed.files;
    snap.warnings.clear();
    snap
}

fn by_rule<'a>(facts: &'a DiffFacts, rule: Rule, path: &str) -> Vec<&'a ReviewFinding> {
    facts
        .findings
        .iter()
        .filter(|f| {
            f.source.rule_id.as_deref() == Some(rule.id)
                && f.anchor.current_path.as_deref().unwrap_or("") == path
        })
        .collect()
}

fn outcome(facts: &DiffFacts, path: &str, rule: Rule) -> ScopeOutcome {
    facts
        .coverage
        .entries
        .iter()
        .find(|e| {
            e.rule_id.as_deref() == Some(rule.id)
                && matches!(&e.scope, CoverageScope::File { path: p, .. } if p == path)
        })
        .map(|e| e.outcome.clone())
        .unwrap_or_else(|| panic!("no coverage for {path} {}", rule.id))
}

/// Invariants every scan output must hold.
fn assert_invariants(snapshot: &PrSnapshot, facts: &DiffFacts) {
    let evidence: BTreeMap<&str, &Evidence> =
        facts.evidence.iter().map(|e| (e.id.as_str(), e)).collect();
    let mut referenced = std::collections::BTreeSet::new();
    for f in &facts.findings {
        assert!(
            matches!(
                f.assertion_kind,
                AssertionKind::MeasuredFact | AssertionKind::AttentionHint
            ),
            "{} asserts {:?}",
            f.title,
            f.assertion_kind
        );
        f.validate().unwrap();
        let rule = rules::ALL
            .iter()
            .find(|r| Some(r.id) == f.source.rule_id.as_deref())
            .expect("known rule");
        assert_eq!(f.source.rule_version.as_deref(), Some(rule.version));
        assert_eq!(f.extensions["ruleVersion"], json!(rule.version));
        assert!(!f.evidence_ids.is_empty(), "{} has no evidence", f.title);
        assert_ne!(f.attention.level, AttentionLevel::High);
        for id in &f.evidence_ids {
            let ev = evidence.get(id.as_str()).expect("evidence exists");
            assert_eq!(ev.method.as_deref(), Some(rule.method().as_str()));
            referenced.insert(id.as_str());
        }
        assert_ne!(
            f.anchor.kind,
            AnchorKind::DiffLine,
            "findings are file/PR anchored"
        );
        if f.anchor.kind == AnchorKind::File {
            let file = snapshot
                .files
                .iter()
                .find(|x| Some(&x.id) == f.anchor.file_id.as_ref())
                .unwrap();
            if !file.patch_complete {
                assert_eq!(f.applicability, Applicability::Unavailable);
            }
        }
    }
    assert_eq!(referenced.len(), evidence.len(), "no orphan evidence");
    // No anchor may point outside the snapshot's actual hunks.
    for ev in &facts.evidence {
        let Some(anchor) = &ev.anchor else { continue };
        if matches!(anchor.kind, AnchorKind::DiffLine | AnchorKind::DiffRange) {
            let file = snapshot
                .files
                .iter()
                .find(|x| Some(&x.id) == anchor.file_id.as_ref())
                .expect("anchor file exists");
            assert!(file.patch_complete && !file.is_binary);
            assert_eq!(anchor.diff_digest.as_ref(), Some(&file.diff_digest));
            let line = find_line(file, anchor.side.unwrap(), anchor.line.unwrap())
                .unwrap_or_else(|| panic!("anchor outside hunks: {anchor:?}"));
            let expected = match anchor.side.unwrap() {
                Side::Right => LineKind::Addition,
                Side::Left => LineKind::Deletion,
            };
            assert_eq!(line.kind, expected);
            assert_eq!(
                ev.value.as_ref().unwrap()["snippet"],
                json!(snippet(&line.text))
            );
            assert!(anchor.start_line.is_none());
        } else {
            assert!(anchor.line.is_none() && anchor.side.is_none());
        }
    }
}

#[test]
fn demo_fixture_is_deterministic_and_order_independent() {
    let snapshot = crate::demo::snapshot(0);
    let config = DiffFactsConfig::default();
    let first = json(&run(&snapshot, &config));
    assert_eq!(first, json(&run(&snapshot, &config)));

    let mut reversed = snapshot.clone();
    reversed.files.reverse();
    assert_eq!(first, json(&run(&reversed, &config)));
    // A deterministic but non-trivial shuffle.
    let mut shuffled = snapshot.clone();
    let n = shuffled.files.len();
    for i in 0..n {
        shuffled.files.swap(i, (i * 7 + 3) % n);
    }
    assert_eq!(first, json(&run(&shuffled, &config)));

    assert!(!first.contains("/home/"), "no absolute paths");
}

#[test]
fn demo_fixture_facts_hold_invariants() {
    let snapshot = crate::demo::snapshot(0);
    let facts = run(&snapshot, &DiffFactsConfig::default());
    assert_invariants(&snapshot, &facts);
    assert_eq!(facts.coverage.completeness, Completeness::Partial); // shell script
    assert_eq!(
        facts.coverage.reason_codes,
        vec!["unsupported_language".to_string()]
    );
    let summary = by_rule(&facts, rules::PR_SUMMARY, "");
    assert_eq!(summary.len(), 1);
    let m = &summary[0].extensions["measurement"];
    let adds: u32 = snapshot.files.iter().map(|f| f.additions).sum();
    assert_eq!(m["additions"], json!(adds));
    assert_eq!(m["changedFiles"], json!(snapshot.files.len()));
    assert_eq!(by_rule(&facts, rules::TESTS_TOUCHED, "").len(), 1);
    assert!(!by_rule(&facts, rules::TEST_ASSERTIONS_ADDED, "api/identity_test.go").is_empty());
    assert_eq!(
        outcome(
            &facts,
            "script/multiaccount-review/02-repository.sh",
            rules::TESTS_REMOVED
        ),
        ScopeOutcome::Unknown {
            reason: "unsupported_language".into()
        }
    );
    // Facts carry the engine identity the scheduler records on the run.
    assert_eq!(facts.engine_version, ENGINE_VERSION);
    assert_eq!(
        facts.configuration_digest,
        DiffFactsConfig::default().digest()
    );
}

#[test]
fn refresh_changes_only_the_affected_file_ids() {
    let config = DiffFactsConfig::default();
    let a = run(&crate::demo::snapshot(0), &config);
    let b = run(&crate::demo::snapshot(1), &config);
    let ids = |f: &DiffFacts, path: &str| -> Vec<String> {
        f.findings
            .iter()
            .filter(|x| x.anchor.current_path.as_deref() == Some(path))
            .map(|x| x.id.to_string())
            .collect()
    };
    assert_eq!(
        ids(&a, "api/identity_test.go"),
        ids(&b, "api/identity_test.go")
    );
    assert_ne!(ids(&a, "git/client.go"), ids(&b, "git/client.go"));
    assert_ne!(a.input_digest, b.input_digest);
}

const MIXED: &str = "\
diff --git a/.env.production b/.env.production
index 1111111..2222222 100644
--- a/.env.production
+++ b/.env.production
@@ -1 +1 @@
-TOKEN=old-secret
+TOKEN=new-secret
diff --git a/.github/workflows/ci.yml b/.github/workflows/ci.yml
index 1111111..2222222 100644
--- a/.github/workflows/ci.yml
+++ b/.github/workflows/ci.yml
@@ -1,2 +1,2 @@
 on: push
-permissions: read-all
+permissions: write-all
diff --git a/Cargo.toml b/Cargo.toml
index 1111111..2222222 100644
--- a/Cargo.toml
+++ b/Cargo.toml
@@ -5,1 +5,2 @@
 serde = \"1\"
+regex = \"1\"
diff --git a/app/src/generated/Thing.ts b/app/src/generated/Thing.ts
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/app/src/generated/Thing.ts
@@ -0,0 +1 @@
+export type Thing = { a: string };
diff --git a/assets/logo.png b/assets/logo.png
index 1111111..2222222 100644
Binary files a/assets/logo.png and b/assets/logo.png differ
diff --git a/lib/tool.rb b/lib/tool.rb
index 1111111..2222222 100644
--- a/lib/tool.rb
+++ b/lib/tool.rb
@@ -1 +1 @@
-def run; end
+def run(x); end
diff --git a/old_name.rs b/new_name.rs
similarity index 100%
rename from old_name.rs
rename to new_name.rs
diff --git a/script.sh b/script.sh
old mode 100644
new mode 100755
diff --git a/src/api.rs b/src/api.rs
index 1111111..2222222 100644
--- a/src/api.rs
+++ b/src/api.rs
@@ -10,3 +10,3 @@
 // context
-pub fn scan(a: u32) -> u32 {
+pub fn scan(a: u64) -> u64 {
     a
diff --git a/tests/cli.rs b/tests/cli.rs
deleted file mode 100644
index 4444444..0000000
--- a/tests/cli.rs
+++ /dev/null
@@ -1,4 +0,0 @@
-#[test]
-fn works() {
-    assert_eq!(1, 1);
-}
";

#[test]
fn edge_cases_have_defined_outcomes() {
    let snapshot = synthetic(MIXED);
    let facts = run(&snapshot, &DiffFactsConfig::default());
    assert_invariants(&snapshot, &facts);
    let file_fact = |path: &str| {
        let f = by_rule(&facts, rules::FILE_CHANGE, path);
        assert_eq!(f.len(), 1, "{path}");
        f[0].extensions["measurement"].clone()
    };

    // Binary: metadata only, counts unknown, content rules not applicable.
    let png = file_fact("assets/logo.png");
    assert_eq!(png["operation"], json!("binary"));
    assert_eq!(png["additions"], Value::Null);
    assert_eq!(
        outcome(&facts, "assets/logo.png", rules::PUBLIC_API),
        ScopeOutcome::NotApplicable {
            reason: "binary_file".into()
        }
    );
    // Binary makes PR line totals unknown rather than undercounted.
    let summary = &by_rule(&facts, rules::PR_SUMMARY, "")[0].extensions["measurement"];
    assert_eq!(summary["additions"], Value::Null);
    assert_eq!(summary["changedFiles"], json!(10));

    // Rename without content, mode-only change.
    let renamed = file_fact("new_name.rs");
    assert_eq!(renamed["operation"], json!("renamed"));
    assert_eq!(renamed["previousPath"], json!("old_name.rs"));
    assert_eq!(
        outcome(&facts, "new_name.rs", rules::PUBLIC_API),
        ScopeOutcome::Analyzed { findings: 0 }
    );
    assert_eq!(file_fact("script.sh")["operation"], json!("mode-only"));

    // Deleted test file: removed tests anchored on the LEFT side.
    assert_eq!(file_fact("tests/cli.rs")["operation"], json!("deleted"));
    let removed = by_rule(&facts, rules::TESTS_REMOVED, "tests/cli.rs");
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].assertion_kind, AssertionKind::AttentionHint);
    let m = &removed[0].extensions["measurement"];
    assert_eq!(
        (m["assertions"].clone(), m["testDefinitions"].clone()),
        (json!(1), json!(1))
    );

    // Public API change, both sides.
    let api = by_rule(&facts, rules::PUBLIC_API, "src/api.rs");
    assert_eq!(api.len(), 1);
    assert_eq!(api[0].evidence_ids.len(), 2);

    // Generated file: flagged, content not scanned by default.
    assert_eq!(
        by_rule(&facts, rules::GENERATED_FILE, "app/src/generated/Thing.ts").len(),
        1
    );
    assert_eq!(
        outcome(&facts, "app/src/generated/Thing.ts", rules::PUBLIC_API),
        ScopeOutcome::NotRun {
            reason: "generated_file".into()
        }
    );
    let scanned = run(
        &snapshot,
        &DiffFactsConfig {
            scan_generated_files: true,
            ..Default::default()
        },
    );
    assert_eq!(
        by_rule(&scanned, rules::PUBLIC_API, "app/src/generated/Thing.ts").len(),
        1
    );
    assert_ne!(scanned.configuration_digest, facts.configuration_digest);

    // Unsupported language: explicit unknown, but still file metadata.
    assert_eq!(
        outcome(&facts, "lib/tool.rb", rules::PUBLIC_API),
        ScopeOutcome::Unknown {
            reason: "unsupported_language".into()
        }
    );
    assert_eq!(file_fact("lib/tool.rb")["language"], Value::Null);

    // Dependency manifest and CI config with line evidence.
    let dep = by_rule(&facts, rules::DEPENDENCY_MANIFEST, "Cargo.toml");
    assert_eq!(dep.len(), 1);
    assert_eq!(dep[0].kinds, vec![FindingKind::Dependency]);
    let ci = by_rule(&facts, rules::CONFIG_CHANGE, ".github/workflows/ci.yml");
    assert_eq!(ci[0].attention.level, AttentionLevel::Investigate);
    assert_eq!(ci[0].evidence_ids.len(), 3); // two lines + measurement

    // Environment files never leak values.
    assert!(!json(&facts).contains("secret"));
    assert_eq!(
        by_rule(&facts, rules::CONFIG_CHANGE, ".env.production").len(),
        1
    );
}

#[test]
fn incomplete_patches_get_no_invented_facts() {
    let mut snapshot = synthetic(MIXED);
    snapshot.complete = false;
    for f in &mut snapshot.files {
        if f.path == "src/api.rs" || f.path == "Cargo.toml" {
            f.patch_complete = false;
        }
    }
    let facts = run(&snapshot, &DiffFactsConfig::default());
    assert_invariants(&snapshot, &facts);
    assert_eq!(facts.coverage.completeness, Completeness::Partial);
    assert!(
        facts
            .coverage
            .reason_codes
            .contains(&"patch_incomplete".into())
    );
    assert!(
        facts
            .coverage
            .reason_codes
            .contains(&"snapshot_incomplete".into())
    );
    assert!(by_rule(&facts, rules::PUBLIC_API, "src/api.rs").is_empty());
    assert_eq!(
        outcome(&facts, "src/api.rs", rules::PUBLIC_API),
        ScopeOutcome::Unknown {
            reason: "patch_incomplete".into()
        }
    );
    let file = &by_rule(&facts, rules::FILE_CHANGE, "src/api.rs")[0];
    assert_eq!(file.applicability, Applicability::Unavailable);
    assert_eq!(file.extensions["measurement"]["additions"], Value::Null);
    let dep = &by_rule(&facts, rules::DEPENDENCY_MANIFEST, "Cargo.toml")[0];
    assert_eq!(dep.applicability, Applicability::Unavailable);
    assert_eq!(dep.evidence_ids.len(), 1, "no line evidence");
    let summary = &by_rule(&facts, rules::PR_SUMMARY, "")[0];
    assert_eq!(summary.applicability, Applicability::Unavailable);
    assert_eq!(
        summary.extensions["measurement"]["changedFiles"],
        Value::Null
    );
    assert_eq!(summary.extensions["measurement"]["additions"], Value::Null);
}

#[test]
fn large_files_are_bounded_not_dropped() {
    let mut diff = String::from(
        "diff --git a/src/big.rs b/src/big.rs\nnew file mode 100644\n--- /dev/null\n+++ b/src/big.rs\n@@ -0,0 +1,50 @@\n",
    );
    for i in 0..50 {
        diff.push_str(&format!("+    assert!(x == {i});\n"));
    }
    let snapshot = synthetic(&diff);
    let small = DiffFactsConfig {
        large_change_lines: 40,
        max_scan_lines_per_file: 10,
        ..Default::default()
    };
    let facts = run(&snapshot, &small);
    assert_invariants(&snapshot, &facts);
    assert_eq!(by_rule(&facts, rules::LARGE_CHANGE, "src/big.rs").len(), 1);
    assert_eq!(
        outcome(&facts, "src/big.rs", rules::TEST_ASSERTIONS_ADDED),
        ScopeOutcome::NotRun {
            reason: "file_too_large".into()
        }
    );

    let capped = run(
        &snapshot,
        &DiffFactsConfig {
            max_evidence_per_finding: 5,
            ..Default::default()
        },
    );
    assert_invariants(&snapshot, &capped);
    let f = &by_rule(&capped, rules::TEST_ASSERTIONS_ADDED, "src/big.rs")[0];
    assert_eq!(f.evidence_ids.len(), 5);
    assert_eq!(f.extensions["measurement"]["assertions"], json!(50));
    assert_eq!(
        f.extensions["measurement"]["evidenceTruncated"],
        json!(true)
    );
}

#[test]
fn line_shift_keeps_finding_ids() {
    let a = synthetic(MIXED);
    let b = synthetic(&MIXED.replace("@@ -10,3 +10,3 @@", "@@ -20,3 +20,3 @@"));
    let config = DiffFactsConfig::default();
    let (fa, fb) = (run(&a, &config), run(&b, &config));
    let api = |f: &DiffFacts| by_rule(f, rules::PUBLIC_API, "src/api.rs")[0].id.clone();
    assert_eq!(api(&fa), api(&fb));
    // Evidence still points at the real, shifted lines.
    assert_invariants(&b, &fb);
}

#[test]
fn run_id_scopes_evidence_but_not_findings() {
    let snapshot = crate::demo::snapshot(0);
    let config = DiffFactsConfig::default();
    let sid = SnapshotId::derive("test/repo", 1, "b", "h");
    let rid = AnalysisRunId::new("run_x");
    let base = DiffFactsInput {
        snapshot: &snapshot,
        snapshot_id: &sid,
        canonical_repo_id: "test/repo",
        run_id: None,
        observed_at: OBSERVED,
    };
    let a = scan(&base, &config);
    let b = scan(
        &DiffFactsInput {
            run_id: Some(&rid),
            ..base
        },
        &config,
    );
    let ids = |f: &DiffFacts| f.findings.iter().map(|x| x.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(&a), ids(&b));
    assert_ne!(a.evidence[0].id, b.evidence[0].id);
    assert!(
        b.findings
            .iter()
            .all(|f| f.source.run_id.as_ref() == Some(&rid))
    );
}
