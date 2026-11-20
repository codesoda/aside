use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};

use crate::model::Repository;
use crate::util::{new_id, now_rfc3339};

use super::Store;

impl Store {
    /// Find or create the repository, preferring the provider's immutable ID.
    /// Handles renames (same provider ID, new name) and name reuse (same name,
    /// different provider ID) without sharing rules across repositories.
    pub fn resolve_repository(
        &mut self,
        host: &str,
        owner: &str,
        name: &str,
        provider_id: Option<&str>,
    ) -> Result<Repository> {
        let alias = format!("{host}/{owner}/{name}").to_lowercase();
        let now = now_rfc3339();
        let tx = self.conn.transaction()?;
        let by_provider: Option<String> = match provider_id {
            Some(pid) => tx
                .query_row(
                    "SELECT id FROM repositories WHERE provider_id = ?1",
                    [pid],
                    |r| r.get(0),
                )
                .optional()?,
            None => None,
        };
        let by_alias: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT r.id, r.provider_id FROM repository_aliases a JOIN repositories r ON r.id = a.repository_id WHERE a.alias = ?1",
                [&alias],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;

        let id = match (by_provider, by_alias, provider_id) {
            (Some(id), _, _) => id,
            (None, Some((id, None)), Some(pid)) => {
                tx.execute(
                    "UPDATE repositories SET provider_id = ?1 WHERE id = ?2",
                    params![pid, id],
                )?;
                id
            }
            (None, Some((id, existing)), pid) if pid.is_none() || existing.as_deref() == pid => id,
            _ => {
                let id = new_id("repo");
                tx.execute(
                    "INSERT INTO repositories (id, host, owner, name, provider_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![id, host, owner, name, provider_id, now],
                )?;
                id
            }
        };
        tx.execute(
            "UPDATE repositories SET owner = ?1, name = ?2 WHERE id = ?3",
            params![owner, name, id],
        )?;
        tx.execute(
            "INSERT INTO repository_aliases (alias, repository_id) VALUES (?1, ?2)
             ON CONFLICT(alias) DO UPDATE SET repository_id = excluded.repository_id",
            params![alias, id],
        )?;
        tx.commit()?;
        Ok(Repository {
            id,
            host: host.into(),
            owner: owner.into(),
            name: name.into(),
            provider_id: provider_id.map(str::to_string),
        })
    }

    pub fn upsert_pull_request(
        &mut self,
        repository_id: &str,
        number: u64,
        title: &str,
        url: &str,
    ) -> Result<String> {
        let now = now_rfc3339();
        let number = i64::try_from(number).context("PR number exceeds SQLite integer range")?;
        let tx = self.conn.transaction()?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT id FROM pull_requests WHERE repository_id = ?1 AND number = ?2",
                params![repository_id, number],
                |r| r.get(0),
            )
            .optional()?;
        let id = match existing {
            Some(id) => {
                tx.execute(
                    "UPDATE pull_requests SET title = ?1, url = ?2, updated_at = ?3 WHERE id = ?4",
                    params![title, url, now, id],
                )?;
                id
            }
            None => {
                let id = new_id("pr");
                tx.execute(
                    "INSERT INTO pull_requests (id, repository_id, number, title, url, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![id, repository_id, number, title, url, now],
                )?;
                id
            }
        };
        tx.execute(
            "INSERT INTO review_sessions (pr_id, created_at, last_opened_at) VALUES (?1, ?2, ?2)
             ON CONFLICT(pr_id) DO UPDATE SET last_opened_at = excluded.last_opened_at",
            params![id, now],
        )?;
        tx.commit()?;
        Ok(id)
    }
}
