//! Persistence for analysis runs, findings, evidence, imported source records
//! and local finding triage (#18, analyze.md §12).
//!
//! # Invariants
//!
//! - **Isolation:** nothing here reads or writes review acknowledgements,
//!   review sessions, threads or comments. Findings cannot change whether a
//!   file is reviewed.
//! - **No false current labels:** whether a finding is current is never
//!   stored on the finding. It is derived on read from the persisted current
//!   snapshot pointer, the observation's own applicability, and the status of
//!   the run that observed it. Reopening the store therefore cannot promote an
//!   old observation to current.
//! - **Source fidelity:** records are kept as the JSON they were written with
//!   ([`Raw`]). Several contract enums decode unknown values to a fallback
//!   (`#[serde(other)]`), so we never re-serialise a decoded record to store it;
//!   a round-trip through the store does not degrade data from a newer build.
//! - **All-or-nothing writes:** every batch runs in one `IMMEDIATE`
//!   transaction; any failure rolls back every row of the batch.
//!
//! Scheduling (queue, retries, cancellation) is owned by #22; this module only
//! records whatever run state the scheduler reports.

use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::model::{
    AnalysisRun, AnalysisRunId, Evidence, FindingId, FindingRelation, FindingUserState,
    NativeSourceRef, RelationTarget, ReviewFinding, RunStatus, SnapshotId, UserDisposition,
};
use crate::util::now_rfc3339;

use super::{NotFound, Store};

/// How many analysed snapshots per PR keep their runs and observations, in
/// addition to the current snapshot and any snapshot with unfinished runs.
pub const ANALYSIS_SNAPSHOT_RETENTION: usize = 5;

// ---------------------------------------------------------------------------
// Source-preserving records
// ---------------------------------------------------------------------------

/// A decoded record together with the exact JSON it came from.
///
/// Persist with [`Raw::json`], never by re-serialising [`Raw::value`]: decoding
/// maps enum values this build does not know to a fallback such as `unknown`,
/// and re-serialising would overwrite what a newer version wrote.
#[derive(Debug, Clone, PartialEq)]
pub struct Raw<T> {
    value: T,
    json: String,
}

impl<T: Serialize + DeserializeOwned> Raw<T> {
    /// Wraps a record produced by this build.
    pub fn new(value: T) -> Result<Self> {
        let json = serde_json::to_string(&value)?;
        Ok(Self { value, json })
    }

    /// Decodes `json`, keeping the original text verbatim.
    pub fn from_json(json: &str) -> Result<Self> {
        let value = serde_json::from_str(json).context("record does not match the contract")?;
        Ok(Self {
            value,
            json: json.to_string(),
        })
    }

    pub fn value(&self) -> &T {
        &self.value
    }

    pub fn json(&self) -> &str {
        &self.json
    }

    #[allow(dead_code)] // TODO(#33): not consumed by the binary yet.
    pub fn into_value(self) -> T {
        self.value
    }

    fn tree(&self) -> Result<Value> {
        Ok(serde_json::from_str(&self.json)?)
    }
}

