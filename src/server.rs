//! Axum server: JSON API under /api/v1 plus the embedded Svelte build.
use std::{net::SocketAddr, sync::Arc};

use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::get,
};
use rust_embed::RustEmbed;
use serde_json::json;

use crate::model::SessionSnapshot;

#[derive(RustEmbed)]
#[folder = "web/dist/"]
struct Assets;

pub type AppState = Arc<SessionSnapshot>;

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/health", get(|| async { Json(json!({ "ok": true })) }))
        .route("/session", get(session))
        .fallback(api_not_found);
    Router::new()
        .nest("/api/v1", api)
        .route("/api", get(api_not_found))
        .route("/api/{*rest}", get(api_not_found))
        .fallback(static_asset)
        .with_state(state)
}

async fn session(State(state): State<AppState>) -> Json<SessionSnapshot> {
    Json((*state).clone())
}

async fn api_not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response()
}

/// Serve a built asset, or fall back to index.html for SPA routes.
async fn static_asset(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
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

pub async fn serve(state: AppState, open_browser: bool) -> anyhow::Result<()> {
    // Port 0: the OS assigns a free port. Callers read the real URL from
    // the `session.started` event on stdout.
    let addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let url = format!("http://{}/", listener.local_addr()?);

    // Machine-readable event for agent callers (CLI-08). Logs go to stderr.
    println!(
        "{}",
        json!({ "event": "session.started", "sessionId": state.session_id, "url": url, "api": format!("{url}api/v1") })
    );
    eprintln!("Aside is running at {url}  (Ctrl-C to stop)");
    if open_browser && let Err(e) = open::that(&url) {
        eprintln!("could not open browser: {e}");
    }

    axum::serve(listener, router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            eprintln!("\nShutting down.");
        })
        .await?;
    Ok(())
}
