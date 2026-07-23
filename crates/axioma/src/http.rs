//! HTTP entry point: every request is dispatched by hostname to the panel,
//! the preview or the published site.

use std::sync::Arc;

use axum::{
    Router,
    extract::{Request, State},
    http::{StatusCode, Uri, header, uri::Authority},
    response::{IntoResponse, Response},
    routing::get,
};
use tower::ServiceExt;

use crate::config::{Config, Role, normalize_host};

struct Apps {
    config: Config,
    panel: Router,
    preview: Router,
    site: Router,
}

/// Builds the router for the whole installation.
pub fn router(config: Config) -> Router {
    let apps = Arc::new(Apps {
        config,
        panel: Router::new().fallback(|| placeholder("panel")),
        preview: Router::new().fallback(|| placeholder("preview")),
        site: Router::new().fallback(site),
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
        placeholder("site").await
    }
}

/// Temporary page until each part has its real content.
async fn placeholder(name: &'static str) -> Response {
    format!("axioma {name}\n").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};

    fn app() -> Router {
        router(
            Config::parse(
                r#"
                listen = "127.0.0.1:4100"
                [hosts]
                panel = "panel.example.com"
                preview = "preview.example.com"
                site = ["example.com", "www.example.com"]
                "#,
            )
            .unwrap(),
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
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn dispatches_by_host() {
        assert_eq!(
            call(Some("panel.example.com"), "/").await.1,
            "axioma panel\n"
        );
        assert_eq!(
            call(Some("preview.example.com"), "/").await.1,
            "axioma preview\n"
        );
        assert_eq!(call(Some("example.com"), "/").await.1, "axioma site\n");
        assert_eq!(
            call(Some("www.example.com"), "/about").await.1,
            "axioma site\n"
        );
    }

    #[tokio::test]
    async fn ignores_port_and_case() {
        let (status, body) = call(Some("Panel.Example.COM:4100"), "/").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "axioma panel\n");
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
