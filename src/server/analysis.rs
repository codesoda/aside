//! Read-only analysis and GitHub-feedback API (#33, analyze.md §13).
//!
//! Every list is paginated with a bounded, server-clamped page size and an
//! opaque keyset cursor: the sort key of the last item served, pinned to the
//! analysis snapshot and to a digest of the query's filters (#64). Pages
//! resume strictly after that key, so a live run appending findings never
//! invalidates an outstanding cursor — the pathological case for #22's
//! non-blocking guarantee. `409 cursor_stale` is reserved for genuine
//! changes: the snapshot moved (head/base push), or the already-paged region
//! itself changed (the cursor's key vanished: a finding relabeled, a thread
//! edited, an exclusion removing items). The response tells the client to
//! resync (reload `GET /api/v1/session`, then restart pagination without a
//! cursor), the same contract as the SSE `resync` event (#27). Pages are
//! never silently shifted.
//!
//! Scope filtering happens here, on the server: items on files excluded by
//! repository rules are dropped unless the client explicitly opts in with
//! `includeIgnored=true`, and every query is confined to the session's PR.
//!
//! Every response carries the coverage of the sources it was derived from
//! (`src/model/lifecycle.rs`), so an empty page from an incomplete source is
//! distinguishable from a genuinely empty result.
//!
//! Stored records are served through [`Raw`] (#18): the verbatim stored JSON,
//! so contract values this build does not recognise are passed through rather
//! than collapsed to `unknown`.
//!
//! TODO(#26): finding triage mutation (`PATCH .../state`) is not exposed here.
//! TODO(#34): rerun/cancel endpoints (`POST /analysis/runs`, `.../cancel`).
//! Findings pages stay in stable first-seen order; the ranked, explainable
//! review order (#30) is served whole by `GET /api/v1/analysis/queue` (#38).

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use super::{ApiError, ApiResult, AppState, db_error};
use crate::{
    model::{
        AnalysisMode, AnalysisSummary, Completeness, FindingId, KnownCount,
        LIFECYCLE_EVENT_VERSION, NativeSourceKey, NativeSourceRef, RunStatus, SourceCoverage,
        SourceKind, SourceState, ThreadSource, stable_digest,
    },
    priority::{self, ObservedFinding, QueueInput, ReviewQueue, ScopeCounts},
    rules,
    session::{AnalysisRead, source},
    store::{ObservationLabel, Raw},
};

pub(super) const DEFAULT_PAGE_SIZE: usize = 50;
pub(super) const MAX_PAGE_SIZE: usize = 200;
// Keyset cursor wire version (#64): `last` replaced the offset + result-set
// digest of version 1.
const CURSOR_VERSION: u32 = 2;

/// Shared list parameters. Values are parsed by hand so an absurd `limit` is
/// clamped rather than rejected and bad booleans are a clean 400.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ListQuery {
    limit: Option<String>,
    cursor: Option<String>,
    /// Optional pin: fail with 409 unless this is still the current snapshot.
    snapshot_id: Option<String>,
    include_ignored: Option<String>,
    include_historical: Option<String>,
    provider_id: Option<String>,
    status: Option<String>,
    run_snapshot_id: Option<String>,
}

fn flag(name: &str, value: Option<&str>) -> ApiResult<bool> {
    match value {
        None => Ok(false),
        Some("true" | "1") => Ok(true),
        Some("false" | "0") => Ok(false),
        Some(_) => Err(ApiError::bad(format!("{name} must be true or false"))),
    }
}

/// Clamped page size. Never trusts the client beyond [`MAX_PAGE_SIZE`].
pub(super) fn page_size(limit: Option<&str>) -> ApiResult<usize> {
    let Some(raw) = limit.map(str::trim) else {
        return Ok(DEFAULT_PAGE_SIZE);
    };
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ApiError::bad("limit must be a non-negative integer"));
    }
    // Overflowing values are absurdly large, not invalid.
    let n = raw.parse::<usize>().unwrap_or(usize::MAX);
    Ok(n.clamp(1, MAX_PAGE_SIZE))
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Cursor {
    v: u32,
    /// Which list it belongs to (`findings`, `runs`, ...).
    k: String,
    /// Analysis snapshot the cursor was issued against.
    s: String,
    /// Digest of the query's filters; a cursor is bound to its query.
    q: String,
    /// Sort key of the last item served; pages resume strictly after it.
    last: String,
}

