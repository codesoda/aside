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
    pub fn extension_preference_ids(
        &self,
        repository_id: &str,
    ) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT rule_id FROM repository_extension_preferences WHERE repository_id = ?1",
        )?;
        Ok(stmt
            .query_map([repository_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Update only the rule tagged as this repository's extension preference.
    /// None restores the default without touching manually authored rules.
    pub fn set_extension_preference(
        &mut self,
        repository_id: &str,
        category: &str,
        action: Option<RuleAction>,
        actor: Actor,
    ) -> Result<Option<ExclusionRule>> {
        anyhow::ensure!(actor == Actor::Human, "only a human can change preferences");
        let expression = crate::rules::extension_expression(category)?;
        let existing: Option<String> = self.conn.query_row(
            "SELECT rule_id FROM repository_extension_preferences WHERE repository_id = ?1 AND category = ?2",
            params![repository_id, category], |r| r.get(0)).optional()?;
        match (existing, action) {
            (Some(id), Some(action)) => self
                .update_rule(
                    repository_id,
                    &id,
                    &RuleUpdate {
                        action: Some(action),
                        enabled: Some(true),
                        ..Default::default()
                    },
                )
                .map(Some),
            (Some(id), None) => {
                self.delete_rule(repository_id, &id)?;
                Ok(None)
            }
            (None, Some(action)) => {
                let rule = self.create_rule(
                    repository_id,
                    action,
                    RuleKind::Glob,
                    &expression,
                    None,
                    actor,
                )?;
                self.conn.execute("INSERT INTO repository_extension_preferences (repository_id, category, rule_id) VALUES (?1, ?2, ?3)", params![repository_id, category, rule.id])?;
                Ok(Some(rule))
            }
            (None, None) => Ok(None),
        }
    }

    /// Repository rules followed by this PR's rules; review rules always win ties.
    pub fn effective_rules(&self, repository_id: &str, pr_id: &str) -> Result<Vec<ExclusionRule>> {
        let mut rules = self.list_rules(repository_id)?;
        let offset = rules.iter().map(|r| r.order).max().unwrap_or(0);
        for mut rule in self.list_review_rules(repository_id, pr_id)? {
            rule.order = offset.saturating_add(rule.order);
            rules.push(rule);
        }
        Ok(rules)
    }

    pub fn list_review_rules(
        &self,
        repository_id: &str,
        pr_id: &str,
    ) -> Result<Vec<ExclusionRule>> {
        let mut stmt = self.conn.prepare(
            "SELECT r.id, r.action, r.kind, r.expression, r.enabled, r.ord, r.created_at, r.updated_at
             FROM review_exclusion_rules r JOIN pull_requests p ON p.id = r.pr_id
             WHERE p.repository_id = ?1 AND r.pr_id = ?2 ORDER BY r.ord",
        )?;
        stmt.query_map(params![repository_id, pr_id], rule_from_row)?
            .collect::<rusqlite::Result<_>>()
            .map_err(Into::into)
    }

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

    #[allow(clippy::too_many_arguments)]
    pub fn create_rule_for_lifetime(
        &mut self,
        repository_id: &str,
        pr_id: &str,
        review: bool,
        action: RuleAction,
        kind: RuleKind,
        expression: &str,
        position: Option<usize>,
        actor: Actor,
    ) -> Result<ExclusionRule> {
        anyhow::ensure!(actor == Actor::Human, "only a human can create rules");
        if review {
            self.create_review_rule(
                repository_id,
                pr_id,
                action,
                kind,
                expression,
                position,
                actor,
            )
        } else {
            self.create_rule(repository_id, action, kind, expression, position, actor)
        }
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
        anyhow::ensure!(
            update.kind_expression.is_none()
                || !self.extension_preference_ids(repository_id)?.contains(id),
            "extension preference kind and expression cannot be changed"
        );
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

    pub fn get_review_rule(
        &self,
        repository_id: &str,
        pr_id: &str,
        id: &str,
    ) -> Result<ExclusionRule> {
        self.list_review_rules(repository_id, pr_id)?
            .into_iter()
            .find(|rule| rule.id == id)
            .ok_or_else(|| NotFound(format!("rule {id}")).into())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_review_rule(
        &mut self,
        repository_id: &str,
        pr_id: &str,
        action: RuleAction,
        kind: RuleKind,
        expression: &str,
        position: Option<usize>,
        actor: Actor,
    ) -> Result<ExclusionRule> {
        anyhow::ensure!(
            actor == Actor::Human,
            "only a human can create review rules"
        );
        let id = new_id("review-rule");
        let now = now_rfc3339();
        let tx = self.conn.transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM pull_requests WHERE id = ?1 AND repository_id = ?2)",
            params![pr_id, repository_id],
            |r| r.get(0),
        )?;
        anyhow::ensure!(exists, "pull request does not belong to repository");
        tx.execute("INSERT INTO review_exclusion_rules (id, pr_id, action, kind, expression, enabled, ord, created_by, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, 1, 1000000000, 'human', ?6, ?6)", params![id, pr_id, action_str(action), kind_str(kind), expression, now])?;
        reorder_review(&tx, pr_id, &id, position)?;
        tx.commit()?;
        self.get_review_rule(repository_id, pr_id, &id)
    }

    pub fn update_review_rule(
        &mut self,
        repository_id: &str,
        pr_id: &str,
        id: &str,
        update: &RuleUpdate,
    ) -> Result<ExclusionRule> {
        self.get_review_rule(repository_id, pr_id, id)?;
        let tx = self.conn.transaction()?;
        let now = now_rfc3339();
        if let Some(action) = update.action {
            tx.execute("UPDATE review_exclusion_rules SET action = ?1, updated_at = ?2 WHERE id = ?3 AND pr_id = ?4", params![action_str(action), now, id, pr_id])?;
        }
        if let Some((kind, expr)) = &update.kind_expression {
            tx.execute("UPDATE review_exclusion_rules SET kind = ?1, expression = ?2, updated_at = ?3 WHERE id = ?4 AND pr_id = ?5", params![kind_str(*kind), expr, now, id, pr_id])?;
        }
        if let Some(enabled) = update.enabled {
            tx.execute("UPDATE review_exclusion_rules SET enabled = ?1, updated_at = ?2 WHERE id = ?3 AND pr_id = ?4", params![enabled, now, id, pr_id])?;
        }
        if let Some(position) = update.position {
            reorder_review(&tx, pr_id, id, Some(position))?;
        }
        tx.commit()?;
        self.get_review_rule(repository_id, pr_id, id)
    }

    pub fn delete_review_rule(&mut self, repository_id: &str, pr_id: &str, id: &str) -> Result<()> {
        self.get_review_rule(repository_id, pr_id, id)?;
        self.conn.execute(
            "DELETE FROM review_exclusion_rules WHERE id = ?1 AND pr_id = ?2",
            params![id, pr_id],
        )?;
        Ok(())
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

fn reorder_review(tx: &Transaction, pr_id: &str, id: &str, position: Option<usize>) -> Result<()> {
    let mut ids: Vec<String> = tx.prepare("SELECT id FROM review_exclusion_rules WHERE pr_id = ?1 AND id != ?2 ORDER BY ord, created_at")?
        .query_map(params![pr_id, id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    ids.insert(position.unwrap_or(ids.len()).min(ids.len()), id.to_string());
    for (i, rid) in ids.iter().enumerate() {
        tx.execute(
            "UPDATE review_exclusion_rules SET ord = ?1 WHERE id = ?2 AND pr_id = ?3",
            params![i as i64 + 1, rid, pr_id],
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

#[cfg(test)]
mod lifetime_tests {
    use super::*;

    #[test]
    fn extension_preference_default_preserves_manual_glob() {
        let mut store = Store::open_in_memory().unwrap();
        let repo = store
            .resolve_repository("github.com", "owner", "repo", None)
            .unwrap();
        let other = store
            .resolve_repository("github.com", "owner", "other", None)
            .unwrap();
        let manual = store
            .create_rule(
                &repo.id,
                RuleAction::Exclude,
                RuleKind::Glob,
                "*.rs",
                None,
                Actor::Human,
            )
            .unwrap();
        let first = store
            .set_extension_preference(&repo.id, ".rs", Some(RuleAction::Exclude), Actor::Human)
            .unwrap()
            .unwrap();
        let replacement = store
            .set_extension_preference(&repo.id, ".rs", Some(RuleAction::Include), Actor::Human)
            .unwrap()
            .unwrap();
        assert_eq!(first.id, replacement.id);
        assert!(
            store
                .update_rule(
                    &repo.id,
                    &first.id,
                    &RuleUpdate {
                        kind_expression: Some((RuleKind::Glob, "*.ts".into())),
                        ..Default::default()
                    }
                )
                .is_err()
        );
        assert_eq!(
            store.get_rule(&repo.id, &first.id).unwrap().expression,
            "@extension:.rs"
        );
        assert_eq!(store.extension_preference_ids(&repo.id).unwrap().len(), 1);
        assert!(
            store
                .extension_preference_ids(&other.id)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .set_extension_preference(&repo.id, ".rs", None, Actor::Agent)
                .is_err()
        );
        store
            .set_extension_preference(&repo.id, ".rs", None, Actor::Human)
            .unwrap();
        assert!(store.extension_preference_ids(&repo.id).unwrap().is_empty());
        assert_eq!(store.list_rules(&repo.id).unwrap()[0].id, manual.id);
        assert!(
            store
                .set_extension_preference(
                    &repo.id,
                    ".rs/evil",
                    Some(RuleAction::Exclude),
                    Actor::Human
                )
                .is_err()
        );
    }

    #[test]
    fn review_rules_survive_restart_and_are_isolated_and_last() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rules.db");
        let (repo_id, pr_id, other_pr, review_id) = {
            let mut store = Store::open(&path).unwrap();
            let repo = store
                .resolve_repository("github.com", "owner", "repo", None)
                .unwrap();
            let pr = store
                .upsert_pull_request(&repo.id, 1, "one", "url1")
                .unwrap();
            let other = store
                .upsert_pull_request(&repo.id, 2, "two", "url2")
                .unwrap();
            store
                .create_rule(
                    &repo.id,
                    RuleAction::Exclude,
                    RuleKind::Directory,
                    "generated",
                    None,
                    Actor::Human,
                )
                .unwrap();
            let review = store
                .create_review_rule(
                    &repo.id,
                    &pr,
                    RuleAction::Include,
                    RuleKind::File,
                    "generated/keep.rs",
                    None,
                    Actor::Human,
                )
                .unwrap();
            assert!(
                store
                    .create_rule_for_lifetime(
                        &repo.id,
                        &pr,
                        true,
                        RuleAction::Exclude,
                        RuleKind::File,
                        "x",
                        None,
                        Actor::Agent
                    )
                    .is_err()
            );
            assert!(store.get_review_rule(&repo.id, &other, &review.id).is_err());
            let foreign = store
                .resolve_repository("github.com", "owner", "foreign", None)
                .unwrap();
            assert!(store.get_review_rule(&foreign.id, &pr, &review.id).is_err());
            assert!(
                store
                    .create_review_rule(
                        &foreign.id,
                        &pr,
                        RuleAction::Exclude,
                        RuleKind::File,
                        "x",
                        None,
                        Actor::Human
                    )
                    .is_err()
            );
            (repo.id, pr, other, review.id)
        };
        let mut store = Store::open(&path).unwrap();
        let rules = store.effective_rules(&repo_id, &pr_id).unwrap();
        assert_eq!(
            crate::rules::evaluate(&rules, "generated/keep.rs")
                .effective_rule_id
                .as_deref(),
            Some(review_id.as_str())
        );
        assert!(!crate::rules::evaluate(&rules, "generated/keep.rs").ignored);
        assert!(
            crate::rules::evaluate(
                &store.effective_rules(&repo_id, &other_pr).unwrap(),
                "generated/keep.rs"
            )
            .ignored
        );
        assert!(
            store
                .update_review_rule(&repo_id, &other_pr, &review_id, &RuleUpdate::default())
                .is_err()
        );
        assert!(
            store
                .delete_review_rule(&repo_id, &other_pr, &review_id)
                .is_err()
        );
    }
}
