//! Conservative, producer-bound SARIF 2.1 classification. Sidecars are trusted only
//! as local producer attestations, not as independent proof of repository identity.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use serde::{
    Deserialize,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Value, json};
use std::fmt;

const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_ITEMS: usize = 10_000;
const MAX_PATHS: usize = 512;
const MAX_BLOB_BYTES: usize = 256 * 1024;
const MAX_TOTAL_BLOB_BYTES: usize = 8 * 1024 * 1024;

// Value alone silently overwrites duplicate JSON object keys.
struct Unique(Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Check;
        impl<'de> Visitor<'de> for Check {
            type Value = Unique;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("JSON without duplicate keys")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Unique, E> {
                Ok(Unique(Value::Bool(v)))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Unique, E> {
                Ok(Unique(json!(v)))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_none<E: de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Unique, A::Error> {
                let mut out = Vec::new();
                while let Some(Unique(value)) = seq.next_element::<Unique>()? {
                    out.push(value);
                }
                Ok(Unique(Value::Array(out)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Unique, A::Error> {
                let mut out = serde_json::Map::new();
                while let Some((key, Unique(value))) = map.next_entry::<String, Unique>()? {
                    if out.insert(key, value).is_some() {
                        return Err(de::Error::custom("duplicate JSON key"));
                    }
                }
                Ok(Unique(Value::Object(out)))
            }
        }
        deserializer.deserialize_any(Check)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Sidecar {
    schema_version: u32,
    repository: String,
    commit: String,
    export_sha256: String,
    analyzed_paths: Vec<AnalyzedPath>,
    tool: Tool,
    enabled_rules: Vec<String>,
    scanner_policy: String,
    config_policy: String,
    export_policy: String,
    suppression_policy: String,
    fingerprint: Fingerprint,
    execution_complete: bool,
    export_complete: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnalyzedPath {
    path: String,
    blob_sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Tool {
    name: String,
    version: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fingerprint {
    key: String,
    version: String,
}

struct Snapshot {
    sidecar: Sidecar,
    // A repeated identity makes both entries ambiguous, never a matched pair.
    entries: BTreeMap<(String, String), Option<Value>>,
    ambiguous: bool,
}

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn safe_path(path: &str) -> bool {
    super::valid_repo_path(path)
        && !path.contains('%')
        && !path.contains('?')
        && !path.contains('#')
}

fn nonempty(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 4096
}

fn parse(
    git: &super::PinnedGit,
    repository: &str,
    revision: &str,
    artifact: &Path,
    metadata: &Path,
) -> Result<Snapshot, &'static str> {
    let bytes = super::read_artifact(artifact, MAX_BYTES).map_err(|e| e.reason)?;
    let meta = super::read_artifact(metadata, MAX_BYTES).map_err(|e| e.reason)?;
    let Unique(meta_value) = serde_json::from_slice(&meta).map_err(|_| "invalid_metadata")?;
    let sidecar: Sidecar = serde_json::from_value(meta_value).map_err(|_| "invalid_metadata")?;
    if sidecar.schema_version != 1
        || sidecar.repository != repository
        || sidecar.commit != revision
        || !digest(&sidecar.export_sha256)
        || sidecar.export_sha256 != super::sha256(&bytes)
        || !sidecar.execution_complete
        || !sidecar.export_complete
        || sidecar.analyzed_paths.is_empty()
        || sidecar.analyzed_paths.len() > MAX_PATHS
        || sidecar.enabled_rules.is_empty()
        || sidecar.enabled_rules.len() > MAX_ITEMS
        || !nonempty(&sidecar.tool.name)
        || !nonempty(&sidecar.tool.version)
        || !nonempty(&sidecar.scanner_policy)
        || !nonempty(&sidecar.config_policy)
        || !nonempty(&sidecar.export_policy)
        || !nonempty(&sidecar.suppression_policy)
        || !nonempty(&sidecar.fingerprint.key)
        || !nonempty(&sidecar.fingerprint.version)
    {
        return Err("invalid_metadata");
    }
    let mut paths = BTreeSet::new();
    let mut total_blob_bytes = 0usize;
    for item in &sidecar.analyzed_paths {
        if !safe_path(&item.path)
            || !digest(&item.blob_sha256)
            || !paths.insert(item.path.as_str())
            || git
                .blob(revision, &item.path, MAX_BLOB_BYTES)
                .map(|b| {
                    total_blob_bytes += b.len();
                    total_blob_bytes > MAX_TOTAL_BLOB_BYTES || super::sha256(&b) != item.blob_sha256
                })
                .unwrap_or(true)
        {
            return Err("invalid_metadata");
        }
    }
    let rules: BTreeSet<_> = sidecar.enabled_rules.iter().map(String::as_str).collect();
    if rules.len() != sidecar.enabled_rules.len() || rules.iter().any(|r| !nonempty(r)) {
        return Err("invalid_metadata");
    }
    let Unique(doc) = serde_json::from_slice(&bytes).map_err(|_| "invalid_sarif")?;
    if doc.get("version").and_then(Value::as_str) != Some("2.1.0")
        || doc.get("$schema").is_some_and(|s| !s.is_string())
    {
        return Err("invalid_sarif");
    }
    let runs = doc
        .get("runs")
        .and_then(Value::as_array)
        .ok_or("invalid_sarif")?;
    if runs.len() != 1 {
        return Err("invalid_sarif");
    }
    let run = &runs[0];
    if doc.get("externalPropertyFileReferences").is_some()
        || run.get("externalPropertyFileReferences").is_some()
        || run
            .get("invocations")
            .and_then(Value::as_array)
            .is_none_or(|v| {
                v.len() != 1 || v[0].get("executionSuccessful") != Some(&Value::Bool(true))
            })
    {
        return Err("incomplete_sarif");
    }
    let empty_array_if_present =
        |value: Option<&Value>| value.is_none_or(|v| v.as_array().is_some_and(Vec::is_empty));
    let invocation = &run["invocations"][0];
    if !empty_array_if_present(invocation.get("toolExecutionNotifications"))
        || !empty_array_if_present(invocation.get("toolConfigurationNotifications"))
    {
        return Err("incomplete_sarif");
    }
    if run
        .pointer("/versionControlProvenance/0/revisionId")
        .and_then(Value::as_str)
        != Some(revision)
        || run
            .get("versionControlProvenance")
            .and_then(Value::as_array)
            .is_none_or(|v| v.len() != 1)
        || run.pointer("/tool/driver/name").and_then(Value::as_str) != Some(&sidecar.tool.name)
        || run.pointer("/tool/driver/version").and_then(Value::as_str)
            != Some(&sidecar.tool.version)
    {
        return Err("invalid_sarif");
    }
    // The sidecar attests only the driver, not extension identity or rules.
    if !empty_array_if_present(run.pointer("/tool/extensions")) {
        return Err("unsupported_sarif_tool_component");
    }
    let results = run
        .get("results")
        .and_then(Value::as_array)
        .ok_or("invalid_sarif")?;
    if results.len() > MAX_ITEMS {
        return Err("limit_exceeded");
    }
    let mut entries = BTreeMap::new();
    let mut ambiguous = false;
    for result in results {
        if result.get("rule").is_some() || result.get("ruleIndex").is_some() {
            return Err("unsupported_sarif_rule_reference");
        }
        let rule = result.get("ruleId").and_then(Value::as_str);
        let fingerprint = result
            .pointer(&format!(
                "/fingerprints/{}",
                sidecar
                    .fingerprint
                    .key
                    .replace('~', "~0")
                    .replace('/', "~1")
            ))
            .and_then(Value::as_str);
        let locations = result.get("locations").and_then(Value::as_array);
        let uri = locations.and_then(|l| {
            if l.len() == 1 {
                l[0].pointer("/physicalLocation/artifactLocation/uri")
                    .and_then(Value::as_str)
            } else {
                None
            }
        });
        let artifact_location = locations
            .and_then(|l| l.first())
            .and_then(|l| l.pointer("/physicalLocation/artifactLocation"));
        let valid = artifact_location
            .is_some_and(|a| a.get("uriBaseId").is_none() && a.get("index").is_none())
            && (result.get("kind").is_none() || result.get("kind") == Some(&json!("fail")))
            && rule.is_some_and(|r| rules.contains(r))
            && fingerprint.is_some_and(nonempty)
            && uri.is_some_and(|p| safe_path(p) && paths.contains(p))
            && result.get("suppressions").is_none();
        // Invalid results make the export scope unusable, including its negatives.
        if !valid {
            return Err("invalid_result");
        }
        let (Some(rule), Some(fingerprint)) = (rule, fingerprint) else {
            return Err("invalid_result");
        };
        let key = (rule.to_owned(), fingerprint.to_owned());
        match entries.entry(key) {
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                ambiguous = true;
                entry.insert(None);
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Some(result.clone()));
            }
        }
    }
    Ok(Snapshot {
        sidecar,
        entries,
        ambiguous,
    })
}

// A head without an attestation is readable, but cannot prove identities or absence.
fn raw_head(artifact: &Path, revision: &str) -> Result<Vec<Value>, &'static str> {
    let bytes = super::read_artifact(artifact, MAX_BYTES).map_err(|e| e.reason)?;
    let Unique(doc) = serde_json::from_slice(&bytes).map_err(|_| "invalid_sarif")?;
    if doc.get("version") != Some(&json!("2.1.0"))
        || doc.get("$schema").is_some_and(|s| !s.is_string())
    {
        return Err("invalid_sarif");
    }
    let runs = doc
        .get("runs")
        .and_then(Value::as_array)
        .ok_or("invalid_sarif")?;
    if runs.len() != 1
        || runs[0]
            .get("versionControlProvenance")
            .and_then(Value::as_array)
            .is_none_or(|v| v.len() != 1 || v[0].get("revisionId") != Some(&json!(revision)))
    {
        return Err("invalid_sarif");
    }
    let results = runs[0]
        .get("results")
        .and_then(Value::as_array)
        .ok_or("invalid_sarif")?;
    if results.len() > MAX_ITEMS {
        return Err("limit_exceeded");
    }
    if results.iter().any(|r| {
        !r.is_object()
            || r.get("locations")
                .and_then(Value::as_array)
                .is_none_or(|l| {
                    l.len() != 1
                        || l[0]
                            .pointer("/physicalLocation/artifactLocation/uri")
                            .and_then(Value::as_str)
                            .is_none_or(|p| !safe_path(p))
                        || l[0]
                            .pointer("/physicalLocation/artifactLocation/uriBaseId")
                            .is_some()
                        || l[0]
                            .pointer("/physicalLocation/artifactLocation/index")
                            .is_some()
                })
    }) {
        return Err("invalid_result");
    }
    Ok(results.clone())
}

fn compatible(a: &Sidecar, b: &Sidecar) -> bool {
    a.repository == b.repository
        && a.tool.name == b.tool.name
        && a.tool.version == b.tool.version
        && a.enabled_rules.iter().collect::<BTreeSet<_>>()
            == b.enabled_rules.iter().collect::<BTreeSet<_>>()
        && a.scanner_policy == b.scanner_policy
        && a.config_policy == b.config_policy
        && a.export_policy == b.export_policy
        && a.suppression_policy == b.suppression_policy
        && a.fingerprint.key == b.fingerprint.key
        && a.fingerprint.version == b.fingerprint.version
        && a.analyzed_paths
            .iter()
            .map(|p| &p.path)
            .collect::<BTreeSet<_>>()
            == b.analyzed_paths
                .iter()
                .map(|p| &p.path)
                .collect::<BTreeSet<_>>()
}

fn classify(
    value: &Option<Value>,
    base: Option<&Snapshot>,
    compatible: bool,
    key: &(String, String),
) -> (&'static str, &'static str) {
    if value.is_none() {
        ("unknown", "duplicate_fingerprint")
    } else if !compatible {
        ("unknown", "unverified_comparison")
    } else if let Some(base) = base.and_then(|b| b.entries.get(key)) {
        if base.is_some() {
            ("existing", "matched_base")
        } else {
            ("unknown", "duplicate_base_fingerprint")
        }
    } else {
        ("introduced", "absent_from_complete_base")
    }
}

pub(super) fn run(
    git: &super::PinnedGit,
    repository: &str,
    head: &Path,
    base: Option<&Path>,
    head_metadata: Option<&Path>,
    base_metadata: Option<&Path>,
) -> super::SourceResult {
    let mut reasons = Vec::new();
    let mut head_error = None;
    let mut base_error = None;
    let mut raw = Vec::new();
    let head_artifact = head;
    let head = head_metadata.and_then(|m| match parse(git, repository, &git.head, head, m) {
        Ok(s) => Some(s),
        Err(reason) => {
            head_error = Some(reason);
            reasons.push(reason);
            None
        }
    });
    if head_metadata.is_none() {
        reasons.push("missing_head_metadata");
        match raw_head(head_artifact, &git.head) {
            Ok(records) => raw = records,
            Err(reason) => {
                head_error = Some(reason);
                reasons.push(reason);
            }
        }
    }
    let base_revision = git.comparison_base.as_deref();
    let baseline = match (base, base_metadata, base_revision) {
        (Some(path), Some(meta), Some(revision)) => {
            match parse(git, repository, revision, path, meta) {
                Ok(s) => Some(s),
                Err(reason) => {
                    base_error = Some(reason);
                    reasons.push(reason);
                    None
                }
            }
        }
        _ => {
            reasons.push("missing_base");
            None
        }
    };
    if head.as_ref().is_some_and(|h| h.ambiguous) || baseline.as_ref().is_some_and(|b| b.ambiguous)
    {
        reasons.push("duplicate_fingerprint");
    }
    let compatible = match (&head, &baseline) {
        (Some(h), Some(b)) if compatible(&h.sidecar, &b.sidecar) && reasons.is_empty() => true,
        (Some(_), Some(_)) => {
            reasons.push("incompatible_scope");
            false
        }
        _ => false,
    };
    let mut results: Vec<Value> = head.as_ref().map(|h| h.entries.iter().map(|((rule, fingerprint), value)| {
        let (classification, reason) = classify(value, baseline.as_ref(), compatible, &(rule.clone(), fingerprint.clone()));
        json!({"rule_id": rule, "fingerprint": fingerprint, "classification": classification,
            "reason": reason, "result": value})
    }).collect()).unwrap_or_default();
    results.extend(raw.into_iter().map(|result| {
        json!({"rule_id": null, "fingerprint": null,
        "classification": "unknown", "reason": "unverified_head", "result": result})
    }));
    let state = if head_error == Some("artifact_permission_denied")
        || base_error == Some("artifact_permission_denied")
    {
        "forbidden"
    } else if head.is_none() && results.is_empty() && head_error.is_some() {
        "unknown"
    } else if compatible && reasons.is_empty() {
        "complete"
    } else {
        "partial"
    };
    Ok(
        json!({"state": state, "coverage": if compatible { "complete" } else { "unknown" },
        "reasons": reasons, "limits": {"max_bytes": MAX_BYTES, "max_items": MAX_ITEMS, "max_paths": MAX_PATHS,
            "max_blob_bytes": MAX_BLOB_BYTES, "max_total_blob_bytes": MAX_TOTAL_BLOB_BYTES},
        "provenance": {"repository": repository, "head": git.head, "comparison_base": git.comparison_base,
            "head_error": head_error, "base_error": base_error,
            "export_sha256": head.as_ref().map(|s| &s.sidecar.export_sha256),
            "base_export_sha256": baseline.as_ref().map(|s| &s.sidecar.export_sha256),
            "scope": head.as_ref().map(|s| json!({"enabled_rules": s.sidecar.enabled_rules,
                "scanner_policy": s.sidecar.scanner_policy, "config_policy": s.sidecar.config_policy,
                "export_policy": s.sidecar.export_policy, "suppression_policy": s.sidecar.suppression_policy})),
            "tool": head.as_ref().map(|s| json!({"name": s.sidecar.tool.name, "version": s.sidecar.tool.version})),
            "fingerprint": head.as_ref().map(|s| json!({"key": s.sidecar.fingerprint.key, "version": s.sidecar.fingerprint.version}))},
        "results": results}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compatibility_rejects_policy_and_path_drift() {
        let make = |policy: &str, path: &str| Sidecar {
            schema_version: 1,
            repository: "a/b".into(),
            commit: "rev".into(),
            export_sha256: "0".repeat(64),
            analyzed_paths: vec![AnalyzedPath {
                path: path.into(),
                blob_sha256: "0".repeat(64),
            }],
            tool: Tool {
                name: "scanner".into(),
                version: "1".into(),
            },
            enabled_rules: vec!["R".into()],
            scanner_policy: policy.into(),
            config_policy: "config".into(),
            export_policy: "all".into(),
            suppression_policy: "all".into(),
            fingerprint: Fingerprint {
                key: "stable".into(),
                version: "1".into(),
            },
            execution_complete: true,
            export_complete: true,
        };
        assert!(compatible(&make("same", "a.rs"), &make("same", "a.rs")));
        assert!(!compatible(
            &make("same", "a.rs"),
            &make("different", "a.rs")
        ));
        assert!(!compatible(&make("same", "a.rs"), &make("same", "b.rs")));
    }
    #[test]
    fn paired_results_and_uncertain_scopes() {
        let make = |entries| Snapshot {
            sidecar: Sidecar {
                schema_version: 1,
                repository: "a/b".into(),
                commit: "rev".into(),
                export_sha256: "0".repeat(64),
                analyzed_paths: vec![],
                tool: Tool {
                    name: "scanner".into(),
                    version: "1".into(),
                },
                enabled_rules: vec![],
                scanner_policy: "p".into(),
                config_policy: "p".into(),
                export_policy: "p".into(),
                suppression_policy: "p".into(),
                fingerprint: Fingerprint {
                    key: "stable".into(),
                    version: "1".into(),
                },
                execution_complete: true,
                export_complete: true,
            },
            entries,
            ambiguous: false,
        };
        let key = ("R".to_owned(), "stable-1".to_owned());
        let value = Some(json!({"ruleId": "R"}));
        let empty = make(BTreeMap::new());
        let matched = make(BTreeMap::from([(key.clone(), value.clone())]));
        let duplicate = make(BTreeMap::from([(key.clone(), None)]));
        assert_eq!(classify(&value, Some(&empty), true, &key).0, "introduced");
        assert_eq!(classify(&value, Some(&matched), true, &key).0, "existing");
        assert_eq!(classify(&value, Some(&duplicate), true, &key).0, "unknown");
        assert_eq!(classify(&None, Some(&empty), true, &key).0, "unknown");
        assert_eq!(classify(&value, Some(&empty), false, &key).0, "unknown");
        assert_eq!(classify(&value, None, false, &key).0, "unknown");
    }
    #[test]
    fn structural_head_and_paired_parser() {
        use std::process::Command;
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "-q"]);
        std::fs::write(dir.path().join("a.rs"), "contents").unwrap();
        git(&["add", "a.rs"]);
        git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.test",
            "commit",
            "-qm",
            "test",
        ]);
        let revision = git(&["rev-parse", "HEAD"]);
        let pinned =
            super::super::PinnedGit::new(dir.path().join(".git"), &revision, &revision, true);
        let doc = |results: Value| {
            json!({"version":"2.1.0", "runs":[{
                "tool":{"driver":{"name":"scanner","version":"1"}},
                "versionControlProvenance":[{"revisionId": revision}],
                "invocations":[{"executionSuccessful":true}], "results":results
            }]})
        };
        let record = json!({"ruleId":"R", "kind":"fail", "fingerprints":{"stable":"id"},
            "locations":[{"physicalLocation":{"artifactLocation":{"uri":"a.rs"}}}]});
        let export = serde_json::to_vec(&doc(json!([record]))).unwrap();
        let head_path = dir.path().join("head.sarif");
        let base_path = dir.path().join("base.sarif");
        std::fs::write(&head_path, &export).unwrap();
        std::fs::write(&base_path, &export).unwrap();
        assert_eq!(raw_head(&head_path, &revision).unwrap().len(), 1);
        let metadata = |bytes: &[u8]| {
            json!({"schema_version":1,"repository":"a/b",
            "commit":revision,"export_sha256":super::super::sha256(bytes),
            "analyzed_paths":[{"path":"a.rs","blob_sha256":super::super::sha256(b"contents")}],
            "tool":{"name":"scanner","version":"1"},"enabled_rules":["R"],
            "scanner_policy":"p","config_policy":"p","export_policy":"p",
            "suppression_policy":"p","fingerprint":{"key":"stable","version":"1"},
            "execution_complete":true,"export_complete":true})
        };
        let head_meta = dir.path().join("head.json");
        let base_meta = dir.path().join("base.json");
        std::fs::write(&head_meta, serde_json::to_vec(&metadata(&export)).unwrap()).unwrap();
        std::fs::write(&base_meta, serde_json::to_vec(&metadata(&export)).unwrap()).unwrap();
        assert_eq!(
            pinned.blob(&revision, "a.rs", MAX_BLOB_BYTES).unwrap(),
            b"contents"
        );
        let paired = run(
            &pinned,
            "a/b",
            &head_path,
            Some(&base_path),
            Some(&head_meta),
            Some(&base_meta),
        )
        .unwrap();
        assert_eq!(paired["coverage"], "complete");
        assert_eq!(paired["results"][0]["classification"], "existing");
        assert_eq!(
            paired["provenance"]["export_sha256"],
            super::super::sha256(&export)
        );
        let unverified = run(&pinned, "a/b", &head_path, None, None, None).unwrap();
        assert_eq!(unverified["results"][0]["classification"], "unknown");
        assert!(unverified["results"][0]["fingerprint"].is_null());
        let missing = run(
            &pinned,
            "a/b",
            &dir.path().join("missing.sarif"),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(missing["provenance"]["head_error"], "artifact_not_found");
        for kind in [Value::Null, json!(7), json!("pass")] {
            let altered = serde_json::to_vec(&doc(json!([{"kind":kind, "ruleId":"R",
                "fingerprints":{"stable":"id"}, "locations":[{"physicalLocation":
                {"artifactLocation":{"uri":"a.rs"}}}]}])))
            .unwrap();
            std::fs::write(&head_path, &altered).unwrap();
            std::fs::write(&head_meta, serde_json::to_vec(&metadata(&altered)).unwrap()).unwrap();
            assert!(matches!(
                parse(&pinned, "a/b", &revision, &head_path, &head_meta),
                Err("invalid_result")
            ));
        }
        // A matching bare rule ID/fingerprint is not identity for different tool components.
        for with_extension in [true, false] {
            let mut reference = record.clone();
            reference["rule"] = json!({"id":"R", "toolComponent":{"name":"ScannerB"}});
            let mut modified = doc(json!([reference]));
            if with_extension {
                modified["runs"][0]["tool"]["extensions"] = json!([{"name":"ScannerB"}]);
            }
            let component = serde_json::to_vec(&modified).unwrap();
            std::fs::write(&base_path, &component).unwrap();
            std::fs::write(
                &base_meta,
                serde_json::to_vec(&metadata(&component)).unwrap(),
            )
            .unwrap();
            std::fs::write(&head_path, &export).unwrap();
            std::fs::write(&head_meta, serde_json::to_vec(&metadata(&export)).unwrap()).unwrap();
            let report = run(
                &pinned,
                "a/b",
                &head_path,
                Some(&base_path),
                Some(&head_meta),
                Some(&base_meta),
            )
            .unwrap();
            assert_eq!(report["coverage"], "unknown");
            assert_eq!(report["results"][0]["classification"], "unknown");
            assert_eq!(
                report["provenance"]["base_error"],
                if with_extension {
                    "unsupported_sarif_tool_component"
                } else {
                    "unsupported_sarif_rule_reference"
                }
            );
        }
        std::fs::write(&base_path, &export).unwrap();
        std::fs::write(&base_meta, serde_json::to_vec(&metadata(&export)).unwrap()).unwrap();
        let duplicated = serde_json::to_vec(&doc(json!([record, record]))).unwrap();
        std::fs::write(&head_path, &duplicated).unwrap();
        std::fs::write(
            &head_meta,
            serde_json::to_vec(&metadata(&duplicated)).unwrap(),
        )
        .unwrap();
        let duplicate = parse(&pinned, "a/b", &revision, &head_path, &head_meta).unwrap();
        assert!(duplicate.ambiguous);
        let output = run(
            &pinned,
            "a/b",
            &head_path,
            Some(&base_path),
            Some(&head_meta),
            Some(&base_meta),
        )
        .unwrap();
        assert_eq!(output["coverage"], "unknown");
        assert_eq!(output["results"][0]["classification"], "unknown");
        std::fs::write(&head_path, &export).unwrap();
        std::fs::write(&head_meta, serde_json::to_vec(&metadata(&export)).unwrap()).unwrap();
        let without_base = run(&pinned, "a/b", &head_path, None, Some(&head_meta), None).unwrap();
        assert_eq!(without_base["coverage"], "unknown");
        assert_eq!(without_base["results"][0]["classification"], "unknown");
    }
    #[test]
    fn conflicting_json_keys_are_rejected_at_any_depth() {
        assert!(
            serde_json::from_str::<Unique>(r#"{"runs":[{"tool":{"name":"a","name":"b"}}]}"#)
                .is_err()
        );
        assert!(serde_json::from_str::<Unique>(r#"{"runs":[{"tool":{"name":"a"}}]}"#).is_ok());
    }
    #[test]
    fn unsafe_uris_are_not_paths() {
        for path in [
            "../a",
            "/absolute",
            "https://example.test/a",
            "a%2fb",
            "a#fragment",
        ] {
            assert!(!safe_path(path), "{path}");
        }
    }
}
