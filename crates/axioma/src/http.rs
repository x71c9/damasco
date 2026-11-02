//! HTTP entry point: every request is dispatched by hostname to the panel,
//! the preview or the published site.

use std::sync::Arc;

use axum::{
    Router,
    extract::{Request, State},
    http::{StatusCode, header, uri::Authority},
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
    let preview_state = Arc::new(Preview {
        releases: config.data_dir.join("releases"),
        db: db.clone(),
        secure: config.hosts.scheme == "https",
        // Browsers keep Secure cookies of localhost names without HTTPS.
        local: config.hosts.preview == "localhost" || config.hosts.preview.ends_with(".localhost"),
    });
    let apps = Arc::new(Apps {
        panel: panel::router(&config, db, builder, account, chat),
        preview: Router::new().fallback(preview).with_state(preview_state),
        site: Router::new().fallback(site).with_state(releases),
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

/// Published site: the release `releases/live` points to. `/api/*` and
/// `/_axioma/*` are reserved for axioma; of these only the pictures,
/// `/_axioma/media/*`, come from the release.
async fn site(State(releases): State<Arc<std::path::PathBuf>>, request: Request) -> Response {
    let path = request.uri().path();
    let reserved = path == "/api"
        || path.starts_with("/api/")
        || (path.starts_with("/_axioma/") && !path.starts_with("/_axioma/media/"));
    if reserved {
        return StatusCode::NOT_FOUND.into_response();
    }
    serve_release(
        &releases.join("live"),
        request,
        "This site is not published yet",
        "It appears here once it is published from the panel.",
    )
    .await
}

/// What the preview's host needs: its releases, and the sessions that
/// open it.
struct Preview {
    releases: std::path::PathBuf,
    db: db::Handle,
    secure: bool,
    /// The preview's host is a localhost name, as in development.
    local: bool,
}

impl Preview {
    fn cookie_name(&self) -> &'static str {
        if self.secure {
            "__Host-axioma_preview"
        } else {
            "axioma_preview"
        }
    }
}

/// The latest build of the site, or the empty-site page before the first
/// one, for people of the panel only: `/_axioma/enter?code=…` exchanges a
/// code of the panel for a session of the preview, in its own cookie.
async fn preview(State(state): State<Arc<Preview>>, request: Request) -> Response {
    if request.uri().path() == "/_axioma/enter" {
        let query = request.uri().query().unwrap_or_default().to_string();
        return enter(&state, &query).await;
    }
    let session = request
        .headers()
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, _)| *key == state.cookie_name())
        .map(|(_, value)| value.to_string());
    let allowed = match session {
        Some(session) => state
            .db
            .run(move |conn| crate::preview::valid(conn, &session))
            .await
            .unwrap_or(false),
        None => false,
    };
    if !allowed {
        let mut response = (
            StatusCode::UNAUTHORIZED,
            empty_site(
                "This preview is private",
                "Open it from the panel of the site: it shows changes that are not online yet.",
            ),
        )
            .into_response();
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-store"),
        );
        return response;
    }
    serve_release(
        &state.releases.join("preview"),
        request,
        "This site is empty",
        "The preview shows the site as soon as the assistant builds its first version.",
    )
    .await
}

/// Exchanges a code of the panel for a session of the preview, then goes
/// to the page asked for.
async fn enter(state: &Preview, query: &str) -> Response {
    let param = |name: &str| {
        query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .find(|(key, _)| *key == name)
            .map(|(_, value)| percent_decode(value))
    };
    let to = crate::preview::safe_path(param("to").as_deref());
    let Some(code) = param("code") else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let session = state
        .db
        .run(move |conn| crate::preview::redeem(conn, &code))
        .await
        .ok()
        .flatten();
    let Some(session) = session else {
        return (
            StatusCode::FORBIDDEN,
            empty_site(
                "This link has expired",
                "Links to the preview work once, for a minute. Open the preview again from the panel.",
            ),
        )
            .into_response();
    };
    // The preview opens inside the panel, another host: a partitioned
    // cookie reaches it there even where third-party cookies are blocked,
    // kept apart from any other site that would frame it. It needs Secure,
    // which plain HTTP gets only on localhost names; elsewhere without
    // HTTPS, Lax, for a panel and a preview on the same domain.
    let attributes = if state.secure || state.local {
        "SameSite=None; Secure; Partitioned"
    } else {
        "SameSite=Lax"
    };
    let cookie = format!(
        "{}={session}; Path=/; HttpOnly; Max-Age=43200; {attributes}",
        state.cookie_name()
    );
    let mut response = StatusCode::SEE_OTHER.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::SET_COOKIE,
        header::HeaderValue::from_str(&cookie).expect("cookie"),
    );
    headers.insert(
        header::LOCATION,
        header::HeaderValue::from_str(&to).unwrap_or(header::HeaderValue::from_static("/")),
    );
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

