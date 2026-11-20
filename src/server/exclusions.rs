use axum::{
    Json,
    extract::{Path, State},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, ApiResult, AppState, db_error, human};
use crate::{
    model::{Actor, RuleAction, RuleKind},
    rules,
    store::RuleUpdate,
};

pub(super) async fn exclusions(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let view = state.session.view(Actor::Human).map_err(db_error)?;
    Ok(Json(
        json!({"rules":view.rules,"fileStates":view.file_states,"sequence":view.sequence}),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RuleInput {
    action: RuleAction,
    kind: RuleKind,
    expression: String,
    position: Option<usize>,
}

pub(super) async fn preview_exclusion(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Json(input): Json<RuleInput>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    let expression = rules::normalize(input.kind, &input.expression)
        .map_err(|e| ApiError::bad(e.to_string()))?;
    let mut view = state.session.view(actor).map_err(db_error)?;
    let rule = crate::model::ExclusionRule {
        id: "preview".into(),
        action: input.action,
        kind: input.kind,
        expression,
        enabled: true,
        order: 0,
        created_at: String::new(),
        updated_at: String::new(),
    };
    let at = input
        .position
        .unwrap_or(view.rules.len())
        .min(view.rules.len());
    view.rules.insert(at, rule);
    for (i, rule) in view.rules.iter_mut().enumerate() {
        rule.order = i as i64;
    }
    let matches: Vec<_> = view.files.iter().map(|file| json!({"fileId":file.id,"path":file.path,"scope":rules::evaluate(&view.rules, &file.path)})).collect();
    Ok(Json(json!({"matches":matches,"sequence":view.sequence})))
}

pub(super) async fn create_exclusion(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Json(input): Json<RuleInput>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    let expression = rules::normalize(input.kind, &input.expression)
        .map_err(|e| ApiError::bad(e.to_string()))?;
    let rule = state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?
        .create_rule(
            &state.session.repository.id,
            input.action,
            input.kind,
            &expression,
            input.position,
            actor,
        )
        .map_err(db_error)?;
    let event = state
        .session
        .notify("exclusions.changed", json!({"ruleId":rule.id}));
    Ok(Json(
        json!({"rule":rule,"sequence":event.sequence,"fileStates":state.session.view(actor).map_err(db_error)?.file_states}),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RulePatch {
    action: Option<RuleAction>,
    kind: Option<RuleKind>,
    expression: Option<String>,
    enabled: Option<bool>,
    position: Option<usize>,
}

pub(super) async fn update_exclusion(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(id): Path<String>,
    Json(input): Json<RulePatch>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    let mut store = state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?;
    let old = store
        .get_rule(&state.session.repository.id, &id)
        .map_err(db_error)?;
    let kind = input.kind.unwrap_or(old.kind);
    let expression = input.expression.as_deref().unwrap_or(&old.expression);
    let update = RuleUpdate {
        action: input.action,
        kind_expression: Some((
            kind,
            rules::normalize(kind, expression).map_err(|e| ApiError::bad(e.to_string()))?,
        )),
        enabled: input.enabled,
        position: input.position,
    };
    let rule = store
        .update_rule(&state.session.repository.id, &id, &update)
        .map_err(db_error)?;
    drop(store);
    let event = state
        .session
        .notify("exclusions.changed", json!({"ruleId":id}));
    Ok(Json(
        json!({"rule":rule,"sequence":event.sequence,"fileStates":state.session.view(actor).map_err(db_error)?.file_states}),
    ))
}

pub(super) async fn delete_exclusion(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?
        .delete_rule(&state.session.repository.id, &id)
        .map_err(db_error)?;
    let event = state
        .session
        .notify("exclusions.changed", json!({"ruleId":id}));
    Ok(Json(
        json!({"sequence":event.sequence,"fileStates":state.session.view(actor).map_err(db_error)?.file_states}),
    ))
}
