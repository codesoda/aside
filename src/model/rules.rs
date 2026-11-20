use serde::{Deserialize, Serialize};
use ts_rs::TS;

// Repository rules

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum RuleAction {
    Include,
    Exclude,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum RuleKind {
    File,
    Directory,
    Glob,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ExclusionRule {
    pub id: String,
    pub action: RuleAction,
    pub kind: RuleKind,
    pub expression: String,
    pub enabled: bool,
    /// Evaluation order. Later rules win.
    pub order: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Repository {
    pub id: String,
    pub host: String,
    pub owner: String,
    pub name: String,
    pub provider_id: Option<String>,
}