/// The files of a release, with the site's own 404 page when it has one;
/// a page with `title` and `text` when there is no release yet.
async fn serve_release(
    root: &std::path::Path,
    request: Request,
    title: &str,
    text: &str,
) -> Response {
    let mut response = if !root.exists() {
        empty_site(title, text).into_response()
    } else {
        let missing = root.join("404.html");
        let serve =
            tower_http::services::ServeDir::new(root).append_index_html_on_directories(true);
        let served = if missing.is_file() {
            serve
                .not_found_service(tower_http::services::ServeFile::new(missing))
                .oneshot(request)
                .await
        } else {
            serve.oneshot(request).await
        };
        match served {
            Ok(response) => response.map(axum::body::Body::new),
            Err(never) => match never {},
        }
    };
    // Every build and every publication replaces the files at the same
    // addresses: the browser asks again each time instead of reusing a copy
    // it guesses to be fresh.
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-cache"),
    );
    response
}

/// A value of a query string, with its %XX escapes decoded.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                (Some(high), Some(low)) => {
                    out.push((high * 16 + low) as u8);
                    i += 3;
                    continue;
                }
                _ => out.push(b'%'),
            },
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Page shown while there is no release to serve.
fn empty_site(title: &str, text: &str) -> Html<String> {
    Html(
        include_str!("empty_site.html")
            .replace("{title}", title)
            .replace("{text}", text),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};

    fn app() -> Router {
        app_with(crate::db::Handle::new(crate::db::memory()))
    }

    fn app_with(db: crate::db::Handle) -> Router {
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
    async fn the_preview_opens_with_a_code_of_the_panel() {
        let conn = crate::db::memory();
        conn.execute(
            "INSERT INTO person (id, email, name, role) VALUES (1, 'a@example.com', 'Ada', 'owner')",
            [],
        )
        .unwrap();
        let code = crate::preview::code(&conn, 1).unwrap();
        let app = app_with(crate::db::Handle::new(conn));
        let get = |path: &str, cookie: Option<&str>| {
            let mut request = Request::get(path).header(header::HOST, "preview.example.com");
            if let Some(cookie) = cookie {
                request = request.header(header::COOKIE, cookie);
            }
            app.clone().oneshot(request.body(Body::empty()).unwrap())
        };
        // Without a session: private, never kept by the browser.
        let closed = get("/", None).await.unwrap();
        assert_eq!(closed.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(closed.headers()[header::CACHE_CONTROL], "no-store");

        let entered = get(
            &format!("/_axioma/enter?code={code}&to=%2Fprojects%2F"),
            None,
        )
        .await
        .unwrap();
        assert_eq!(entered.status(), StatusCode::SEE_OTHER);
        assert_eq!(entered.headers()[header::LOCATION], "/projects/");
        let cookie = entered.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .to_string();
        // HTTPS in front: the cookie is Secure and bound to the host.
        assert!(
            cookie.starts_with("__Host-axioma_preview=")
                && cookie.contains("HttpOnly")
                && cookie.contains("Secure")
        );
        let session = cookie.split(';').next().unwrap();
        let open = get("/", Some(session)).await.unwrap();
        assert_eq!(open.status(), StatusCode::OK);
        assert_eq!(open.headers()[header::CACHE_CONTROL], "no-cache");
        let body = to_bytes(open.into_body(), 1 << 20).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("This site is empty"));
        // The code works once.
        let again = get(&format!("/_axioma/enter?code={code}"), None)
            .await
            .unwrap();
        assert_eq!(again.status(), StatusCode::FORBIDDEN);
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
                .contains("This preview is private")
        );
        assert!(
            call(Some("example.com"), "/")
                .await
                .1
                .contains("This site is not published yet")
        );
        assert!(
            call(Some("www.example.com"), "/about")
                .await
                .1
                .contains("not published yet")
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
