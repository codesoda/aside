//! Read-only analysis and GitHub-feedback API (#33, analyze.md §13).
//!
//! Every list is paginated with a bounded, server-clamped page size and an
//! opaque cursor pinned to the analysis snapshot and to a digest of the exact
//! result set it was issued for. If either moved on, the request fails with a
//! recoverable `409 cursor_stale` telling the client to resync (reload
//! `GET /api/v1/session`, then restart pagination without a cursor), the same
//! contract as the SSE `resync` event (#27). Pages are never silently shifted.
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
//! TODO(#30): ranked order and priority buckets; findings are currently in
//! stable first-seen order.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{ApiError, ApiResult, AppState, db_error};
use crate::{
    model::{
        AnalysisMode, AnalysisSummary, FindingId, LIFECYCLE_EVENT_VERSION, SourceCoverage,
        SourceState, ThreadSource, stable_digest,
    },
    rules,
    session::{AnalysisRead, source},
    store::{ObservationLabel, Raw},
};

pub(super) const DEFAULT_PAGE_SIZE: usize = 50;
pub(super) const MAX_PAGE_SIZE: usize = 200;
const CURSOR_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Query parameters
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Cursors
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Cursor {
    v: u32,
    /// Which list it belongs to (`findings`, `runs`, ...).
    k: String,
    /// Analysis snapshot the cursor was issued against.
    s: String,
    /// Digest of the query's filters; a cursor is bound to its query.
    q: String,
    /// Digest of the full ordered result set.
    r: String,
    /// Index of the next item.
    o: usize,
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

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

/// Coverage of the sources a list was derived from. `complete` is true only
/// when every one of them ran over its whole scope, so an empty page with
/// `complete: false` never means "nothing there".
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Page<T> {
    event_version: u32,
    snapshot_id: String,
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
    let keys: Vec<Option<&str>> = items.iter().map(|(k, _)| Some(k.as_str())).collect();
    let result_digest = stable_digest("aside-api-results/v1", &keys);
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
            if cursor.r != result_digest || cursor.o > items.len() {
                return Err(stale(&cursor.s, read, "results_changed"));
            }
            cursor.o
        }
    };
    let total = items.len();
    let end = offset.saturating_add(limit).min(total);
    let has_more = end < total;
    let next_cursor = has_more.then(|| {
        Cursor {
            v: CURSOR_VERSION,
            k: listing.kind.into(),
            s: snapshot.into(),
            q: query_digest.clone(),
            r: result_digest.clone(),
            o: end,
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

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FindingItem {
    /// The stored finding, verbatim.
    finding: Value,
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
            for thread in read
                .threads
                .iter()
                .filter(|t| t.source == ThreadSource::Github)
            {
                if !include_ignored && !in_scope(&read, Some(&thread.path)) {
                    excluded += 1;
                    continue;
                }
                let value = serde_json::to_value(thread).map_err(|_| ApiError::internal())?;
                // Comment IDs and resolution are part of the key so an
                // edited thread invalidates outstanding cursors.
                let comments: Vec<&str> = thread.comments.iter().map(|c| c.id.as_str()).collect();
                let key = format!("{}\0{}\0{}", thread.id, thread.resolved, comments.join(","));
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

#[cfg(test)]
#[path = "analysis_tests.rs"]
mod tests;
