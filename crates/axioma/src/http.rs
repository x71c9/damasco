//! HTTP entry point: every request is dispatched by hostname to the panel,
//! the preview or the published site.

use std::sync::Arc;

use axum::{
    Router,
    extract::{Request, State},
    http::{StatusCode, Uri, header, uri::Authority},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use tower::ServiceExt;

use crate::build::Builder;
use crate::config::{Config, Role, normalize_host};
use crate::db;
use crate::panel;

struct Apps {
    config: Config,
    panel: Router,
    preview: Router,
    site: Router,
}

/// Builds the router for the whole installation.
pub fn router(
    config: Config,
    db: db::Handle,
    builder: Arc<Builder>,
    account: Arc<crate::agent::Account>,
    chat: Arc<crate::chat::Chat>,
) -> Router {
    let releases = Arc::new(config.data_dir.join("releases"));
    let apps = Arc::new(Apps {
        panel: panel::router(&config, db, builder, account, chat),
        preview: Router::new().fallback(preview).with_state(releases),
        site: Router::new().fallback(site),
        config,
    });
    Router::new()
        .route("/_axioma/health", get(health))
        .fallback(dispatch)
        .with_state(apps)
}

/// Answers on every hostname, for local health checks.
async fn health() -> &'static str {
    "ok"
}

async fn dispatch(State(apps): State<Arc<Apps>>, request: Request) -> Response {
    let role = request_host(&request).and_then(|host| apps.config.role_of(&host));
    let app = match role {
        Some(Role::Panel) => apps.panel.clone(),
        Some(Role::Preview) => apps.preview.clone(),
        Some(Role::Site) => apps.site.clone(),
        None => return (StatusCode::MISDIRECTED_REQUEST, "unknown host\n").into_response(),
    };
    match app.oneshot(request).await {
        Ok(response) => response,
        Err(never) => match never {},
    }
}

/// Hostname of a request: the URI authority (HTTP/2) or the `Host` header
/// (HTTP/1.1), lowercased and without port.
fn request_host(request: &Request) -> Option<String> {
    let authority = match request.uri().authority() {
        Some(authority) => authority.clone(),
        None => request
            .headers()
            .get(header::HOST)?
            .to_str()
            .ok()?
            .parse::<Authority>()
            .ok()?,
    };
    normalize_host("host", authority.host()).ok()
}

/// Published site. `/api/*` and `/_axioma/*` are reserved for axioma and
/// never served from the site's files.
async fn site(uri: Uri) -> Response {
    let path = uri.path();
    let reserved = path == "/api" || path.starts_with("/api/") || path.starts_with("/_axioma/");
    if reserved {
        StatusCode::NOT_FOUND.into_response()
    } else {
        "axioma site\n".into_response()
    }
}

/// The latest build of the site, or the empty-site page before the first one.
/// Access to the preview is not restricted yet: that comes with the preview
/// sessions of stage 6.
async fn preview(State(releases): State<Arc<std::path::PathBuf>>, request: Request) -> Response {
    let root = releases.join("preview");
    let mut response = if !root.exists() {
        empty_site().await.into_response()
    } else {
        let serve =
            tower_http::services::ServeDir::new(root).append_index_html_on_directories(true);
        match serve.oneshot(request).await {
            Ok(response) => response.map(axum::body::Body::new),
            Err(never) => match never {},
        }
    };
    // Every build replaces the files at the same addresses: the browser asks
    // again each time instead of reusing a copy it guesses to be fresh.
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-cache"),
    );
    response
}

/// Page shown by the preview until the site has its first build.
async fn empty_site() -> Html<&'static str> {
    Html(include_str!("empty_site.html"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};

    fn app() -> Router {
        let db = crate::db::Handle::new(crate::db::memory());
        let chat = crate::chat::idle(db.clone());
        router(
            Config::parse(
                r#"
                listen = "127.0.0.1:4100"
                data_dir = "/var/lib/axioma"
                [hosts]
                panel = "panel.example.com"
                preview = "preview.example.com"
                site = ["example.com", "www.example.com"]
                "#,
            )
            .unwrap(),
            db,
            crate::build::idle(std::path::Path::new("/var/lib/axioma")),
            crate::agent::idle(),
            chat,
        )
    }

    async fn call(host: Option<&str>, path: &str) -> (StatusCode, String) {
        let mut request = Request::get(path);
        if let Some(host) = host {
            request = request.header(header::HOST, host);
        }
        let response = app()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    #[tokio::test]
    async fn the_preview_is_always_checked_again() {
        let response = app()
            .oneshot(
                Request::get("/")
                    .header(header::HOST, "preview.example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-cache");
    }

    #[tokio::test]
    async fn dispatches_by_host() {
        assert_eq!(
            call(Some("panel.example.com"), "/api/auth/me").await,
            (
                StatusCode::UNAUTHORIZED,
                r#"{"error":"sign in first"}"#.into()
            )
        );
        assert!(
            call(Some("preview.example.com"), "/")
                .await
                .1
                .contains("This site is empty")
        );
        assert_eq!(call(Some("example.com"), "/").await.1, "axioma site\n");
        assert_eq!(
            call(Some("www.example.com"), "/about").await.1,
            "axioma site\n"
        );
    }

    #[tokio::test]
    async fn ignores_port_and_case() {
        let (status, _) = call(Some("Panel.Example.COM:4100"), "/api/auth/me").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn panel_api_unknown_route_is_not_found() {
        assert_eq!(
            call(Some("panel.example.com"), "/api/nothing").await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(Some("panel.example.com"), "/assets/missing.js")
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn unknown_or_missing_host_is_misdirected() {
        assert_eq!(
            call(Some("other.example.com"), "/").await.0,
            StatusCode::MISDIRECTED_REQUEST
        );
        assert_eq!(call(None, "/").await.0, StatusCode::MISDIRECTED_REQUEST);
    }

    #[tokio::test]
    async fn site_reserves_api_and_axioma_paths() {
        for path in ["/api", "/api/orders", "/_axioma/anything"] {
            assert_eq!(
                call(Some("example.com"), path).await.0,
                StatusCode::NOT_FOUND,
                "{path}"
            );
        }
        assert_eq!(call(Some("example.com"), "/apiary").await.0, StatusCode::OK);
    }

    #[tokio::test]
    async fn health_answers_on_any_host() {
        assert_eq!(
            call(None, "/_axioma/health").await,
            (StatusCode::OK, "ok".into())
        );
    }
}
