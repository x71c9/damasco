//! The control panel: the single-page app built from `panel/` and its API.
//!
//! The API answers JSON under `/api/`. Every route except the ones needed to
//! sign in requires a session cookie. Requests that change state accept only
//! `application/json` bodies and the cookie is `SameSite=Strict`, so another
//! site cannot submit them on behalf of a signed-in person.

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
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
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

use crate::access::{self, Action, Resource, Role, Scope};
use crate::activity;
use crate::chat::{Event as ChatEvent, SendError};
use crate::config::{Config, Proxy};
use crate::db;
use crate::limit::{self, Attempts};
use crate::people::{self, Actor, PeopleError, Purpose, Redeem};
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

/// Wrong sign-ins allowed per address and per account in the window.
const LOGIN_ATTEMPTS: usize = 10;
const LOGIN_WINDOW: Duration = Duration::from_secs(15 * 60);

#[derive(Clone)]
struct PanelState {
    db: db::Handle,
    installation: Arc<Installation>,
    proxy: Arc<Proxy>,
    attempts: Arc<Attempts>,
    /// Public address of the panel, to build one-time links.
    panel_url: Arc<str>,
    builder: Arc<crate::build::Builder>,
    account: Arc<crate::agent::Account>,
    chat: Arc<crate::chat::Chat>,
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

pub fn router(
    config: &Config,
    db: db::Handle,
    builder: Arc<crate::build::Builder>,
    account: Arc<crate::agent::Account>,
    chat: Arc<crate::chat::Chat>,
) -> Router {
    let state = PanelState {
        db,
        installation: Arc::new(Installation {
            preview_host: config.hosts.preview.clone(),
            site_hosts: config.hosts.site.clone(),
        }),
        proxy: Arc::new(config.proxy.clone()),
        attempts: Arc::new(Attempts::new(LOGIN_ATTEMPTS, LOGIN_WINDOW)),
        panel_url: Arc::from(config.panel_url()),
        builder,
        account,
        chat,
        secure: config.hosts.scheme == "https",
    };
    let private = Router::new()
        .route("/api/installation", get(installation))
        .route("/api/people", get(people_list).post(people_invite))
        .route("/api/people/{id}/role", post(people_role))
        .route("/api/people/{id}/link", post(people_link))
        .route("/api/people/{id}/revoke", post(people_revoke))
        .route("/api/activity", get(activity_list))
        .route("/api/build", get(build_status))
        .route("/api/agent/account", get(agent_account))
        .route("/api/agent/login", post(agent_login))
        .route("/api/agent/login/code", post(agent_login_code))
        .route("/api/agent/logout", post(agent_logout))
        .route("/api/chat/events", get(chat_events))
        .route("/api/chat/messages", post(chat_send))
        .route("/api/chat/stop", post(chat_stop))
        .route("/api/chat/new", post(chat_new))
        .route("/api/chat/take-over", post(chat_take_over))
        .route("/api/chat/retry", post(chat_retry))
        .route("/api/chat/back/{id}", post(chat_go_back))
        .route("/api/chat/undo/{id}", post(chat_undo_back))
        .route("/api/terminal", get(terminal_socket))
        .route("/api/pages", get(pages_list).post(page_create))
        .route(
            "/api/pages/{id}",
            get(page_read).patch(page_update).delete(page_delete),
        )
        .route("/api/page-types", get(page_types))
        .route(
            "/api/media",
            get(media_list)
                .post(media_upload)
                .layer(axum::extract::DefaultBodyLimit::max(
                    crate::media::LIMIT + 1024,
                )),
        )
        .route(
            "/api/media/{id}",
            axum::routing::patch(media_update).delete(media_delete),
        )
        .route("/api/media/{id}/file", get(media_file))
        .route("/api/terminal/close", post(terminal_close))
        .route("/api/chat/permissions/{id}", post(chat_permission))
        .route("/api/chat/questions/{id}", post(chat_answers))
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
        .with_state(state)
        .fallback(asset)
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

fn people_error(err: PeopleError) -> Response {
    if let Some(field) = err.field() {
        let status = if matches!(err, PeopleError::EmailTaken) {
            StatusCode::CONFLICT
        } else {
            StatusCode::BAD_REQUEST
        };
        let body = json!({ "error": err.to_string(), "field": field });
        return (status, Json(body)).into_response();
    }
    match err {
        PeopleError::InvalidLink => error(StatusCode::GONE, err.to_string()),
        PeopleError::Forbidden(_) => error(StatusCode::FORBIDDEN, err.to_string()),
        PeopleError::NotFound => error(StatusCode::NOT_FOUND, err.to_string()),
        PeopleError::WrongCredentials => error(StatusCode::UNAUTHORIZED, err.to_string()),
        PeopleError::EmailTaken | PeopleError::Invalid { .. } => {
            error(StatusCode::BAD_REQUEST, err.to_string())
        }
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

/// What the sign-in page shows before anyone signs in: the site's domain,
/// and whether the owner exists, so that a new installation explains how to
/// create it instead of asking for a password nobody has.
async fn public(State(state): State<PanelState>) -> Response {
    match state.db.run(people::has_owner).await {
        Ok(has_owner) => Json(json!({
            "site": state.installation.site_hosts.first(),
            "has_owner": has_owner,
        }))
        .into_response(),
        Err(err) => people_error(err.into()),
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
        .run(move |conn| people::find_link(conn, &body.token))
        .await
    {
        Ok(found) => {
            let purpose = match found.purpose {
                Purpose::Setup => "setup",
                Purpose::Reset => "reset",
            };
            Json(json!({ "purpose": purpose, "name": found.name })).into_response()
        }
        Err(err) => people_error(err),
    }
}

#[derive(Deserialize)]
struct SetupBody {
    token: String,
    email: Option<String>,
    name: Option<String>,
    password: String,
}

/// Redeems a one-time link and signs the person in.
async fn setup(State(state): State<PanelState>, Json(body): Json<SetupBody>) -> Response {
    let result = state
        .db
        .run(move |conn| {
            let input = Redeem {
                email: body.email.as_deref(),
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
    let mut response = me_json(me);
    response
        .headers_mut()
        .insert(header::SET_COOKIE, state.session_cookie(token));
    response
}

#[derive(Deserialize)]
struct LoginBody {
    email: String,
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
    let account = format!("email:{}", body.email.trim().to_lowercase());
    if !state.attempts.allowed(&address) || !state.attempts.allowed(&account) {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "too many attempts; try again in a few minutes",
        );
    }
    let result = state
        .db
        .run(move |conn| {
            let token = session::login(conn, &body.email, &body.password)?;
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
        Ok(Some(person)) => me_json(person),
        Ok(None) => error(StatusCode::UNAUTHORIZED, "sign in first"),
        Err(err) => people_error(err.into()),
    }
}

/// The signed-in person with the capabilities of their role, so that the
/// panel shows only the sections the person can use.
fn me_json(person: Person) -> Response {
    let capabilities = Role::parse(&person.role)
        .map(access::capabilities)
        .unwrap_or_default();
    Json(json!({
        "id": person.id,
        "email": person.email,
        "name": person.name,
        "role": person.role,
        "capabilities": capabilities,
    }))
    .into_response()
}

fn actor(person: &Person) -> Actor {
    Actor {
        id: person.id,
        // An unknown role gets the fewest capabilities.
        role: Role::parse(&person.role).unwrap_or(Role::Editor),
    }
}

fn link_url(state: &PanelState, token: &str) -> String {
    format!("{}/setup#{token}", state.panel_url)
}

fn parse_role(value: &str) -> Result<Role, PeopleError> {
    Role::parse(value).ok_or_else(|| PeopleError::Invalid {
        field: "role",
        message: "unknown role".into(),
    })
}

async fn people_list(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
) -> Response {
    let role = actor(&person).role;
    if !access::allows(role, Scope::Installation, Resource::People, Action::Read) {
        return error(
            StatusCode::FORBIDDEN,
            "managing people needs the owner or an administrator",
        );
    }
    match state.db.run(people::list).await {
        Ok(list) => Json(list).into_response(),
        Err(err) => people_error(err.into()),
    }
}

#[derive(Deserialize)]
struct InviteBody {
    name: String,
    role: String,
}

/// Creates an invitation and returns the link to hand over.
async fn people_invite(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
    Json(body): Json<InviteBody>,
) -> Response {
    let role = match parse_role(&body.role) {
        Ok(role) => role,
        Err(err) => return people_error(err),
    };
    let actor = actor(&person);
    match state
        .db
        .run(move |conn| people::invite(conn, actor, &body.name, role))
        .await
    {
        Ok((id, token)) => {
            Json(json!({ "id": id, "link": link_url(&state, &token) })).into_response()
        }
        Err(err) => people_error(err),
    }
}

#[derive(Deserialize)]
struct RoleBody {
    role: String,
}

async fn people_role(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
    Path(id): Path<i64>,
    Json(body): Json<RoleBody>,
) -> Response {
    let role = match parse_role(&body.role) {
        Ok(role) => role,
        Err(err) => return people_error(err),
    };
    let actor = actor(&person);
    match state
        .db
        .run(move |conn| people::change_role(conn, actor, id, role))
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => people_error(err),
    }
}

/// A new one-time link for someone else: a fresh invitation or a reset.
async fn people_link(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
    Path(id): Path<i64>,
) -> Response {
    let actor = actor(&person);
    match state
        .db
        .run(move |conn| people::link_for(conn, actor, id))
        .await
    {
        Ok((token, purpose)) => {
            let purpose = match purpose {
                Purpose::Setup => "setup",
                Purpose::Reset => "reset",
            };
            Json(json!({ "purpose": purpose, "link": link_url(&state, &token) })).into_response()
        }
        Err(err) => people_error(err),
    }
}

async fn people_revoke(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
    Path(id): Path<i64>,
) -> Response {
    let actor = actor(&person);
    match state
        .db
        .run(move |conn| people::revoke(conn, actor, id))
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => people_error(err),
    }
}

/// State of the builds and the release the preview shows.
async fn build_status(State(state): State<PanelState>) -> Json<crate::build::Status> {
    Json(state.builder.status())
}

/// The agent's account is for those who use the chat: owner and
/// administrators.
fn chat_refused(person: &Person) -> Option<Response> {
    if access::allows(
        actor(person).role,
        Scope::Preview,
        Resource::Chat,
        Action::Write,
    ) {
        None
    } else {
        Some(error(
            StatusCode::FORBIDDEN,
            "the assistant is for the owner and administrators",
        ))
    }
}

async fn agent_account(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
) -> Response {
    if let Some(response) = chat_refused(&person) {
        return response;
    }
    Json(state.account.status().await).into_response()
}

/// Starts the sign-in of the agent's account and returns the address of the
/// sign-in page to open.
async fn agent_login(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
) -> Response {
    if let Some(response) = chat_refused(&person) {
        return response;
    }
    match state.account.start_login().await {
        Ok(url) => Json(json!({ "url": url })).into_response(),
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
    if let Some(response) = chat_refused(&person) {
        return response;
    }
    match state.account.finish_login(&body.code).await {
        Ok(()) => {
            let id = person.id;
            let _ = state
                .db
                .run(move |conn| {
                    activity::record(
                        conn,
                        Some(id),
                        activity::Environment::Installation,
                        "agent.signed_in",
                        None,
                    )
                })
                .await;
            Json(state.account.status().await).into_response()
        }
        Err(message) => {
            let body = json!({ "error": message, "field": "code" });
            (StatusCode::BAD_REQUEST, Json(body)).into_response()
        }
    }
}

async fn agent_logout(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
) -> Response {
    if let Some(response) = chat_refused(&person) {
        return response;
    }
    match state.account.logout().await {
        Ok(()) => {
            let id = person.id;
            let _ = state
                .db
                .run(move |conn| {
                    activity::record(
                        conn,
                        Some(id),
                        activity::Environment::Installation,
                        "agent.signed_out",
                        None,
                    )
                })
                .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(message) => error(StatusCode::SERVICE_UNAVAILABLE, message),
    }
}

/// The chat as server-sent events: a snapshot first, then every change. A
/// `reset` event asks the panel to connect again for a new snapshot.
async fn chat_events(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
) -> Response {
    if let Some(response) = chat_refused(&person) {
        return response;
    }
    // Subscribed before the snapshot is taken, so nothing falls in between;
    // the panel ignores what the snapshot already has.
    let receiver = state.chat.subscribe();
    let snapshot = state.chat.snapshot().await;
    let first = stream::once(async move { sse_event(&ChatEvent::Snapshot(snapshot)) });
    let rest = stream::unfold(receiver, |mut receiver| async move {
        let event = match receiver.recv().await {
            Ok(event) => event,
            Err(broadcast::error::RecvError::Lagged(_)) => ChatEvent::Reset,
            Err(broadcast::error::RecvError::Closed) => return None,
        };
        Some((sse_event(&event), receiver))
    });
    Sse::new(first.chain(rest).take_until(state.chat.closing()))
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
    Extension(person): Extension<Person>,
    Json(body): Json<MessageBody>,
) -> Response {
    if let Some(response) = chat_refused(&person) {
        return response;
    }
    match state.chat.send(person.id, &person.name, &body.text).await {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(err @ (SendError::Busy | SendError::InUse(_) | SendError::Paused)) => {
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

async fn chat_stop(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
) -> Response {
    if let Some(response) = chat_refused(&person) {
        return response;
    }
    match state.chat.stop_by(person.id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(message) => error(StatusCode::CONFLICT, message),
    }
}

#[derive(Deserialize)]
struct PermissionBody {
    allow: bool,
}

/// A person answers a permission request of the assistant. The answer is
/// recorded in the activity log.
async fn chat_permission(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
    Path(id): Path<i64>,
    Json(body): Json<PermissionBody>,
) -> Response {
    if let Some(response) = chat_refused(&person) {
        return response;
    }
    if let Err(message) = state
        .chat
        .answer_permission(person.id, id, body.allow)
        .await
    {
        return error(StatusCode::CONFLICT, message);
    }
    let action = if body.allow {
        "agent.permission_allowed"
    } else {
        "agent.permission_denied"
    };
    let actor = person.id;
    let _ = state
        .db
        .run(move |conn| {
            activity::record(
                conn,
                Some(actor),
                activity::Environment::Preview,
                action,
                Some(&format!("chat_item:{id}")),
            )
        })
        .await;
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
struct AnswersBody {
    answers: Vec<crate::chat::Answer>,
}

/// A person answers the questions of the assistant.
async fn chat_answers(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
    Path(id): Path<i64>,
    Json(body): Json<AnswersBody>,
) -> Response {
    if let Some(response) = chat_refused(&person) {
        return response;
    }
    match state
        .chat
        .answer_questions(person.id, id, body.answers)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(message) => error(StatusCode::CONFLICT, message),
    }
}

/// A person takes the chat from the one holding it, once the turn is over.
/// Recorded in the activity log.
async fn chat_take_over(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
) -> Response {
    if let Some(response) = chat_refused(&person) {
        return response;
    }
    match state.chat.take_over(person.id, &person.name) {
        Ok(previous) => {
            if let Some(previous) = previous {
                let actor = person.id;
                let _ = state
                    .db
                    .run(move |conn| {
                        activity::record(
                            conn,
                            Some(actor),
                            activity::Environment::Installation,
                            "chat.taken_over",
                            Some(&format!("person:{previous}")),
                        )
                    })
                    .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(err) => error(StatusCode::CONFLICT, err.to_string()),
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
    Extension(person): Extension<Person>,
    Query(size): Query<TerminalSize>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if let Some(response) = chat_refused(&person) {
        return response;
    }
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
    match state
        .chat
        .open_terminal(person.id, &person.name, size.cols, size.rows, &size.theme)
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
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
) -> Response {
    if let Some(response) = chat_refused(&person) {
        return response;
    }
    match state.chat.close_terminal(person.id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(message) => error(StatusCode::CONFLICT, message),
    }
}

/// Takes the site back to before a message; answers with the message, for
/// the person to change it.
async fn chat_go_back(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
    Path(id): Path<i64>,
) -> Response {
    if let Some(response) = chat_refused(&person) {
        return response;
    }
    match state.chat.go_back(person.id, id).await {
        Ok(text) => Json(json!({ "text": text })).into_response(),
        Err(message) => error(StatusCode::CONFLICT, message),
    }
}

/// Undoes a going back.
async fn chat_undo_back(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
    Path(id): Path<i64>,
) -> Response {
    if let Some(response) = chat_refused(&person) {
        return response;
    }
    match state.chat.undo_back(person.id, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(message) => error(StatusCode::CONFLICT, message),
    }
}

/// Tries the last request again after something went wrong.
async fn chat_retry(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
) -> Response {
    if let Some(response) = chat_refused(&person) {
        return response;
    }
    match state.chat.retry(person.id, &person.name).await {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(err) => error(StatusCode::CONFLICT, err.to_string()),
    }
}

/// Pages are for every role, in the preview.
fn content_refused(person: &Person, action: Action) -> Option<Response> {
    if access::allows(
        actor(person).role,
        Scope::Preview,
        Resource::Content,
        action,
    ) {
        None
    } else {
        Some(error(StatusCode::FORBIDDEN, "pages are not for your role"))
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

async fn record_content(
    state: &PanelState,
    person: i64,
    action: &'static str,
    page: &serde_json::Value,
) {
    let subject = format!("page:{}", page["id"].as_str().unwrap_or_default());
    let _ = state
        .db
        .run(move |conn| {
            activity::record(
                conn,
                Some(person),
                activity::Environment::Preview,
                action,
                Some(&subject),
            )
        })
        .await;
}

/// The pages in tree order, and whether they can be changed now.
async fn pages_list(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
) -> Response {
    if let Some(response) = content_refused(&person, Action::Read) {
        return response;
    }
    match state.chat.read_content(|cms| cms.list()).await {
        Ok(pages) => {
            Json(json!({ "pages": pages, "locked": state.chat.content_locked() })).into_response()
        }
        Err(err) => content_error(crate::chat::ContentError::Cms(err)),
    }
}

async fn page_read(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
    Path(id): Path<String>,
) -> Response {
    if let Some(response) = content_refused(&person, Action::Read) {
        return response;
    }
    match state.chat.read_content(move |cms| cms.read(&id)).await {
        Ok(page) => Json(page).into_response(),
        Err(err) => content_error(crate::chat::ContentError::Cms(err)),
    }
}

async fn page_types(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
) -> Response {
    if let Some(response) = content_refused(&person, Action::Read) {
        return response;
    }
    match state.chat.read_content(|cms| cms.types()).await {
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
    Extension(person): Extension<Person>,
    Path(id): Path<String>,
    Json(body): Json<PageChanges>,
) -> Response {
    if let Some(response) = content_refused(&person, Action::Write) {
        return response;
    }
    let result = state
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
            record_content(&state, person.id, "content.page_changed", &page).await;
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
    Extension(person): Extension<Person>,
    Json(body): Json<NewPage>,
) -> Response {
    if let Some(response) = content_refused(&person, Action::Write) {
        return response;
    }
    let result = state
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
            record_content(&state, person.id, "content.page_created", &page).await;
            (StatusCode::CREATED, Json(page)).into_response()
        }
        Err(err) => content_error(err),
    }
}

async fn page_delete(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
    Path(id): Path<String>,
) -> Response {
    if let Some(response) = content_refused(&person, Action::Write) {
        return response;
    }
    let result = state
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
            record_content(&state, person.id, "content.page_deleted", &page).await;
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
async fn media_list(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
) -> Response {
    if let Some(response) = content_refused(&person, Action::Read) {
        return response;
    }
    let listed = state
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
            Json(json!({ "media": items, "locked": state.chat.content_locked() })).into_response()
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
    Extension(person): Extension<Person>,
    Query(query): Query<UploadQuery>,
    body: axum::body::Bytes,
) -> Response {
    if let Some(response) = content_refused(&person, Action::Write) {
        return response;
    }
    let result = state
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
            record_media(&state, person.id, "content.media_added", &item).await;
            (StatusCode::CREATED, Json(item)).into_response()
        }
        Err(err) => content_error(err),
    }
}

/// The file of a picture, for the panel. Files never change: it is kept.
async fn media_file(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
    Path(id): Path<String>,
) -> Response {
    if let Some(response) = content_refused(&person, Action::Read) {
        return response;
    }
    let found = state
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
    Extension(person): Extension<Person>,
    Path(id): Path<String>,
    Json(body): Json<MediaChanges>,
) -> Response {
    if let Some(response) = content_refused(&person, Action::Write) {
        return response;
    }
    let result = state
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
            record_media(&state, person.id, "content.media_changed", &item).await;
            Json(item).into_response()
        }
        Err(err) => content_error(err),
    }
}

async fn media_delete(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
    Path(id): Path<String>,
) -> Response {
    if let Some(response) = content_refused(&person, Action::Write) {
        return response;
    }
    let result = state
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
            record_media(&state, person.id, "content.media_removed", &item).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(err) => content_error(err),
    }
}

async fn record_media(
    state: &PanelState,
    person: i64,
    action: &'static str,
    item: &serde_json::Value,
) {
    let subject = format!("media:{}", item["id"].as_str().unwrap_or_default());
    let _ = state
        .db
        .run(move |conn| {
            activity::record(
                conn,
                Some(person),
                activity::Environment::Preview,
                action,
                Some(&subject),
            )
        })
        .await;
}

async fn chat_new(
    State(state): State<PanelState>,
    Extension(person): Extension<Person>,
) -> Response {
    if let Some(response) = chat_refused(&person) {
        return response;
    }
    match state.chat.new_conversation(person.id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => error(StatusCode::CONFLICT, err.to_string()),
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
    Extension(person): Extension<Person>,
    Query(query): Query<ActivityQuery>,
) -> Response {
    let role = actor(&person).role;
    if !access::allows(role, Scope::Installation, Resource::Activity, Action::Read) {
        return error(
            StatusCode::FORBIDDEN,
            "the activity log is for the owner and administrators",
        );
    }
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
        Err(err) => people_error(err.into()),
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
        let (token, _) = people::owner_link(&conn, true).unwrap();
        let db = db::Handle::new(conn);
        (
            router(
                &config(),
                db.clone(),
                crate::build::idle(std::path::Path::new("/nonexistent")),
                crate::agent::idle(),
                crate::chat::idle(db),
            ),
            token,
        )
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
            Some(json!({ "token": token, "email": "giulia@example.com", "name": "Giulia", "password": "correct horse battery" })),
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
        assert_eq!(me.body["email"], "giulia@example.com");
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
            Some(json!({ "token": token, "email": "giulia@example.com", "name": "Giulia", "password": "correct horse battery" })),
        )
        .await;
        let cookie = reply.cookie.unwrap();
        assert!(cookie.starts_with("__Host-axioma_session="), "{cookie}");
        for attribute in ["HttpOnly", "SameSite=Strict", "Secure", "Path=/"] {
            assert!(cookie.contains(attribute), "{cookie} lacks {attribute}");
        }
    }

    #[tokio::test]
    async fn the_site_domain_is_public() {
        let (app, _) = panel();
        let reply = send(&app, "GET", "/api/public", None, None).await;
        assert_eq!(reply.body["site"], "example.com");
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
            Some(json!({ "token": token, "email": "not an email", "name": "Giulia", "password": "correct horse battery" })),
        )
        .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST);
        assert_eq!(reply.body["field"], "email");
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
            Some(json!({ "email": "giulia@example.com", "password": "correct horse battery" })),
        )
        .await;
        assert_eq!(ok.status, StatusCode::OK);
        for _ in 0..LOGIN_ATTEMPTS {
            let wrong = send(
                &app,
                "POST",
                "/api/auth/login",
                None,
                Some(json!({ "email": "giulia@example.com", "password": "wrong password" })),
            )
            .await;
            assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
        }
        let blocked = send(
            &app,
            "POST",
            "/api/auth/login",
            None,
            Some(json!({ "email": "giulia@example.com", "password": "correct horse battery" })),
        )
        .await;
        assert_eq!(blocked.status, StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn state_changes_need_a_json_body() {
        let (app, _) = panel();
        let request = Request::post("/api/auth/login")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from("email=giulia@example.com&password=x"))
            .unwrap();
        let status = app.oneshot(request).await.unwrap().status();
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn people_api_invites_and_restricts_editors() {
        let (app, token) = panel();
        let owner = sign_up(&app, &token).await;
        let me = send(&app, "GET", "/api/auth/me", Some(&owner), None).await;
        let caps = me.body["capabilities"].as_array().unwrap();
        assert!(
            caps.iter()
                .any(|c| c["resource"] == "people" && c["action"] == "write")
        );

        let invite = send(
            &app,
            "POST",
            "/api/people",
            Some(&owner),
            Some(json!({ "name": "Marco", "role": "editor" })),
        )
        .await;
        assert_eq!(invite.status, StatusCode::OK, "{}", invite.body);
        let link = invite.body["link"].as_str().unwrap();
        assert!(
            link.starts_with("https://panel.example.com/setup#"),
            "{link}"
        );
        let token = link.rsplit('#').next().unwrap();
        let info = send(
            &app,
            "POST",
            "/api/auth/link",
            None,
            Some(json!({ "token": token })),
        )
        .await;
        assert_eq!(info.body["name"], "Marco");

        let joined = send(&app, "POST", "/api/auth/setup", None, Some(json!({ "token": token, "email": "marco@example.com", "name": "Marco", "password": "another long password" }))).await;
        let editor = pair(&joined.cookie.unwrap());
        let list = send(&app, "GET", "/api/people", Some(&editor), None).await;
        assert_eq!(list.status, StatusCode::FORBIDDEN);
        let me = send(&app, "GET", "/api/auth/me", Some(&editor), None).await;
        assert!(
            !me.body["capabilities"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["resource"] == "people")
        );

        let list = send(&app, "GET", "/api/people", Some(&owner), None).await;
        let rows = list.body.as_array().unwrap();
        assert_eq!((rows.len(), rows[0]["role"].as_str()), (2, Some("owner")));
        let id = rows[1]["id"].as_i64().unwrap();
        let revoked = send(
            &app,
            "POST",
            &format!("/api/people/{id}/revoke"),
            Some(&owner),
            None,
        )
        .await;
        assert_eq!(revoked.status, StatusCode::NO_CONTENT);
        let after = send(&app, "GET", "/api/auth/me", Some(&editor), None).await;
        assert_eq!(after.status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn activity_is_for_owner_and_admins() {
        let (app, token) = panel();
        let owner = sign_up(&app, &token).await;
        let log = send(&app, "GET", "/api/activity", Some(&owner), None).await;
        assert_eq!(log.status, StatusCode::OK);
        let entries = log.body["entries"].as_array().unwrap();
        assert_eq!(entries[0]["action"], "person.setup");
        assert_eq!(entries[0]["actor_name"], "Giulia");
        assert_eq!(log.body["next"], serde_json::Value::Null);

        let invite = send(
            &app,
            "POST",
            "/api/people",
            Some(&owner),
            Some(json!({ "name": "Marco", "role": "editor" })),
        )
        .await;
        let link = invite.body["link"].as_str().unwrap();
        let token = link.rsplit('#').next().unwrap();
        let joined = send(&app, "POST", "/api/auth/setup", None, Some(json!({ "token": token, "email": "marco@example.com", "name": "Marco", "password": "another long password" }))).await;
        let editor = pair(&joined.cookie.unwrap());
        let denied = send(&app, "GET", "/api/activity", Some(&editor), None).await;
        assert_eq!(denied.status, StatusCode::FORBIDDEN);
    }
}
