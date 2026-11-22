use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, ApiResult, AppState, body_text, db_error, files::checked_file, human};
use crate::{
    diff,
    model::{
        Actor, AgentState, AnchorState, Comment, LineKind, PublishState, Side, ThreadSource,
        ThreadSubject,
    },
    store::NewThread,
    util::{new_id, now_rfc3339},
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ThreadInput {
    file_id: String,
    side: Side,
    line: Option<u32>,
    start_line: Option<u32>,
    body: Option<String>,
    diff_digest: String,
    #[serde(default)]
    subject: ThreadSubject,
    finding_id: Option<String>,
}

pub(super) async fn create_thread(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Json(input): Json<ThreadInput>,
) -> ApiResult<Json<Value>> {
    let body = input.body.as_deref().unwrap_or("").trim();
    if input.finding_id.is_none() || !body.is_empty() {
        body_text(body)?;
    }
    let snapshot = state
        .session
        .snapshot
        .read()
        .map_err(|_| ApiError::internal())?;
    let file = checked_file(&snapshot, &input.file_id, &input.diff_digest)?;
    let line = match input.subject {
        ThreadSubject::Line => {
            if !file.patch_complete {
                return Err(ApiError::bad("file patch is incomplete"));
            }
            let number = input
                .line
                .ok_or_else(|| ApiError::bad("line is required"))?;
            let line = diff::find_line(file, input.side, number)
                .ok_or_else(|| ApiError::bad("line is not in the current diff"))?;
            if line.kind == LineKind::Meta {
                return Err(ApiError::bad("cannot anchor to metadata"));
            }
            if let Some(start) = input.start_line
                && (start > number || diff::find_line(file, input.side, start).is_none())
            {
                return Err(ApiError::bad("invalid startLine"));
            }
            Some(line)
        }
        ThreadSubject::File => {
            if input.line.is_some() || input.start_line.is_some() {
                return Err(ApiError::bad("file subject cannot have a line"));
            }
            None
        }
    };
    let mut store = state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?;
    if let Some(finding_id) = input.finding_id.as_deref() {
        let finding = store
            .list_findings(&state.session.pr_id, true)
            .map_err(db_error)?
            .into_iter()
            .find(|f| f.finding.value().id.as_str() == finding_id)
            .ok_or_else(ApiError::missing)?;
        let anchor = &finding.finding.value().anchor;
        if anchor.file_id.as_deref() != Some(file.id.as_str())
            && anchor.current_path.as_deref() != Some(file.path.as_str())
            && anchor.previous_path.as_deref() != Some(file.path.as_str())
        {
            return Err(ApiError::bad("finding does not belong to this file"));
        }
        if let Some(existing) = store
            .find_thread_by_finding(&state.session.pr_id, finding_id)
            .map_err(db_error)?
        {
            let thread = store
                .get_thread(&state.session.pr_id, &existing)
                .map_err(db_error)?;
            return Ok(Json(
                json!({"thread": thread, "sequence": state.session.sequence.load(std::sync::atomic::Ordering::SeqCst)}),
            ));
        }
    }
    let context = json!({
        "path": file.path, "side": input.side, "line": input.line,
        "startLine": input.start_line, "lineText": line.map(|l| &l.text),
        "headSha": snapshot.pr.head_sha, "body": body,
    });
    let id = new_id("thr");
    let thread = NewThread {
        id: id.clone(),
        source: ThreadSource::Local,
        github_thread_id: None,
        path: file.path.clone(),
        side: input.side,
        line: input.line,
        start_line: input.start_line,
        line_text: line.map(|l| l.text.clone()),
        diff_digest: Some(file.diff_digest.clone()),
        head_sha: snapshot.pr.head_sha.clone(),
        anchor_state: AnchorState::Current,
        anchor_reason: None,
        resolved: false,
        created_at: None,
        subject: input.subject,
        finding_id: input.finding_id,
    };
    let now = now_rfc3339();
    store
        .insert_thread(&state.session.pr_id, &thread)
        .map_err(db_error)?;
    let comment = if body.is_empty() {
        None
    } else {
        let comment = Comment {
            id: new_id("cmt"),
            thread_id: id.clone(),
            author_kind: actor,
            author: actor.as_str().into(),
            body: body.into(),
            created_at: now.clone(),
            updated_at: now,
            publish_state: PublishState::Private,
            github_comment_id: None,
        };
        if actor == Actor::Human {
            if !store.insert_human_request(&comment).map_err(db_error)? {
                return Err(conflict("an agent request is already active"));
            }
        } else {
            store.insert_comment(&comment, None).map_err(db_error)?;
        }
        Some(comment)
    };
    drop(store);
    drop(snapshot);
    let event = state.session.notify(
        "thread.created",
        if actor == Actor::Human {
            let mut context = context;
            context["threadId"] = json!(id);
            if let Some(comment) = &comment {
                context["commentId"] = json!(comment.id);
                context["requestId"] = json!(comment.id);
            }
            context
        } else {
            json!({"threadId":id})
        },
    );
    Ok(Json(
        json!({"thread":state.session.store.lock().map_err(|_| ApiError::internal())?.get_thread(&state.session.pr_id, &id).map_err(db_error)?,"sequence":event.sequence}),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ReplyInput {
    body: String,
    in_reply_to: Option<String>,
}
pub(super) async fn reply(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(id): Path<String>,
    Json(input): Json<ReplyInput>,
) -> ApiResult<Json<Value>> {
    body_text(&input.body)?;
    let mut store = state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?;
    let thread = store
        .get_thread(&state.session.pr_id, &id)
        .map_err(db_error)?;
    if let Some(parent) = input.in_reply_to.as_deref()
        && !thread.comments.iter().any(|c| c.id == parent)
    {
        return Err(ApiError::bad("inReplyTo is not in this thread"));
    }
    if actor == Actor::Agent {
        let request = input
            .in_reply_to
            .as_deref()
            .ok_or_else(|| ApiError::bad("inReplyTo is required"))?;
        if thread.agent.request_comment_id.as_deref() != Some(request)
            || !matches!(
                thread.agent.state,
                AgentState::Waiting | AgentState::Responding
            )
        {
            return Err(conflict("agent request is no longer active"));
        }
    }
    let now = now_rfc3339();
    let comment = Comment {
        id: new_id("cmt"),
        thread_id: id.clone(),
        author_kind: actor,
        author: actor.as_str().into(),
        body: input.body.trim().into(),
        created_at: now.clone(),
        updated_at: now,
        publish_state: PublishState::Private,
        github_comment_id: None,
    };
    if actor == Actor::Human {
        if !store.insert_human_request(&comment).map_err(db_error)? {
            return Err(conflict(
                "an agent request is already active; reply after it completes",
            ));
        }
    } else {
        let request = input.in_reply_to.as_deref().expect("validated above");
        if thread.agent.state == AgentState::Responding
            && !store
                .transition_agent_request(
                    &id,
                    request,
                    AgentState::Responding,
                    AgentState::Waiting,
                    None,
                )
                .map_err(db_error)?
        {
            return Err(conflict("agent request changed"));
        }
        if !store
            .insert_agent_response(&id, request, &comment)
            .map_err(db_error)?
        {
            return Err(conflict("agent request is no longer active"));
        }
    }
    drop(store);
    let event = state.session.notify(
        "thread.replied",
        if actor == Actor::Human {
            json!({"threadId":id,"commentId":comment.id,"authorKind":actor,"requestId":comment.id,"body":comment.body})
        } else { json!({"threadId":id,"commentId":comment.id}) },
    );
    Ok(Json(json!({"comment":comment,"sequence":event.sequence})))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ThreadPatch {
    resolved: Option<bool>,
    read_through: Option<String>,
}
pub(super) async fn update_thread(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(id): Path<String>,
    Json(input): Json<ThreadPatch>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    let mut store = state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?;
    let thread = store
        .get_thread(&state.session.pr_id, &id)
        .map_err(db_error)?;
    if thread.source != ThreadSource::Local {
        return Err(ApiError::forbidden());
    }
    if input.resolved.is_none() && input.read_through.is_none() {
        return Err(ApiError::bad("no thread changes"));
    }
    if let Some(resolved) = input.resolved {
        store.set_thread_resolved(&id, resolved).map_err(db_error)?;
    }
    if let Some(through) = input.read_through
        && !store.mark_thread_read(&id, &through).map_err(db_error)?
    {
        return Err(ApiError::bad("readThrough is not in this thread"));
    }
    drop(store);
    let event = state
        .session
        .notify("thread.changed", json!({"threadId":id}));
    Ok(Json(
        json!({"thread":state.session.store.lock().map_err(|_| ApiError::internal())?.get_thread(&state.session.pr_id, &id).map_err(db_error)?,"sequence":event.sequence}),
    ))
}

#[cfg(test)]
#[path = "threads_tests.rs"]
mod conversation_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn human_thread_event_gives_host_the_message_and_code_context() {
        let session = crate::session::Session::new(
            crate::demo::snapshot(0),
            crate::store::Store::open_in_memory().unwrap(),
            None,
            None,
        )
        .unwrap();
        let (file_id, digest, side, line, path, text) = {
            let snapshot = session.snapshot.read().unwrap();
            let file = snapshot
                .files
                .iter()
                .find(|file| {
                    file.hunks.iter().any(|hunk| {
                        hunk.lines
                            .iter()
                            .any(|line| line.new_line.is_some() && line.kind != LineKind::Meta)
                    })
                })
                .unwrap();
            let line = file
                .hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .find(|line| line.new_line.is_some() && line.kind != LineKind::Meta)
                .unwrap();
            (
                file.id.clone(),
                file.diff_digest.clone(),
                Side::Right,
                line.new_line.unwrap(),
                file.path.clone(),
                line.text.clone(),
            )
        };
        let state = AppState::new(Arc::clone(&session), "ui".into(), "agent".into());
        let mut events = session.events.subscribe();
        let input = || ThreadInput {
            file_id: file_id.clone(),
            diff_digest: digest.clone(),
            side,
            line: Some(line),
            start_line: None,
            body: Some("  Could you check this?  ".into()),
            subject: ThreadSubject::Line,
            finding_id: None,
        };
        let _ = create_thread(
            State(state.clone()),
            axum::Extension(Actor::Human),
            Json(input()),
        )
        .await
        .unwrap();
        let event = events.recv().await.unwrap();
        assert_eq!(event.event_type, "thread.created");
        assert_eq!(event.payload["path"], path);
        assert_eq!(event.payload["lineText"], text);
        assert_eq!(event.payload["line"], line);
        assert_eq!(event.payload["body"], "Could you check this?");
        assert!(event.payload["threadId"].as_str().is_some());

        let _ = create_thread(State(state), axum::Extension(Actor::Agent), Json(input()))
            .await
            .unwrap();
        let event = events.recv().await.unwrap();
        assert_eq!(
            event.payload.as_object().unwrap().len(),
            1,
            "agent drafts do not request a reply"
        );
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ReviewCommentInput {
    body: String,
    source_comment_id: Option<String>,
}

pub(super) async fn put_review_comment(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(id): Path<String>,
    Json(input): Json<ReviewCommentInput>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    let mut store = state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?;
    store
        .get_thread(&state.session.pr_id, &id)
        .map_err(db_error)?;
    let draft = if input.body.trim().is_empty() {
        store
            .clear_review_comment(&state.session.pr_id, &id)
            .map_err(db_error)?;
        None
    } else {
        body_text(&input.body)?;
        Some(
            store
                .upsert_review_comment(
                    &state.session.pr_id,
                    &id,
                    input.body.trim(),
                    input.source_comment_id.as_deref(),
                )
                .map_err(db_error)?,
        )
    };
    drop(store);
    let event = state
        .session
        .notify("thread.changed", json!({"threadId":id}));
    Ok(Json(
        json!({"reviewComment":draft,"sequence":event.sequence}),
    ))
}

pub(super) async fn delete_review_comment(
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
        .clear_review_comment(&state.session.pr_id, &id)
        .map_err(db_error)?;
    let event = state
        .session
        .notify("thread.changed", json!({"threadId":id}));
    Ok(Json(json!({"sequence":event.sequence})))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AgentResponseInput {
    request_id: String,
    state: AgentState,
    error: Option<String>,
}

pub(super) async fn agent_response(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(id): Path<String>,
    Json(input): Json<AgentResponseInput>,
) -> ApiResult<Json<Value>> {
    if actor != Actor::Agent {
        return Err(ApiError::forbidden());
    }
    if !matches!(input.state, AgentState::Responding | AgentState::Failed) {
        return Err(ApiError::bad("state must be responding or failed"));
    }
    let mut store = state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?;
    store
        .get_thread(&state.session.pr_id, &id)
        .map_err(db_error)?;
    let from = if input.state == AgentState::Failed {
        let thread = store
            .get_thread(&state.session.pr_id, &id)
            .map_err(db_error)?;
        if !matches!(
            thread.agent.state,
            AgentState::Waiting | AgentState::Responding
        ) {
            return Err(conflict("agent request is no longer active"));
        }
        thread.agent.state
    } else {
        AgentState::Waiting
    };
    if !store
        .transition_agent_request(
            &id,
            &input.request_id,
            from,
            input.state,
            input.error.as_deref(),
        )
        .map_err(db_error)?
    {
        return Err(conflict("agent request is no longer waiting"));
    }
    drop(store);
    let event = state
        .session
        .notify("thread.changed", json!({"threadId":id}));
    Ok(Json(json!({"sequence":event.sequence})))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RetryInput {
    request_id: String,
}

pub(super) async fn retry_agent_request(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(id): Path<String>,
    Json(input): Json<RetryInput>,
) -> ApiResult<Json<Value>> {
    human(actor)?;
    let mut store = state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?;
    let thread = store
        .get_thread(&state.session.pr_id, &id)
        .map_err(db_error)?;
    if thread.agent.request_comment_id.as_deref() != Some(&input.request_id)
        || thread.agent.state != AgentState::Failed
    {
        return Err(conflict("agent request is not failed"));
    }
    if !store
        .transition_agent_request(
            &id,
            &input.request_id,
            AgentState::Failed,
            AgentState::Waiting,
            None,
        )
        .map_err(db_error)?
    {
        return Err(conflict("agent request changed"));
    }
    drop(store);
    let event = state.session.notify("thread.replied", json!({"threadId":id,"commentId":input.request_id,"requestId":input.request_id,"authorKind":"human","body":thread.comments.iter().find(|c| c.id == input.request_id).map(|c| c.body.as_str())}));
    Ok(Json(json!({"sequence":event.sequence})))
}

fn conflict(message: &str) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, "request_conflict", message)
}

fn editable_comment(state: &AppState, id: &str, actor: Actor) -> ApiResult<Comment> {
    let store = state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?;
    let comment = store.get_comment(id).map_err(db_error)?;
    store
        .get_thread(&state.session.pr_id, &comment.thread_id)
        .map_err(db_error)?;
    if comment.publish_state != PublishState::Private || comment.author_kind != actor {
        return Err(ApiError::forbidden());
    }
    Ok(comment)
}
pub(super) async fn update_comment(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(id): Path<String>,
    Json(input): Json<ReplyInput>,
) -> ApiResult<Json<Value>> {
    body_text(&input.body)?;
    editable_comment(&state, &id, actor)?;
    state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?
        .update_comment_body(&id, input.body.trim())
        .map_err(db_error)?;
    let event = state
        .session
        .notify("comment.changed", json!({"commentId":id}));
    Ok(Json(
        json!({"comment":state.session.store.lock().map_err(|_| ApiError::internal())?.get_comment(&id).map_err(db_error)?,"sequence":event.sequence}),
    ))
}
pub(super) async fn delete_comment(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    editable_comment(&state, &id, actor)?;
    state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?
        .delete_comment(&id)
        .map_err(db_error)?;
    let event = state
        .session
        .notify("comment.deleted", json!({"commentId":id}));
    Ok(Json(json!({"sequence":event.sequence})))
}
