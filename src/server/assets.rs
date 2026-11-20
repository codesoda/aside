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