impl Cursor {
    fn encode(&self) -> String {
        hex::encode(serde_json::to_vec(self).unwrap_or_default())
    }

    fn decode(raw: &str) -> ApiResult<Self> {
        let invalid = || ApiError::new(StatusCode::BAD_REQUEST, "invalid_cursor", "invalid cursor");
        let bytes = hex::decode(raw.trim()).map_err(|_| invalid())?;
        let cursor: Self = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        if cursor.v != CURSOR_VERSION {
            return Err(invalid());
        }
        Ok(cursor)
    }
}

fn stale(expected: &str, read: &AnalysisRead<'_>, reason: &str) -> ApiError {
    let mut error = ApiError::new(
        StatusCode::CONFLICT,
        "cursor_stale",
        "the analysis results changed since this page was issued; resync and restart pagination",
    );
    error.details = Some(json!({
        "retryable": true,
        "reason": reason,
        "expectedSnapshotId": expected,
        "snapshotId": read.summary.snapshot_id,
        "sequence": read.sequence,
        "eventVersion": LIFECYCLE_EVENT_VERSION,
        // Same recovery as the SSE `resync` event (#27).
        "resync": {"reload": "/api/v1/session", "restartWithoutCursor": true},
    }));
    error
}

/// Coverage of the sources a list was derived from. `complete` is true only
/// when every one of them ran over its whole scope, so an empty page with
/// `complete: false` never means "nothing there".
#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub(super) struct PageCoverage {
    complete: bool,
    sources: Vec<SourceCoverage>,
}

impl PageCoverage {
    fn of(summary: &AnalysisSummary, relevant: impl Fn(&SourceCoverage) -> bool) -> Self {
        let sources: Vec<SourceCoverage> = summary
            .sources
            .iter()
            .filter(|s| relevant(s))
            .cloned()
            .collect();
        Self {
            // No relevant source at all proves nothing either.
            complete: !sources.is_empty()
                && sources
                    .iter()
                    .all(|s| matches!(s.state, SourceState::Clean | SourceState::HasResults)),
            sources,
        }
    }
}

/// One page of a list. `T` is typed per endpoint on the client: stored
/// records are served verbatim (`Raw`), so the TypeScript item type is the
/// generated contract type of that record.
#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub(super) struct Page<T> {
    event_version: u32,
    snapshot_id: String,
    #[ts(type = "number")]
    sequence: u64,
    items: Vec<T>,
    next_cursor: Option<String>,
    has_more: bool,
    /// Items matching the query across all pages (after scope filtering).
    total: usize,
    limit: usize,
    include_ignored: bool,
    /// Items withheld because their file is excluded by repository rules.
    excluded_by_scope: usize,
    coverage: PageCoverage,
}

/// Inputs to [`paginate`] other than the items.
struct Listing<'q> {
    kind: &'static str,
    query: &'q ListQuery,
    /// Canonical filter values the cursor is bound to.
    filters: Vec<Option<String>>,
    include_ignored: bool,
    excluded_by_scope: usize,
}

