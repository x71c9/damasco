//! The control panel: the single-page app built from `panel/` and its API.
//!
//! The API answers JSON under `/api/`. Every route except the ones needed to
//! sign in requires a session cookie. The cookie is `SameSite=Strict`, which
//! keeps other sites out but not the preview, a sibling host that browsers
//! count as the same site: every API request whose `Origin` or
//! `Sec-Fetch-Site` names another origin is refused (`panel_only`), whatever
//! its method or body.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Extension, Json, Router,
    body::Body,
    extract::{
        ConnectInfo, Path, Query, Request, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response,
        sse::{Event as SseEvent, KeepAlive, Sse},
    },
    routing::{any, get, post},
};
use futures_util::{SinkExt, StreamExt, stream};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::broadcast;

use crate::activity;
use crate::assistant::Provider;
use crate::chat::{Event as ChatEvent, SendError};
use crate::config::{Config, Proxy};
use crate::db;
use crate::limit::{self, Attempts};
use crate::owner::{self, OwnerError};
use crate::registry::{Registry, SiteError, SiteRuntime};
use crate::session::{self, Person};

/// Build output of `panel/`. Debug builds read it from disk at runtime, so a
/// new `npm run build` shows up without recompiling; release builds embed it.
#[derive(RustEmbed)]
#[folder = "../../panel/dist"]
#[allow_missing = true]
struct Assets;

/// Public facts about the installation. Each site's addresses come with the
/// list of sites.
#[derive(Serialize, Clone)]
struct Installation {
    /// The panel's hostname.
    panel: String,
    /// Version of alogeno, shown in Settings.
    version: &'static str,
}

/// Wrong sign-ins allowed per address in the window. Only per address: with
/// a single account, a limit per account would let anyone lock the owner
/// out.
const LOGIN_ATTEMPTS: usize = 10;
const LOGIN_WINDOW: Duration = Duration::from_secs(15 * 60);

/// How the chat names the one person who uses it, in messages such as
/// "… has the terminal open".
const OWNER_NAME: &str = "The owner";

/// When the latest release was asked of GitHub, and the answer.
type Asked = (std::time::Instant, Option<String>);

#[derive(Clone)]
struct PanelState {
    /// The installation's database: the owner, sessions, the assistant in
    /// use, the activity log.
    db: db::Handle,
    /// The sites, each with its database, builds and chat.
    registry: Arc<Registry>,
    /// One assistant at work at a time, across the sites.
    slot: Arc<crate::gate::AgentSlot>,
    config: Arc<Config>,
    installation: Arc<Installation>,
    /// Hosts the assistant may always reach because the configuration
    /// says so, besides Anthropic and the npm registry.
    configured_hosts: Arc<Vec<String>>,
    proxy: Arc<Proxy>,
    attempts: Arc<Attempts>,
    /// Public address of the panel, to build one-time links.
    panel_url: Arc<str>,
    account: Arc<crate::agent::Account>,
    /// Data directory, where the panel leaves a request to update.
    data_dir: Arc<std::path::PathBuf>,
    /// The latest release on GitHub and when it was asked, at most every
    /// few hours.
    latest: Arc<tokio::sync::Mutex<Option<Asked>>>,
    /// HTTPS in front: the cookie gets `Secure` and the `__Host-` prefix.
    secure: bool,
}

