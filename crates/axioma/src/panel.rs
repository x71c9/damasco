//! The control panel: the single-page app built from `panel/` and its API.
//!
//! The API answers JSON under `/api/`. Every route except the ones needed to
//! sign in requires a session cookie. Requests that change state accept only
//! `application/json` bodies and the cookie is `SameSite=Strict`, so another
//! site cannot submit them on behalf of a signed-in person.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Extension, Json, Router,
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::{Config, Proxy};
use crate::db;
use crate::limit::{self, Attempts};
use crate::people::{self, PeopleError, Purpose, Redeem};
use crate::session::{self, Person};

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

/// Wrong logins allowed per address and per login in the window.
const LOGIN_ATTEMPTS: usize = 10;
const LOGIN_WINDOW: Duration = Duration::from_secs(15 * 60);

#[derive(Clone)]
struct PanelState {
    db: db::Handle,
    installation: Arc<Installation>,
    proxy: Arc<Proxy>,
    attempts: Arc<Attempts>,
    /// HTTPS in front: the cookie gets `Secure` and the `__Host-` prefix.
    secure: bool,
}

impl PanelState {
    fn cookie_name(&self) -> &'static str {
        if self.secure {
            "__Host-axioma_session"
        } else {
            "axioma_session"
        }
    }

    fn session_cookie(&self, token: &str) -> HeaderValue {
        let secure = if self.secure { "; Secure" } else { "" };
        let value = format!(
            "{}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age=2592000{secure}",
            self.cookie_name()
        );
        HeaderValue::from_str(&value).expect("cookie value")
    }

    fn cleared_cookie(&self) -> HeaderValue {
        let secure = if self.secure { "; Secure" } else { "" };
        let value = format!(
            "{}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{secure}",
            self.cookie_name()
        );
        HeaderValue::from_str(&value).expect("cookie value")
    }

    fn token(&self, headers: &HeaderMap) -> Option<String> {
        let name = self.cookie_name();
        headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(';'))
            .filter_map(|pair| pair.trim().split_once('='))
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.to_string())
    }
}

pub fn router(config: &Config, db: db::Handle) -> Router {
    let state = PanelState {
        db,
        installation: Arc::new(Installation {
            preview_host: config.hosts.preview.clone(),
            site_hosts: config.hosts.site.clone(),
        }),
        proxy: Arc::new(config.proxy.clone()),
        attempts: Arc::new(Attempts::new(LOGIN_ATTEMPTS, LOGIN_WINDOW)),
        secure: config.hosts.scheme == "https",
    };
    let private = Router::new()
        .route("/api/installation", get(installation))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_session,
        ));
    Router::new()
        .route("/api/auth/link", post(link))
        .route("/api/auth/setup", post(setup))
        .route("/api/auth/login", post(login))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/me", get(me))
        .merge(private)
        .route(
            "/api/{*rest}",
            any(|| async { error(StatusCode::NOT_FOUND, "no such API") }),
        )
        .with_state(state)
        .fallback(asset)
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

