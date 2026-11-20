//! Authentication, permissions and host checks for the HTTP API.

use super::auth::host_ok;
use super::*;
use axum::http::{HeaderMap, Method, header};
fn app() -> Router {
    let session = Session::new(
        crate::demo::snapshot(0),
        crate::store::Store::open_in_memory().unwrap(),
        None,
        None,
    )
    .unwrap();
    router(AppState::new(
        session,
        "ui-secret".into(),
        "agent-secret".into(),
    ))
}

async fn request(
    method: Method,
    path: &str,
    credential: Option<&str>,
    origin: Option<&str>,
    body: Option<Value>,
) -> Response {
    use tower::ServiceExt;
    let mut builder = axum::http::Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "127.0.0.1:1234");
    if let Some(value) = credential {
        builder = builder.header(
            if value.starts_with("Bearer ") {
                header::AUTHORIZATION
            } else {
                header::COOKIE
            },
            value,
        );
    }
    if let Some(value) = origin {
        builder = builder.header(header::ORIGIN, value);
    }
    let bytes = if let Some(body) = body {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
        body.to_string()
    } else {
        String::new()
    };
    app()
        .oneshot(builder.body(axum::body::Body::from(bytes)).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn auth_and_permissions() {
    let path = "/api/v1/session";
    assert_eq!(
        request(Method::GET, path, None, None, None).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(Method::GET, path, Some("Bearer agent-secret"), None, None)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        request(Method::GET, path, Some("aside_ui=ui-secret"), None, None)
            .await
            .status(),
        StatusCode::OK
    );
    let rule = json!({"action":"exclude","kind":"glob","expression":"*.rs"});
    let route = "/api/v1/repositories/current/exclusions";
    assert_eq!(
        request(
            Method::POST,
            route,
            Some("Bearer agent-secret"),
            None,
            Some(rule.clone())
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            Method::POST,
            route,
            Some("aside_ui=ui-secret"),
            None,
            Some(rule.clone())
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            Method::POST,
            route,
            Some("aside_ui=ui-secret"),
            Some("http://127.0.0.1:1234"),
            Some(rule)
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        request(
            Method::POST,
            "/api/v1/review/publish",
            Some("Bearer agent-secret"),
            None,
            Some(json!({"planId":"x","confirm":true}))
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            Method::POST,
            "/api/v1/review/publish/preview",
            Some("aside_ui=ui-secret"),
            Some("http://127.0.0.1:1234"),
            None
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn stale_digest_and_host_rejection() {
    use tower::ServiceExt;
    let id = crate::demo::snapshot(0).files[0].id.clone();
    let path = format!("/api/v1/files/{id}/reviewed");
    let response = request(
        Method::POST,
        &path,
        Some("aside_ui=ui-secret"),
        Some("http://127.0.0.1:1234"),
        Some(json!({"diffDigest":"stale"})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = axum::body::to_bytes(response.into_body(), 100_000)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["error"],
        "stale_fingerprint"
    );
    let request = axum::http::Request::builder()
        .uri("/api/v1/health")
        .header(header::HOST, "evil.example:1234")
        .body(axum::body::Body::empty())
        .unwrap();
    assert_eq!(
        app().oneshot(request).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
}

#[test]
fn login_url_keeps_port() {
    assert_eq!(
        login_url("http://127.0.0.1:1234/", "secret"),
        "http://127.0.0.1:1234/auth?token=secret"
    );
}
#[test]
fn loopback_hosts_only() {
    for host in ["localhost:1234", "127.0.0.1:54321"] {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, host.parse().unwrap());
        assert!(host_ok(&h));
    }
    for host in [
        "example.com:1234",
        "127.0.0.1:0",
        "localhost",
        "localhost:1234.evil.com",
        "[::1]:1234",
    ] {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, host.parse().unwrap());
        assert!(!host_ok(&h));
    }
}
