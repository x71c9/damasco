//! The control panel: the single-page app built from `panel/` and its API.

use std::path::Path;

use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::get,
};
use rust_embed::RustEmbed;
use serde::Serialize;

use crate::config::Config;

/// Build output of `panel/`. Debug builds read it from disk at runtime, so a
/// new `npm run build` shows up without recompiling; release builds embed it.
#[derive(RustEmbed)]
#[folder = "../../panel/dist"]
#[allow_missing = true]
struct Assets;

/// Public facts about the installation, needed by the panel to address the
/// preview and the site.
#[derive(Serialize, Clone)]
struct Installation {
    preview_host: String,
    site_hosts: Vec<String>,
}

pub fn router(config: &Config) -> Router {
    let installation = std::sync::Arc::new(Installation {
        preview_host: config.hosts.preview.clone(),
        site_hosts: config.hosts.site.clone(),
    });
    Router::new()
        .route("/api/installation", get(installation_handler))
        .route("/api/{*rest}", get(|| async { StatusCode::NOT_FOUND }))
        .with_state(installation)
        .fallback(asset)
}

async fn installation_handler(
    State(installation): State<std::sync::Arc<Installation>>,
) -> Json<Installation> {
    Json(installation.as_ref().clone())
}

/// Serves a file of the panel, or `index.html` for the app's own routes.
async fn asset(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if let Some(response) = file(path) {
        return response;
    }
    if Path::new(path).extension().is_some() {
        return StatusCode::NOT_FOUND.into_response();
    }
    file("index.html").unwrap_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "The panel is not built. Run `npm run build` in panel/.\n",
        )
            .into_response()
    })
}

fn file(path: &str) -> Option<Response> {
    let file = Assets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    // Built assets have content hashes in their names and never change.
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    Some(
        (
            [
                (header::CONTENT_TYPE, mime.as_ref()),
                (header::CACHE_CONTROL, cache),
            ],
            Body::from(file.data),
        )
            .into_response(),
    )
}
