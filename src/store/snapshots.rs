use anyhow::Result;
use rusqlite::params;

use crate::model::PrSnapshot;

use super::Store;

/// How many snapshots to keep per PR.
pub(super) const SNAPSHOT_RETENTION: i64 = 20;

impl Store {
    pub fn save_snapshot(&mut self, pr_id: &str, snapshot: &PrSnapshot) -> Result<i64> {
        let json = serde_json::to_string(snapshot)?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO pr_snapshots (pr_id, head_sha, base_sha, fetched_at, snapshot_json) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![pr_id, snapshot.pr.head_sha, snapshot.pr.base_sha, snapshot.fetched_at, json],
        )?;
        let id = tx.last_insert_rowid();
        for f in &snapshot.files {
            tx.execute(
                "INSERT OR REPLACE INTO snapshot_files (snapshot_id, path, previous_path, fingerprint_version, diff_digest, content_digest)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, f.path, f.previous_path, f.fingerprint_version, f.diff_digest, f.content_digest],
            )?;
        }
        tx.execute(
            "DELETE FROM pr_snapshots WHERE pr_id = ?1 AND id NOT IN
             (SELECT id FROM pr_snapshots WHERE pr_id = ?1 ORDER BY id DESC LIMIT ?2)",
            params![pr_id, SNAPSHOT_RETENTION],
        )?;
        tx.commit()?;
        Ok(id)
    }
}