impl PanelState {
    fn cookie_name(&self) -> &'static str {
        if self.secure {
            "__Host-alogeno_session"
        } else {
            "alogeno_session"
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

pub fn router(
    config: &Config,
    db: db::Handle,
    registry: Arc<Registry>,
    account: Arc<crate::agent::Account>,
    slot: Arc<crate::gate::AgentSlot>,
) -> Router {
    let state = PanelState {
        db,
        registry,
        slot,
        config: Arc::new(config.clone()),
        installation: Arc::new(Installation {
            panel: config.hosts.panel.clone(),
            version: env!("CARGO_PKG_VERSION"),
        }),
        configured_hosts: Arc::new(config.sandbox.agent_allow.clone()),
        proxy: Arc::new(config.proxy.clone()),
        attempts: Arc::new(Attempts::new(LOGIN_ATTEMPTS, LOGIN_WINDOW)),
        panel_url: Arc::from(config.panel_url()),
        account,
        data_dir: Arc::new(config.data_dir.clone()),
        latest: Arc::default(),
        secure: config.hosts.scheme == "https",
    };
    // What one site has: its chat, contents, builds and publications, under
    // /api/sites/{site}/….
    let site = Router::new()
        .route("/build", get(build_status))
        .route("/agent/hosts", get(agent_hosts))
        .route(
            "/agent/hosts/{host}",
            axum::routing::delete(agent_host_remove),
        )
        .route("/chat/events", get(chat_events))
        .route("/chat/messages", post(chat_send))
        .route("/chat/stop", post(chat_stop))
        .route("/chat/new", post(chat_new))
        .route("/chat/conversations", get(chat_conversations))
        .route("/chat/conversations/{id}", post(chat_open_conversation))
        .route("/chat/mode", post(chat_mode))
        .route("/chat/model", post(chat_model))
        .route("/chat/retry", post(chat_retry))
        .route("/chat/back/{id}", post(chat_go_back))
        .route("/chat/undo/{id}", post(chat_undo_back))
        .route("/chat/permissions/{id}", post(chat_permission))
        .route("/chat/questions/{id}", post(chat_answers))
        .route("/terminal", get(terminal_socket))
        .route("/terminal/close", post(terminal_close))
        .route("/pages", get(pages_list).post(page_create))
        .route(
            "/pages/{id}",
            get(page_read).patch(page_update).delete(page_delete),
        )
        .route("/page-types", get(page_types))
        .route("/components", get(components_list).post(component_create))
        .route(
            "/components/{id}",
            get(component_read)
                .patch(component_update)
                .delete(component_delete),
        )
        .route("/changes", get(changes_list))
        .route("/preview/code", post(preview_code))
        .route("/publish", post(publish_site))
        .route("/publish/plan", post(publish_plan))
        .route("/publish/back", post(publish_back))
        .route(
            "/media",
            get(media_list)
                .post(media_upload)
                .layer(axum::extract::DefaultBodyLimit::max(
                    crate::media::LIMIT + 1024,
                )),
        )
        .route(
            "/media/{id}",
            axum::routing::patch(media_update).delete(media_delete),
        )
        .route("/media/{id}/file", get(media_file))
        .route_layer(middleware::from_fn_with_state(state.clone(), with_site));
    let private = Router::new()
        .route("/api/installation", get(installation))
        .route("/api/sites", get(sites_list).post(sites_create))
        .route("/api/sites/{site}", axum::routing::delete(sites_delete))
        // A site's domains, from Settings › Sites for any site: without
        // starting the site, unlike the routes nested below.
        .route(
            "/api/sites/{site}/domains",
            get(domains_list).post(domains_add),
        )
        .route(
            "/api/sites/{site}/domains/{host}",
            axum::routing::delete(domains_remove),
        )
        .route(
            "/api/sites/{site}/domains/{host}/check",
            post(domains_check),
        )
        .route("/api/events", get(events))
        .route("/api/update", get(update_status).post(update_request))
        .route("/api/activity", get(activity_list))
        .route("/api/agent/account", get(agent_account))
        .route("/api/agent/login", post(agent_login))
        .route("/api/agent/login/code", post(agent_login_code))
        .route("/api/agent/key", post(agent_key))
        .route("/api/agent/logout", post(agent_logout))
        .route("/api/agent/use", post(agent_use))
        .route("/api/agent/models", get(agent_models))
        .route("/api/agent/usage", get(agent_usage))
        .nest("/api/sites/{site}", site)
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
        .route("/api/public", get(public))
        .merge(private)
        .route(
            "/api/{*rest}",
            any(|| async { error(StatusCode::NOT_FOUND, "no such API") }),
        )
        .with_state(state.clone())
        .fallback(asset)
        .layer(middleware::from_fn_with_state(state, panel_only))
}

/// Whether a request to the API comes from a page of another origin, as
/// browsers tell with `Origin` and `Sec-Fetch-Site`. The preview is on a
/// sibling host of the panel: for the browser it is the same site, so the
/// `SameSite=Strict` session cookie travels with its requests, and only these
/// headers tell them apart. A request with neither header comes from a
/// program such as curl, which does not hold the browser's cookie.
fn cross_origin(headers: &HeaderMap, panel_origin: &str) -> bool {
    let value = |name| {
        headers
            .get(name)
            .and_then(|value: &HeaderValue| value.to_str().ok())
    };
    let other_origin = value(header::ORIGIN).is_some_and(|origin| origin != panel_origin);
    let other_site =
        value(HeaderName::from_static("sec-fetch-site")).is_some_and(|site| site != "same-origin");
    other_origin || other_site
}

/// Refuses API requests from pages of other origins, whatever the method:
/// some reads start the CLI. Keeps the panel out of frames of other pages.
async fn panel_only(State(state): State<PanelState>, request: Request, next: Next) -> Response {
    if request.uri().path().starts_with("/api/")
        && cross_origin(request.headers(), state.panel_url.trim_end_matches('/'))
    {
        return error(StatusCode::FORBIDDEN, "cross-origin request");
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("frame-ancestors 'none'"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

fn owner_error(err: OwnerError) -> Response {
    if let Some(field) = err.field() {
        let body = json!({ "error": err.to_string(), "field": field });
        return (StatusCode::BAD_REQUEST, Json(body)).into_response();
    }
    match err {
        OwnerError::InvalidLink => error(StatusCode::GONE, err.to_string()),
        OwnerError::WrongPassword => error(StatusCode::UNAUTHORIZED, err.to_string()),
        OwnerError::Invalid { .. } => error(StatusCode::BAD_REQUEST, err.to_string()),
        OwnerError::Database(_) | OwnerError::Hash(_) => {
            eprintln!("alogeno: {err}");
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
        Err(err) => owner_error(err.into()),
    }
}

/// The site a request under /api/sites/{site}/ is about, made available to
/// its handler: 404 for an unknown site, 503 for one that cannot start.
async fn with_site(
    State(state): State<PanelState>,
    Path(params): Path<std::collections::HashMap<String, String>>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(id) = params.get("site") else {
        return error(StatusCode::NOT_FOUND, "no such site");
    };
    match state.registry.get(id).await {
        Ok(site) => {
            request.extensions_mut().insert(site);
            next.run(request).await
        }
        Err(SiteError::NotFound) => error(StatusCode::NOT_FOUND, "no such site"),
        Err(SiteError::Unavailable(problem)) => {
            eprintln!("alogeno: site {id} unavailable: {problem}");
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("this site cannot start: {problem}"),
            )
        }
    }
}

/// 409 because the assistant works on another site, naming it, so that
/// the panel can offer to open it.
fn busy(message: &str, state: &PanelState) -> Response {
    let body = json!({ "error": message, "busy": state.slot.current() });
    (StatusCode::CONFLICT, Json(body)).into_response()
}

/// Deletes a site: its addresses at once, its files right after. Refused
/// for the only site and while its assistant or a build works.
async fn sites_delete(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
    Path(id): Path<String>,
) -> Response {
    use crate::registry::DeleteError;
    match state.registry.delete(&id, &state.slot).await {
        Ok(site) => {
            let subject = format!("site:{}", site.id);
            let _ = state
                .db
                .run(move |conn| {
                    activity::record_site(
                        conn,
                        Some(&site.id),
                        Some(person.id),
                        activity::Environment::Installation,
                        "site.deleted",
                        Some(&subject),
                    )
                })
                .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(DeleteError::NotFound) => error(StatusCode::NOT_FOUND, "no such site"),
        Err(DeleteError::Refused(reason)) => error(StatusCode::CONFLICT, reason),
        Err(DeleteError::Failed(problem)) => error(StatusCode::INTERNAL_SERVER_ERROR, problem),
    }
}

/// What the assistant is doing across the sites: an event `busy` with
/// `{"agent": occupant or null}` now and after each change.
async fn events(State(state): State<PanelState>) -> Response {
    let Some(watch) = state.slot.watch() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "no events");
    };
    let updates = stream::unfold((watch, true), |(mut watch, first)| async move {
        if !first && watch.changed().await.is_err() {
            return None;
        }
        let occupant = watch.borrow_and_update().clone();
        let event = SseEvent::default()
            .event("busy")
            .json_data(json!({ "agent": occupant }))
            .ok()?;
        Some((Ok::<_, Infallible>(event), (watch, false)))
    });
    Sse::new(updates.take_until(state.slot.closed()))
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// One site as the panel lists it, with its addresses.
#[derive(Serialize)]
struct SiteInfo {
    #[serde(flatten)]
    site: crate::sites::Site,
    /// The address of its preview.
    preview_host: String,
    /// The addresses of the published site: its domains, without the ones
    /// that redirect, and in local development the one next to the panel.
    site_hosts: Vec<String>,
}

fn site_info(
    config: &Config,
    site: crate::sites::Site,
    domains: &[crate::domains::Domain],
) -> SiteInfo {
    let own = domains
        .iter()
        .filter(|domain| domain.site_id == site.id && domain.redirect_to.is_none())
        .map(|domain| domain.host.clone());
    SiteInfo {
        preview_host: config.preview_host(&site.slug),
        site_hosts: own.chain(config.local_host(&site.slug)).collect(),
        site,
    }
}

/// Every site, oldest first, with its addresses.
async fn sites_list(State(state): State<PanelState>) -> Response {
    let found = state
        .db
        .run(|conn| {
            Ok::<_, rusqlite::Error>((crate::sites::list(conn)?, crate::domains::list(conn)?))
        })
        .await;
    match found {
        Ok((list, domains)) => {
            let list: Vec<SiteInfo> = list
                .into_iter()
                .map(|site| site_info(&state.config, site, &domains))
                .collect();
            Json(list).into_response()
        }
        Err(err) => error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

#[derive(Deserialize)]
struct NewSite {
    name: String,
    slug: String,
}

/// Creates a site: its folder, repository and database, and its addresses.
async fn sites_create(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
    Json(body): Json<NewSite>,
) -> Response {
    match state.registry.create(body.name, body.slug).await {
        Ok(site) => {
            let subject = format!("site:{}", site.id);
            let (actor, site_id) = (person.id, site.id.clone());
            let _ = state
                .db
                .run(move |conn| {
                    activity::record_site(
                        conn,
                        Some(&site_id),
                        Some(actor),
                        activity::Environment::Installation,
                        "site.created",
                        Some(&subject),
                    )
                })
                .await;
            (
                StatusCode::CREATED,
                Json(site_info(&state.config, site, &[])),
            )
                .into_response()
        }
        Err(err) => {
            let body = json!({ "error": err, "field": "slug" });
            (StatusCode::BAD_REQUEST, Json(body)).into_response()
        }
    }
}

/// The facts of the installation, with what Settings warns about: the
/// previews' wildcard record missing, the disk filling up.
#[derive(Serialize)]
struct InstallationStatus {
    #[serde(flatten)]
    facts: Installation,
    /// `auto`: the reverse proxy serves every name alogeno serves, asking
    /// it; `manual`: names are added to the proxy by hand; `local`:
    /// development on localhost names.
    mode: &'static str,
    /// Where the reverse proxy forwards the names to.
    listen: String,
    /// The addresses the panel's name resolves to.
    addresses: Vec<String>,
    /// Whether the previews' names, `*.preview.<panel>`, resolve to them.
    wildcard: bool,
    /// Free space for the data directory, in bytes.
    free_bytes: Option<u64>,
}

async fn installation(State(state): State<PanelState>) -> Json<InstallationStatus> {
    let config = state.config.clone();
    let (addresses, wildcard) = if config.local() {
        (Vec::new(), true)
    } else {
        crate::domains::panel_and_wildcard(&config.hosts.panel).await
    };
    let mode = if config.local() {
        "local"
    } else if config.domains.ask_listen.is_some() {
        "auto"
    } else {
        "manual"
    };
    Json(InstallationStatus {
        facts: state.installation.as_ref().clone(),
        mode,
        listen: config.listen.to_string(),
        addresses: addresses.iter().map(ToString::to_string).collect(),
        wildcard,
        free_bytes: free_bytes(&config.data_dir),
    })
}

/// Free space for an unprivileged user on the filesystem of `path`.
fn free_bytes(path: &std::path::Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is a valid C string and `stat` is written by the call
    // before it is read.
    let stat = unsafe {
        if libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) != 0 {
            return None;
        }
        stat.assume_init()
    };
    #[allow(clippy::unnecessary_cast)]
    Some(stat.f_bavail as u64 * stat.f_frsize as u64)
}

/// The site `id` exists, or the 404 to answer.
async fn site_exists(state: &PanelState, id: &str) -> Result<(), Response> {
    match state.registry.list().await {
        Ok(list) if list.iter().any(|site| site.id == id) => Ok(()),
        Ok(_) => Err(error(StatusCode::NOT_FOUND, "no such site")),
        Err(err) => Err(error(StatusCode::INTERNAL_SERVER_ERROR, err)),
    }
}

fn domain_error(err: crate::domains::DomainError) -> Response {
    use crate::domains::DomainError;
    match err {
        DomainError::Invalid(message) => error(StatusCode::BAD_REQUEST, message),
        DomainError::Taken(message) => error(StatusCode::CONFLICT, message),
        DomainError::NotFound => error(StatusCode::NOT_FOUND, "no such domain on this site"),
        DomainError::Db(message) => error(StatusCode::INTERNAL_SERVER_ERROR, message),
    }
}

/// A domain of a site, with the DNS records it needs.
#[derive(Serialize)]
struct DomainInfo {
    #[serde(flatten)]
    domain: crate::domains::Domain,
    records: Vec<crate::domains::Record>,
}

/// The domains of a site, with their records, and the addresses of the
/// panel they follow.
async fn domains_list(State(state): State<PanelState>, Path(site): Path<String>) -> Response {
    if let Err(response) = site_exists(&state, &site).await {
        return response;
    }
    let list = match state.registry.domains(&site).await {
        Ok(list) => list,
        Err(err) => return error(StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let addresses = crate::domains::lookup(state.config.hosts.panel.clone()).await;
    let domains: Vec<DomainInfo> = list
        .into_iter()
        .map(|domain| DomainInfo {
            records: crate::domains::records(&domain, &addresses),
            domain,
        })
        .collect();
    Json(json!({
        "domains": domains,
        "addresses": addresses.iter().map(ToString::to_string).collect::<Vec<_>>(),
    }))
    .into_response()
}

#[derive(Deserialize)]
struct NewDomain {
    host: String,
    /// Also `www.<host>`, redirecting to it.
    #[serde(default)]
    www: bool,
}

/// Adds a domain to a site: served at once.
async fn domains_add(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
    Path(site): Path<String>,
    Json(body): Json<NewDomain>,
) -> Response {
    if let Err(response) = site_exists(&state, &site).await {
        return response;
    }
    match state.registry.add_domain(&site, &body.host, body.www).await {
        Ok(added) => {
            for domain in &added {
                log_domain(&state, &site, person.id, "domain.added", &domain.host).await;
            }
            (StatusCode::CREATED, Json(added)).into_response()
        }
        Err(err) => domain_error(err),
    }
}

/// Removes a domain of a site, with its www.
async fn domains_remove(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
    Path((site, host)): Path<(String, String)>,
) -> Response {
    match state.registry.remove_domain(&site, &host).await {
        Ok(removed) => {
            for name in &removed {
                log_domain(&state, &site, person.id, "domain.removed", name).await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(err) => domain_error(err),
    }
}

/// Whether a domain of a site reaches this installation, and where its
/// name points.
async fn domains_check(
    State(state): State<PanelState>,
    Path((site, host)): Path<(String, String)>,
) -> Response {
    match state.registry.domains(&site).await {
        Ok(list) if list.iter().any(|domain| domain.host == host) => {}
        Ok(_) => return error(StatusCode::NOT_FOUND, "no such domain on this site"),
        Err(err) => return error(StatusCode::INTERNAL_SERVER_ERROR, err),
    }
    let instance_id = match state.db.run(crate::domains::instance_id).await {
        Ok(id) => id,
        Err(err) => return error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };
    // A name that does not resolve needs no request.
    let addresses = crate::domains::lookup(host.clone()).await;
    let result = if addresses.is_empty() {
        crate::domains::Check::Unreachable
    } else {
        tokio::task::spawn_blocking(move || crate::domains::check(&host, &instance_id))
            .await
            .unwrap_or(crate::domains::Check::Unreachable)
    };
    Json(json!({
        "result": result,
        "addresses": addresses.iter().map(ToString::to_string).collect::<Vec<_>>(),
    }))
    .into_response()
}

async fn log_domain(state: &PanelState, site: &str, person: i64, action: &'static str, host: &str) {
    let (site, subject) = (site.to_string(), format!("domain:{host}"));
    let _ = state
        .db
        .run(move |conn| {
            activity::record_site(
                conn,
                Some(&site),
                Some(person),
                activity::Environment::Installation,
                action,
                Some(&subject),
            )
        })
        .await;
}

/// The latest release, asked of GitHub at most every six hours (every
/// fifteen minutes after a failure).
async fn latest_release(state: &PanelState) -> Option<String> {
    let mut cached = state.latest.lock().await;
    if let Some((asked, tag)) = cached.as_ref() {
        let fresh = if tag.is_some() { 6 * 3600 } else { 15 * 60 };
        if asked.elapsed() < Duration::from_secs(fresh) {
            return tag.clone();
        }
    }
    let tag = tokio::task::spawn_blocking(crate::update::latest)
        .await
        .ok()
        .and_then(|found| {
            found
                .map_err(|err| eprintln!("alogeno: latest release: {err}"))
                .ok()
        });
    *cached = Some((std::time::Instant::now(), tag.clone()));
    tag
}

#[derive(Serialize)]
struct UpdateStatus {
    current: &'static str,
    /// A newer release, when there is one.
    available: Option<String>,
    /// The units that run updates from the panel are installed.
    from_panel: bool,
    /// An update was asked for and has not started yet.
    requested: bool,
    /// Installed with Nix: updates come from the system configuration.
    nix: bool,
}

async fn update_status(State(state): State<PanelState>) -> Response {
    let available = latest_release(&state)
        .await
        .and_then(|tag| crate::update::newer(&tag));
    Json(UpdateStatus {
        current: crate::update::CURRENT,
        available,
        from_panel: crate::update::from_panel(),
        requested: crate::update::request_path(&state.data_dir).exists(),
        nix: crate::update::by_nix(),
    })
    .into_response()
}

/// Leaves the request that `alogeno-update.path` turns into
/// `alogeno update`, run as root.
async fn update_request(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
) -> Response {
    if !crate::update::from_panel() {
        return error(
            StatusCode::CONFLICT,
            "this installation cannot update from the panel: run the installer again, or sudo alogeno update",
        );
    }
    let Some(tag) = latest_release(&state)
        .await
        .and_then(|tag| crate::update::newer(&tag))
    else {
        return error(StatusCode::CONFLICT, "alogeno is already up to date");
    };
    let path = crate::update::request_path(&state.data_dir);
    if let Err(err) = std::fs::write(&path, b"") {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("cannot ask for the update: {err}"),
        );
    }
    let subject = format!("version:{tag}");
    let actor = person.id;
    let _ = state
        .db
        .run(move |conn| {
            activity::record(
                conn,
                Some(actor),
                activity::Environment::Installation,
                "installation.update_requested",
                Some(&subject),
            )
        })
        .await;
    Json(json!({ "requested": tag })).into_response()
}

/// What the sign-in page shows before anyone signs in: the site's domain,
/// and whether the owner exists, so that a new installation explains how to
/// create it instead of asking for a password nobody has.
async fn public(State(state): State<PanelState>) -> Response {
    match state.db.run(owner::has_owner).await {
        Ok(has_owner) => Json(json!({
            "site": state.installation.panel,
            "has_owner": has_owner,
        }))
        .into_response(),
        Err(err) => owner_error(err.into()),
    }
}

#[derive(Deserialize)]
struct LinkBody {
    token: String,
}

/// What a one-time link is for, so the panel shows the right form.
async fn link(State(state): State<PanelState>, Json(body): Json<LinkBody>) -> Response {
    match state
        .db
        .run(move |conn| owner::find_link(conn, &body.token))
        .await
    {
        Ok(found) => Json(json!({ "purpose": found.purpose.as_str() })).into_response(),
        Err(err) => owner_error(err),
    }
}

#[derive(Deserialize)]
struct SetupBody {
    token: String,
    password: String,
}

/// Redeems a one-time link, sets the owner's password and signs in.
async fn setup(State(state): State<PanelState>, Json(body): Json<SetupBody>) -> Response {
    let result = state
        .db
        .run(move |conn| {
            let person = owner::redeem(conn, &body.token, &body.password)?;
            let token = session::create(conn, person)?;
            Ok::<_, OwnerError>((token, Person { id: person }))
        })
        .await;
    match result {
        Ok((token, me)) => signed_in(&state, &token, me),
        Err(err) => owner_error(err),
    }
}

fn signed_in(state: &PanelState, token: &str, me: Person) -> Response {
    let mut response = me_json(me);
    response
        .headers_mut()
        .insert(header::SET_COOKIE, state.session_cookie(token));
    response
}

#[derive(Deserialize)]
struct LoginBody {
    password: String,
}

/// One password check at a time: Argon2 takes memory and CPU, and repeated
/// sign-ins must not pile them up.
static PASSWORD_CHECK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn login(
    State(state): State<PanelState>,
    connect: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    Json(body): Json<LoginBody>,
) -> Response {
    let peer = connect.map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |c| c.0.0.ip());
    let address = limit::address_key(limit::client_ip(peer, &headers, &state.proxy));
    if !state.attempts.allowed(&address) {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "too many attempts; try again in a few minutes",
        );
    }
    // The hash, then the check without holding the database, then the
    // session or the record of the failure.
    let found = match state.db.run(owner::credentials).await {
        Ok(found) => found,
        Err(err) => return owner_error(err.into()),
    };
    let attempted = found.as_ref().map(|(person, _)| *person);
    let owner_id = {
        let _one_at_a_time = PASSWORD_CHECK.lock().await;
        tokio::task::spawn_blocking(move || session::check_password(found.as_ref(), &body.password))
            .await
            .unwrap_or(None)
    };
    let result = state
        .db
        .run(move |conn| session::finish_login(conn, owner_id, attempted))
        .await;
    match result {
        Ok((token, me)) => {
            state.attempts.clear(&address);
            signed_in(&state, &token, me)
        }
        Err(err) => {
            if matches!(err, OwnerError::WrongPassword) {
                state.attempts.failed(&address);
            }
            owner_error(err)
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
        return owner_error(err.into());
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
        Ok(Some(person)) => me_json(person),
        Ok(None) => error(StatusCode::UNAUTHORIZED, "sign in first"),
        Err(err) => owner_error(err.into()),
    }
}

/// The signed-in owner.
fn me_json(person: Person) -> Response {
    Json(json!({ "id": person.id })).into_response()
}

/// State of the builds and the release the preview shows.
/// The state of the builds, and whether a version of the site is online.
#[derive(serde::Serialize)]
struct BuildStatus {
    #[serde(flatten)]
    status: crate::build::Status,
    live: bool,
}

async fn build_status(Extension(site): Extension<Arc<SiteRuntime>>) -> Json<BuildStatus> {
    Json(BuildStatus {
        status: site.builder.status(),
        live: crate::build::pointed(&site.builder.releases(), "live").is_some(),
    })
}

/// Which assistant a request is about: the one named in the body, or the
/// one in use.
#[derive(Deserialize, Default)]
struct ProviderBody {
    provider: Option<String>,
}

fn provider_of(state: &PanelState, named: Option<&str>) -> Result<Provider, &'static str> {
    match named {
        None => Ok(state.account.environment().active()),
        Some(id) => Provider::parse(id)
            .filter(|p| state.account.offered().iter().any(|o| o.provider == *p))
            .ok_or("this assistant is not available"),
    }
}

/// A body that may be empty: older panels post none.
fn body_of<T: serde::de::DeserializeOwned + Default>(bytes: &[u8]) -> Result<T, &'static str> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(bytes).map_err(|_| "invalid request")
}

async fn record_agent(state: &PanelState, person: i64, action: &'static str, provider: Provider) {
    let subject = format!("assistant:{}", provider.id());
    let _ = state
        .db
        .run(move |conn| {
            activity::record(
                conn,
                Some(person),
                activity::Environment::Installation,
                action,
                Some(&subject),
            )
        })
        .await;
}

#[derive(Deserialize)]
struct AccountQuery {
    provider: Option<String>,
}

/// The account of the assistant in use, or of the one named, with the
/// assistants this installation offers.
async fn agent_account(
    State(state): State<PanelState>,
    Query(query): Query<AccountQuery>,
) -> Response {
    let provider = match provider_of(&state, query.provider.as_deref()) {
        Ok(provider) => provider,
        Err(message) => return error(StatusCode::BAD_REQUEST, message),
    };
    let status = state.account.status_of(provider).await;
    let mut body = serde_json::to_value(status).unwrap_or_default();
    body["active"] = json!(state.account.environment().active());
    body["assistants"] = json!(state.account.offered());
    Json(body).into_response()
}

/// Starts the sign-in of an assistant's account: the address of the
/// sign-in page to open and, for a device code, the code to enter there.
async fn agent_login(State(state): State<PanelState>, body: axum::body::Bytes) -> Response {
    let provider = match body_of::<ProviderBody>(&body)
        .and_then(|body| provider_of(&state, body.provider.as_deref()))
    {
        Ok(provider) => provider,
        Err(message) => return error(StatusCode::BAD_REQUEST, message),
    };
    match state.account.start_login(provider).await {
        Ok(start) => Json(start).into_response(),
        Err(message) => error(StatusCode::SERVICE_UNAVAILABLE, message),
    }
}

#[derive(Deserialize)]
struct CodeBody {
    code: String,
}

async fn agent_login_code(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
    Json(body): Json<CodeBody>,
) -> Response {
    match state.account.finish_login(&body.code).await {
        Ok(()) => {
            record_agent(&state, person.id, "agent.signed_in", Provider::Claude).await;
            Json(state.account.status_of(Provider::Claude).await).into_response()
        }
        Err(message) => {
            let body = json!({ "error": message, "field": "code" });
            (StatusCode::BAD_REQUEST, Json(body)).into_response()
        }
    }
}

#[derive(Deserialize)]
struct KeyBody {
    key: String,
    provider: Option<String>,
}

/// The hosts people allowed the assistant to reach.
async fn agent_hosts(
    State(state): State<PanelState>,
    Extension(site): Extension<Arc<SiteRuntime>>,
) -> Response {
    match site.chat.allowed_hosts().await {
        Ok(hosts) => {
            Json(json!({ "hosts": hosts, "configured": *state.configured_hosts })).into_response()
        }
        Err(message) => error(StatusCode::INTERNAL_SERVER_ERROR, message),
    }
}

/// Takes back the permission to reach a host.
async fn agent_host_remove(
    Extension(site): Extension<Arc<SiteRuntime>>,
    Extension(person): Extension<Person>,
    Path((_site, host)): Path<(String, String)>,
) -> Response {
    match site.chat.forget_host(&host, person.id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => error(
            StatusCode::NOT_FOUND,
            "this host is not among the allowed ones",
        ),
        Err(message) => error(StatusCode::INTERNAL_SERVER_ERROR, message),
    }
}

/// Connects an assistant with an API key of its vendor instead of a
/// subscription.
async fn agent_key(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
    Json(body): Json<KeyBody>,
) -> Response {
    let provider = match provider_of(&state, body.provider.as_deref()) {
        Ok(provider) => provider,
        Err(message) => return error(StatusCode::BAD_REQUEST, message),
    };
    match state.account.use_key(provider, &body.key).await {
        Ok(()) => {
            record_agent(&state, person.id, "agent.signed_in", provider).await;
            Json(state.account.status_of(provider).await).into_response()
        }
        Err(message) => {
            let body = json!({ "error": message, "field": "key" });
            (StatusCode::BAD_REQUEST, Json(body)).into_response()
        }
    }
}

async fn agent_logout(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
    body: axum::body::Bytes,
) -> Response {
    let provider = match body_of::<ProviderBody>(&body)
        .and_then(|body| provider_of(&state, body.provider.as_deref()))
    {
        Ok(provider) => provider,
        Err(message) => return error(StatusCode::BAD_REQUEST, message),
    };
    match state.account.logout(provider).await {
        Ok(()) => {
            record_agent(&state, person.id, "agent.signed_out", provider).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(message) => error(StatusCode::SERVICE_UNAVAILABLE, message),
    }
}

/// Makes another assistant the one in use, once it is signed in. The chat
/// starts a new conversation; the site, its versions and the other
/// assistant's sign-in stay as they are.
async fn agent_use(
    State(state): State<PanelState>,
    Extension(site): Extension<Arc<SiteRuntime>>,
    Extension(person): Extension<Person>,
    Json(body): Json<ProviderBody>,
) -> Response {
    let provider = match provider_of(&state, body.provider.as_deref()) {
        Ok(provider) => provider,
        Err(message) => return error(StatusCode::BAD_REQUEST, message),
    };
    let environment = state.account.environment();
    let from = environment.active();
    if from == provider {
        return Json(state.account.status_of(provider).await).into_response();
    }
    if let Err(message) = site.chat.check_switch() {
        return error(StatusCode::CONFLICT, message);
    }
    let status = state.account.status_of(provider).await;
    if !status.signed_in {
        let message = status
            .problem
            .unwrap_or_else(|| format!("{} is not connected yet", provider.name()));
        return error(StatusCode::CONFLICT, message);
    }
    let id = provider.id();
    if let Err(err) = state
        .db
        .run(move |conn| conn.execute("UPDATE installation SET assistant = ?1", [id]))
        .await
    {
        return error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string());
    }
    environment.set_active(provider);
    site.chat.switched().await;
    let subject = format!("assistant:{}>{}", from.id(), provider.id());
    let actor = person.id;
    let _ = state
        .db
        .run(move |conn| {
            activity::record(
                conn,
                Some(actor),
                activity::Environment::Installation,
                "agent.switched",
                Some(&subject),
            )
        })
        .await;
    Json(status).into_response()
}

/// The chat as server-sent events: a snapshot first, then every change. A
/// `reset` event asks the panel to connect again for a new snapshot.
async fn chat_events(Extension(site): Extension<Arc<SiteRuntime>>) -> Response {
    // Subscribed before the snapshot is taken, so nothing falls in between;
    // the panel ignores what the snapshot already has.
    let receiver = site.chat.subscribe();
    let snapshot = site.chat.snapshot().await;
    let first = stream::once(async move { sse_event(&ChatEvent::Snapshot(snapshot)) });
    let rest = stream::unfold(receiver, |mut receiver| async move {
        let event = match receiver.recv().await {
            Ok(event) => event,
            Err(broadcast::error::RecvError::Lagged(_)) => ChatEvent::Reset,
            Err(broadcast::error::RecvError::Closed) => return None,
        };
        Some((sse_event(&event), receiver))
    });
    Sse::new(first.chain(rest).take_until(site.chat.closing()))
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn sse_event(event: &ChatEvent) -> Result<SseEvent, Infallible> {
    Ok(SseEvent::default()
        .json_data(event)
        .unwrap_or_else(|_| SseEvent::default().data("{\"type\":\"reset\"}")))
}

#[derive(Deserialize)]
struct MessageBody {
    text: String,
}

async fn chat_send(
    State(state): State<PanelState>,
    Extension(site): Extension<Arc<SiteRuntime>>,
    Extension(person): Extension<Person>,
    Json(body): Json<MessageBody>,
) -> Response {
    match site.chat.send(person.id, &body.text).await {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(err @ SendError::Elsewhere(_)) => busy(&err.to_string(), &state),
        Err(err @ (SendError::Busy | SendError::Paused)) => {
            error(StatusCode::CONFLICT, err.to_string())
        }
        Err(err @ (SendError::Empty | SendError::TooLong)) => {
            error(StatusCode::BAD_REQUEST, err.to_string())
        }
        Err(err @ SendError::Unavailable(_)) => {
            error(StatusCode::SERVICE_UNAVAILABLE, err.to_string())
        }
    }
}

async fn chat_stop(Extension(site): Extension<Arc<SiteRuntime>>) -> Response {
    site.chat.stop();
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
struct PermissionBody {
    allow: bool,
}

/// A person answers a permission request of the assistant. The answer is
/// recorded in the activity log.
async fn chat_permission(
    State(state): State<PanelState>,
    Extension(site): Extension<Arc<SiteRuntime>>,
    Extension(person): Extension<Person>,
    Path((_site, id)): Path<(String, i64)>,
    Json(body): Json<PermissionBody>,
) -> Response {
    if let Err(message) = site.chat.answer_permission(person.id, id, body.allow).await {
        return error(StatusCode::CONFLICT, message);
    }
    let action = if body.allow {
        "agent.permission_allowed"
    } else {
        "agent.permission_denied"
    };
    log_site(
        &state,
        &site,
        person.id,
        activity::Environment::Preview,
        action,
        format!("chat_item:{id}"),
    )
    .await;
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
struct AnswersBody {
    answers: Vec<crate::chat::Answer>,
}

/// A person answers the questions of the assistant.
async fn chat_answers(
    Extension(site): Extension<Arc<SiteRuntime>>,
    Extension(person): Extension<Person>,
    Path((_site, id)): Path<(String, i64)>,
    Json(body): Json<AnswersBody>,
) -> Response {
    match site
        .chat
        .answer_questions(person.id, id, body.answers)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(message) => error(StatusCode::CONFLICT, message),
    }
}

#[derive(Deserialize)]
struct TerminalSize {
    #[serde(default = "default_columns")]
    cols: u16,
    #[serde(default = "default_rows")]
    rows: u16,
    /// The panel's theme, dark or light, for the CLI's text style.
    #[serde(default)]
    theme: String,
}

fn default_columns() -> u16 {
    100
}

fn default_rows() -> u16 {
    30
}

/// The terminal: the interactive CLI on the current conversation, over a
/// WebSocket. Output goes to the browser as binary messages; the browser
/// sends `{"type":"input","data":…}` and `{"type":"resize","cols":…,"rows":…}`.
async fn terminal_socket(
    State(state): State<PanelState>,
    Extension(site): Extension<Arc<SiteRuntime>>,
    Extension(person): Extension<Person>,
    Query(size): Query<TerminalSize>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    // Only the panel's own pages may open it.
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if origin != state.panel_url.trim_end_matches('/') {
        return error(
            StatusCode::FORBIDDEN,
            "the terminal opens from the panel only",
        );
    }
    match site
        .chat
        .open_terminal(person.id, OWNER_NAME, size.cols, size.rows, &size.theme)
        .await
    {
        Ok(terminal) => upgrade.on_upgrade(move |socket| serve_terminal(socket, terminal)),
        Err(message) => error(StatusCode::CONFLICT, message),
    }
}

async fn serve_terminal(socket: WebSocket, terminal: Arc<crate::terminal::Terminal>) {
    let (mut sink, mut incoming) = socket.split();
    let (history, mut output) = terminal.attach();
    if sink.send(Message::Binary(history.into())).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            chunk = output.recv() => match chunk {
                Ok(chunk) => {
                    if sink.send(Message::Binary(chunk.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            },
            message = incoming.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    let Ok(message) = serde_json::from_str::<serde_json::Value>(&text) else {
                        continue;
                    };
                    match message["type"].as_str() {
                        Some("input") => {
                            if let Some(data) = message["data"].as_str() {
                                let _ = terminal.write(data.as_bytes());
                            }
                        }
                        Some("resize") => {
                            let size = |key: &str| {
                                message[key].as_u64().and_then(|n| u16::try_from(n).ok()).unwrap_or(1)
                            };
                            terminal.resize(size("cols"), size("rows"));
                        }
                        _ => {}
                    }
                }
                Some(Ok(Message::Binary(data))) => {
                    let _ = terminal.write(&data);
                }
                Some(Ok(_)) => {}
                // The browser left; the terminal stays open for it to come back.
                Some(Err(_)) | None => break,
            },
            _ = terminal.exited() => {
                let _ = sink.send(Message::Close(None)).await;
                break;
            }
        }
    }
}

async fn terminal_close(
    Extension(site): Extension<Arc<SiteRuntime>>,
    Extension(person): Extension<Person>,
) -> Response {
    match site.chat.close_terminal(person.id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(message) => error(StatusCode::CONFLICT, message),
    }
}

/// Takes the site back to before a message; answers with the message, for
/// the person to change it.
async fn chat_go_back(
    Extension(site): Extension<Arc<SiteRuntime>>,
    Extension(person): Extension<Person>,
    Path((_site, id)): Path<(String, i64)>,
) -> Response {
    match site.chat.go_back(person.id, id).await {
        Ok(text) => Json(json!({ "text": text })).into_response(),
        Err(message) => error(StatusCode::CONFLICT, message),
    }
}

/// Undoes a going back.
async fn chat_undo_back(
    Extension(site): Extension<Arc<SiteRuntime>>,
    Extension(person): Extension<Person>,
    Path((_site, id)): Path<(String, i64)>,
) -> Response {
    match site.chat.undo_back(person.id, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(message) => error(StatusCode::CONFLICT, message),
    }
}

/// Tries the last request again after something went wrong.
async fn chat_retry(
    Extension(site): Extension<Arc<SiteRuntime>>,
    Extension(person): Extension<Person>,
) -> Response {
    match site.chat.retry(person.id).await {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(err) => error(StatusCode::CONFLICT, err.to_string()),
    }
}

/// A refused change, for the panel: the problems name the field they are
/// about, so the editor can show each one next to its field.
fn content_error(err: crate::chat::ContentError) -> Response {
    use crate::chat::ContentError;
    use crate::cms::CmsError;
    match err {
        ContentError::Locked(reason) => error(
            StatusCode::CONFLICT,
            format!("pages are read-only now: {reason}"),
        ),
        ContentError::Cms(CmsError::NotFound) => {
            error(StatusCode::NOT_FOUND, "no page has this id")
        }
        ContentError::Cms(CmsError::Invalid(problems)) => {
            let problems: Vec<_> = problems
                .iter()
                .map(|p| {
                    let field = p.at.split_once(' ').map(|(_, f)| f).unwrap_or("");
                    json!({ "field": field, "message": p.message })
                })
                .collect();
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "error": "the page was not saved", "problems": problems })),
            )
                .into_response()
        }
        ContentError::Cms(CmsError::Refused(reason)) => error(StatusCode::CONFLICT, reason),
        ContentError::Cms(CmsError::Io(problem)) | ContentError::Save(problem) => {
            error(StatusCode::INTERNAL_SERVER_ERROR, problem)
        }
    }
}

/// Records a change of a page or a component (`kind`) in the activity.
async fn record_content(
    state: &PanelState,
    site: &SiteRuntime,
    person: i64,
    action: &'static str,
    kind: &str,
    item: &serde_json::Value,
) {
    let subject = format!("{kind}:{}", item["id"].as_str().unwrap_or_default());
    log_site(
        state,
        site,
        person,
        activity::Environment::Preview,
        action,
        subject,
    )
    .await;
}

/// What is not online yet.
async fn changes_list(Extension(site): Extension<Arc<SiteRuntime>>) -> Response {
    let releases = site.builder.releases();
    let pending = site
        .chat
        .read_content(move |cms| {
            let site = cms.site().to_path_buf();
            if !site.join(".git").is_dir() {
                return Ok((
                    crate::changes::Pending::default(),
                    crate::publish::ready(&site, &releases),
                ));
            }
            let pending = crate::changes::pending(&site).map_err(crate::cms::CmsError::Io)?;
            Ok((pending, crate::publish::ready(&site, &releases)))
        })
        .await;
    let (pending, ready) = match pending {
        Ok(found) => found,
        Err(err) => return content_error(crate::chat::ContentError::Cms(err)),
    };
    // Why publishing cannot happen now, if it cannot.
    let blocked = match (site.chat.content_locked(), ready) {
        (Some(reason), _) => Some(format!("the site cannot be published while {reason}")),
        (None, Err(reason)) => Some(reason),
        (None, Ok(_)) => None,
    };
    let last = match site.db.run(crate::publish::last).await {
        Ok(last) => last,
        Err(err) => return owner_error(err.into()),
    };
    Json(json!({
        "count": pending.count(),
        "pending": pending,
        "blocked": blocked,
        "last_publication": last,
        "previous_publication": crate::build::pointed(&site.builder.releases(), "previous").is_some(),
    }))
    .into_response()
}

/// A one-time code that opens the preview, whose host does not share the
/// panel's cookie. Everyone with access to the panel sees the preview.
async fn preview_code(
    Extension(site): Extension<Arc<SiteRuntime>>,
    Extension(person): Extension<Person>,
) -> Response {
    match site
        .db
        .run(move |conn| crate::preview::code(conn, person.id))
        .await
    {
        Ok(code) => Json(json!({ "code": code })).into_response(),
        Err(err) => owner_error(err.into()),
    }
}

#[derive(Deserialize)]
struct PublishBody {
    /// The saved version the person saw the changes of.
    commit: String,
    /// The changes to publish, by key; all of them when missing.
    items: Option<Vec<String>>,
}

/// Publishing waits while the assistant changes the site.
fn publish_refused(site: &SiteRuntime) -> Option<Response> {
    site.chat.content_locked().map(|reason| {
        error(
            StatusCode::CONFLICT,
            format!("the site cannot be published while {reason}"),
        )
    })
}

fn publish_error(err: crate::publish::PublishError) -> Response {
    use crate::publish::PublishError;
    match err {
        PublishError::Changed => error(
            StatusCode::CONFLICT,
            "the site changed while the list was open: check the list again",
        ),
        PublishError::Nothing => error(StatusCode::CONFLICT, "everything is online already"),
        PublishError::NotReady(reason) => error(StatusCode::CONFLICT, reason),
        PublishError::Failed(reason) => error(StatusCode::INTERNAL_SERVER_ERROR, reason),
    }
}

/// Keeps a publication in the history and the activity log.
async fn record_publication(
    state: &PanelState,
    site: &SiteRuntime,
    person: i64,
    published: &crate::publish::Published,
    changes: usize,
    action: &'static str,
) {
    let record = published.clone();
    let subject = format!("release:{}", published.release);
    let saved = site
        .db
        .run(move |conn| crate::publish::record(conn, person, &record, changes))
        .await;
    if let Err(err) = saved {
        eprintln!("alogeno: cannot record the publication: {err}");
    }
    log_site(
        state,
        site,
        person,
        activity::Environment::Production,
        action,
        subject,
    )
    .await;
}

/// Appends an entry about the site to the installation's activity log.
async fn log_site(
    state: &PanelState,
    site: &SiteRuntime,
    person: i64,
    environment: activity::Environment,
    action: &'static str,
    subject: String,
) {
    let site_id = site.chat.site_id().to_string();
    let _ = state
        .db
        .run(move |conn| {
            activity::record_site(
                conn,
                Some(&site_id),
                Some(person),
                environment,
                action,
                Some(&subject),
            )
        })
        .await;
}

/// A selection of the changes, checked: what it brings with it and why it
/// cannot go online by itself, if it cannot. Nothing is built.
async fn publish_plan(
    Extension(site): Extension<Arc<SiteRuntime>>,
    Extension(person): Extension<Person>,
    Json(body): Json<PublishBody>,
) -> Response {
    let items = body.items.unwrap_or_default();
    let check_dir = site.builder.work_dir("check");
    let person_id = person.id;
    let prepared = site
        .chat
        .read_content(move |cms| {
            let site = cms.site().to_path_buf();
            if crate::git::head(&site).ok().as_deref() != Some(body.commit.as_str()) {
                return Ok(Err(crate::publish::PublishError::Changed));
            }
            let pending = crate::changes::pending(&site).map_err(crate::cms::CmsError::Io)?;
            Ok(
                crate::selection::prepare(&site, &pending, &items, person_id, &check_dir)
                    .map_err(crate::publish::PublishError::NotReady),
            )
        })
        .await;
    match prepared {
        Ok(Ok(prepared)) => Json(prepared.plan).into_response(),
        Ok(Err(err)) => publish_error(err),
        Err(err) => content_error(crate::chat::ContentError::Cms(err)),
    }
}

/// Puts changes online. All of them: the version in the preview, when it
/// is the one the person saw the changes of. Some of them: a version
/// composed of the published one and those changes, built apart first.
async fn publish_site(
    State(state): State<PanelState>,
    Extension(site): Extension<Arc<SiteRuntime>>,
    Extension(person): Extension<Person>,
    Json(body): Json<PublishBody>,
) -> Response {
    if let Some(response) = publish_refused(&site) {
        return response;
    }
    let releases = site.builder.releases();
    let check_dir = site.builder.work_dir("check");
    let person_id = person.id;
    let expected = body.commit.clone();
    // A selection is composed and checked first; everything needs neither.
    let selected = match body.items {
        None => None,
        Some(items) => {
            let prepared = site
                .chat
                .read_content(move |cms| {
                    let site = cms.site().to_path_buf();
                    if crate::git::head(&site).ok().as_deref() != Some(expected.as_str()) {
                        return Ok(Err(crate::publish::PublishError::Changed));
                    }
                    let pending =
                        crate::changes::pending(&site).map_err(crate::cms::CmsError::Io)?;
                    Ok(
                        crate::selection::prepare(&site, &pending, &items, person_id, &check_dir)
                            .map_err(crate::publish::PublishError::NotReady),
                    )
                })
                .await;
            match prepared {
                Ok(Ok(prepared)) if !prepared.plan.problems.is_empty() => {
                    return error(
                        StatusCode::CONFLICT,
                        format!(
                            "these changes cannot go online by themselves: {}",
                            prepared.plan.problems[0]
                        ),
                    );
                }
                Ok(Ok(prepared)) => prepared
                    .commit
                    .map(|commit| (commit, prepared.plan.keys.len())),
                Ok(Err(err)) => return publish_error(err),
                Err(err) => return content_error(crate::chat::ContentError::Cms(err)),
            }
        }
    };

    let (published, count) = match selected {
        Some((commit, count)) => {
            let release = match site.builder.build_commit(&commit).await {
                Ok(release) => release,
                Err(reason) => {
                    return error(
                        StatusCode::CONFLICT,
                        format!("the selection could not be built: {reason}"),
                    );
                }
            };
            let releases = releases.clone();
            let outcome = site
                .chat
                .read_content(move |cms| {
                    Ok(crate::publish::go_live(
                        cms.site(),
                        &releases,
                        &release,
                        &commit,
                    ))
                })
                .await;
            match outcome {
                Ok(Ok(published)) => (published, count),
                Ok(Err(reason)) => return error(StatusCode::INTERNAL_SERVER_ERROR, reason),
                Err(err) => return content_error(crate::chat::ContentError::Cms(err)),
            }
        }
        None => {
            let commit = body.commit.clone();
            let outcome = site
                .chat
                .read_content(move |cms| {
                    let site = cms.site().to_path_buf();
                    let count = crate::changes::pending(&site)
                        .map(|pending| pending.count())
                        .unwrap_or(0);
                    Ok((crate::publish::publish(&site, &releases, &commit), count))
                })
                .await;
            match outcome {
                Ok((Ok(published), count)) => (published, count),
                Ok((Err(err), _)) => return publish_error(err),
                Err(err) => return content_error(crate::chat::ContentError::Cms(err)),
            }
        }
    };
    record_publication(
        &state,
        &site,
        person_id,
        &published,
        count,
        "site.published",
    )
    .await;
    Json(json!({ "release": published.release, "commit": published.commit, "changes": count }))
        .into_response()
}

/// Puts the publication before the last one back online.
async fn publish_back(
    State(state): State<PanelState>,
    Extension(site): Extension<Arc<SiteRuntime>>,
    Extension(person): Extension<Person>,
) -> Response {
    if let Some(response) = publish_refused(&site) {
        return response;
    }
    let releases = site.builder.releases();
    let outcome = site
        .chat
        .read_content(move |cms| Ok(crate::publish::back(cms.site(), &releases)))
        .await;
    match outcome {
        Ok(Ok(published)) => {
            record_publication(&state, &site, person.id, &published, 0, "site.went_back").await;
            Json(json!({ "release": published.release, "commit": published.commit }))
                .into_response()
        }
        Ok(Err(reason)) => error(StatusCode::CONFLICT, reason),
        Err(err) => content_error(crate::chat::ContentError::Cms(err)),
    }
}

/// The pages in tree order, and whether they can be changed now.
async fn pages_list(Extension(site): Extension<Arc<SiteRuntime>>) -> Response {
    match site.chat.read_content(|cms| cms.list()).await {
        Ok(pages) => {
            Json(json!({ "pages": pages, "locked": site.chat.content_locked() })).into_response()
        }
        Err(err) => content_error(crate::chat::ContentError::Cms(err)),
    }
}

async fn page_read(
    Extension(site): Extension<Arc<SiteRuntime>>,
    Path((_site, id)): Path<(String, String)>,
) -> Response {
    match site.chat.read_content(move |cms| cms.read(&id)).await {
        Ok(page) => Json(page).into_response(),
        Err(err) => content_error(crate::chat::ContentError::Cms(err)),
    }
}

async fn page_types(Extension(site): Extension<Arc<SiteRuntime>>) -> Response {
    match site.chat.read_content(|cms| cms.types()).await {
        Ok(types) => Json(types).into_response(),
        Err(err) => content_error(crate::chat::ContentError::Cms(err)),
    }
}

#[derive(Deserialize)]
struct PageChanges {
    changes: serde_json::Value,
}

async fn page_update(
    State(state): State<PanelState>,
    Extension(site): Extension<Arc<SiteRuntime>>,
    Extension(person): Extension<Person>,
    Path((_site, id)): Path<(String, String)>,
    Json(body): Json<PageChanges>,
) -> Response {
    let result = site
        .chat
        .edit_content(person.id, move |cms| {
            let page = cms.update(&id, &body.changes)?;
            let subject = format!(
                "Edited the page \u{201c}{}\u{201d}",
                page["title"].as_str().unwrap_or_default()
            );
            Ok((page, subject))
        })
        .await;
    match result {
        Ok(page) => {
            record_content(
                &state,
                &site,
                person.id,
                "content.page_changed",
                "page",
                &page,
            )
            .await;
            Json(page).into_response()
        }
        Err(err) => content_error(err),
    }
}

#[derive(Deserialize)]
struct NewPage {
    page: serde_json::Value,
}

async fn page_create(
    State(state): State<PanelState>,
    Extension(site): Extension<Arc<SiteRuntime>>,
    Extension(person): Extension<Person>,
    Json(body): Json<NewPage>,
) -> Response {
    let result = site
        .chat
        .edit_content(person.id, move |cms| {
            let page = cms.create(&body.page)?;
            let subject = format!(
                "Created the page \u{201c}{}\u{201d}",
                page["title"].as_str().unwrap_or_default()
            );
            Ok((page, subject))
        })
        .await;
    match result {
        Ok(page) => {
            record_content(
                &state,
                &site,
                person.id,
                "content.page_created",
                "page",
                &page,
            )
            .await;
            (StatusCode::CREATED, Json(page)).into_response()
        }
        Err(err) => content_error(err),
    }
}

async fn page_delete(
    State(state): State<PanelState>,
    Extension(site): Extension<Arc<SiteRuntime>>,
    Extension(person): Extension<Person>,
    Path((_site, id)): Path<(String, String)>,
) -> Response {
    let result = site
        .chat
        .edit_content(person.id, move |cms| {
            let page = cms.check_delete(&id)?;
            cms.delete(&id)?;
            let subject = format!(
                "Deleted the page \u{201c}{}\u{201d}",
                page["title"].as_str().unwrap_or_default()
            );
            Ok((page, subject))
        })
        .await;
    match result {
        Ok(page) => {
            record_content(
                &state,
                &site,
                person.id,
                "content.page_deleted",
                "page",
                &page,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(err) => content_error(err),
    }
}

/// The components, by name, and whether they can be changed now.
async fn components_list(Extension(site): Extension<Arc<SiteRuntime>>) -> Response {
    match site.chat.read_content(|cms| cms.list_components()).await {
        Ok(list) => Json(json!({ "components": list, "locked": site.chat.content_locked() }))
            .into_response(),
        Err(err) => content_error(crate::chat::ContentError::Cms(err)),
    }
}

async fn component_read(
    Extension(site): Extension<Arc<SiteRuntime>>,
    Path((_site, id)): Path<(String, String)>,
) -> Response {
    match site
        .chat
        .read_content(move |cms| cms.read_component(&id))
        .await
    {
        Ok(component) => Json(component).into_response(),
        Err(err) => content_error(crate::chat::ContentError::Cms(err)),
    }
}

async fn component_update(
    State(state): State<PanelState>,
    Extension(site): Extension<Arc<SiteRuntime>>,
    Extension(person): Extension<Person>,
    Path((_site, id)): Path<(String, String)>,
    Json(body): Json<PageChanges>,
) -> Response {
    let result = site
        .chat
        .edit_content(person.id, move |cms| {
            let component = cms.update_component(&id, &body.changes)?;
            let subject = format!(
                "Edited the component \u{201c}{}\u{201d}",
                component["name"].as_str().unwrap_or_default()
            );
            Ok((component, subject))
        })
        .await;
    match result {
        Ok(component) => {
            record_content(
                &state,
                &site,
                person.id,
                "content.component_changed",
                "component",
                &component,
            )
            .await;
            Json(component).into_response()
        }
        Err(err) => content_error(err),
    }
}

#[derive(Deserialize)]
struct NewComponent {
    component: serde_json::Value,
}

async fn component_create(
    State(state): State<PanelState>,
    Extension(site): Extension<Arc<SiteRuntime>>,
    Extension(person): Extension<Person>,
    Json(body): Json<NewComponent>,
) -> Response {
    let result = site
        .chat
        .edit_content(person.id, move |cms| {
            let component = cms.create_component(&body.component)?;
            let subject = format!(
                "Created the component \u{201c}{}\u{201d}",
                component["name"].as_str().unwrap_or_default()
            );
            Ok((component, subject))
        })
        .await;
    match result {
        Ok(component) => {
            record_content(
                &state,
                &site,
                person.id,
                "content.component_created",
                "component",
                &component,
            )
            .await;
            (StatusCode::CREATED, Json(component)).into_response()
        }
        Err(err) => content_error(err),
    }
}

async fn component_delete(
    State(state): State<PanelState>,
    Extension(site): Extension<Arc<SiteRuntime>>,
    Extension(person): Extension<Person>,
    Path((_site, id)): Path<(String, String)>,
) -> Response {
    let result = site
        .chat
        .edit_content(person.id, move |cms| {
            let component = cms.delete_component(&id)?;
            let subject = format!(
                "Deleted the component \u{201c}{}\u{201d}",
                component["name"].as_str().unwrap_or_default()
            );
            Ok((component, subject))
        })
        .await;
    match result {
        Ok(component) => {
            record_content(
                &state,
                &site,
                person.id,
                "content.component_deleted",
                "component",
                &component,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(err) => content_error(err),
    }
}

/// The archive of picture files of the installation.
fn archive_of(cms: &crate::cms::Cms) -> crate::media::Archive {
    crate::media::Archive::at(cms.site().parent().unwrap_or(cms.site()))
}

/// The pictures of the site, with the pages using each.
async fn media_list(Extension(site): Extension<Arc<SiteRuntime>>) -> Response {
    let listed = site
        .chat
        .read_content(|cms| {
            let mut items = crate::media::list(cms.site())?;
            for item in &mut items {
                let id = item["id"].as_str().unwrap_or_default().to_string();
                item["used_on"] = json!(crate::media::used_on(cms.site(), &id)?);
            }
            Ok(items)
        })
        .await;
    match listed {
        Ok(items) => {
            Json(json!({ "media": items, "locked": site.chat.content_locked() })).into_response()
        }
        Err(err) => content_error(crate::chat::ContentError::Cms(err)),
    }
}

#[derive(Deserialize)]
struct UploadQuery {
    #[serde(default)]
    name: String,
}

/// A picture uploaded as the body of the request, its name in the query.
async fn media_upload(
    State(state): State<PanelState>,
    Extension(site): Extension<Arc<SiteRuntime>>,
    Extension(person): Extension<Person>,
    Query(query): Query<UploadQuery>,
    body: axum::body::Bytes,
) -> Response {
    let result = site
        .chat
        .edit_content(person.id, move |cms| {
            let item = crate::media::add(cms.site(), &archive_of(cms), &query.name, &body)?;
            let subject = format!(
                "Added the picture \u{201c}{}\u{201d}",
                item["name"].as_str().unwrap_or_default()
            );
            Ok((item, subject))
        })
        .await;
    match result {
        Ok(item) => {
            record_media(&state, &site, person.id, "content.media_added", &item).await;
            (StatusCode::CREATED, Json(item)).into_response()
        }
        Err(err) => content_error(err),
    }
}

/// The file of a picture, for the panel. Files never change: it is kept.
async fn media_file(
    Extension(site): Extension<Arc<SiteRuntime>>,
    Path((_site, id)): Path<(String, String)>,
) -> Response {
    let found = site
        .chat
        .read_content(move |cms| {
            let item = crate::media::read(cms.site(), &id)?;
            let mime = item["type"].as_str().unwrap_or_default().to_string();
            let extension =
                crate::media::extension_of(&mime).ok_or(crate::cms::CmsError::NotFound)?;
            let sha256 = item["sha256"].as_str().unwrap_or_default();
            if !sha256.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(crate::cms::CmsError::NotFound);
            }
            let bytes = std::fs::read(archive_of(cms).path(sha256, extension))
                .map_err(|_| crate::cms::CmsError::NotFound)?;
            Ok((mime, bytes))
        })
        .await;
    match found {
        Ok((mime, bytes)) => (
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_str(&mime)
                        .unwrap_or(HeaderValue::from_static("application/octet-stream")),
                ),
                (
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("private, max-age=31536000, immutable"),
                ),
                (
                    header::X_CONTENT_TYPE_OPTIONS,
                    HeaderValue::from_static("nosniff"),
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(err) => content_error(crate::chat::ContentError::Cms(err)),
    }
}

#[derive(Deserialize)]
struct MediaChanges {
    changes: serde_json::Value,
}

async fn media_update(
    State(state): State<PanelState>,
    Extension(site): Extension<Arc<SiteRuntime>>,
    Extension(person): Extension<Person>,
    Path((_site, id)): Path<(String, String)>,
    Json(body): Json<MediaChanges>,
) -> Response {
    let result = site
        .chat
        .edit_content(person.id, move |cms| {
            let item = crate::media::update(cms.site(), &id, &body.changes)?;
            let subject = format!(
                "Described the picture \u{201c}{}\u{201d}",
                item["name"].as_str().unwrap_or_default()
            );
            Ok((item, subject))
        })
        .await;
    match result {
        Ok(item) => {
            record_media(&state, &site, person.id, "content.media_changed", &item).await;
            Json(item).into_response()
        }
        Err(err) => content_error(err),
    }
}

async fn media_delete(
    State(state): State<PanelState>,
    Extension(site): Extension<Arc<SiteRuntime>>,
    Extension(person): Extension<Person>,
    Path((_site, id)): Path<(String, String)>,
) -> Response {
    let result = site
        .chat
        .edit_content(person.id, move |cms| {
            let item = crate::media::delete(cms.site(), &id)?;
            let subject = format!(
                "Removed the picture \u{201c}{}\u{201d}",
                item["name"].as_str().unwrap_or_default()
            );
            Ok((item, subject))
        })
        .await;
    match result {
        Ok(item) => {
            record_media(&state, &site, person.id, "content.media_removed", &item).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(err) => content_error(err),
    }
}

async fn record_media(
    state: &PanelState,
    site: &SiteRuntime,
    person: i64,
    action: &'static str,
    item: &serde_json::Value,
) {
    let subject = format!("media:{}", item["id"].as_str().unwrap_or_default());
    log_site(
        state,
        site,
        person,
        activity::Environment::Preview,
        action,
        subject,
    )
    .await;
}

async fn chat_new(Extension(site): Extension<Arc<SiteRuntime>>) -> Response {
    match site.chat.new_conversation() {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => error(StatusCode::CONFLICT, err.to_string()),
    }
}

/// The past conversations, newest first.
async fn chat_conversations(Extension(site): Extension<Arc<SiteRuntime>>) -> Response {
    match site.chat.conversations().await {
        Ok(list) => Json(json!({ "conversations": list })).into_response(),
        Err(message) => error(StatusCode::INTERNAL_SERVER_ERROR, message),
    }
}

/// Goes back to a past conversation.
async fn chat_open_conversation(
    Extension(site): Extension<Arc<SiteRuntime>>,
    Path((_site, id)): Path<(String, i64)>,
) -> Response {
    match site.chat.open_conversation(id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(message) => error(StatusCode::CONFLICT, message),
    }
}

#[derive(Deserialize)]
struct ModeBody {
    mode: String,
}

/// Chooses the mode of the assistant's CLI for the next messages.
async fn chat_mode(
    Extension(site): Extension<Arc<SiteRuntime>>,
    Json(body): Json<ModeBody>,
) -> Response {
    match site.chat.set_mode(&body.mode).await {
        Ok(mode) => Json(json!({ "mode": mode })).into_response(),
        Err(message) => error(StatusCode::CONFLICT, message),
    }
}

/// The models the assistant in use can work with.
async fn agent_models(State(state): State<PanelState>) -> Response {
    let provider = state.account.environment().active();
    match state.account.models(provider).await {
        Ok(models) => Json(json!({ "models": models })).into_response(),
        Err(message) => error(StatusCode::SERVICE_UNAVAILABLE, message),
    }
}

/// What an assistant used, today and over the last 30 days, as its CLI
/// reported it turn by turn.
async fn agent_usage(
    State(state): State<PanelState>,
    Query(query): Query<AccountQuery>,
) -> Response {
    let provider = match provider_of(&state, query.provider.as_deref()) {
        Ok(provider) => provider,
        Err(message) => return error(StatusCode::BAD_REQUEST, message),
    };
    match crate::chat::usage(&state.db, provider).await {
        Ok(usage) => Json(usage).into_response(),
        Err(message) => error(StatusCode::INTERNAL_SERVER_ERROR, &message),
    }
}

#[derive(Deserialize)]
struct ModelBody {
    model: Option<String>,
}

/// Chooses the model for the next messages: one the assistant in use has,
/// or none for its default.
async fn chat_model(
    State(state): State<PanelState>,
    Extension(site): Extension<Arc<SiteRuntime>>,
    Json(body): Json<ModelBody>,
) -> Response {
    if let Some(model) = &body.model {
        let provider = state.account.environment().active();
        let known = match state.account.models(provider).await {
            Ok(models) => models.iter().any(|m| &m.id == model),
            Err(message) => return error(StatusCode::SERVICE_UNAVAILABLE, message),
        };
        if !known {
            return error(StatusCode::BAD_REQUEST, "this model is not available");
        }
    }
    match site.chat.set_model(body.model.clone()).await {
        Ok(()) => Json(json!({ "model": body.model })).into_response(),
        Err(message) => error(StatusCode::CONFLICT, message),
    }
}

/// Entries per page of the activity log.
const ACTIVITY_PAGE: u32 = 50;

#[derive(Deserialize)]
struct ActivityQuery {
    /// Id of the oldest entry already shown; the page continues after it.
    before: Option<i64>,
}

/// The activity log, newest first, one page at a time.
async fn activity_list(
    State(state): State<PanelState>,
    Query(query): Query<ActivityQuery>,
) -> Response {
    // One entry more than the page tells whether older ones exist.
    let result = state
        .db
        .run(move |conn| activity::list(conn, query.before, ACTIVITY_PAGE + 1))
        .await;
    match result {
        Ok(mut entries) => {
            let next = if entries.len() > ACTIVITY_PAGE as usize {
                entries.truncate(ACTIVITY_PAGE as usize);
                entries.last().map(|entry| entry.id)
            } else {
                None
            };
            Json(json!({ "entries": entries, "next": next })).into_response()
        }
        Err(err) => owner_error(err.into()),
    }
}

/// Serves a file of the panel, or `index.html` for the app's own routes.
async fn asset(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if let Some(response) = file(path) {
        return response;
    }
    if std::path::Path::new(path).extension().is_some() {
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
            data_dir = "/var/lib/alogeno"
            [hosts]
            panel = "panel.example.com"
            "#,
        )
        .unwrap()
    }

    /// A panel with an owner link ready to be redeemed.
    fn panel() -> (Router, String) {
        let (app, token, _) = panel_with_site();
        (app, token)
    }

    /// A panel with an owner link and the database of its one site, `main`
    /// (id `01TESTSITE`).
    fn panel_with_site() -> (Router, String, db::Handle) {
        let conn = db::memory_installation();
        let (token, _) = owner::owner_link(&conn, true).unwrap();
        let site_db = db::Handle::new(db::memory_site());
        let config = config();
        let (db, registry, _) = crate::registry::single(conn, site_db.clone(), &config);
        (
            router(
                &config,
                db,
                registry,
                crate::agent::idle(),
                crate::gate::AgentSlot::new(),
            ),
            token,
            site_db,
        )
    }

    const PANEL_ORIGIN: &str = "https://panel.example.com";

    struct Reply {
        status: StatusCode,
        cookie: Option<String>,
        body: serde_json::Value,
    }

    /// A request with the given browser headers, its status and headers.
    async fn from_page(
        app: &Router,
        method: &str,
        path: &str,
        cookie: Option<&str>,
        headers: &[(&str, &str)],
    ) -> (StatusCode, HeaderMap) {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        (response.status(), response.headers().clone())
    }

    #[tokio::test]
    async fn the_preview_cannot_drive_the_panel() {
        let (app, token) = panel();
        let cookie = sign_up(&app, &token).await;
        let preview = [
            ("origin", "https://preview.example.com"),
            ("sec-fetch-site", "same-site"),
        ];
        for (method, path) in [
            ("POST", "/api/sites/01TESTSITE/publish/back"),
            ("POST", "/api/update"),
            ("GET", "/api/agent/models"),
            ("GET", "/api/auth/me"),
        ] {
            let (status, _) = from_page(&app, method, path, Some(&cookie), &preview).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
        }
        // A form or a fetch without Origin still names the site.
        let (status, _) = from_page(
            &app,
            "POST",
            "/api/sites/01TESTSITE/publish/back",
            Some(&cookie),
            &[("sec-fetch-site", "same-site")],
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        // A WebSocket from another origin.
        let (status, _) = from_page(
            &app,
            "GET",
            "/api/sites/01TESTSITE/terminal",
            Some(&cookie),
            &[
                ("origin", "https://preview.example.com"),
                ("connection", "upgrade"),
                ("upgrade", "websocket"),
                ("sec-websocket-version", "13"),
                ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn the_panel_and_programs_pass_the_origin_check() {
        let (app, token) = panel();
        let cookie = sign_up(&app, &token).await;
        let (status, _) = from_page(
            &app,
            "GET",
            "/api/auth/me",
            Some(&cookie),
            &[("origin", PANEL_ORIGIN), ("sec-fetch-site", "same-origin")],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        // curl and health checks send neither header.
        let (status, _) = from_page(&app, "GET", "/api/public", None, &[]).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = from_page(&app, "GET", "/api/auth/me", Some(&cookie), &[]).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn the_panel_stays_out_of_frames() {
        let (app, _) = panel();
        for path in ["/", "/api/public"] {
            let (_, headers) = from_page(&app, "GET", path, None, &[]).await;
            assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY", "{path}");
            assert_eq!(
                headers[header::CONTENT_SECURITY_POLICY],
                "frame-ancestors 'none'",
                "{path}"
            );
        }
    }

    async fn send(
        app: &Router,
        method: &str,
        path: &str,
        cookie: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> Reply {
        // As a browser on a page of the panel sends it.
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::ORIGIN, PANEL_ORIGIN)
            .header("sec-fetch-site", "same-origin");
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
            Some(json!({ "token": token, "password": "correct horse battery" })),
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        assert!(reply.body["id"].is_i64(), "{}", reply.body);
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
        assert_eq!(me.body, json!({ "id": 1 }));
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
            Some(json!({ "token": token, "password": "correct horse battery" })),
        )
        .await;
        let cookie = reply.cookie.unwrap();
        assert!(cookie.starts_with("__Host-alogeno_session="), "{cookie}");
        for attribute in ["HttpOnly", "SameSite=Strict", "Secure", "Path=/"] {
            assert!(cookie.contains(attribute), "{cookie} lacks {attribute}");
        }
    }

    #[tokio::test]
    async fn the_only_site_and_unknown_ones_are_not_deleted() {
        let (app, token) = panel();
        let cookie = sign_up(&app, &token).await;
        let unknown = send(&app, "DELETE", "/api/sites/NOPE", Some(&cookie), None).await;
        assert_eq!(unknown.status, StatusCode::NOT_FOUND);
        let only = send(&app, "DELETE", "/api/sites/01TESTSITE", Some(&cookie), None).await;
        assert_eq!(only.status, StatusCode::CONFLICT);
        assert!(
            only.body["error"]
                .as_str()
                .unwrap()
                .contains("at least one site")
        );
        let sites = send(&app, "GET", "/api/sites", Some(&cookie), None).await;
        assert_eq!(sites.body.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_site_s_domains_are_added_listed_and_removed() {
        let (app, token) = panel();
        let cookie = sign_up(&app, &token).await;
        let path = "/api/sites/01TESTSITE/domains";
        let added = send(
            &app,
            "POST",
            path,
            Some(&cookie),
            Some(json!({ "host": "Shop.example", "www": true })),
        )
        .await;
        assert_eq!(added.status, StatusCode::CREATED, "{:?}", added.body);
        assert_eq!(added.body.as_array().unwrap().len(), 2);

        let listed = send(&app, "GET", path, Some(&cookie), None).await;
        assert_eq!(listed.status, StatusCode::OK);
        let hosts: Vec<_> = listed.body["domains"]
            .as_array()
            .unwrap()
            .iter()
            .map(|domain| domain["host"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(hosts, ["shop.example", "www.shop.example"]);
        let sites = send(&app, "GET", "/api/sites", Some(&cookie), None).await;
        assert_eq!(sites.body[0]["site_hosts"], json!(["shop.example"]));

        for (host, status) in [
            ("panel.example.com", StatusCode::BAD_REQUEST),
            ("shop.example", StatusCode::CONFLICT),
            ("www.shop.example", StatusCode::CONFLICT),
        ] {
            let refused = send(
                &app,
                "POST",
                path,
                Some(&cookie),
                Some(json!({ "host": host })),
            )
            .await;
            assert_eq!(refused.status, status, "{host}");
        }
        let unknown = send(&app, "GET", "/api/sites/NOPE/domains", Some(&cookie), None).await;
        assert_eq!(unknown.status, StatusCode::NOT_FOUND);

        let removed = send(
            &app,
            "DELETE",
            &format!("{path}/shop.example"),
            Some(&cookie),
            None,
        )
        .await;
        assert_eq!(removed.status, StatusCode::NO_CONTENT);
        let listed = send(&app, "GET", path, Some(&cookie), None).await;
        assert_eq!(listed.body["domains"], json!([]));
        let gone = send(
            &app,
            "POST",
            &format!("{path}/shop.example/check"),
            Some(&cookie),
            None,
        )
        .await;
        assert_eq!(gone.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_panel_s_domain_is_public() {
        let (app, _) = panel();
        let reply = send(&app, "GET", "/api/public", None, None).await;
        assert_eq!(reply.body["site"], "panel.example.com");
        assert_eq!(reply.body["has_owner"], false);
    }

    #[tokio::test]
    async fn invalid_fields_are_named() {
        let (app, token) = panel();
        let reply = send(
            &app,
            "POST",
            "/api/auth/setup",
            None,
            Some(json!({ "token": token, "password": "short" })),
        )
        .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST);
        assert_eq!(reply.body["field"], "password");
        sign_up(&app, &token).await;
        let public = send(&app, "GET", "/api/public", None, None).await;
        assert_eq!(public.body["has_owner"], true);
    }

    #[tokio::test]
    async fn private_api_needs_a_session() {
        let (app, token) = panel();
        let anonymous = send(&app, "GET", "/api/installation", None, None).await;
        assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
        let cookie = sign_up(&app, &token).await;
        let signed_in = send(&app, "GET", "/api/installation", Some(&cookie), None).await;
        assert_eq!(signed_in.body["panel"], "panel.example.com");
        let sites = send(&app, "GET", "/api/sites", Some(&cookie), None).await;
        assert_eq!(sites.body[0]["slug"], "main");
        assert_eq!(
            sites.body[0]["preview_host"],
            "main.preview.panel.example.com"
        );
    }

    #[tokio::test]
    async fn allowed_hosts_are_listed_and_taken_back() {
        let (app, token, site_db) = panel_with_site();
        let cookie = sign_up(&app, &token).await;
        site_db
            .run(|conn| {
                conn.execute(
                    "INSERT INTO allowed_host (host, approved_by) VALUES ('api.example.org', 1)",
                    [],
                )
            })
            .await
            .unwrap();
        let listed = send(
            &app,
            "GET",
            "/api/sites/01TESTSITE/agent/hosts",
            Some(&cookie),
            None,
        )
        .await;
        assert_eq!(listed.status, StatusCode::OK);
        assert_eq!(listed.body["hosts"][0]["host"], "api.example.org");
        let path = "/api/sites/01TESTSITE/agent/hosts/api.example.org";
        let removed = send(&app, "DELETE", path, Some(&cookie), None).await;
        assert_eq!(removed.status, StatusCode::NO_CONTENT);
        let again = send(&app, "DELETE", path, Some(&cookie), None).await;
        assert_eq!(again.status, StatusCode::NOT_FOUND);
        let listed = send(
            &app,
            "GET",
            "/api/sites/01TESTSITE/agent/hosts",
            Some(&cookie),
            None,
        )
        .await;
        assert_eq!(listed.body["hosts"], json!([]));
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
            Some(json!({ "password": "correct horse battery" })),
        )
        .await;
        assert_eq!(ok.status, StatusCode::OK);
        for _ in 0..LOGIN_ATTEMPTS {
            let wrong = send(
                &app,
                "POST",
                "/api/auth/login",
                None,
                Some(json!({ "password": "wrong password" })),
            )
            .await;
            assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
        }
        let blocked = send(
            &app,
            "POST",
            "/api/auth/login",
            None,
            Some(json!({ "password": "correct horse battery" })),
        )
        .await;
        assert_eq!(blocked.status, StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn state_changes_need_a_json_body() {
        let (app, _) = panel();
        let request = Request::post("/api/auth/login")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from("password=x"))
            .unwrap();
        let status = app.oneshot(request).await.unwrap().status();
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn the_activity_log_shows_the_setup() {
        let (app, token) = panel();
        let owner = sign_up(&app, &token).await;
        let log = send(&app, "GET", "/api/activity", Some(&owner), None).await;
        assert_eq!(log.status, StatusCode::OK);
        let entries = log.body["entries"].as_array().unwrap();
        assert_eq!(entries[0]["action"], "person.setup");
        assert_eq!(log.body["next"], serde_json::Value::Null);
    }
}
