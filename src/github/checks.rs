//! Bounded, read-only import of checks and commit statuses for an exact SHA.
use super::{GhRunner, PrRef};
use crate::model::{
    AnnotationLevel, Completeness, ImportedCheckAnnotation, ImportedCheckRun, ImportedCommitStatus,
    NativeSourceKey,
};
use crate::util::now_rfc3339;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const PAGE_SIZE: usize = 100;
const MAX_REQUESTS: usize = 20;
const MAX_RECORDS: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coverage {
    pub completeness: Completeness,
    pub reasons: Vec<String>,
}
impl Default for Coverage {
    fn default() -> Self {
        Self {
            completeness: Completeness::Unknown,
            reasons: vec!["not_imported".into()],
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckImport {
    pub sha: String,
    pub runs: Vec<ImportedCheckRun>,
    pub annotations: Vec<ImportedCheckAnnotation>,
    pub statuses: Vec<ImportedCommitStatus>,
    pub runs_coverage: Coverage,
    pub statuses_coverage: Coverage,
    pub annotation_coverage: Vec<(u64, Coverage)>,
}
struct Budget {
    requests: usize,
    records: usize,
}
impl Budget {
    fn get(&mut self, gh: &dyn GhRunner, url: &str) -> Result<Value, String> {
        if self.requests >= MAX_REQUESTS {
            return Err("aggregate_request_limit_reached".into());
        }
        if self.records >= MAX_RECORDS {
            return Err("aggregate_record_limit_reached".into());
        }
        self.requests += 1;
        let bytes = gh
            .run(&["api", url], None)
            .map_err(|e| classify(&e.to_string()))?;
        serde_json::from_slice(&bytes).map_err(|_| "malformed_response".into())
    }
    fn take(&mut self, count: usize) -> usize {
        let accepted = count.min(MAX_RECORDS - self.records);
        self.records += accepted;
        accepted
    }
}
fn classify(error: &str) -> String {
    let e = error.to_ascii_lowercase();
    if e.contains("403") || e.contains("forbidden") || e.contains("resource not accessible") {
        "forbidden"
    } else if e.contains("429") || e.contains("rate limit") {
        "rate_limited"
    } else {
        "unavailable"
    }
    .into()
}
fn coverage(reason: Option<String>, has_records: bool) -> Coverage {
    let mut result = Coverage {
        completeness: Completeness::Complete,
        reasons: vec![],
    };
    if let Some(reason) = reason {
        problem(&mut result, reason, has_records);
    }
    result
}
fn problem(result: &mut Coverage, reason: impl Into<String>, has_records: bool) {
    let reason = reason.into();
    if !result.reasons.contains(&reason) {
        result.reasons.push(reason);
    }
    result.completeness = if has_records {
        Completeness::Partial
    } else {
        Completeness::Failed
    };
}
fn finalize(result: &mut Coverage, accepted: usize) {
    if !result.reasons.is_empty() {
        result.completeness = if accepted == 0 {
            Completeness::Failed
        } else {
            Completeness::Partial
        };
    }
}
fn pages(
    budget: &mut Budget,
    gh: &dyn GhRunner,
    base: &str,
    field: &str,
    expected: Option<usize>,
    count_field: bool,
    sha: Option<&str>,
) -> (Vec<Value>, Coverage) {
    let mut records = Vec::new();
    let mut expected = expected;
    for page in 1..=10 {
        let url = format!(
            "{base}{}per_page={PAGE_SIZE}&page={page}",
            if base.contains('?') { '&' } else { '?' }
        );
        let result = budget.get(gh, &url).and_then(|v| {
            if sha.is_some_and(|expected| {
                v.get("sha")
                    .and_then(Value::as_str)
                    .is_some_and(|actual| actual != expected)
            }) {
                return Err("sha_mismatch".into());
            }
            if count_field && page == 1 {
                expected = Some(
                    v.get("total_count")
                        .and_then(Value::as_u64)
                        .and_then(|n| usize::try_from(n).ok())
                        .ok_or("missing_total_count")?,
                );
            }
            let arr = if field.is_empty() {
                v.as_array()
            } else {
                v.get(field).and_then(Value::as_array)
            };
            arr.cloned().ok_or_else(|| "malformed_response".into())
        });
        let batch = match result {
            Ok(batch) => batch,
            Err(e) => return (records.clone(), coverage(Some(e), !records.is_empty())),
        };
        let len = batch.len();
        let accepted = budget.take(len);
        records.extend(batch.into_iter().take(accepted));
        if accepted < len {
            return (
                records.clone(),
                coverage(
                    Some("aggregate_record_limit_reached".into()),
                    !records.is_empty(),
                ),
            );
        }
        if len < PAGE_SIZE {
            return if expected.is_some_and(|n| records.len() != n) {
                (
                    records.clone(),
                    coverage(Some("count_mismatch".into()), !records.is_empty()),
                )
            } else {
                (records, coverage(None, true))
            };
        }
        if expected.is_some_and(|n| records.len() >= n) {
            return if Some(records.len()) == expected {
                (records, coverage(None, true))
            } else {
                (
                    records.clone(),
                    coverage(Some("count_mismatch".into()), !records.is_empty()),
                )
            };
        }
    }
    (
        records.clone(),
        coverage(Some("page_limit_reached".into()), !records.is_empty()),
    )
}
fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}
fn integer(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(Value::as_u64)
}

pub fn fetch(gh: &dyn GhRunner, pr: &PrRef, sha: &str) -> CheckImport {
    let mut out = CheckImport {
        sha: sha.into(),
        ..Default::default()
    };
    let mut budget = Budget {
        requests: 0,
        records: 0,
    };
    let root = format!("repos/{}/{}/commits/{sha}", pr.owner, pr.repo);
    let (runs, mut run_coverage) = pages(
        &mut budget,
        gh,
        &format!("{root}/check-runs"),
        "check_runs",
        None,
        true,
        None,
    );
    // Reserve status retrieval ahead of potentially numerous annotation requests.
    let (statuses, mut status_coverage) = pages(
        &mut budget,
        gh,
        &format!("{root}/status"),
        "statuses",
        None,
        false,
        Some(sha),
    );
    for status in statuses {
        if let Some(id) = integer(&status, "id") {
            out.statuses.push(ImportedCommitStatus {
                github_status_id: id,
                context: text(&status, "context").unwrap_or_default(),
                state: text(&status, "state").unwrap_or_default(),
                description: text(&status, "description"),
                target_url: text(&status, "target_url"),
                sha: sha.into(),
                updated_at: text(&status, "updated_at"),
                observed_at: now_rfc3339(),
            });
        } else {
            problem(
                &mut status_coverage,
                "missing_status_id",
                !out.statuses.is_empty(),
            );
        }
    }
    finalize(&mut status_coverage, out.statuses.len());
    out.statuses_coverage = status_coverage;
    for run in runs {
        let Some(id) = integer(&run, "id") else {
            problem(&mut run_coverage, "missing_run_id", !out.runs.is_empty());
            continue;
        };
        if text(&run, "head_sha").as_deref() != Some(sha) {
            problem(&mut run_coverage, "sha_mismatch", !out.runs.is_empty());
            continue;
        }
        let Some(count) = run
            .pointer("/output/annotations_count")
            .and_then(Value::as_u64)
        else {
            problem(
                &mut run_coverage,
                "missing_annotation_count",
                !out.runs.is_empty(),
            );
            continue;
        };
        if text(&run, "name").is_none() || text(&run, "status").is_none() {
            problem(&mut run_coverage, "malformed_run", !out.runs.is_empty());
            continue;
        }
        let (annotations, mut ann_coverage) = if count == 0 {
            (vec![], coverage(None, true))
        } else {
            pages(
                &mut budget,
                gh,
                &format!("repos/{}/{}/check-runs/{id}/annotations", pr.owner, pr.repo),
                "",
                Some(count as usize),
                false,
                None,
            )
        };
        for (ordinal, item) in annotations.iter().enumerate() {
            let (Some(path), Some(start), Some(end), Some(message)) = (
                text(item, "path"),
                integer(item, "start_line"),
                integer(item, "end_line"),
                text(item, "message"),
            ) else {
                problem(
                    &mut ann_coverage,
                    "malformed_annotation",
                    out.annotations.iter().any(|a| a.check_run_id == id),
                );
                continue;
            };
            let key = integer(item, "id")
                .map(|id| NativeSourceKey::Upstream(id.to_string()))
                .unwrap_or_else(|| {
                    ImportedCheckAnnotation::content_fingerprint(
                        id,
                        &path,
                        start as u32,
                        end as u32,
                        &message,
                        ordinal as u32,
                    )
                });
            let level = match text(item, "annotation_level").as_deref() {
                Some("notice") => AnnotationLevel::Notice,
                Some("warning") => AnnotationLevel::Warning,
                Some("failure") => AnnotationLevel::Failure,
                _ => AnnotationLevel::Unknown,
            };
            out.annotations.push(ImportedCheckAnnotation {
                check_run_id: id,
                key,
                path,
                start_line: start as u32,
                end_line: end as u32,
                start_column: integer(item, "start_column").map(|n| n as u32),
                end_column: integer(item, "end_column").map(|n| n as u32),
                annotation_level: level,
                title: text(item, "title"),
                message,
                raw_details: text(item, "raw_details"),
                head_sha: sha.into(),
                observed_at: now_rfc3339(),
            });
        }
        let accepted = out
            .annotations
            .iter()
            .filter(|a| a.check_run_id == id)
            .count();
        if accepted != count as usize {
            problem(
                &mut ann_coverage,
                "annotation_count_mismatch",
                accepted != 0,
            );
        }
        finalize(&mut ann_coverage, accepted);
        out.annotation_coverage.push((id, ann_coverage));
        out.runs.push(ImportedCheckRun {
            github_check_run_id: id,
            app_slug: run
                .pointer("/app/slug")
                .and_then(Value::as_str)
                .map(str::to_owned),
            name: text(&run, "name").unwrap_or_default(),
            status: text(&run, "status").unwrap_or_default(),
            conclusion: text(&run, "conclusion"),
            head_sha: sha.into(),
            started_at: text(&run, "started_at"),
            completed_at: text(&run, "completed_at"),
            details_url: text(&run, "details_url"),
            summary: run
                .pointer("/output/summary")
                .and_then(Value::as_str)
                .map(str::to_owned),
            annotations_count: count as u32,
            observed_at: now_rfc3339(),
        });
    }
    finalize(&mut run_coverage, out.runs.len());
    out.runs_coverage = run_coverage;
    out
}