/// Cuts one page out of an already filtered, stably ordered list of
/// `(key, item)` pairs, validating the cursor against the current state.
fn paginate<T>(
    read: &AnalysisRead<'_>,
    listing: Listing<'_>,
    items: Vec<(String, T)>,
    coverage: PageCoverage,
) -> ApiResult<Page<T>> {
    let limit = page_size(listing.query.limit.as_deref())?;
    let snapshot = read.summary.snapshot_id.as_str();
    if let Some(pinned) = listing.query.snapshot_id.as_deref()
        && pinned != snapshot
    {
        return Err(stale(pinned, read, "snapshot_changed"));
    }
    let mut filter_fields = vec![Some(listing.kind.to_string())];
    filter_fields.extend(listing.filters);
    let filter_refs: Vec<Option<&str>> = filter_fields.iter().map(Option::as_deref).collect();
    let query_digest = stable_digest("aside-api-query/v1", &filter_refs);
    let offset = match listing.query.cursor.as_deref() {
        None => 0,
        Some(raw) => {
            let cursor = Cursor::decode(raw)?;
            if cursor.k != listing.kind || cursor.q != query_digest {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_cursor",
                    "cursor belongs to a different query",
                ));
            }
            if cursor.s != snapshot {
                return Err(stale(&cursor.s, read, "snapshot_changed"));
            }
            // Keyset resume (#64): continue strictly after the last key
            // served. Items appended past the cursor — the normal case while
            // a provider is streaming findings, whose store order
            // (`first_seen_at, id`) puts new arrivals at the tail — never
            // invalidate it and are picked up by later pages. A key that
            // vanished means the already-paged region itself changed (a
            // finding relabeled, a thread edited, an exclusion removing
            // items); the client must resync rather than silently skip or
            // duplicate. A mid-walk insertion whose key sorts *before* the
            // cursor is not served by this walk; the SSE event that follows
            // the write triggers the client's next full load, so it is never
            // silently lost from durable state.
            let Some(offset) = items
                .iter()
                .position(|(key, _)| key == &cursor.last)
                .map(|position| position + 1)
            else {
                return Err(stale(&cursor.s, read, "results_changed"));
            };
            offset
        }
    };
    let total = items.len();
    let end = offset.saturating_add(limit).min(total);
    let has_more = end < total;
    // `has_more` implies `end >= 1` because the page size is clamped to at
    // least one, so the last served key always exists.
    let next_cursor = has_more.then(|| {
        Cursor {
            v: CURSOR_VERSION,
            k: listing.kind.into(),
            s: snapshot.into(),
            q: query_digest.clone(),
            last: items[end - 1].0.clone(),
        }
        .encode()
    });
    let items = items
        .into_iter()
        .skip(offset)
        .take(end - offset)
        .map(|(_, item)| item)
        .collect();
    Ok(Page {
        event_version: LIFECYCLE_EVENT_VERSION,
        snapshot_id: snapshot.into(),
        sequence: read.sequence,
        items,
        next_cursor,
        has_more,
        total,
        limit,
        include_ignored: listing.include_ignored,
        excluded_by_scope: listing.excluded_by_scope,
        coverage,
    })
}

fn raw_json<T: Serialize + serde::de::DeserializeOwned>(raw: &Raw<T>) -> ApiResult<Value> {
    serde_json::from_str(raw.json()).map_err(|_| ApiError::internal())
}

fn in_scope(read: &AnalysisRead<'_>, path: Option<&str>) -> bool {
    // PR-level items (no path) are never excluded by file rules.
    path.is_none_or(|p| !rules::evaluate(&read.rules, p).ignored)
}