/// String at a JSON pointer in the raw record, unknown values included.
fn raw_str(tree: &Value, pointer: &str) -> Option<String> {
    tree.pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn required_str(tree: &Value, pointer: &str) -> Result<String> {
    raw_str(tree, pointer).with_context(|| format!("record is missing {pointer}"))
}

fn enum_str<T: Serialize>(value: &T) -> Result<String> {
    match serde_json::to_value(value)? {
        Value::String(s) => Ok(s),
        other => bail!("expected a string enum, got {other}"),
    }
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Results of one run, written atomically.
#[derive(Debug, Clone)]
pub struct AnalysisBatch {
    /// The run's state after this batch (usually terminal). The run must have
    /// been recorded with [`Store::record_analysis_run`] first.
    pub run: Raw<AnalysisRun>,
    pub findings: Vec<Raw<ReviewFinding>>,
    /// Evidence is owned by `run`; it is deleted when the run is pruned.
    pub evidence: Vec<Raw<Evidence>>,
    pub relations: Vec<FindingRelation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchOutcome {
    /// Status actually stored. `Stale` when the PR's current snapshot had
    /// already moved past the run's snapshot.
    pub stored_status: RunStatus,
    pub stale: bool,
}

/// Whether a finding is shown as current. Derived on every read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationLabel {
    Current,
    Historical,
}

#[derive(Debug, Clone)]
pub struct StoredFinding {
    pub finding: Raw<ReviewFinding>,
    pub label: ObservationLabel,
    /// Most recent snapshot that observed this finding.
    pub latest_snapshot_id: SnapshotId,
    /// Raw applicability recorded by that observation.
    pub latest_applicability: String,
    pub first_seen_at: String,
    pub last_seen_at: String,
}

/// One native record in an import batch.
#[derive(Debug, Clone)]
#[allow(dead_code)] // TODO(#23): not consumed by the binary yet.
pub struct ImportedSourceWrite {
    pub native: Raw<NativeSourceRef>,
    /// The record as received from GitHub (or the importer's typed record
    /// JSON), stored verbatim.
    pub raw_json: String,
    /// Alternative identities, for example `("graphql_node_id", "PRRC_…")`.
    pub aliases: Vec<(String, String)>,
}

/// One transactional refresh of a GitHub resource group.
#[derive(Debug, Clone)]
#[allow(dead_code)] // TODO(#23): not consumed by the binary yet.
pub struct ImportBatch {
    /// For example `check_runs`, `check_annotations`, `commit_statuses`.
    pub resource_kind: String,
    /// A complete refresh marks records of this resource kind that were not
    /// seen as unavailable; a partial refresh never does (analyze.md §12.4).
    pub complete: bool,
    pub observed_at: String,
    pub records: Vec<ImportedSourceWrite>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // TODO(#23): not consumed by the binary yet.
pub struct StoredImportedSource {
    pub id: i64,
    pub resource_kind: String,
    pub native: Raw<NativeSourceRef>,
    pub raw_json: String,
    pub available: bool,
    pub first_seen_at: String,
    pub last_seen_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneReport {
    pub snapshots: usize,
    pub runs: usize,
    pub findings: usize,
}

/// Optimistic-concurrency failure for finding triage (HTTP 409 in #33).
#[derive(Debug)]
pub struct RevisionConflict {
    pub current: Option<FindingUserState>,
}

impl std::fmt::Display for RevisionConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let rev = self.current.as_ref().map_or(0, |s| s.revision);
        write!(f, "finding state changed; current revision is {rev}")
    }
}

impl std::error::Error for RevisionConflict {}

// ---------------------------------------------------------------------------
// Snapshots
// ---------------------------------------------------------------------------

impl Store {
    fn write_tx(&mut self) -> Result<Transaction<'_>> {
        // IMMEDIATE takes the writer lock up front, so concurrent processes
        // wait on busy_timeout instead of failing a read→write upgrade.
        Ok(self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?)
    }

    /// Records that `snapshot_id` (base/head pair) exists for a PR.
    /// Idempotent; links the fetched `pr_snapshots` row when given.
    pub fn register_analysis_snapshot(
        &mut self,
        pr_id: &str,
        snapshot_id: &SnapshotId,
        base_sha: &str,
        head_sha: &str,
        pr_snapshot_id: Option<i64>,
    ) -> Result<()> {
        let tx = self.write_tx()?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT pr_id FROM analysis_snapshots WHERE id = ?1",
                [snapshot_id.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        match existing {
            Some(owner) if owner != pr_id => bail!("snapshot {snapshot_id} belongs to another PR"),
            Some(_) => {
                tx.execute(
                    "UPDATE analysis_snapshots SET pr_snapshot_id = COALESCE(?2, pr_snapshot_id) WHERE id = ?1",
                    params![snapshot_id.as_str(), pr_snapshot_id],
                )?;
            }
            None => {
                tx.execute(
                    "INSERT INTO analysis_snapshots (id, pr_id, base_sha, head_sha, pr_snapshot_id, registered_at, seq)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, (SELECT COALESCE(MAX(seq), 0) + 1 FROM analysis_snapshots))",
                    params![snapshot_id.as_str(), pr_id, base_sha, head_sha, pr_snapshot_id, now_rfc3339()],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Makes `snapshot_id` the PR's current snapshot and applies retention.
    /// Everything observed on other snapshots reads as historical from now on.
    pub fn set_current_analysis_snapshot(
        &mut self,
        pr_id: &str,
        snapshot_id: &SnapshotId,
    ) -> Result<PruneReport> {
        let tx = self.write_tx()?;
        tx.execute(
            "INSERT INTO analysis_current_snapshots (pr_id, snapshot_id, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(pr_id) DO UPDATE SET snapshot_id = excluded.snapshot_id, updated_at = excluded.updated_at",
            params![pr_id, snapshot_id.as_str(), now_rfc3339()],
        )
        .with_context(|| format!("snapshot {snapshot_id} is not registered for this PR"))?;
        let report = prune(&tx, pr_id, ANALYSIS_SNAPSHOT_RETENTION)?;
        tx.commit()?;
        Ok(report)
    }

    pub fn current_analysis_snapshot(&self, pr_id: &str) -> Result<Option<SnapshotId>> {
        Ok(self
            .conn
            .query_row(
                "SELECT snapshot_id FROM analysis_current_snapshots WHERE pr_id = ?1",
                [pr_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .map(SnapshotId::new))
    }

    /// Base and head SHAs a registered snapshot was analysed for.
    pub fn analysis_snapshot_shas(
        &self,
        snapshot_id: &SnapshotId,
    ) -> Result<Option<(String, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT base_sha, head_sha FROM analysis_snapshots WHERE id = ?1",
                [snapshot_id.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// Findings one run observed (its committed batch size, deduplicated).
    pub fn count_run_findings(&self, run_id: &AnalysisRunId) -> Result<u32> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM finding_occurrences WHERE run_id = ?1",
            [run_id.as_str()],
            |r| r.get(0),
        )?)
    }

    /// Deletes runs, evidence and observations of old snapshots, keeping the
    /// current snapshot, the `keep` most recent ones, and any snapshot with
    /// queued or running runs. Local dispositions are never pruned.
    #[allow(dead_code)] // TODO(#34): not consumed by the binary yet.
    pub fn prune_analysis(&mut self, pr_id: &str, keep: usize) -> Result<PruneReport> {
        let tx = self.write_tx()?;
        let report = prune(&tx, pr_id, keep)?;
        tx.commit()?;
        Ok(report)
    }

    // -----------------------------------------------------------------------
    // Runs
    // -----------------------------------------------------------------------

    /// Inserts or updates a run's state (queued, running, partial, skipped,
    /// failed, stale, ...). A terminal run can only move to `stale`.
    /// Output-bearing states for a snapshot that is no longer current are
    /// stored as `stale`.
    pub fn record_analysis_run(
        &mut self,
        pr_id: &str,
        run: &Raw<AnalysisRun>,
    ) -> Result<BatchOutcome> {
        let tx = self.write_tx()?;
        let outcome = upsert_run(&tx, Some(pr_id), run)?;
        tx.commit()?;
        Ok(outcome)
    }

    pub fn get_analysis_run(&self, id: &AnalysisRunId) -> Result<Raw<AnalysisRun>> {
        let json: String = self
            .conn
            .query_row(
                "SELECT run_json FROM analysis_runs WHERE id = ?1",
                [id.as_str()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| NotFound(format!("analysis run {id}")))?;
        Raw::from_json(&json)
    }

    /// Runs for a PR, oldest snapshot first.
    pub fn list_analysis_runs(&self, pr_id: &str) -> Result<Vec<Raw<AnalysisRun>>> {
        let mut stmt = self.conn.prepare(
            "SELECT r.run_json FROM analysis_runs r JOIN analysis_snapshots s ON s.id = r.snapshot_id
             WHERE r.pr_id = ?1 ORDER BY s.seq, r.created_at, r.id",
        )?;
        let rows = stmt.query_map([pr_id], |r| r.get::<_, String>(0))?;
        rows.map(|json| Raw::from_json(&json?)).collect()
    }

    // -----------------------------------------------------------------------
    // Result batches
    // -----------------------------------------------------------------------

    /// Writes a run's findings, evidence and relations in one transaction.
    /// Re-delivering the same batch is idempotent.
    pub fn commit_analysis_batch(&mut self, batch: &AnalysisBatch) -> Result<BatchOutcome> {
        let run = batch.run.value();
        if !run.id_is_consistent() {
            bail!("run {} does not match its derived ID", run.id);
        }
        for raw in &batch.findings {
            let f = raw.value();
            if let Err(errors) = f.validate() {
                bail!("finding {} breaks the contract: {errors:?}", f.id);
            }
            if f.observed_snapshot_id != run.snapshot_id {
                bail!(
                    "finding {} was observed on another snapshot than run {}",
                    f.id,
                    run.id
                );
            }
            if f.source.run_id.as_ref().is_some_and(|id| id != &run.id) {
                bail!("finding {} names another run", f.id);
            }
        }
        let batch_findings: HashSet<&FindingId> =
            batch.findings.iter().map(|f| &f.value().id).collect();
        for rel in &batch.relations {
            if !batch_findings.contains(&rel.source_finding_id) {
                bail!(
                    "relation source {} is not in this batch",
                    rel.source_finding_id
                );
            }
        }

        let tx = self.write_tx()?;
        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT pr_id, status FROM analysis_runs WHERE id = ?1",
                [run.id.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((pr_id, status)) = existing else {
            return Err(NotFound(format!("analysis run {}", run.id)).into());
        };
        if status == "cancelled" {
            bail!("run {} was cancelled; its results are discarded", run.id);
        }
        let outcome = upsert_run(&tx, None, &batch.run)?;
        let now = now_rfc3339();

        for raw in &batch.evidence {
            let ev = raw.value();
            tx.execute(
                "INSERT INTO finding_evidence (id, owner_run_id, kind, observed_at, evidence_json)
                 VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(id) DO NOTHING",
                params![
                    ev.id.as_str(),
                    run.id.as_str(),
                    ev.kind.as_str(),
                    ev.observed_at,
                    raw.json()
                ],
            )?;
        }

        for raw in &batch.findings {
            let f = raw.value();
            let tree = raw.tree()?;
            let owner: Option<String> = tx
                .query_row(
                    "SELECT pr_id FROM review_findings WHERE id = ?1",
                    [f.id.as_str()],
                    |r| r.get(0),
                )
                .optional()?;
            if owner.as_deref().is_some_and(|o| o != pr_id) {
                bail!("finding {} belongs to another PR", f.id);
            }
            tx.execute(
                "INSERT INTO review_findings
                 (id, pr_id, observed_snapshot_id, schema_version, source_kind, provider_id, assertion_kind,
                  attention_level, source_status, file_id, current_path, github_thread_id,
                  first_seen_at, last_seen_at, finding_json, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
                 ON CONFLICT(id) DO UPDATE SET
                   first_seen_at = MIN(first_seen_at, excluded.first_seen_at),
                   last_seen_at = MAX(last_seen_at, excluded.last_seen_at)",
                params![
                    f.id.as_str(),
                    pr_id,
                    f.observed_snapshot_id.as_str(),
                    f.schema_version,
                    required_str(&tree, "/source/kind")?,
                    f.source.provider_id,
                    required_str(&tree, "/assertionKind")?,
                    required_str(&tree, "/attention/level")?,
                    required_str(&tree, "/sourceStatus")?,
                    f.anchor.file_id,
                    f.anchor.current_path,
                    f.anchor.github_thread_id,
                    f.first_seen_at,
                    f.last_seen_at,
                    raw.json(),
                    now,
                ],
            )?;
            tx.execute(
                "INSERT INTO finding_occurrences (finding_id, run_id, snapshot_id, applicability, observed_at, schema_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(finding_id, run_id) DO NOTHING",
                params![
                    f.id.as_str(),
                    run.id.as_str(),
                    run.snapshot_id.as_str(),
                    required_str(&tree, "/applicability")?,
                    f.last_seen_at,
                    f.schema_version,
                ],
            )?;
            for ev in &f.evidence_ids {
                // The FK rejects references to evidence that was never written.
                tx.execute(
                    "INSERT INTO finding_evidence_links (finding_id, evidence_id) VALUES (?1, ?2)
                     ON CONFLICT DO NOTHING",
                    params![f.id.as_str(), ev.as_str()],
                )
                .with_context(|| format!("finding {} cites missing evidence {ev}", f.id))?;
            }
            if let Some(native) = tree.pointer("/source/native").filter(|v| !v.is_null()) {
                let key = native_key(native)?;
                if let Some(id) = find_source_record(&tx, &key)? {
                    tx.execute(
                        "INSERT INTO finding_source_links (finding_id, source_record_id) VALUES (?1, ?2)
                         ON CONFLICT DO NOTHING",
                        params![f.id.as_str(), id],
                    )?;
                }
            }
        }

        for rel in &batch.relations {
            let (target_kind, target_id) = match &rel.target {
                RelationTarget::Finding(id) => ("finding", id.as_str()),
                RelationTarget::Thread(id) => ("thread", id.as_str()),
                RelationTarget::Entity(id) => ("entity", id.as_str()),
            };
            tx.execute(
                "INSERT OR REPLACE INTO finding_relations
                 (source_finding_id, kind, target_kind, target_id, run_id, relation_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    rel.source_finding_id.as_str(),
                    enum_str(&rel.kind)?,
                    target_kind,
                    target_id,
                    run.id.as_str(),
                    serde_json::to_string(rel)?,
                ],
            )?;
        }
        tx.commit()?;
        Ok(outcome)
    }

    // -----------------------------------------------------------------------
    // Reads
    // -----------------------------------------------------------------------

    /// Findings for a PR with their derived label. With `include_historical`
    /// false only current findings are returned.
    pub fn list_findings(
        &self,
        pr_id: &str,
        include_historical: bool,
    ) -> Result<Vec<StoredFinding>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.finding_json, f.first_seen_at, f.last_seen_at,
               EXISTS (
                 SELECT 1 FROM finding_occurrences o
                 JOIN analysis_runs r ON r.id = o.run_id
                 JOIN analysis_current_snapshots c ON c.pr_id = f.pr_id AND c.snapshot_id = o.snapshot_id
                 WHERE o.finding_id = f.id
                   AND r.status IN ('running', 'succeeded', 'partial')
                   AND o.applicability IN ('current_exact', 'current_related')
               ),
               (SELECT o.snapshot_id || char(0) || o.applicability FROM finding_occurrences o
                  JOIN analysis_snapshots s ON s.id = o.snapshot_id
                  WHERE o.finding_id = f.id ORDER BY s.seq DESC, o.observed_at DESC LIMIT 1)
             FROM review_findings f WHERE f.pr_id = ?1
             ORDER BY f.first_seen_at, f.id",
        )?;
        let rows = stmt.query_map([pr_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, bool>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (json, first_seen_at, last_seen_at, current, latest) = row?;
            if !current && !include_historical {
                continue;
            }
            let latest = latest.context("finding has no observation")?;
            let (snap, applicability) = latest.split_once('\0').context("bad observation")?;
            out.push(StoredFinding {
                finding: Raw::from_json(&json)?,
                label: if current {
                    ObservationLabel::Current
                } else {
                    ObservationLabel::Historical
                },
                latest_snapshot_id: SnapshotId::new(snap),
                latest_applicability: applicability.to_string(),
                first_seen_at,
                last_seen_at,
            });
        }
        Ok(out)
    }

    pub fn list_finding_evidence(&self, finding_id: &FindingId) -> Result<Vec<Raw<Evidence>>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.evidence_json FROM finding_evidence_links l
             JOIN finding_evidence e ON e.id = l.evidence_id
             WHERE l.finding_id = ?1 ORDER BY e.id",
        )?;
        let rows = stmt.query_map([finding_id.as_str()], |r| r.get::<_, String>(0))?;
        rows.map(|json| Raw::from_json(&json?)).collect()
    }

    #[allow(dead_code)] // TODO(#33): not consumed by the binary yet.
    pub fn list_finding_relations(&self, finding_id: &FindingId) -> Result<Vec<FindingRelation>> {
        let mut stmt = self.conn.prepare(
            "SELECT relation_json FROM finding_relations WHERE source_finding_id = ?1
             ORDER BY kind, target_kind, target_id",
        )?;
        let rows = stmt.query_map([finding_id.as_str()], |r| r.get::<_, String>(0))?;
        rows.map(|json| Ok(serde_json::from_str(&json?)?)).collect()
    }

    // -----------------------------------------------------------------------
    // Finding aliases
    // -----------------------------------------------------------------------

    /// Records that `alias` reports the same logical claim as `canonical`.
    /// Both source findings stay intact.
    #[allow(dead_code)] // TODO(#23): not consumed by the binary yet.
    pub fn add_finding_alias(
        &mut self,
        alias: &FindingId,
        canonical: &FindingId,
        method: &str,
    ) -> Result<()> {
        let tx = self.write_tx()?;
        let prs: Vec<Option<String>> = [alias, canonical]
            .iter()
            .map(|id| {
                tx.query_row(
                    "SELECT pr_id FROM review_findings WHERE id = ?1",
                    [id.as_str()],
                    |r| r.get(0),
                )
                .optional()
            })
            .collect::<rusqlite::Result<_>>()?;
        match (&prs[0], &prs[1]) {
            (Some(a), Some(c)) if a == c => {}
            (Some(_), Some(_)) => bail!("aliased findings belong to different PRs"),
            _ => return Err(NotFound("finding".into()).into()),
        }
        let chained: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM finding_aliases WHERE alias_finding_id = ?1)
                 OR EXISTS (SELECT 1 FROM finding_aliases WHERE canonical_finding_id = ?2)",
            params![canonical.as_str(), alias.as_str()],
            |r| r.get(0),
        )?;
        if chained {
            bail!("finding aliases must point at a canonical finding, not another alias");
        }
        tx.execute(
            "INSERT INTO finding_aliases (alias_finding_id, canonical_finding_id, method, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(alias_finding_id) DO UPDATE SET canonical_finding_id = excluded.canonical_finding_id,
               method = excluded.method",
            params![alias.as_str(), canonical.as_str(), method, now_rfc3339()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Findings aliased to `canonical`.
    #[allow(dead_code)] // TODO(#33): not consumed by the binary yet.
    pub fn list_finding_aliases(&self, canonical: &FindingId) -> Result<Vec<FindingId>> {
        let mut stmt = self.conn.prepare(
            "SELECT alias_finding_id FROM finding_aliases WHERE canonical_finding_id = ?1 ORDER BY alias_finding_id",
        )?;
        let rows = stmt.query_map([canonical.as_str()], |r| r.get::<_, String>(0))?;
        rows.map(|id| Ok(FindingId::new(id?))).collect()
    }

    // -----------------------------------------------------------------------
    // Imported native records
    // -----------------------------------------------------------------------

    /// Upserts one GitHub resource group in a single transaction. Returns the
    /// record IDs in input order.
    #[allow(dead_code)] // TODO(#23): not consumed by the binary yet.
    pub fn commit_import_batch(&mut self, pr_id: &str, batch: &ImportBatch) -> Result<Vec<i64>> {
        for rec in &batch.records {
            serde_json::from_str::<Value>(&rec.raw_json).context("imported record is not JSON")?;
        }
        let tx = self.write_tx()?;
        let mut ids = Vec::with_capacity(batch.records.len());
        for rec in &batch.records {
            let tree = rec.native.tree()?;
            let key = native_key(&tree)?;
            let id: i64 = tx.query_row(
                "INSERT INTO imported_source_records
                 (pr_id, host, repository_id, source_kind, key_kind, key_value, resource_kind,
                  native_json, raw_json, available, first_seen_at, last_seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10, ?10)
                 ON CONFLICT(host, repository_id, source_kind, key_kind, key_value) DO UPDATE SET
                   native_json = excluded.native_json, raw_json = excluded.raw_json,
                   available = 1, last_seen_at = excluded.last_seen_at
                 RETURNING id, pr_id",
                params![
                    pr_id,
                    key.0,
                    key.1,
                    key.2,
                    key.3,
                    key.4,
                    batch.resource_kind,
                    rec.native.json(),
                    rec.raw_json,
                    batch.observed_at
                ],
                |r| {
                    let owner: String = r.get(1)?;
                    Ok(if owner == pr_id { r.get(0)? } else { -1 })
                },
            )?;
            if id < 0 {
                bail!("imported record belongs to another PR");
            }
            for (kind, value) in &rec.aliases {
                tx.execute(
                    "INSERT INTO imported_source_aliases (alias_kind, alias_value, source_record_id)
                     VALUES (?1, ?2, ?3) ON CONFLICT DO NOTHING",
                    params![kind, value, id],
                )?;
                let mapped: i64 = tx.query_row(
                    "SELECT source_record_id FROM imported_source_aliases WHERE alias_kind = ?1 AND alias_value = ?2",
                    params![kind, value],
                    |r| r.get(0),
                )?;
                if mapped != id {
                    bail!("alias {kind}:{value} already names another record");
                }
            }
            ids.push(id);
        }
        if batch.complete {
            let seen: HashSet<i64> = ids.iter().copied().collect();
            let mut stmt = tx.prepare(
                "SELECT id FROM imported_source_records WHERE pr_id = ?1 AND resource_kind = ?2 AND available = 1",
            )?;
            let present: Vec<i64> = stmt
                .query_map(params![pr_id, batch.resource_kind], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            drop(stmt);
            for id in present.into_iter().filter(|id| !seen.contains(id)) {
                tx.execute(
                    "UPDATE imported_source_records SET available = 0 WHERE id = ?1",
                    [id],
                )?;
            }
        }
        tx.commit()?;
        Ok(ids)
    }

    #[allow(dead_code)] // TODO(#23): not consumed by the binary yet.
    pub fn list_imported_sources(
        &self,
        pr_id: &str,
        resource_kind: Option<&str>,
    ) -> Result<Vec<StoredImportedSource>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, resource_kind, native_json, raw_json, available, first_seen_at, last_seen_at
             FROM imported_source_records WHERE pr_id = ?1 AND (?2 IS NULL OR resource_kind = ?2)
             ORDER BY id",
        )?;
        let rows = stmt.query_map(params![pr_id, resource_kind], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, bool>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
            ))
        })?;
        rows.map(|row| {
            let (id, resource_kind, native, raw_json, available, first_seen_at, last_seen_at) =
                row?;
            Ok(StoredImportedSource {
                id,
                resource_kind,
                native: Raw::from_json(&native)?,
                raw_json,
                available,
                first_seen_at,
                last_seen_at,
            })
        })
        .collect()
    }

    /// Record ID for an alias such as a GraphQL node ID.
    #[allow(dead_code)] // TODO(#23): not consumed by the binary yet.
    pub fn resolve_imported_alias(&self, kind: &str, value: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT source_record_id FROM imported_source_aliases WHERE alias_kind = ?1 AND alias_value = ?2",
                params![kind, value],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Findings that project the native record `source_record_id`.
    #[allow(dead_code)] // TODO(#23): not consumed by the binary yet.
    pub fn findings_for_imported_source(&self, source_record_id: i64) -> Result<Vec<FindingId>> {
        let mut stmt = self.conn.prepare(
            "SELECT finding_id FROM finding_source_links WHERE source_record_id = ?1 ORDER BY finding_id",
        )?;
        let rows = stmt.query_map([source_record_id], |r| r.get::<_, String>(0))?;
        rows.map(|id| Ok(FindingId::new(id?))).collect()
    }

    // -----------------------------------------------------------------------
    // Local triage
    // -----------------------------------------------------------------------

    /// Compare-and-set update of local triage. `expected_revision` is `0` when
    /// no state exists yet. Returns the stored state with its new revision.
    /// Never touches review state and is never published to GitHub.
    #[allow(dead_code)] // TODO(#26): not consumed by the binary yet.
    pub fn put_finding_user_state(
        &mut self,
        pr_id: &str,
        state: &FindingUserState,
        expected_revision: u64,
    ) -> Result<FindingUserState> {
        if state.disposition == UserDisposition::Snoozed && state.snoozed_until.is_none() {
            bail!("a snoozed finding needs snoozedUntil");
        }
        let tx = self.write_tx()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM review_findings WHERE id = ?1 AND pr_id = ?2)",
            params![state.finding_id.as_str(), pr_id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(NotFound(format!("finding {}", state.finding_id)).into());
        }
        let current = read_user_state(&tx, &state.session_id, &state.finding_id)?;
        if current.as_ref().map_or(0, |s| s.revision) != expected_revision {
            return Err(RevisionConflict { current }.into());
        }
        let mut stored = state.clone();
        stored.revision = expected_revision + 1;
        stored.updated_at = now_rfc3339();
        tx.execute(
            "INSERT INTO finding_user_state
             (session_id, finding_id, pr_id, disposition, snoozed_until, note, promoted_draft_id, updated_at, revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(session_id, finding_id) DO UPDATE SET
               disposition = excluded.disposition, snoozed_until = excluded.snoozed_until,
               note = excluded.note, promoted_draft_id = excluded.promoted_draft_id,
               updated_at = excluded.updated_at, revision = excluded.revision",
            params![
                stored.session_id,
                stored.finding_id.as_str(),
                pr_id,
                enum_str(&stored.disposition)?,
                stored.snoozed_until,
                stored.note,
                stored.promoted_draft_id,
                stored.updated_at,
                stored.revision as i64,
            ],
        )?;
        tx.commit()?;
        Ok(stored)
    }

    #[allow(dead_code)] // TODO(#26): not consumed by the binary yet.
    pub fn get_finding_user_state(
        &self,
        session_id: &str,
        finding_id: &FindingId,
    ) -> Result<Option<FindingUserState>> {
        read_user_state(&self.conn, session_id, finding_id)
    }

    /// Explicitly deletes local triage (the only way it is removed).
    #[allow(dead_code)] // TODO(#26): not consumed by the binary yet.
    pub fn delete_finding_user_state(
        &mut self,
        session_id: &str,
        finding_id: &FindingId,
    ) -> Result<()> {
        self.conn.execute(
            "DELETE FROM finding_user_state WHERE session_id = ?1 AND finding_id = ?2",
            params![session_id, finding_id.as_str()],
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

type NativeKey = (String, String, String, String, String);

/// `(host, repositoryId, sourceKind, key kind, key value)` read from raw JSON
/// so an unknown source kind keeps its own identity.
fn native_key(native: &Value) -> Result<NativeKey> {
    Ok((
        required_str(native, "/host")?,
        required_str(native, "/repositoryId")?,
        required_str(native, "/sourceKind")?,
        required_str(native, "/key/kind")?,
        required_str(native, "/key/value")?,
    ))
}

fn find_source_record(tx: &Transaction<'_>, key: &NativeKey) -> Result<Option<i64>> {
    Ok(tx
        .query_row(
            "SELECT id FROM imported_source_records
             WHERE host = ?1 AND repository_id = ?2 AND source_kind = ?3 AND key_kind = ?4 AND key_value = ?5",
            params![key.0, key.1, key.2, key.3, key.4],
            |r| r.get(0),
        )
        .optional()?)
}

fn upsert_run(
    tx: &Transaction<'_>,
    pr_id: Option<&str>,
    raw: &Raw<AnalysisRun>,
) -> Result<BatchOutcome> {
    let run = raw.value();
    if !run.id_is_consistent() {
        bail!("run {} does not match its derived ID", run.id);
    }
    let snapshot_pr: String = tx
        .query_row(
            "SELECT pr_id FROM analysis_snapshots WHERE id = ?1",
            [run.snapshot_id.as_str()],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| NotFound(format!("analysis snapshot {}", run.snapshot_id)))?;
    if pr_id.is_some_and(|p| p != snapshot_pr) {
        bail!("run {} is pinned to another PR's snapshot", run.id);
    }
    let previous: Option<String> = tx
        .query_row(
            "SELECT status FROM analysis_runs WHERE id = ?1",
            [run.id.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    let current: Option<String> = tx
        .query_row(
            "SELECT snapshot_id FROM analysis_current_snapshots WHERE pr_id = ?1",
            [&snapshot_pr],
            |r| r.get(0),
        )
        .optional()?;
    let behind = current.is_some_and(|c| c != run.snapshot_id.as_str());
    // Output for a snapshot that is no longer current is historical only.
    let stale = behind
        && matches!(
            run.status,
            RunStatus::Running | RunStatus::Succeeded | RunStatus::Partial | RunStatus::Stale
        );
    let stored_status = if stale { RunStatus::Stale } else { run.status };
    if let Some(prev) = &previous {
        let prev_status: RunStatus = serde_json::from_value(Value::String(prev.clone()))?;
        if prev_status.is_terminal() && stored_status != prev_status {
            let allowed =
                stored_status == RunStatus::Stale || (prev_status == RunStatus::Stale && stale);
            if !allowed {
                bail!(
                    "run {} is already {prev}; a retry must be a new attempt",
                    run.id
                );
            }
        }
    }
    let json = if stored_status == run.status {
        raw.json().to_string()
    } else {
        // Only the status field changes; everything else stays as written.
        let mut tree = raw.tree()?;
        tree["status"] = Value::String(enum_str(&stored_status)?);
        serde_json::to_string(&tree)?
    };
    let tree = raw.tree()?;
    let now = now_rfc3339();
    tx.execute(
        "INSERT INTO analysis_runs
         (id, pr_id, snapshot_id, provider_id, engine_version, configuration_digest, input_digest, attempt,
          mode, status, reported_status, completeness, started_at, finished_at, run_json, revision, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, 0, ?16, ?16)
         ON CONFLICT(id) DO UPDATE SET
           status = excluded.status, reported_status = excluded.reported_status,
           completeness = excluded.completeness, started_at = excluded.started_at,
           finished_at = excluded.finished_at, run_json = excluded.run_json,
           revision = revision + 1, updated_at = excluded.updated_at",
        params![
            run.id.as_str(),
            snapshot_pr,
            run.snapshot_id.as_str(),
            run.provider_id,
            run.engine_version,
            run.configuration_digest,
            run.input_digest,
            run.attempt,
            required_str(&tree, "/mode")?,
            enum_str(&stored_status)?,
            enum_str(&run.status)?,
            raw_str(&tree, "/coverage/completeness"),
            run.started_at,
            run.finished_at,
            json,
            now,
        ],
    )?;
    Ok(BatchOutcome {
        stored_status,
        stale,
    })
}

#[allow(dead_code)] // TODO(#26): not consumed by the binary yet.
fn read_user_state(
    conn: &rusqlite::Connection,
    session_id: &str,
    finding_id: &FindingId,
) -> Result<Option<FindingUserState>> {
    let row = conn
        .query_row(
            "SELECT disposition, snoozed_until, note, promoted_draft_id, updated_at, revision
             FROM finding_user_state WHERE session_id = ?1 AND finding_id = ?2",
            params![session_id, finding_id.as_str()],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            },
        )
        .optional()?;
    row.map(
        |(disposition, snoozed_until, note, promoted_draft_id, updated_at, revision)| {
            Ok(FindingUserState {
                finding_id: finding_id.clone(),
                session_id: session_id.to_string(),
                disposition: serde_json::from_value(Value::String(disposition))?,
                snoozed_until,
                note,
                promoted_draft_id,
                updated_at,
                revision: revision as u64,
            })
        },
    )
    .transpose()
}

fn prune(tx: &Transaction<'_>, pr_id: &str, keep: usize) -> Result<PruneReport> {
    let mut stmt = tx.prepare(
        "SELECT s.id FROM analysis_snapshots s
         WHERE s.pr_id = ?1
           AND s.id NOT IN (SELECT snapshot_id FROM analysis_current_snapshots WHERE pr_id = ?1)
           AND s.id NOT IN (SELECT id FROM analysis_snapshots WHERE pr_id = ?1 ORDER BY seq DESC LIMIT ?2)
           AND NOT EXISTS (SELECT 1 FROM analysis_runs r WHERE r.snapshot_id = s.id AND r.status IN ('queued', 'running'))",
    )?;
    let doomed: Vec<String> = stmt
        .query_map(params![pr_id, keep as i64], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut report = PruneReport::default();
    for id in &doomed {
        report.runs += tx.execute("DELETE FROM analysis_runs WHERE snapshot_id = ?1", [id])?;
        report.snapshots += tx.execute("DELETE FROM analysis_snapshots WHERE id = ?1", [id])?;
    }
    if !doomed.is_empty() {
        // Findings with no remaining observation. Local triage has no FK to
        // findings and is kept.
        report.findings = tx.execute(
            "DELETE FROM review_findings WHERE pr_id = ?1
             AND NOT EXISTS (SELECT 1 FROM finding_occurrences o WHERE o.finding_id = review_findings.id)",
            [pr_id],
        )?;
    }
    Ok(report)
}

#[cfg(test)]
#[path = "analysis_tests.rs"]
mod tests;
