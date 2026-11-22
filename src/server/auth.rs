use axum::{
    extract::{Query, Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};
use serde::Deserialize;

use super::{ApiError, ApiResult, AppState};
use crate::{model::Actor, util::secure_eq};

pub(super) fn host_ok(headers: &HeaderMap) -> bool {
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|host| {
            let Some((name, port)) = host.rsplit_once(':') else {
                return false;
            };
            (name == "127.0.0.1" || name == "localhost")
                && port.parse::<u16>().is_ok_and(|n| n != 0)
        })
}

fn same_origin(headers: &HeaderMap) -> bool {
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    origin == format!("{}://{host}", "http")
        || std::env::var("ASIDE_DEV_ORIGIN").is_ok_and(|dev| origin == dev)
}

/// Only these exact API mutations are available to the agent. Keep this
/// independent of router registration so new routes fail closed.
pub(super) fn agent_mutation_allowed(method: &Method, path: &str) -> bool {
    let Some(route) = path.strip_prefix("/api/v1/") else {
        return false;
    };
    let segments: Vec<_> = route.split('/').collect();
    match (method, segments.as_slice()) {
        (&Method::POST, ["threads" | "refresh"]) => true,
        (&Method::POST, ["threads", id, "replies"]) if !id.is_empty() => true,
        (&Method::PATCH | &Method::DELETE, ["comments", id]) if !id.is_empty() => true,
        _ => false,
    }
}

pub(super) async fn guard(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if !host_ok(request.headers()) {
        return ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_host",
            "invalid loopback host",
        )
        .into_response();
    }
    if path == "/auth" || path == "/api/v1/health" || !path.starts_with("/api/") {
        return next.run(request).await;
    }
    let bearer = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let cookie = request
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.split(';')
                .map(str::trim)
                .find_map(|part| part.strip_prefix("aside_ui="))
        });
    let actor = if bearer.is_some_and(|v| secure_eq(v, &state.agent_token)) {
        Some(Actor::Agent)
    } else if bearer.is_none() && cookie.is_some_and(|v| secure_eq(v, &state.ui_token)) {
        Some(Actor::Human)
    } else {
        None
    };
    let Some(actor) = actor else {
        return ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "authentication required",
        )
        .into_response();
    };
    if actor == Actor::Agent
        && !matches!(
            *request.method(),
            Method::GET | Method::HEAD | Method::OPTIONS
        )
        && !agent_mutation_allowed(request.method(), path)
    {
        return ApiError::forbidden().into_response();
    }
    if actor == Actor::Human
        && !matches!(
            *request.method(),
            Method::GET | Method::HEAD | Method::OPTIONS
        )
        && !same_origin(request.headers())
    {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            "invalid_origin",
            "a matching Origin is required",
        )
        .into_response();
    }
    request.extensions_mut().insert(actor);
    let response = next.run(request).await;
    if response.status().is_client_error()
        && response
            .headers()
            .get(header::CONTENT_TYPE)
            .is_none_or(|v| !v.as_bytes().starts_with(b"application/json"))
    {
        return ApiError::new(response.status(), "invalid_request", "invalid API request")
            .into_response();
    }
    response
}

#[derive(Deserialize)]
pub(super) struct LoginQuery {
    token: String,
}

pub(super) async fn login(
    State(state): State<AppState>,
    Query(query): Query<LoginQuery>,
) -> ApiResult<Response> {
    if !secure_eq(&query.token, &state.ui_token) {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "invalid login credential",
        ));
    }
    let mut response = Redirect::to("/").into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        format!(
            "aside_ui={}; HttpOnly; SameSite=Strict; Path=/",
            state.ui_token
        )
        .parse()
        .map_err(|_| ApiError::internal())?,
    );
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        "no-referrer".parse().map_err(|_| ApiError::internal())?,
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-store".parse().map_err(|_| ApiError::internal())?,
    );
    Ok(response)
}
