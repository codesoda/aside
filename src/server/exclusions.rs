use axum::{
    Json,
    extract::{Path, State},
};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum PreferenceMode {
    Include,
    Exclude,
    Default,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PreferenceInput {
    mode: PreferenceMode,
}

pub(super) async fn extension_preferences(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let store = state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?;
    let ids = store
        .extension_preference_ids(&state.session.repository.id)
        .map_err(db_error)?;
    let rules: Vec<_> = store
        .list_rules(&state.session.repository.id)
        .map_err(db_error)?
        .into_iter()
        .filter(|r| ids.contains(&r.id))
        .map(|r| rule_json(r, Lifetime::Repository, true))
        .collect();
    Ok(Json(json!({"rules":rules})))
}

pub(super) async fn set_extension_preference(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(category): Path<String>,
    Json(input): Json<PreferenceInput>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    rules::extension_expression(&category).map_err(|e| ApiError::bad(e.to_string()))?;
    let mut store = state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?;
    let action = match input.mode {
        PreferenceMode::Include => Some(RuleAction::Include),
        PreferenceMode::Exclude => Some(RuleAction::Exclude),
        PreferenceMode::Default => None,
    };
    let rule = store
        .set_extension_preference(&state.session.repository.id, &category, action, actor)
        .map_err(db_error)?;
    drop(store);
    let event = state
        .session
        .notify("exclusions.changed", json!({"category":category}));
    Ok(Json(
        json!({"rule":rule.map(|r| rule_json(r, Lifetime::Repository, true)), "sequence":event.sequence,
        "fileStates":state.session.view(actor).map_err(db_error)?.file_states}),
    ))
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Lifetime {
    Review,
    #[default]
    Repository,
}

fn rule_json(rule: crate::model::ExclusionRule, lifetime: Lifetime, preference: bool) -> Value {
    let mut value = serde_json::to_value(rule).expect("rule serializes");
    value["lifetime"] = json!(if lifetime == Lifetime::Review {
        "review"
    } else {
        "repository"
    });
    value["source"] = json!(if preference {
        "extension-preference"
    } else {
        "rule"
    });
    if preference {
        value["category"] = json!(
            value["expression"]
                .as_str()
                .unwrap_or("")
                .trim_start_matches("@extension:")
        );
    }
    value
}
use serde_json::{Value, json};

use super::{ApiError, ApiResult, AppState, db_error, human};
use crate::{
    model::{Actor, RuleAction, RuleKind},
    rules,
    store::RuleUpdate,
};

pub(super) async fn exclusions(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let view = state.session.view(Actor::Human).map_err(db_error)?;
    let store = state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?;
    let preference_ids = store
        .extension_preference_ids(&state.session.repository.id)
        .map_err(db_error)?;
    let review_ids: std::collections::HashSet<_> = store
        .list_review_rules(&state.session.repository.id, &state.session.pr_id)
        .map_err(db_error)?
        .into_iter()
        .map(|r| r.id)
        .collect();
    let rules: Vec<_> = store
        .effective_rules(&state.session.repository.id, &state.session.pr_id)
        .map_err(db_error)?
        .into_iter()
        .map(|r| {
            let lifetime = if review_ids.contains(&r.id) {
                Lifetime::Review
            } else {
                Lifetime::Repository
            };
            let preference = preference_ids.contains(&r.id);
            rule_json(r, lifetime, preference)
        })
        .collect();
    Ok(Json(
        json!({"rules":rules,"fileStates":view.file_states,"sequence":view.sequence}),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RuleInput {
    action: RuleAction,
    kind: RuleKind,
    expression: String,
    position: Option<usize>,
    #[serde(default)]
    lifetime: Lifetime,
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
    // Capture both lifetime groups under the same lock, after view() releases it.
    let (repo_rules, review_rules) = preview_rule_groups(
        &state.session.store,
        &state.session.repository.id,
        &state.session.pr_id,
    )?;
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
    view.rules = preview_rules(
        repo_rules,
        review_rules,
        input.lifetime,
        input.position,
        rule,
    );
    let matches: Vec<_> = view.files.iter().map(|file| json!({"fileId":file.id,"path":file.path,"scope":rules::evaluate(&view.rules, &file.path)})).collect();
    Ok(Json(json!({"matches":matches,"sequence":view.sequence})))
}

fn preview_rule_groups(
    store: &std::sync::Mutex<crate::store::Store>,
    repository_id: &str,
    pr_id: &str,
) -> ApiResult<(
    Vec<crate::model::ExclusionRule>,
    Vec<crate::model::ExclusionRule>,
)> {
    let store = store.lock().map_err(|_| ApiError::internal())?;
    Ok((
        store.list_rules(repository_id).map_err(db_error)?,
        store
            .list_review_rules(repository_id, pr_id)
            .map_err(db_error)?,
    ))
}

fn preview_rules(
    mut repo_rules: Vec<crate::model::ExclusionRule>,
    review_rules: Vec<crate::model::ExclusionRule>,
    lifetime: Lifetime,
    position: Option<usize>,
    rule: crate::model::ExclusionRule,
) -> Vec<crate::model::ExclusionRule> {
    let repo_len = repo_rules.len();
    let review_len = review_rules.len();
    repo_rules.extend(review_rules);
    let at = if lifetime == Lifetime::Review {
        repo_len + position.unwrap_or(review_len).min(review_len)
    } else {
        position.unwrap_or(repo_len).min(repo_len)
    };
    repo_rules.insert(at, rule);
    for (i, rule) in repo_rules.iter_mut().enumerate() {
        rule.order = i as i64;
    }
    repo_rules
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
        .create_rule_for_lifetime(
            &state.session.repository.id,
            &state.session.pr_id,
            input.lifetime == Lifetime::Review,
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
        json!({"rule":rule_json(rule, input.lifetime, false),"sequence":event.sequence,"fileStates":state.session.view(actor).map_err(db_error)?.file_states}),
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
    lifetime: Option<Lifetime>,
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
    let review = store
        .get_review_rule(&state.session.repository.id, &state.session.pr_id, &id)
        .ok();
    let lifetime = if review.is_some() {
        Lifetime::Review
    } else {
        Lifetime::Repository
    };
    if input
        .lifetime
        .is_some_and(|requested| requested != lifetime)
    {
        return Err(ApiError::bad("rule lifetime does not match"));
    }
    let old = match review {
        Some(rule) => rule,
        None => store
            .get_rule(&state.session.repository.id, &id)
            .map_err(db_error)?,
    };
    let preference = store
        .extension_preference_ids(&state.session.repository.id)
        .map_err(db_error)?
        .contains(&id);
    if preference && (input.kind.is_some() || input.expression.is_some()) {
        return Err(ApiError::bad(
            "extension preference kind and expression cannot be changed",
        ));
    }
    let kind_expression = if input.kind.is_some() || input.expression.is_some() {
        let kind = input.kind.unwrap_or(old.kind);
        let expression = input.expression.as_deref().unwrap_or(&old.expression);
        Some((
            kind,
            rules::normalize(kind, expression).map_err(|e| ApiError::bad(e.to_string()))?,
        ))
    } else {
        None
    };
    let update = RuleUpdate {
        action: input.action,
        kind_expression,
        enabled: input.enabled,
        position: input.position,
    };
    let rule = if lifetime == Lifetime::Review {
        store.update_review_rule(
            &state.session.repository.id,
            &state.session.pr_id,
            &id,
            &update,
        )
    } else {
        store.update_rule(&state.session.repository.id, &id, &update)
    }
    .map_err(db_error)?;
    drop(store);
    let event = state
        .session
        .notify("exclusions.changed", json!({"ruleId":id}));
    Ok(Json(
        json!({"rule":rule_json(rule, lifetime, preference),"sequence":event.sequence,"fileStates":state.session.view(actor).map_err(db_error)?.file_states}),
    ))
}

pub(super) async fn delete_exclusion(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    let mut store = state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?;
    if store
        .get_review_rule(&state.session.repository.id, &state.session.pr_id, &id)
        .is_ok()
    {
        store.delete_review_rule(&state.session.repository.id, &state.session.pr_id, &id)
    } else {
        store.delete_rule(&state.session.repository.id, &id)
    }
    .map_err(db_error)?;
    drop(store);
    let event = state
        .session
        .notify("exclusions.changed", json!({"ruleId":id}));
    Ok(Json(
        json!({"sequence":event.sequence,"fileStates":state.session.view(actor).map_err(db_error)?.file_states}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{Method, StatusCode, header};
    use tower::ServiceExt;

    async fn send(
        app: axum::Router,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let req = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "127.0.0.1:1234")
            .header(header::ORIGIN, "http://127.0.0.1:1234")
            .header(header::COOKIE, "aside_ui=ui-secret")
            .header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(
                body.map_or(String::new(), |v| v.to_string()),
            ))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 2_000_000)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    #[test]
    fn concurrent_rule_mutations_keep_preview_lifetimes_grouped() {
        use std::sync::{Arc, Mutex};
        let mut store = crate::store::Store::open_in_memory().unwrap();
        let repo = store
            .resolve_repository("github.com", "owner", "repo", None)
            .unwrap();
        let pr = store
            .upsert_pull_request(&repo.id, 1, "one", "url")
            .unwrap();
        let store = Arc::new(Mutex::new(store));
        let writer_store = Arc::clone(&store);
        let repo_id = repo.id.clone();
        let pr_id = pr.clone();
        let writer = std::thread::spawn(move || {
            for _ in 0..100 {
                let mut store = writer_store.lock().unwrap();
                let repo_rule = store
                    .create_rule(
                        &repo_id,
                        RuleAction::Exclude,
                        RuleKind::File,
                        "x",
                        None,
                        Actor::Human,
                    )
                    .unwrap();
                let review_rule = store
                    .create_review_rule(
                        &repo_id,
                        &pr_id,
                        RuleAction::Include,
                        RuleKind::File,
                        "x",
                        None,
                        Actor::Human,
                    )
                    .unwrap();
                drop(store);
                std::thread::yield_now();
                let mut store = writer_store.lock().unwrap();
                store.delete_rule(&repo_id, &repo_rule.id).unwrap();
                store
                    .delete_review_rule(&repo_id, &pr_id, &review_rule.id)
                    .unwrap();
                drop(store);
                std::thread::yield_now();
            }
        });
        for lifetime in [Lifetime::Repository, Lifetime::Review] {
            for position in [None, Some(0), Some(usize::MAX)] {
                for _ in 0..100 {
                    let (repos, reviews) = preview_rule_groups(&store, &repo.id, &pr).unwrap();
                    let repo_len = repos.len();
                    let review_len = reviews.len();
                    let preview = crate::model::ExclusionRule {
                        id: "preview".into(),
                        action: RuleAction::Exclude,
                        kind: RuleKind::File,
                        expression: "x".into(),
                        enabled: true,
                        order: 0,
                        created_at: String::new(),
                        updated_at: String::new(),
                    };
                    let rules = preview_rules(repos, reviews, lifetime, position, preview);
                    let index = rules.iter().position(|rule| rule.id == "preview").unwrap();
                    assert_eq!(rules.len(), repo_len + review_len + 1);
                    assert_eq!(
                        index,
                        if lifetime == Lifetime::Review {
                            repo_len + position.unwrap_or(review_len).min(review_len)
                        } else {
                            position.unwrap_or(repo_len).min(repo_len)
                        }
                    );
                    assert!(
                        rules[..index]
                            .iter()
                            .all(|rule| lifetime == Lifetime::Review
                                || !rule.id.starts_with("review-rule"))
                    );
                    assert!(
                        rules[index + 1..]
                            .iter()
                            .all(|rule| lifetime == Lifetime::Repository
                                || rule.id.starts_with("review-rule"))
                    );
                    assert!(rules.windows(2).all(|pair| pair[0].order < pair[1].order));
                }
            }
        }
        writer.join().unwrap();
    }

    #[tokio::test]
    async fn preference_patch_cannot_repurpose_but_can_change_action_position_enabled() {
        let session = crate::session::Session::new(
            crate::demo::snapshot(0),
            crate::store::Store::open_in_memory().unwrap(),
            None,
            None,
        )
        .unwrap();
        let app = crate::server::router(crate::server::AppState::new(
            session,
            "ui-secret".into(),
            "agent-secret".into(),
        ));
        let pref = "/api/v1/repositories/current/extension-preferences/.rs";
        let (_, created) = send(
            app.clone(),
            Method::PUT,
            pref,
            Some(json!({"mode":"exclude"})),
        )
        .await;
        let id = created["rule"]["id"].as_str().unwrap();
        let route = format!("/api/v1/repositories/current/exclusions/{id}");
        let (status, _) = send(
            app.clone(),
            Method::PATCH,
            &route,
            Some(json!({"expression":"*.ts"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = send(
            app.clone(),
            Method::PATCH,
            &route,
            Some(json!({"kind":"file"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, changed) = send(
            app.clone(),
            Method::PATCH,
            &route,
            Some(json!({"action":"include", "enabled":false, "position":0})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(changed["rule"]["expression"], "@extension:.rs");
        assert_eq!(changed["rule"]["action"], "include");
        assert_eq!(changed["rule"]["enabled"], false);
        let (status, updated) = send(
            app.clone(),
            Method::PUT,
            pref,
            Some(json!({"mode":"exclude"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(updated["rule"]["id"], id);
        assert_eq!(updated["rule"]["enabled"], true);
        let (status, removed) = send(
            app.clone(),
            Method::PUT,
            pref,
            Some(json!({"mode":"default"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(removed["rule"].is_null());
        let (_, listed) = send(
            app,
            Method::GET,
            "/api/v1/repositories/current/exclusions",
            None,
        )
        .await;
        assert!(
            !listed["rules"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["id"] == id)
        );
    }
}
