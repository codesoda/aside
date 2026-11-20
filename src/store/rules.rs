use anyhow::Result;
use rusqlite::{OptionalExtension, Row, Transaction, params};

use crate::model::{Actor, ExclusionRule, RuleAction, RuleKind};
use crate::util::{new_id, now_rfc3339};

use super::{NotFound, Store};

#[derive(Debug, Default)]
pub struct RuleUpdate {
    pub action: Option<RuleAction>,
    pub kind_expression: Option<(RuleKind, String)>,
    pub enabled: Option<bool>,
    pub position: Option<usize>,
}

impl Store {
    pub fn list_rules(&self, repository_id: &str) -> Result<Vec<ExclusionRule>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, action, kind, expression, enabled, ord, created_at, updated_at
             FROM repository_exclusion_rules WHERE repository_id = ?1 ORDER BY ord",
        )?;
        let rows = stmt.query_map([repository_id], rule_from_row)?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    /// Insert a rule at `position` (0-based; default: last, so it wins).
    pub fn create_rule(
        &mut self,
        repository_id: &str,
        action: RuleAction,
        kind: RuleKind,
        expression: &str,
        position: Option<usize>,
        actor: Actor,
    ) -> Result<ExclusionRule> {
        let id = new_id("rule");
        let now = now_rfc3339();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO repository_exclusion_rules (id, repository_id, action, kind, expression, enabled, ord, created_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, 1000000000, ?6, ?7, ?7)",
            params![id, repository_id, action_str(action), kind_str(kind), expression, actor.as_str(), now],
        )?;
        reorder(&tx, repository_id, &id, position)?;
        tx.commit()?;
        self.get_rule(repository_id, &id)
    }

    pub fn get_rule(&self, repository_id: &str, id: &str) -> Result<ExclusionRule> {
        self.conn
            .query_row(
                "SELECT id, action, kind, expression, enabled, ord, created_at, updated_at
                 FROM repository_exclusion_rules WHERE repository_id = ?1 AND id = ?2",
                params![repository_id, id],
                rule_from_row,
            )
            .optional()?
            .ok_or_else(|| NotFound(format!("rule {id}")).into())
    }

    pub fn update_rule(
        &mut self,
        repository_id: &str,
        id: &str,
        update: &RuleUpdate,
    ) -> Result<ExclusionRule> {
        self.get_rule(repository_id, id)?;
        let tx = self.conn.transaction()?;
        let now = now_rfc3339();
        if let Some(action) = update.action {
            tx.execute(
                "UPDATE repository_exclusion_rules SET action = ?1, updated_at = ?2 WHERE id = ?3",
                params![action_str(action), now, id],
            )?;
        }
        if let Some((kind, expr)) = &update.kind_expression {
            tx.execute(
                "UPDATE repository_exclusion_rules SET kind = ?1, expression = ?2, updated_at = ?3 WHERE id = ?4",
                params![kind_str(*kind), expr, now, id],
            )?;
        }
        if let Some(enabled) = update.enabled {
            tx.execute(
                "UPDATE repository_exclusion_rules SET enabled = ?1, updated_at = ?2 WHERE id = ?3",
                params![enabled, now, id],
            )?;
        }
        if let Some(position) = update.position {
            tx.execute(
                "UPDATE repository_exclusion_rules SET updated_at = ?1 WHERE id = ?2",
                params![now, id],
            )?;
            reorder(&tx, repository_id, id, Some(position))?;
        }
        tx.commit()?;
        self.get_rule(repository_id, id)
    }

    pub fn delete_rule(&mut self, repository_id: &str, id: &str) -> Result<()> {
        let n = self.conn.execute(
            "DELETE FROM repository_exclusion_rules WHERE repository_id = ?1 AND id = ?2",
            params![repository_id, id],
        )?;
        if n == 0 {
            return Err(NotFound(format!("rule {id}")).into());
        }
        Ok(())
    }
}

/// Move `id` to `position` and renumber all rules 1..n.
fn reorder(tx: &Transaction, repository_id: &str, id: &str, position: Option<usize>) -> Result<()> {
    let mut ids: Vec<String> = {
        let mut stmt = tx.prepare("SELECT id FROM repository_exclusion_rules WHERE repository_id = ?1 AND id != ?2 ORDER BY ord, created_at")?;
        stmt.query_map(params![repository_id, id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    let at = position.unwrap_or(ids.len()).min(ids.len());
    ids.insert(at, id.to_string());
    for (i, rid) in ids.iter().enumerate() {
        tx.execute(
            "UPDATE repository_exclusion_rules SET ord = ?1 WHERE id = ?2",
            params![i as i64 + 1, rid],
        )?;
    }
    Ok(())
}

fn rule_from_row(r: &Row) -> rusqlite::Result<ExclusionRule> {
    Ok(ExclusionRule {
        id: r.get(0)?,
        action: if r.get::<_, String>(1)? == "include" {
            RuleAction::Include
        } else {
            RuleAction::Exclude
        },
        kind: match r.get::<_, String>(2)?.as_str() {
            "file" => RuleKind::File,
            "directory" => RuleKind::Directory,
            _ => RuleKind::Glob,
        },
        expression: r.get(3)?,
        enabled: r.get(4)?,
        order: r.get(5)?,
        created_at: r.get(6)?,
        updated_at: r.get(7)?,
    })
}

fn action_str(a: RuleAction) -> &'static str {
    match a {
        RuleAction::Include => "include",
        RuleAction::Exclude => "exclude",
    }
}

fn kind_str(k: RuleKind) -> &'static str {
    match k {
        RuleKind::File => "file",
        RuleKind::Directory => "directory",
        RuleKind::Glob => "glob",
    }
}