fn people_error(err: PeopleError) -> Response {
    match err {
        PeopleError::InvalidLink => error(StatusCode::GONE, err.to_string()),
        PeopleError::LoginTaken => error(StatusCode::CONFLICT, err.to_string()),
        PeopleError::WrongCredentials => error(StatusCode::UNAUTHORIZED, err.to_string()),
        PeopleError::Invalid(_) => error(StatusCode::BAD_REQUEST, err.to_string()),
        PeopleError::Database(_) | PeopleError::Hash(_) => {
            eprintln!("axioma: {err}");
            error(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

/// Requires a valid session and makes the person available to handlers.
async fn require_session(
    State(state): State<PanelState>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(token) = state.token(request.headers()) else {
        return error(StatusCode::UNAUTHORIZED, "sign in first");
    };
    match state
        .db
        .run(move |conn| session::lookup(conn, &token))
        .await
    {
        Ok(Some(person)) => {
            request.extensions_mut().insert(person);
            next.run(request).await
        }
        Ok(None) => error(StatusCode::UNAUTHORIZED, "sign in first"),
        Err(err) => people_error(err.into()),
    }
}

async fn installation(State(state): State<PanelState>) -> Json<Installation> {
    Json(state.installation.as_ref().clone())
}

#[derive(Deserialize)]
struct LinkBody {
    token: String,
}

/// What a one-time link is for, so the panel shows the right form.
async fn link(State(state): State<PanelState>, Json(body): Json<LinkBody>) -> Response {
    match state
        .db
        .run(move |conn| people::find_link(conn, &body.token))
        .await
    {
        Ok(found) => {
            let purpose = match found.purpose {
                Purpose::Setup => "setup",
                Purpose::Reset => "reset",
            };
            Json(json!({ "purpose": purpose })).into_response()
        }
        Err(err) => people_error(err),
    }
}

#[derive(Deserialize)]
struct SetupBody {
    token: String,
    login: Option<String>,
    name: Option<String>,
    password: String,
}

/// Redeems a one-time link and signs the person in.
async fn setup(State(state): State<PanelState>, Json(body): Json<SetupBody>) -> Response {
    let result = state
        .db
        .run(move |conn| {
            let input = Redeem {
                login: body.login.as_deref(),
                name: body.name.as_deref(),
                password: &body.password,
            };
            let person = people::redeem(conn, &body.token, input)?;
            let token = session::create(conn, person)?;
            let me = session::lookup(conn, &token)?.ok_or(PeopleError::InvalidLink)?;
            Ok::<_, PeopleError>((token, me))
        })
        .await;
    match result {
        Ok((token, me)) => signed_in(&state, &token, me),
        Err(err) => people_error(err),
    }
}

fn signed_in(state: &PanelState, token: &str, me: Person) -> Response {
    let mut response = Json(me).into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, state.session_cookie(token));
    response
}

#[derive(Deserialize)]
struct LoginBody {
    login: String,
    password: String,
}

async fn login(
    State(state): State<PanelState>,
    connect: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    Json(body): Json<LoginBody>,
) -> Response {
    let peer = connect.map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |c| c.0.0.ip());
    let address = limit::address_key(limit::client_ip(peer, &headers, &state.proxy));
    let account = format!("login:{}", body.login.trim().to_lowercase());
    if !state.attempts.allowed(&address) || !state.attempts.allowed(&account) {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "too many attempts; try again in a few minutes",
        );
    }
    let result = state
        .db
        .run(move |conn| {
            let token = session::login(conn, &body.login, &body.password)?;
            let me = session::lookup(conn, &token)?.ok_or(PeopleError::WrongCredentials)?;
            Ok::<_, PeopleError>((token, me))
        })
        .await;
    match result {
        Ok((token, me)) => {
            state.attempts.clear(&address);
            state.attempts.clear(&account);
            signed_in(&state, &token, me)
        }
        Err(err) => {
            if matches!(err, PeopleError::WrongCredentials) {
                state.attempts.failed(&address);
                state.attempts.failed(&account);
            }
            people_error(err)
        }
    }
}

async fn logout(State(state): State<PanelState>, headers: HeaderMap) -> Response {
    if let Some(token) = state.token(&headers)
        && let Err(err) = state
            .db
            .run(move |conn| session::delete(conn, &token))
            .await
    {
        return people_error(err.into());
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, state.cleared_cookie());
    response
}

async fn me(State(state): State<PanelState>, headers: HeaderMap) -> Response {
    let Some(token) = state.token(&headers) else {
        return error(StatusCode::UNAUTHORIZED, "sign in first");
    };
    match state
        .db
        .run(move |conn| session::lookup(conn, &token))
        .await
    {
        Ok(Some(person)) => Json(person).into_response(),
        Ok(None) => error(StatusCode::UNAUTHORIZED, "sign in first"),
        Err(err) => people_error(err.into()),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use tower::ServiceExt;

    fn config() -> Config {
        Config::parse(
            r#"
            listen = "127.0.0.1:4100"
            data_dir = "/var/lib/axioma"
            [hosts]
            panel = "panel.example.com"
            preview = "preview.example.com"
            site = ["example.com"]
            "#,
        )
        .unwrap()
    }

    /// A panel with an owner link ready to be redeemed.
    fn panel() -> (Router, String) {
        let conn = db::memory();
        let (token, _) = people::owner_link(&conn).unwrap();
        (router(&config(), db::Handle::new(conn)), token)
    }

    struct Reply {
        status: StatusCode,
        cookie: Option<String>,
        body: serde_json::Value,
    }

    async fn send(
        app: &Router,
        method: &str,
        path: &str,
        cookie: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> Reply {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        let request = match body {
            Some(body) => request
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string())),
            None => request.body(Body::empty()),
        }
        .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .map(|v| v.to_str().unwrap().to_string());
        let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        Reply {
            status,
            cookie,
            body,
        }
    }

    /// `name=value` from a Set-Cookie header.
    fn pair(set_cookie: &str) -> String {
        set_cookie.split(';').next().unwrap().to_string()
    }

    async fn sign_up(app: &Router, token: &str) -> String {
        let reply = send(
            app,
            "POST",
            "/api/auth/setup",
            None,
            Some(json!({ "token": token, "login": "giulia", "name": "Giulia", "password": "correct horse battery" })),
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        assert_eq!(reply.body["role"], "owner");
        pair(&reply.cookie.unwrap())
    }

    #[tokio::test]
    async fn owner_signs_up_through_the_link() {
        let (app, token) = panel();
        let link = send(
            &app,
            "POST",
            "/api/auth/link",
            None,
            Some(json!({ "token": token })),
        )
        .await;
        assert_eq!(link.body["purpose"], "setup");
        let cookie = sign_up(&app, &token).await;
        let me = send(&app, "GET", "/api/auth/me", Some(&cookie), None).await;
        assert_eq!(me.body["login"], "giulia");
        let used = send(
            &app,
            "POST",
            "/api/auth/link",
            None,
            Some(json!({ "token": token })),
        )
        .await;
        assert_eq!(used.status, StatusCode::GONE);
    }

    #[tokio::test]
    async fn the_session_cookie_is_strict_and_http_only() {
        let (app, token) = panel();
        let reply = send(
            &app,
            "POST",
            "/api/auth/setup",
            None,
            Some(json!({ "token": token, "login": "giulia", "name": "Giulia", "password": "correct horse battery" })),
        )
        .await;
        let cookie = reply.cookie.unwrap();
        assert!(cookie.starts_with("__Host-axioma_session="), "{cookie}");
        for attribute in ["HttpOnly", "SameSite=Strict", "Secure", "Path=/"] {
            assert!(cookie.contains(attribute), "{cookie} lacks {attribute}");
        }
    }

    #[tokio::test]
    async fn private_api_needs_a_session() {
        let (app, token) = panel();
        let anonymous = send(&app, "GET", "/api/installation", None, None).await;
        assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
        let cookie = sign_up(&app, &token).await;
        let signed_in = send(&app, "GET", "/api/installation", Some(&cookie), None).await;
        assert_eq!(signed_in.body["preview_host"], "preview.example.com");
    }

    #[tokio::test]
    async fn logout_ends_the_session() {
        let (app, token) = panel();
        let cookie = sign_up(&app, &token).await;
        let out = send(&app, "POST", "/api/auth/logout", Some(&cookie), None).await;
        assert_eq!(out.status, StatusCode::NO_CONTENT);
        assert!(out.cookie.unwrap().contains("Max-Age=0"));
        let me = send(&app, "GET", "/api/auth/me", Some(&cookie), None).await;
        assert_eq!(me.status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn login_works_and_repeated_failures_are_blocked() {
        let (app, token) = panel();
        sign_up(&app, &token).await;
        let ok = send(
            &app,
            "POST",
            "/api/auth/login",
            None,
            Some(json!({ "login": "giulia", "password": "correct horse battery" })),
        )
        .await;
        assert_eq!(ok.status, StatusCode::OK);
        for _ in 0..LOGIN_ATTEMPTS {
            let wrong = send(
                &app,
                "POST",
                "/api/auth/login",
                None,
                Some(json!({ "login": "giulia", "password": "wrong password" })),
            )
            .await;
            assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
        }
        let blocked = send(
            &app,
            "POST",
            "/api/auth/login",
            None,
            Some(json!({ "login": "giulia", "password": "correct horse battery" })),
        )
        .await;
        assert_eq!(blocked.status, StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn state_changes_need_a_json_body() {
        let (app, _) = panel();
        let request = Request::post("/api/auth/login")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from("login=giulia&password=x"))
            .unwrap();
        let status = app.oneshot(request).await.unwrap().status();
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
}
