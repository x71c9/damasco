//! HTTP entry point: every request is dispatched by hostname, through the
//! table of addresses (`resolver`), to the panel, the preview of a site or a
//! published site.

use std::sync::Arc;

use axum::{
    Router,
    extract::{Request, State},
    http::{StatusCode, header, uri::Authority},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use tower::ServiceExt;

use crate::config::{Config, normalize_host};
use crate::db;
use crate::panel;
use crate::registry::{Registry, SiteError};
use crate::resolver::{self, Surface};

struct Apps {
    config: Config,
    panel: Router,
    registry: Arc<Registry>,
    resolver: resolver::Shared,
    /// HTTPS in front: the preview's cookie gets `Secure` and `__Host-`.
    secure: bool,
    /// `frame-ancestors` naming the panel, the only page that may frame a
    /// preview.
    frame_ancestors: header::HeaderValue,
    /// The answer of `/_alogeno/health`, naming the installation.
    health: String,
}

/// Builds the router for the whole installation. `db` is the installation's
/// database; each site's own comes from `registry`.
pub fn router(
    config: Config,
    db: db::Handle,
    registry: Arc<Registry>,
    resolver: resolver::Shared,
    account: Arc<crate::agent::Account>,
    slot: Arc<crate::gate::AgentSlot>,
    instance_id: &str,
) -> Router {
    let apps = Arc::new(Apps {
        health: crate::domains::health_body(instance_id),
        panel: panel::router(&config, db, registry.clone(), account, slot),
        registry,
        resolver,
        secure: config.hosts.scheme == "https",
        frame_ancestors: header::HeaderValue::from_str(&format!(
            "frame-ancestors {}",
            config.panel_url()
        ))
        .expect("hostnames and ports are valid header characters"),
        config,
    });
    Router::new()
        .route("/_alogeno/health", get(health))
        .fallback(dispatch)
        .with_state(apps)
}

/// Answers on every hostname with the id of the installation: local health
/// checks, and the panel's check of a domain, which tells this installation
/// from any other server.
async fn health(State(apps): State<Arc<Apps>>) -> String {
    apps.health.clone()
}

async fn dispatch(State(apps): State<Arc<Apps>>, request: Request) -> Response {
    let surface =
        request_host(&request).and_then(|host| resolver::current(&apps.resolver).resolve(&host));
    match surface {
        Some(Surface::Panel) => match apps.panel.clone().oneshot(request).await {
            Ok(response) => response,
            Err(never) => match never {},
        },
        Some(Surface::Preview(id)) => match apps.registry.get(&id).await {
            Ok(runtime) => {
                let state = Preview {
                    releases: runtime.root.join("releases"),
                    db: runtime.db.clone(),
                    secure: apps.secure,
                    // Browsers keep Secure cookies of localhost names
                    // without HTTPS.
                    local: apps.config.local(),
                    frame_ancestors: apps.frame_ancestors.clone(),
                };
                preview(&state, request).await
            }
            Err(SiteError::NotFound) => {
                (StatusCode::MISDIRECTED_REQUEST, "unknown host\n").into_response()
            }
            Err(SiteError::Unavailable(problem)) => {
                eprintln!("alogeno: site {id} unavailable: {problem}");
                (StatusCode::SERVICE_UNAVAILABLE, "this site cannot start\n").into_response()
            }
        },
        // A published site is files on the disk: nothing to start.
        Some(Surface::Live(id)) => {
            let releases = crate::sites::root(&apps.config.data_dir, &id).join("releases");
            site(&releases, request).await
        }
        // The www added with a domain: the same path on the domain.
        Some(Surface::Redirect(target)) => {
            let path = request
                .uri()
                .path_and_query()
                .map_or("/", |path| path.as_str());
            let location = format!("{}{path}", apps.config.url_of(&target));
            match header::HeaderValue::from_str(&location) {
                Ok(location) => (
                    StatusCode::PERMANENT_REDIRECT,
                    [(header::LOCATION, location)],
                )
                    .into_response(),
                Err(_) => StatusCode::BAD_REQUEST.into_response(),
            }
        }
        None => (StatusCode::MISDIRECTED_REQUEST, "unknown host\n").into_response(),
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
/// `/_alogeno/*` are reserved for alogeno; of these only the pictures,
/// `/_alogeno/media/*`, come from the release.
async fn site(releases: &std::path::Path, request: Request) -> Response {
    let path = request.uri().path();
    let reserved = path == "/api"
        || path.starts_with("/api/")
        || (path.starts_with("/_alogeno/") && !path.starts_with("/_alogeno/media/"));
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
    /// The panel is a localhost name, as in development.
    local: bool,
    /// `frame-ancestors` naming the panel, the only page that may frame
    /// the preview.
    frame_ancestors: header::HeaderValue,
}

impl Preview {
    fn cookie_name(&self) -> &'static str {
        if self.secure {
            "__Host-alogeno_preview"
        } else {
            "alogeno_preview"
        }
    }
}

/// The preview, which only the panel may show in a frame.
async fn preview(state: &Preview, request: Request) -> Response {
    let mut response = preview_page(state, request).await;
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        state.frame_ancestors.clone(),
    );
    response
}

/// The latest build of the site, or the empty-site page before the first
/// one, for people of the panel only: `/_alogeno/enter?code=…` exchanges a
/// code of the panel for a session of the preview, in its own cookie.
async fn preview_page(state: &Preview, request: Request) -> Response {
    if request.uri().path() == "/_alogeno/enter" {
        let query = request.uri().query().unwrap_or_default().to_string();
        return enter(state, &query).await;
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
        app_with(crate::db::Handle::new(crate::db::memory_site()))
    }

    /// The router of an installation whose one site, `main`, has `site_db`.
    /// The panel is a localhost name, so that the published site has an
    /// address (`main.localhost`) before domains exist; the scheme stays
    /// https.
    fn app_with(site_db: crate::db::Handle) -> Router {
        let config = Config::parse(
            r#"
            listen = "127.0.0.1:4100"
            data_dir = "/var/lib/alogeno"
            [hosts]
            panel = "panel.localhost"
            "#,
        )
        .unwrap();
        let (db, registry, resolver) =
            crate::registry::single(crate::db::memory_installation(), site_db, &config);
        router(
            config,
            db,
            registry,
            resolver,
            crate::agent::idle(),
            crate::gate::AgentSlot::new(),
            "ID",
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
        let conn = crate::db::memory_site();
        let code = crate::preview::code(&conn, 1).unwrap();
        let app = app_with(crate::db::Handle::new(conn));
        let get = |path: &str, cookie: Option<&str>| {
            let mut request =
                Request::get(path).header(header::HOST, "main.preview.panel.localhost");
            if let Some(cookie) = cookie {
                request = request.header(header::COOKIE, cookie);
            }
            app.clone().oneshot(request.body(Body::empty()).unwrap())
        };
        // Without a session: private, never kept by the browser.
        let closed = get("/", None).await.unwrap();
        assert_eq!(closed.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(closed.headers()[header::CACHE_CONTROL], "no-store");
        // Only the panel may show it in a frame.
        assert_eq!(
            closed.headers()[header::CONTENT_SECURITY_POLICY],
            "frame-ancestors https://panel.localhost"
        );

        let entered = get(
            &format!("/_alogeno/enter?code={code}&to=%2Fprojects%2F"),
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
            cookie.starts_with("__Host-alogeno_preview=")
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
        let again = get(&format!("/_alogeno/enter?code={code}"), None)
            .await
            .unwrap();
        assert_eq!(again.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn dispatches_by_host() {
        assert_eq!(
            call(Some("panel.localhost"), "/api/auth/me").await,
            (
                StatusCode::UNAUTHORIZED,
                r#"{"error":"sign in first"}"#.into()
            )
        );
        assert!(
            call(Some("main.preview.panel.localhost"), "/")
                .await
                .1
                .contains("This preview is private")
        );
        assert!(
            call(Some("main.localhost"), "/")
                .await
                .1
                .contains("This site is not published yet")
        );
        assert!(
            call(Some("main.localhost"), "/about")
                .await
                .1
                .contains("not published yet")
        );
    }

    #[tokio::test]
    async fn ignores_port_and_case() {
        let (status, _) = call(Some("Panel.LocalHost:4100"), "/api/auth/me").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn panel_api_unknown_route_is_not_found() {
        assert_eq!(
            call(Some("panel.localhost"), "/api/nothing").await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(Some("panel.localhost"), "/assets/missing.js").await.0,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn unknown_or_missing_host_is_misdirected() {
        assert_eq!(
            call(Some("other.localhost"), "/").await.0,
            StatusCode::MISDIRECTED_REQUEST
        );
        assert_eq!(call(None, "/").await.0, StatusCode::MISDIRECTED_REQUEST);
    }

    #[tokio::test]
    async fn site_reserves_api_and_alogeno_paths() {
        for path in ["/api", "/api/orders", "/_alogeno/anything"] {
            assert_eq!(
                call(Some("main.localhost"), path).await.0,
                StatusCode::NOT_FOUND,
                "{path}"
            );
        }
        assert_eq!(
            call(Some("main.localhost"), "/apiary").await.0,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn health_answers_on_any_host() {
        assert_eq!(
            call(None, "/_alogeno/health").await,
            (StatusCode::OK, "ok ID".into())
        );
    }

    #[tokio::test]
    async fn a_domain_publishes_the_site_and_its_www_redirects_to_it() {
        let config = Config::parse(
            "listen = \"127.0.0.1:4100\"\ndata_dir = \"/var/lib/alogeno\"\n[hosts]\npanel = \"admin.example.com\"\n",
        )
        .unwrap();
        let site_db = crate::db::Handle::new(crate::db::memory_site());
        let (db, registry, resolver) =
            crate::registry::single(crate::db::memory_installation(), site_db, &config);
        registry
            .add_domain("01TESTSITE", "Shop.it", true)
            .await
            .unwrap();
        let app = router(
            config,
            db,
            registry,
            resolver,
            crate::agent::idle(),
            crate::gate::AgentSlot::new(),
            "ID",
        );
        let get = |host: &str, path: &str| {
            app.clone().oneshot(
                Request::get(path)
                    .header(header::HOST, host)
                    .body(Body::empty())
                    .unwrap(),
            )
        };
        let www = get("www.shop.it", "/about?x=1").await.unwrap();
        assert_eq!(www.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(www.headers()[header::LOCATION], "https://shop.it/about?x=1");
        // The site has no release yet: the published site answers, empty.
        let site = get("shop.it", "/").await.unwrap();
        assert_ne!(site.status(), StatusCode::MISDIRECTED_REQUEST);
        assert_eq!(
            get("other.it", "/").await.unwrap().status(),
            StatusCode::MISDIRECTED_REQUEST
        );
    }
}
