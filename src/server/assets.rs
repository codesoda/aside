use axum::{
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

use super::ApiError;

#[derive(RustEmbed)]
#[folder = "app/dist/"]
struct Assets;

pub(super) async fn static_asset(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path
        .split('/')
        .any(|part| part == ".." || part.starts_with('.'))
    {
        return ApiError::missing().into_response();
    }
    if let Some(file) = Assets::get(path).filter(|_| !path.is_empty()) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        let cache = if path.starts_with("assets/") {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };
        return (
            [
                (header::CONTENT_TYPE, mime.as_ref().to_string()),
                (header::CACHE_CONTROL, cache.into()),
            ],
            file.data,
        )
            .into_response();
    }
    match Assets::get("index.html") {
        Some(index) => (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
                (header::CACHE_CONTROL, "no-cache".into()),
            ],
            index.data,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "frontend not embedded").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use axum::{body::to_bytes, http::StatusCode};

    use super::static_asset;

    #[tokio::test]
    async fn embedded_index_is_served_and_dot_paths_are_rejected() {
        let response = static_asset("/".parse().unwrap()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1_000_000).await.unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(
            text.to_ascii_lowercase().contains("<!doctype html"),
            "embedded document missing: {text}"
        );
        assert!(
            !text.contains("frontend not embedded"),
            "index fallback must be the embedded document"
        );

        for path in ["/../Cargo.toml", "/.git/config", "/assets/../index.html"] {
            let denied = static_asset(path.parse().unwrap()).await;
            assert_eq!(denied.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }
}