fn is_analysis_source(s: &SourceCoverage) -> bool {
    s.mode != AnalysisMode::GithubImport
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SummaryView {
    session_id: String,
    sequence: u64,
    #[serde(flatten)]
    summary: AnalysisSummary,
}

/// `GET /api/v1/analysis/summary`: the same authoritative summary the session
/// view carries. TODO(#30): scope-aware counts and priority buckets.
pub(super) async fn summary(State(state): State<AppState>) -> ApiResult<Json<SummaryView>> {
    let view = state
        .session
        .read_analysis(|read| {
            Ok(SummaryView {
                session_id: state.session.id.clone(),
                sequence: read.sequence,
                summary: read.summary,
            })
        })
        .map_err(db_error)?;
    Ok(Json(view))
}

/// `GET /api/v1/analysis/runs`: every run and import attempt of this PR,
/// oldest snapshot first. Filters: `providerId`, `status`, `runSnapshotId`.
pub(super) async fn runs(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Page<Value>>> {
    let page = state.session.read_analysis(|read| {
        Ok((|| {
            let mut items = Vec::new();
            for run in read
                .store
                .list_analysis_runs(read.pr_id)
                .map_err(db_error)?
            {
                let value = raw_json(&run)?;
                let field = |name: &str| value.get(name).and_then(Value::as_str);
                let keep = query
                    .provider_id
                    .as_deref()
                    .is_none_or(|p| field("providerId") == Some(p))
                    && query
                        .status
                        .as_deref()
                        .is_none_or(|s| field("status") == Some(s))
                    && query
                        .run_snapshot_id
                        .as_deref()
                        .is_none_or(|s| field("snapshotId") == Some(s));
                if keep {
                    // Status is part of the key: a run finishing between
                    // pages changes what earlier pages showed.
                    let key = format!(
                        "{}\0{}",
                        run.value().id.as_str(),
                        field("status").unwrap_or_default()
                    );
                    items.push((key, value));
                }
            }
            let coverage = PageCoverage::of(&read.summary, |_| true);
            paginate(
                &read,
                Listing {
                    kind: "runs",
                    query: &query,
                    filters: vec![
                        query.provider_id.clone(),
                        query.status.clone(),
                        query.run_snapshot_id.clone(),
                    ],
                    include_ignored: false,
                    excluded_by_scope: 0,
                },
                items,
                coverage,
            )
        })())
    });
    Ok(Json(page.map_err(db_error)??))
}

#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub(super) struct FindingItem {
    /// The stored finding, verbatim.
    #[ts(as = "crate::model::ReviewFinding")]
    finding: Value,
    #[ts(type = "\"current\" | \"historical\"")]
    label: &'static str,
    latest_snapshot_id: String,
    latest_applicability: String,
    first_seen_at: String,
    last_seen_at: String,
}

/// `GET /api/v1/analysis/findings`: current findings (`includeHistorical`
/// adds historical ones), excluding files out of scope unless
/// `includeIgnored=true`.
pub(super) async fn findings(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Page<FindingItem>>> {
    let include_ignored = flag("includeIgnored", query.include_ignored.as_deref())?;
    let include_historical = flag("includeHistorical", query.include_historical.as_deref())?;
    let page = state.session.read_analysis(|read| {
        Ok((|| {
            let mut items = Vec::new();
            let mut excluded = 0;
            for stored in read
                .store
                .list_findings(read.pr_id, include_historical)
                .map_err(db_error)?
            {
                let path = stored.finding.value().anchor.current_path.as_deref();
                if !include_ignored && !in_scope(&read, path) {
                    excluded += 1;
                    continue;
                }
                let label = match stored.label {
                    ObservationLabel::Current => "current",
                    ObservationLabel::Historical => "historical",
                };
                let key = format!("{}\0{label}", stored.finding.value().id.as_str());
                items.push((
                    key,
                    FindingItem {
                        finding: raw_json(&stored.finding)?,
                        label,
                        latest_snapshot_id: stored.latest_snapshot_id.as_str().into(),
                        latest_applicability: stored.latest_applicability,
                        first_seen_at: stored.first_seen_at,
                        last_seen_at: stored.last_seen_at,
                    },
                ));
            }
            let coverage = PageCoverage::of(&read.summary, is_analysis_source);
            paginate(
                &read,
                Listing {
                    kind: "findings",
                    query: &query,
                    filters: vec![
                        Some(include_ignored.to_string()),
                        Some(include_historical.to_string()),
                    ],
                    include_ignored,
                    excluded_by_scope: excluded,
                },
                items,
                coverage,
            )
        })())
    });
    Ok(Json(page.map_err(db_error)??))
}

/// `GET /api/v1/analysis/findings/{findingId}/evidence`. A finding of another
/// PR, or one on an excluded file without `includeIgnored=true`, is a 404.
pub(super) async fn evidence(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Page<Value>>> {
    let include_ignored = flag("includeIgnored", query.include_ignored.as_deref())?;
    let page = state.session.read_analysis(|read| {
        Ok((|| {
            let finding = read
                .store
                .list_findings(read.pr_id, true)
                .map_err(db_error)?
                .into_iter()
                .find(|f| f.finding.value().id.as_str() == id)
                .ok_or_else(ApiError::missing)?;
            let path = finding.finding.value().anchor.current_path.as_deref();
            if !include_ignored && !in_scope(&read, path) {
                return Err(ApiError::missing());
            }
            let mut items = Vec::new();
            for evidence in read
                .store
                .list_finding_evidence(&FindingId::new(&id))
                .map_err(db_error)?
            {
                items.push((
                    evidence.value().id.as_str().to_string(),
                    raw_json(&evidence)?,
                ));
            }
            let coverage = PageCoverage::of(&read.summary, is_analysis_source);
            paginate(
                &read,
                Listing {
                    kind: "evidence",
                    query: &query,
                    filters: vec![Some(id.clone()), Some(include_ignored.to_string())],
                    include_ignored,
                    excluded_by_scope: 0,
                },
                items,
                coverage,
            )
        })())
    });
    Ok(Json(page.map_err(db_error)??))
}

/// Read-only non-thread GitHub imports, with source-specific coverage.
/// Reviews and PR comments live in the snapshot; checks live in the imported
/// source store. Neither is converted into a finding or modified on read.
pub(super) async fn records(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Page<Value>>> {
    let repository_id = state.session.repository.id.clone();
    let page = state.session.read_analysis(|read| {
        Ok((|| {
            let mut items = Vec::new();
            for (kind, source_kind, records) in [
                (
                    "reviews",
                    SourceKind::GithubReviewSummary,
                    read.reviews
                        .iter()
                        .map(serde_json::to_value)
                        .collect::<Result<Vec<_>, _>>(),
                ),
                (
                    "issue_comments",
                    SourceKind::GithubIssueComment,
                    read.issue_comments
                        .iter()
                        .map(serde_json::to_value)
                        .collect::<Result<Vec<_>, _>>(),
                ),
            ] {
                for (index, value) in records
                    .map_err(|_| ApiError::internal())?
                    .into_iter()
                    .enumerate()
                {
                    let mut value = value;
                    let native_id = value
                        .get("nodeId")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .or_else(|| {
                            value
                                .get("databaseId")
                                .and_then(Value::as_u64)
                                .map(|id| id.to_string())
                        });
                    let url = value.get("url").and_then(Value::as_str).map(str::to_owned);
                    if let Value::Object(ref mut object) = value {
                        object.insert("resourceKind".into(), json!(kind));
                        if native_id.is_none() {
                            object.insert("native".into(), Value::Null);
                        }
                        if let Some(native_id) = native_id {
                            object.insert(
                                "native".into(),
                                serde_json::to_value(NativeSourceRef {
                                    host: "github.com".into(),
                                    repository_id: repository_id.clone(),
                                    source_kind,
                                    key: NativeSourceKey::Upstream(native_id),
                                    external_url: url,
                                })
                                .map_err(|_| ApiError::internal())?,
                            );
                        }
                    }
                    items.push((format!("{kind}:{index:020}"), value));
                }
            }
            items.extend(imported_check_items(&read)?);
            items.sort_by(|a, b| a.0.cmp(&b.0));
            let mut coverage = PageCoverage::of(&read.summary, |s| {
                matches!(
                    s.source_id.as_str(),
                    source::CHECK_RUNS | source::CHECK_ANNOTATIONS | source::COMMIT_STATUSES
                )
            });
            if let Some(template) = read
                .summary
                .sources
                .iter()
                .find(|s| s.source_id == source::REVIEWS_AND_COMMENTS)
            {
                for (id, kind, count, completeness) in [
                    (
                        "github.reviews",
                        "reviews",
                        read.reviews.len(),
                        read.reviews_completeness,
                    ),
                    (
                        "github.issue_comments",
                        "issue_comments",
                        read.issue_comments.len(),
                        read.issue_comments_completeness,
                    ),
                ] {
                    let mut source = template.clone();
                    source.source_id = id.into();
                    source.item_kind = kind.into();
                    source.completeness = completeness;
                    source.status = Some(match completeness {
                        Completeness::Complete => RunStatus::Succeeded,
                        Completeness::Failed => RunStatus::Failed,
                        _ => RunStatus::Partial,
                    });
                    source.items = if completeness == Completeness::Failed {
                        KnownCount::Unknown
                    } else {
                        KnownCount::Known {
                            value: count as u32,
                        }
                    };
                    source.state = match completeness {
                        Completeness::Complete if count == 0 => SourceState::Clean,
                        Completeness::Complete => SourceState::HasResults,
                        Completeness::Failed => SourceState::Failed,
                        _ => SourceState::Incomplete,
                    };
                    // The combined source's limitations may concern the other
                    // import; avoid misattributing them to this connection.
                    source.reason_codes = if completeness == Completeness::Complete {
                        vec![]
                    } else {
                        vec!["github_source_incomplete".into()]
                    };
                    source.limitations = if completeness == Completeness::Complete {
                        vec![]
                    } else {
                        vec![format!(
                            "{kind} import is {completeness:?}; coverage is not complete"
                        )]
                    };
                    coverage.sources.push(source);
                }
            }
            coverage.complete = !coverage.sources.is_empty()
                && coverage
                    .sources
                    .iter()
                    .all(|s| matches!(s.state, SourceState::Clean | SourceState::HasResults));
            paginate(
                &read,
                Listing {
                    kind: "records",
                    query: &query,
                    filters: vec![],
                    include_ignored: true,
                    excluded_by_scope: 0,
                },
                items,
                coverage,
            )
        })())
    });
    Ok(Json(page.map_err(db_error)??))
}

/// Legacy checks-only listing; does not include reviews or PR comments.
pub(super) async fn checks(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Page<Value>>> {
    let page = state.session.read_analysis(|read| {
        Ok((|| {
            let items = imported_check_items(&read)?;
            let coverage = PageCoverage::of(&read.summary, |s| {
                matches!(
                    s.source_id.as_str(),
                    source::CHECK_RUNS | source::CHECK_ANNOTATIONS | source::COMMIT_STATUSES
                )
            });
            paginate(
                &read,
                Listing {
                    kind: "checks",
                    query: &query,
                    filters: vec![],
                    include_ignored: true,
                    excluded_by_scope: 0,
                },
                items,
                coverage,
            )
        })())
    });
    Ok(Json(page.map_err(db_error)??))
}

fn imported_check_items(read: &AnalysisRead<'_>) -> ApiResult<Vec<(String, Value)>> {
    let mut items = Vec::new();
    for record in read
        .store
        .list_imported_sources(read.pr_id, None)
        .map_err(db_error)?
    {
        // The source table can contain other native imports; this listing
        // claims only the three check-family coverage sources.
        let kind = record.resource_kind.split(':').next().unwrap_or_default();
        if !matches!(kind, "check_runs" | "check_annotations" | "commit_statuses") {
            continue;
        }
        if !record.available {
            continue;
        }
        let mut value: Value =
            serde_json::from_str(&record.raw_json).map_err(|_| ApiError::internal())?;
        if let Value::Object(ref mut object) = value {
            if record.resource_kind.starts_with("check_runs:")
                && record.resource_kind.ends_with(&read.head_sha)
                && let Some(id) = object.get("githubCheckRunId").and_then(Value::as_u64)
                && let Some((_, coverage)) = read
                    .annotation_coverage
                    .iter()
                    .find(|(run_id, _)| *run_id == id)
            {
                object.insert(
                    "annotationCoverage".into(),
                    serde_json::to_value(coverage).map_err(|_| ApiError::internal())?,
                );
            }
            object.insert("resourceKind".into(), json!(record.resource_kind));
            object.insert(
                "native".into(),
                serde_json::to_value(record.native.value()).map_err(|_| ApiError::internal())?,
            );
            if let Some((_, sha)) = record.resource_kind.split_once(':') {
                object.insert("currentHead".into(), json!(sha == read.head_sha));
            }
        }
        items.push((format!("imported:{:020}", record.id), value));
    }
    Ok(items)
}

/// `GET /api/v1/github-feedback/threads`: imported GitHub review threads with
/// every comment, scope-filtered, with the thread import's completeness.
pub(super) async fn feedback(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Page<Value>>> {
    let include_ignored = flag("includeIgnored", query.include_ignored.as_deref())?;
    let page = state.session.read_analysis(|read| {
        Ok((|| {
            let mut items = Vec::new();
            let mut excluded = 0;
            let comment_nodes = read
                .store
                .github_comment_node_ids(read.pr_id)
                .map_err(db_error)?;
            for thread in read
                .threads
                .iter()
                .filter(|t| t.source == ThreadSource::Github)
            {
                if !include_ignored && !in_scope(&read, Some(&thread.path)) {
                    excluded += 1;
                    continue;
                }
                let mut value = serde_json::to_value(thread).map_err(|_| ApiError::internal())?;
                let native = read.github_threads.iter().find(|native| {
                    thread.github_thread_id.as_deref() == Some(native.node_id.as_str())
                });
                if let Value::Object(ref mut object) = value {
                    if let Some(Value::Array(comments)) = object.get_mut("comments") {
                        for comment in comments {
                            if let Value::Object(comment) = comment
                                && let Some(id) = comment.get("id").and_then(Value::as_str)
                                && let Some(node_id) = comment_nodes.get(id)
                            {
                                comment.insert("githubNodeId".into(), json!(node_id));
                            }
                        }
                    }
                    object.insert(
                        "github".into(),
                        native
                            .map(serde_json::to_value)
                            .transpose()
                            .map_err(|_| ApiError::internal())?
                            .unwrap_or(Value::Null),
                    );
                    object.insert(
                        "resolutionKnown".into(),
                        json!(native.is_some_and(|native| {
                            !native.rest_only
                                && !native
                                    .unavailable_fields
                                    .iter()
                                    .any(|field| field == "*" || field == "isResolved")
                        })),
                    );
                }
                // Pin the cursor to the entire served payload: native edits,
                // reply metadata, and resolution changes must stale an old walk.
                let payload = serde_json::to_string(&value).map_err(|_| ApiError::internal())?;
                let key = format!(
                    "{}\0{}",
                    thread.id,
                    stable_digest("aside-feedback-item/v1", &[Some(&payload)])
                );
                items.push((key, value));
            }
            let coverage =
                PageCoverage::of(&read.summary, |s| s.source_id == source::REVIEW_THREADS);
            paginate(
                &read,
                Listing {
                    kind: "feedback",
                    query: &query,
                    filters: vec![Some(include_ignored.to_string())],
                    include_ignored,
                    excluded_by_scope: excluded,
                },
                items,
                coverage,
            )
        })())
    });
    Ok(Json(page.map_err(db_error)??))
}

/// `GET /api/v1/analysis/queue` body: the explainable review queue and
/// scope-aware counts (#30), derived on read. Purely presentational: reading
/// it writes nothing, so it can never mark a file reviewed, and exclusion
/// edits change it without rerunning any engine.
#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub(super) struct QueueView {
    session_id: String,
    #[ts(type = "number")]
    sequence: u64,
    event_version: u32,
    snapshot_id: String,
    queue: ReviewQueue,
    counts: ScopeCounts,
}

/// `GET /api/v1/analysis/queue`. Not paginated: one item per changed file,
/// already bounded by the PR. Reads decoded findings (`Raw::value`); nothing
/// is written back, so lossy `#[serde(other)]` decoding cannot persist.
pub(super) async fn queue(State(state): State<AppState>) -> ApiResult<Json<QueueView>> {
    let view = state.session.read_analysis(|read| {
        Ok((|| {
            let stored = read
                .store
                .list_findings(read.pr_id, true)
                .map_err(db_error)?;
            let findings: Vec<ObservedFinding> = stored.iter().map(ObservedFinding::from).collect();
            let mut relations = Vec::new();
            let mut user_states = Vec::new();
            for f in &findings {
                relations.extend(
                    read.store
                        .list_finding_relations(&f.finding.id)
                        .map_err(db_error)?,
                );
                if let Some(state) = read
                    .store
                    .get_finding_user_state(read.session_id, &f.finding.id)
                    .map_err(db_error)?
                {
                    user_states.push(state);
                }
            }
            let input = QueueInput {
                files: &read.file_states,
                rules: &read.rules,
                findings: &findings,
                relations: &relations,
                threads: &read.github_threads,
                user_states: &user_states,
                coverage: &read.summary.sources,
            };
            Ok(QueueView {
                session_id: read.session_id.into(),
                sequence: read.sequence,
                event_version: LIFECYCLE_EVENT_VERSION,
                snapshot_id: read.summary.snapshot_id.as_str().into(),
                queue: priority::build_queue(&input),
                counts: priority::scope_counts(&input),
            })
        })())
    });
    Ok(Json(view.map_err(db_error)??))
}

#[cfg(test)]
#[path = "analysis_tests.rs"]
mod tests;
