use axum::{
    Json,
    extract::{Path, State},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, ApiResult, AppState, body_text, db_error, files::checked_file, human};
use crate::{
    diff,
    model::{Actor, AnchorState, Comment, LineKind, PublishState, Side, ThreadSource},
    store::NewThread,
    util::{new_id, now_rfc3339},
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ThreadInput {
    file_id: String,
    side: Side,
    line: u32,
    start_line: Option<u32>,
    body: String,
    diff_digest: String,
}

pub(super) async fn create_thread(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Json(input): Json<ThreadInput>,
) -> ApiResult<Json<Value>> {
    body_text(&input.body)?;
    let snapshot = state
        .session
        .snapshot
        .read()
        .map_err(|_| ApiError::internal())?;
    let file = checked_file(&snapshot, &input.file_id, &input.diff_digest)?;
    if !file.patch_complete {
        return Err(ApiError::bad("file patch is incomplete"));
    }
    let line = diff::find_line(file, input.side, input.line)
        .ok_or_else(|| ApiError::bad("line is not in the current diff"))?;
    if line.kind == LineKind::Meta {
        return Err(ApiError::bad("cannot anchor to metadata"));
    }
    if let Some(start) = input.start_line
        && (start > input.line || diff::find_line(file, input.side, start).is_none())
    {
        return Err(ApiError::bad("invalid startLine"));
    }
    let context = json!({
        "path": file.path, "side": input.side, "line": input.line,
        "startLine": input.start_line, "lineText": line.text,
        "headSha": snapshot.pr.head_sha, "body": input.body.trim(),
    });
    let id = new_id("thr");
    let thread = NewThread {
        id: id.clone(),
        source: ThreadSource::Local,
        github_thread_id: None,
        path: file.path.clone(),
        side: input.side,
        line: Some(input.line),
        start_line: input.start_line,
        line_text: Some(line.text.clone()),
        diff_digest: Some(file.diff_digest.clone()),
        head_sha: snapshot.pr.head_sha.clone(),
        anchor_state: AnchorState::Current,
        anchor_reason: None,
        resolved: false,
        created_at: None,
    };
    let now = now_rfc3339();
    let comment = Comment {
        id: new_id("cmt"),
        thread_id: id.clone(),
        author_kind: actor,
        author: actor.as_str().into(),
        body: input.body.trim().into(),
        created_at: now.clone(),
        updated_at: now,
        publish_state: PublishState::Draft,
        github_comment_id: None,
    };
    let mut store = state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?;
    store
        .insert_thread(&state.session.pr_id, &thread)
        .map_err(db_error)?;
    store.insert_comment(&comment, None).map_err(db_error)?;
    drop(store);
    drop(snapshot);
    let event = state.session.notify(
        "thread.created",
        if actor == Actor::Human {
            let mut context = context;
            context["threadId"] = json!(id);
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
pub(super) struct ReplyInput {
    body: String,
}
pub(super) async fn reply(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(id): Path<String>,
    Json(input): Json<ReplyInput>,
) -> ApiResult<Json<Value>> {
    body_text(&input.body)?;
    state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?
        .get_thread(&state.session.pr_id, &id)
        .map_err(db_error)?;
    let now = now_rfc3339();
    let comment = Comment {
        id: new_id("cmt"),
        thread_id: id.clone(),
        author_kind: actor,
        author: actor.as_str().into(),
        body: input.body.trim().into(),
        created_at: now.clone(),
        updated_at: now,
        publish_state: PublishState::Draft,
        github_comment_id: None,
    };
    state
        .session
        .store
        .lock()
        .map_err(|_| ApiError::internal())?
        .insert_comment(&comment, None)
        .map_err(db_error)?;
    let event = state.session.notify(
        "thread.replied",
        json!({"threadId":id,"commentId":comment.id}),
    );
    Ok(Json(json!({"comment":comment,"sequence":event.sequence})))
}

#[derive(Deserialize)]
pub(super) struct ThreadPatch {
    resolved: bool,
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
    store
        .set_thread_resolved(&id, input.resolved)
        .map_err(db_error)?;
    drop(store);
    let event = state
        .session
        .notify("thread.changed", json!({"threadId":id}));
    Ok(Json(
        json!({"thread":state.session.store.lock().map_err(|_| ApiError::internal())?.get_thread(&state.session.pr_id, &id).map_err(db_error)?,"sequence":event.sequence}),
    ))
}

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
            line,
            start_line: None,
            body: "  Could you check this?  ".into(),
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
    if comment.publish_state != PublishState::Draft || comment.author_kind != actor {
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
