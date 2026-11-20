//! The chat with the assistant.
//!
//! Each turn runs the CLI once in the agent sandbox, in its documented
//! structured mode: `claude -p` reads the message as stream-json and writes
//! its events as stream-json. The first turn of a conversation creates the
//! CLI session with `--session-id`; later turns resume it with `--resume`,
//! so the CLI keeps the memory of the conversation in the agent's home.
//!
//! The chat keeps in SQLite what the panel shows: messages, answers,
//! reasoning, actions, notices and errors. The panel follows a turn through
//! server-sent events: a snapshot first, then each change. One turn runs at
//! a time; when it ends, the preview is built again.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{Notify, broadcast, oneshot};

use crate::agent::Environment;
use crate::assistant::{AgentEvent, Decision, Problem, Provider, Said, Tool, claude, codex};
use crate::build::Builder;
use crate::cms::Cms;
use crate::db;
use crate::terminal::Terminal;

/// Runs of the CLI for one turn when it keeps ending unexpectedly.
const ATTEMPTS: u32 = 3;
/// What the assistant is told when it starts again.
const CONTINUE: &str =
    "The previous run stopped before it finished. Continue the last request from where it stopped.";

/// Longest message accepted, in characters.
pub const MESSAGE_LIMIT: usize = 20_000;
/// Characters of the first message kept as the conversation's title.
const TITLE_LENGTH: usize = 60;
/// Characters kept of the second line of an action.
const DETAIL_LENGTH: usize = 300;
/// Characters kept of a plan the person decides on.
const PLAN_LENGTH: usize = 4000;
/// Longest wait for the build that ends a turn.
const BUILD_WAIT: Duration = Duration::from_secs(10 * 60);
/// How long the person of the last turn keeps the chat after it.
const HOLD: Duration = Duration::from_secs(10 * 60);
/// Longest wait for a person to answer a request; the CLI waits for its
/// hook a little longer (see policy/managed-settings.json).
const REQUEST_WAIT: Duration = Duration::from_secs(55 * 60);

/// One line of the chat, as the panel shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Item {
    pub id: i64,
    /// person, answer, thinking, activity, notice or error.
    pub kind: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// running, done, failed or stopped, for an action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Name of the person who wrote a message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub at: String,
    /// Pieces of text received while the item is being written; 0 once it
    /// is complete. The panel applies a piece only if it is the next one.
    pub seq: u32,
    /// For a message, the site can go back to before it; for the line of a
    /// going back, it can be undone.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub back: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Conversation {
    pub id: i64,
    pub title: String,
}

/// A conversation in the list of past ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConversationSummary {
    pub id: i64,
    pub title: String,
    /// The assistant it is with.
    pub provider: String,
    pub updated_at: String,
    /// Messages people sent in it.
    pub messages: i64,
    /// It is the one the chat shows.
    pub current: bool,
}

/// What an assistant used over a period, from the turns' usage.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct UsageTotal {
    pub turns: i64,
    pub input: i64,
    pub cached: i64,
    pub output: i64,
    /// In US dollars, summed over the turns with a price; none without.
    pub cost: Option<f64>,
}

/// An assistant's usage today (UTC) and over the last 30 days.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct UsageSummary {
    pub today: UsageTotal,
    pub month: UsageTotal,
}

/// The usage of `provider` since `since` (an SQLite time modifier).
fn usage_since(
    conn: &rusqlite::Connection,
    provider: &str,
    since: &str,
) -> rusqlite::Result<UsageTotal> {
    conn.query_row(
        "SELECT count(*), coalesce(sum(input_tokens), 0), coalesce(sum(cached_tokens), 0),
                coalesce(sum(output_tokens), 0), sum(cost)
         FROM turn_usage
         WHERE provider = ?1 AND at >= strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?2)",
        params![provider, since],
        |row| {
            Ok(UsageTotal {
                turns: row.get(0)?,
                input: row.get(1)?,
                cached: row.get(2)?,
                output: row.get(3)?,
                cost: row.get(4)?,
            })
        },
    )
}

/// What `provider` used today and over the last 30 days.
pub async fn usage(db: &crate::db::Handle, provider: Provider) -> Result<UsageSummary, String> {
    let provider = provider.id();
    db.run(move |conn| {
        Ok(UsageSummary {
            today: usage_since(conn, provider, "start of day")?,
            month: usage_since(conn, provider, "-30 days")?,
        })
    })
    .await
    .map_err(|err: rusqlite::Error| err.to_string())
}

/// A turn in progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Turn {
    pub elapsed_ms: u64,
    pub stopping: bool,
    /// The assistant is done; the preview is being built.
    pub updating: bool,
    /// The assistant waits for a person to answer a request.
    pub waiting: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub conversation: Option<Conversation>,
    pub items: Vec<Item>,
    pub turn: Option<Turn>,
    /// Plan limits as the CLI last reported them.
    pub limits: Option<Value>,
    pub holder: Option<HolderInfo>,
    /// Who has the terminal open: the chat waits meanwhile.
    pub terminal: Option<TerminalInfo>,
    /// Why the last turn did not finish, and what can be done.
    pub problem: Option<ProblemInfo>,
    /// The mode of the assistant's CLI for the next message.
    pub mode: &'static str,
    /// The model for the next message; the CLI's default if none.
    pub model: Option<String>,
}

/// Why the last turn did not finish.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProblemInfo {
    /// `error`: something went wrong, the turn can be tried again;
    /// `limit`: the plan of the account is used up until `until`;
    /// `login`: the account must sign in again.
    pub kind: &'static str,
    /// For a limit, when the plan is available again (Unix seconds).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<i64>,
}

impl ProblemInfo {
    fn of(problem: Problem) -> Self {
        let (kind, until) = match problem {
            Problem::Limit(until) => ("limit", until),
            Problem::Login => ("login", None),
            Problem::Other => ("error", None),
        };
        Self { kind, until }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TerminalInfo {
    pub id: i64,
    pub name: String,
}

/// The person using the chat: during a turn, and for a while after it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HolderInfo {
    pub id: i64,
    pub name: String,
    /// How long the person keeps the chat after the turn; none during it.
    pub remaining_ms: Option<u64>,
}

/// What the panel receives.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Snapshot(Snapshot),
    /// An item added or changed.
    Item {
        item: Item,
    },
    /// A piece of the text of an item being written.
    Delta {
        id: i64,
        seq: u32,
        text: String,
    },
    /// A turn started, is stopping or ended.
    Turn {
        turn: Option<Turn>,
        conversation: Option<Conversation>,
        holder: Option<HolderInfo>,
        terminal: Option<TerminalInfo>,
        problem: Option<ProblemInfo>,
    },
    Limits {
        limits: Value,
    },
    /// The mode for the next message changed.
    Mode {
        mode: &'static str,
    },
    /// The model for the next message changed.
    Model {
        model: Option<String>,
    },
    /// Another conversation is shown: the panel asks for a new snapshot.
    Reset,
}

/// Why a change of the content from the panel did not happen.
#[derive(Debug)]
pub enum ContentError {
    /// The assistant is at work: the content is read-only meanwhile.
    Locked(String),
    Cms(crate::cms::CmsError),
    /// The change could not be recorded.
    Save(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum SendError {
    Busy,
    /// Another person holds the chat.
    InUse(String),
    /// The terminal is open: the chat waits.
    Paused,
    /// The assistant works on another site.
    Elsewhere(crate::gate::Occupant),
    Empty,
    TooLong,
    Unavailable(String),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::Busy => write!(f, "the assistant is still working"),
            SendError::InUse(name) => write!(f, "{name} is using the assistant"),
            SendError::Paused => write!(f, "the chat is paused while the terminal is open"),
            SendError::Elsewhere(_) => write!(f, "the assistant is working on another site"),
            SendError::Empty => write!(f, "the message is empty"),
            SendError::TooLong => write!(f, "the message is too long"),
            SendError::Unavailable(problem) => write!(f, "{problem}"),
        }
    }
}

#[derive(Debug, Clone)]
struct Current {
    id: i64,
    session: String,
    title: String,
    started: bool,
    provider: Provider,
    /// The CLI's own id of the conversation, when it chooses it.
    cli_session: Option<String>,
    /// The mode chosen for the conversation.
    mode: Option<String>,
    /// The model chosen for the conversation.
    model: Option<String>,
}

struct Running {
    started: Instant,
    stop: Arc<Notify>,
    stopping: bool,
    updating: bool,
    /// Text of the items being written, with the count of pieces.
    live: HashMap<i64, (String, u32)>,
    /// Requests waiting for a person, by item: the answer goes through the
    /// sender. Dropping a sender refuses the request.
    requests: HashMap<i64, oneshot::Sender<Reply>>,
    /// The turn showed at least one request.
    asked: bool,
    /// Who sent the message, and the message: the commit of the turn
    /// names them.
    person: i64,
    request: String,
    /// The assistant's slot, held for this site until the turn ends: dropping
    /// the turn releases it.
    _slot: Option<crate::gate::SlotGuard>,
}

/// The person using the chat.
struct Holder {
    person: i64,
    name: String,
    /// When the person stops holding the chat; none during a turn.
    until: Option<Instant>,
}

/// The terminal open on the conversation.
struct OpenTerminal {
    person: i64,
    name: String,
    /// The assistant's slot, held for this site while the terminal is open:
    /// closing it releases the slot.
    _slot: Option<crate::gate::SlotGuard>,
    conversation: i64,
    session: String,
    provider: Provider,
    /// Codex's thread, when the conversation has one.
    cli_session: Option<String>,
    handle: Arc<Terminal>,
    /// Where what was said in the terminal starts: for Claude the size of
    /// the transcript when the terminal opened, for Codex the time (Unix
    /// seconds).
    offset: u64,
}

#[derive(Default)]
struct Inner {
    holder: Option<Holder>,
    terminal: Option<OpenTerminal>,
    problem: Option<ProblemInfo>,
    /// Told to the assistant with the next message: what changed on the
    /// site outside its turns.
    note: Option<String>,
    current: Option<Current>,
    running: Option<Running>,
    limits: Option<Value>,
    /// The mode chosen for the next message; the assistant's default if none.
    mode: Option<String>,
    /// The model chosen for the next message; the CLI's default if none.
    model: Option<String>,
}

pub struct Chat {
    env: Arc<Environment>,
    /// The site's database: conversations, messages, allowed hosts.
    db: db::Handle,
    /// The installation's database: the assistant's modes, what turns
    /// consumed, the activity log.
    installation: db::Handle,
    /// The id of the site this chat works on.
    site_id: String,
    /// One assistant at work at a time, across the sites.
    slot: Arc<crate::gate::AgentSlot>,
    builder: Option<Arc<Builder>>,
    inner: Mutex<Inner>,
    events: broadcast::Sender<Event>,
    /// True once the server is shutting down: the event streams end.
    closed: tokio::sync::watch::Sender<bool>,
    /// One change of the content at a time.
    content: tokio::sync::Mutex<()>,
    /// Hosts people allowed the assistant to reach.
    hosts: Mutex<std::collections::HashSet<String>>,
    /// Hosts waiting for a person, with the answer once given.
    asking_hosts: Mutex<HashMap<String, tokio::sync::watch::Receiver<Option<bool>>>>,
}

/// What a turn needs to run.
struct Setup {
    conversation: i64,
    session: String,
    resume: bool,
    provider: Provider,
    mode: &'static str,
    model: Option<String>,
    /// The CLI's own id of the conversation, to resume it.
    cli_session: Option<String>,
    text: String,
    stop: Arc<Notify>,
}

impl Chat {
    /// Opens the chat on the latest conversation. Actions left running by a
    /// previous process are marked as stopped.
    pub async fn start(
        env: Arc<Environment>,
        installation: db::Handle,
        db: db::Handle,
        site_id: String,
        slot: Arc<crate::gate::AgentSlot>,
        builder: Option<Arc<Builder>>,
    ) -> Arc<Self> {
        let provider = env.active().id();
        let current = db
            .run(move |conn| {
                conn.execute(
                    "UPDATE chat_item SET state = 'stopped' WHERE state = 'running'",
                    [],
                )?;
                latest(conn, provider)
            })
            .await
            .unwrap_or_else(|err| {
                eprintln!("alogeno: cannot read the conversations: {err}");
                None
            });
        let hosts = db
            .run(|conn| -> rusqlite::Result<Vec<String>> {
                let mut statement = conn.prepare("SELECT host FROM allowed_host")?;
                statement.query_map([], |row| row.get(0))?.collect()
            })
            .await
            .unwrap_or_default();
        let active = env.active();
        let chat = Self::with(env, installation, db, site_id, slot, builder, current);
        lock(&chat.hosts).extend(hosts);
        // Without a conversation, the mode last chosen for the assistant.
        if chat.inner().mode.is_none() {
            chat.inner().mode = chat.last_mode(active).await;
        }
        chat
    }

    /// The mode last chosen for `provider`, if any.
    async fn last_mode(&self, provider: Provider) -> Option<String> {
        let id = provider.id();
        self.installation
            .run(move |conn| {
                conn.query_row(
                    "SELECT mode FROM assistant_mode WHERE provider = ?1",
                    [id],
                    |row| row.get(0),
                )
                .optional()
            })
            .await
            .ok()
            .flatten()
    }

    fn with(
        env: Arc<Environment>,
        installation: db::Handle,
        db: db::Handle,
        site_id: String,
        slot: Arc<crate::gate::AgentSlot>,
        builder: Option<Arc<Builder>>,
        current: Option<Current>,
    ) -> Arc<Self> {
        let (events, _) = broadcast::channel(1024);
        let (closed, _) = tokio::sync::watch::channel(false);
        Arc::new(Self {
            env,
            db,
            installation,
            site_id,
            slot,
            builder,
            inner: Mutex::new(Inner {
                mode: current.as_ref().and_then(|c| c.mode.clone()),
                model: current.as_ref().and_then(|c| c.model.clone()),
                current,
                ..Inner::default()
            }),
            events,
            closed,
            content: tokio::sync::Mutex::new(()),
            hosts: Mutex::new(Default::default()),
            asking_hosts: Mutex::new(HashMap::new()),
        })
    }

    /// Ends the event streams, so that the server can shut down.
    /// A turn in progress is stopped, so that what it wrote is saved.
    pub fn close(&self) {
        self.closed.send_replace(true);
        self.stop();
    }

    fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }

    /// Waits until no turn is in progress, at most `limit`.
    pub async fn wait_idle(&self, limit: Duration) {
        let deadline = Instant::now() + limit;
        while self.inner().running.is_some() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Completes when the server is shutting down.
    pub fn closing(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let mut closed = self.closed.subscribe();
        async move {
            let _ = closed.wait_for(|closed| *closed).await;
        }
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn emit(&self, event: Event) {
        // Nobody listening is fine: the next snapshot has everything.
        let _ = self.events.send(event);
    }

    /// The id of the site this chat works on.
    pub fn site_id(&self) -> &str {
        &self.site_id
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    fn turn_info(inner: &Inner) -> Option<Turn> {
        inner.running.as_ref().map(|running| Turn {
            elapsed_ms: running.started.elapsed().as_millis() as u64,
            stopping: running.stopping,
            updating: running.updating,
            waiting: !running.requests.is_empty(),
        })
    }

    fn conversation_info(inner: &Inner) -> Option<Conversation> {
        inner.current.as_ref().map(|current| Conversation {
            id: current.id,
            title: current.title.clone(),
        })
    }

    pub async fn snapshot(&self) -> Snapshot {
        let id = self.inner().current.as_ref().map(|current| current.id);
        let items = match id {
            Some(id) => self
                .db
                .run(move |conn| items(conn, id))
                .await
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let inner = self.inner();
        // The conversation may have changed meanwhile: then its snapshot
        // follows the Reset event.
        let items = if inner.current.as_ref().map(|current| current.id) == id {
            items
                .into_iter()
                .map(|mut item| {
                    if let Some((text, seq)) =
                        inner.running.as_ref().and_then(|r| r.live.get(&item.id))
                    {
                        item.text = text.clone();
                        item.seq = *seq;
                    }
                    item
                })
                .collect()
        } else {
            Vec::new()
        };
        Snapshot {
            conversation: Self::conversation_info(&inner),
            items,
            turn: Self::turn_info(&inner),
            limits: inner.limits.clone(),
            holder: Self::holder_info(&inner),
            terminal: Self::terminal_info(&inner),
            problem: inner.problem.clone(),
            mode: self.env.active().mode(inner.mode.as_deref()),
            model: inner.model.clone(),
        }
    }

    /// Chooses the model for the next messages of the conversation; none
    /// for the CLI's default. The caller checked that the CLI has it.
    pub async fn set_model(&self, person: i64, model: Option<String>) -> Result<(), String> {
        self.check_holder(person)?;
        let conversation = {
            let mut inner = self.inner();
            inner.model = model.clone();
            inner.current.as_mut().map(|current| {
                current.model = model.clone();
                current.id
            })
        };
        if let Some(id) = conversation {
            let saved = model.clone();
            let _ = self
                .db
                .run(move |conn| {
                    conn.execute(
                        "UPDATE conversation SET model = ?2 WHERE id = ?1",
                        params![id, saved],
                    )
                })
                .await;
        }
        self.emit(Event::Model { model });
        Ok(())
    }

    /// Chooses the mode of the assistant's CLI for the next messages of the
    /// conversation: one of the modes the CLI has.
    pub async fn set_mode(&self, person: i64, mode: &str) -> Result<&'static str, String> {
        self.check_holder(person)?;
        let provider = self.env.active();
        let mode = *provider
            .modes()
            .iter()
            .find(|m| **m == mode)
            .ok_or_else(|| format!("{} has no mode {mode}", provider.name()))?;
        self.choose_mode(mode).await;
        Ok(mode)
    }

    /// Makes `mode` the mode of the next messages of the conversation.
    async fn choose_mode(&self, mode: &'static str) {
        let conversation = {
            let mut inner = self.inner();
            inner.mode = Some(mode.to_string());
            inner.current.as_mut().map(|current| {
                current.mode = Some(mode.to_string());
                current.id
            })
        };
        if let Some(id) = conversation {
            let _ = self
                .db
                .run(move |conn| {
                    conn.execute(
                        "UPDATE conversation SET mode = ?2 WHERE id = ?1",
                        params![id, mode],
                    )
                })
                .await;
        }
        // New conversations start in it too.
        let provider = self.env.active().id();
        let _ = self
            .installation
            .run(move |conn| {
                conn.execute(
                    "INSERT INTO assistant_mode (provider, mode) VALUES (?1, ?2)
                     ON CONFLICT (provider) DO UPDATE SET mode = excluded.mode",
                    params![provider, mode],
                )
            })
            .await;
        self.emit(Event::Mode { mode });
    }

    /// Records the message and starts a turn with it.
    pub async fn send(
        self: &Arc<Self>,
        person: i64,
        name: &str,
        text: &str,
    ) -> Result<(), SendError> {
        let setup = self.prepare(person, name, text).await?;
        let chat = self.clone();
        tokio::spawn(async move { chat.run(setup).await });
        Ok(())
    }

    async fn prepare(&self, person: i64, name: &str, text: &str) -> Result<Setup, SendError> {
        self.prepare_turn(person, name, text, true).await
    }

    /// Starts a turn with `text`; `shown` false sends it to the assistant
    /// without a message of the person in the chat (trying again).
    async fn prepare_turn(
        &self,
        person: i64,
        name: &str,
        text: &str,
        shown: bool,
    ) -> Result<Setup, SendError> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err(SendError::Empty);
        }
        if text.chars().count() > MESSAGE_LIMIT {
            return Err(SendError::TooLong);
        }
        let stop = Arc::new(Notify::new());
        // Claims the turn before any await, so two messages cannot both start.
        let current = {
            let mut inner = self.inner();
            if let Some(other) = Self::held_by_other(&inner, person) {
                return Err(SendError::InUse(other));
            }
            if inner.terminal.is_some() {
                return Err(SendError::Paused);
            }
            if inner.running.is_some() {
                return Err(SendError::Busy);
            }
            let slot = self
                .slot
                .try_take(&self.site_id, crate::gate::Kind::Turn)
                .map_err(SendError::Elsewhere)?;
            inner.holder = Some(Holder {
                person,
                name: name.to_string(),
                until: None,
            });
            inner.problem = None;
            inner.running = Some(Running {
                started: Instant::now(),
                stop: stop.clone(),
                stopping: false,
                updating: false,
                live: HashMap::new(),
                requests: HashMap::new(),
                asked: false,
                person,
                request: if shown {
                    text.clone()
                } else {
                    "Tried the last request again".into()
                },
                _slot: Some(slot),
            });
            inner.current.clone()
        };
        // The site before the message: going back returns to it.
        let version = if shown {
            self.site_version().await
        } else {
            None
        };
        let provider = self.env.active();
        let mode = provider.mode(self.inner().mode.as_deref());
        let model = self.inner().model.clone();
        let created = {
            let text = text.clone();
            self.db
                .run(move |conn| -> rusqlite::Result<(Current, Option<Item>)> {
                    let current = match current.filter(|c| c.provider == provider) {
                        Some(current) => current,
                        None => {
                            let session = uuid();
                            let title = title_of(&text);
                            conn.execute(
                                "INSERT INTO conversation (session, title, created_by, provider, mode, model) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                                params![session, title, person, provider.id(), mode, model],
                            )?;
                            Current {
                                id: conn.last_insert_rowid(),
                                session,
                                title,
                                started: false,
                                provider,
                                cli_session: None,
                                mode: Some(mode.to_string()),
                                model: model.clone(),
                            }
                        }
                    };
                    let item = if shown {
                        let item = insert_item(conn, current.id, "person", &text, None, None, Some(person))?;
                        if let Some(version) = &version {
                            conn.execute(
                                "UPDATE chat_item SET version = ?2 WHERE id = ?1",
                                params![item.id, version],
                            )?;
                        }
                        Some(get_item(conn, item.id)?)
                    } else {
                        None
                    };
                    Ok((current, item))
                })
                .await
        };
        let (current, item) = match created {
            Ok(created) => created,
            Err(err) => {
                self.inner().running = None;
                return Err(SendError::Unavailable(err.to_string()));
            }
        };
        {
            let mut inner = self.inner();
            inner.current = Some(current.clone());
            self.emit_turn(&inner);
        }
        if let Some(item) = item {
            self.emit(Event::Item { item });
        }
        // What changed on the site meanwhile comes before the message.
        let text = match self.inner().note.take() {
            Some(note) => format!("[{note}]\n\n{text}"),
            None => text,
        };
        let resume = self.resumable(current.provider, &current.session, current.started);
        Ok(Setup {
            conversation: current.id,
            session: current.session,
            resume,
            provider: current.provider,
            mode,
            model: current.model.clone(),
            cli_session: current.cli_session,
            text,
            stop,
        })
    }

    /// The current version of the site, if it is a repository.
    async fn site_version(&self) -> Option<String> {
        let site = self.env.site.clone();
        if !site.join(".git").is_dir() {
            return None;
        }
        tokio::task::spawn_blocking(move || crate::git::head(&site).ok())
            .await
            .ok()
            .flatten()
    }

    /// Checks that `person` may change the site from the chat now: they
    /// hold it, and the assistant and the terminal are not at work.
    fn check_idle(&self, person: i64) -> Result<(), String> {
        self.check_holder(person)?;
        let inner = self.inner();
        if inner.running.is_some() {
            return Err("the assistant is still working".into());
        }
        if inner.terminal.is_some() {
            return Err("the terminal is open".into());
        }
        Ok(())
    }

    /// Takes the site back to how it was before the message `id`: what the
    /// assistant changed after it is undone, in a new version, so it can be
    /// undone in turn. Returns the message, for the person to change it.
    pub async fn go_back(&self, person: i64, id: i64) -> Result<String, String> {
        self.check_idle(person)?;
        let (item, target, conversation) = self
            .db
            .run(move |conn| {
                conn.query_row(
                    "SELECT kind, text, version, conversation_id FROM chat_item WHERE id = ?1",
                    [id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, i64>(3)?,
                        ))
                    },
                )
            })
            .await
            .map(|(kind, text, version, conversation)| ((kind, text), version, conversation))
            .map_err(|_| "no such message".to_string())?;
        let (kind, text) = item;
        let Some(target) = target.filter(|_| kind == "person") else {
            return Err("there is no version to go back to for this line".into());
        };
        let subject = format!("Went back to before: {}", title_of(&text));
        let undo = self.switch_site(person, target.clone(), subject).await?;
        let line = format!("Went back to before \u{201c}{}\u{201d}.", title_of(&text));
        self.add_versioned(conversation, line, undo).await;
        self.inner().note = Some(format!(
            "alogeno: the site went back to how it was before the message \u{201c}{}\u{201d}; everything done after it was undone",
            clip(&text, 200)
        ));
        self.record(person, "chat.went_back", Some(format!("commit:{target}")))
            .await;
        Ok(text)
    }

    /// Undoes a going back: the site returns to how it was before it.
    pub async fn undo_back(&self, person: i64, id: i64) -> Result<(), String> {
        self.check_idle(person)?;
        let (version, conversation) = self
            .db
            .run(move |conn| {
                conn.query_row(
                    "SELECT version, conversation_id FROM chat_item WHERE id = ?1 AND kind = 'notice'",
                    [id],
                    |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?)),
                )
            })
            .await
            .map_err(|_| "no such line".to_string())?;
        let version = version.ok_or("this going back was undone already")?;
        self.switch_site(person, version.clone(), "Undid going back".into())
            .await?;
        let item = self
            .db
            .run(move |conn| -> rusqlite::Result<Item> {
                conn.execute("UPDATE chat_item SET version = NULL WHERE id = ?1", [id])?;
                get_item(conn, id)
            })
            .await
            .map_err(|err| err.to_string())?;
        self.emit(Event::Item { item });
        self.add(
            conversation,
            "notice",
            "Undid going back.".into(),
            None,
            None,
        )
        .await;
        self.inner().note = Some(
            "alogeno: the going back was undone; the site is again as it was before it".into(),
        );
        self.record(
            person,
            "chat.undid_going_back",
            Some(format!("commit:{version}")),
        )
        .await;
        Ok(())
    }

    /// Puts the site at `version`, saving what is there first, and builds
    /// the preview. Returns the version before the switch.
    async fn switch_site(
        &self,
        person: i64,
        version: String,
        subject: String,
    ) -> Result<String, String> {
        let site = self.env.site.clone();
        if !site.join(".git").is_dir() {
            return Err("the site has no history".into());
        }
        let before = tokio::task::spawn_blocking(move || -> Result<String, String> {
            let by = format!("\n\nIn the panel: person {person}.");
            crate::git::commit_all(&site, &format!("Saved before going back{by}"))?;
            let before = crate::git::head(&site)?;
            if crate::git::same_tree(&site, &before, &version)? {
                return Err("the site is already like that".into());
            }
            crate::git::restore(&site, &version)?;
            crate::git::commit_all(&site, &format!("{subject}{by}"))?;
            Ok(before)
        })
        .await
        .map_err(|err| err.to_string())??;
        if let Some(builder) = &self.builder {
            builder.request();
        }
        Ok(before)
    }

    async fn add_versioned(&self, conversation: i64, text: String, version: String) {
        let item = self
            .db
            .run(move |conn| -> rusqlite::Result<Item> {
                let item = insert_item(conn, conversation, "notice", &text, None, None, None)?;
                conn.execute(
                    "UPDATE chat_item SET version = ?2 WHERE id = ?1",
                    params![item.id, version],
                )?;
                get_item(conn, item.id)
            })
            .await;
        if let Ok(item) = item {
            self.emit(Event::Item { item });
        }
    }

    /// Tries the last request again after something went wrong.
    pub async fn retry(self: &Arc<Self>, person: i64, name: &str) -> Result<(), SendError> {
        if self.inner().problem.as_ref().map(|p| p.kind) != Some("error") {
            return Err(SendError::Unavailable(
                "there is nothing to try again".into(),
            ));
        }
        let setup = self.prepare_turn(person, name, CONTINUE, false).await?;
        let chat = self.clone();
        tokio::spawn(async move { chat.run(setup).await });
        Ok(())
    }

    /// Stops the turn in progress for `person`, who must hold the chat.
    pub fn stop_by(&self, person: i64) -> Result<(), String> {
        self.check_holder(person)?;
        self.stop();
        Ok(())
    }

    /// Stops the turn in progress, if any.
    pub fn stop(&self) {
        let mut inner = self.inner();
        if let Some(running) = inner.running.as_mut() {
            running.stopping = true;
            running.stop.notify_one();
            // Requests still waiting are refused.
            running.requests.clear();
            self.emit_turn(&inner);
        }
    }

    /// Leaves the current conversation: the next message starts a new one.
    /// Whether `person` can change the assistant now: nobody else holds the
    /// chat, and no turn, terminal or request is under way.
    pub fn check_switch(&self, person: i64) -> Result<(), String> {
        self.check_idle(person)
    }

    /// The assistant changed: the next message starts a new conversation.
    pub async fn switched(&self) {
        let mode = self.last_mode(self.env.active()).await;
        let mut inner = self.inner();
        inner.current = None;
        inner.problem = None;
        inner.limits = None;
        inner.mode = mode;
        inner.model = None;
        self.emit(Event::Reset);
    }

    /// The conversations, newest first, with the assistant of each.
    pub async fn conversations(&self) -> Result<Vec<ConversationSummary>, String> {
        let current = self.inner().current.as_ref().map(|c| c.id);
        self.db
            .run(move |conn| {
                let mut statement = conn.prepare(
                    "SELECT c.id, c.title, c.provider, c.updated_at,
                            (SELECT count(*) FROM chat_item i WHERE i.conversation_id = c.id AND i.kind = 'person')
                     FROM conversation c
                     WHERE EXISTS (SELECT 1 FROM chat_item i WHERE i.conversation_id = c.id)
                     ORDER BY c.updated_at DESC, c.id DESC LIMIT 200",
                )?;
                statement
                    .query_map([], |row| {
                        let id: i64 = row.get(0)?;
                        Ok(ConversationSummary {
                            id,
                            title: row.get(1)?,
                            provider: row.get(2)?,
                            updated_at: row.get(3)?,
                            messages: row.get(4)?,
                            current: Some(id) == current,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .await
            .map_err(|err| err.to_string())
    }

    /// Goes back to a conversation of the assistant in use: the next
    /// message continues it, with its mode and model.
    pub async fn open_conversation(&self, person: i64, id: i64) -> Result<(), String> {
        self.check_idle(person)?;
        let provider = self.env.active();
        let found = self
            .db
            .run(move |conn| {
                conn.query_row(
                    "SELECT id, session, title, started, provider, cli_session, mode, model
                     FROM conversation WHERE id = ?1",
                    [id],
                    |row| {
                        Ok(Current {
                            id: row.get(0)?,
                            session: row.get(1)?,
                            title: row.get(2)?,
                            started: row.get::<_, i64>(3)? == 1,
                            provider: Provider::parse(&row.get::<_, String>(4)?)
                                .unwrap_or(Provider::Claude),
                            cli_session: row.get(5)?,
                            mode: row.get(6)?,
                            model: row.get(7)?,
                        })
                    },
                )
                .optional()
            })
            .await
            .map_err(|err| err.to_string())?
            .ok_or("this conversation does not exist")?;
        if found.provider != provider {
            return Err(format!(
                "this conversation is with {}: connect it to continue it",
                found.provider.name()
            ));
        }
        {
            let mut inner = self.inner();
            if inner.running.is_some() || inner.terminal.is_some() {
                return Err("the assistant is still working".into());
            }
            inner.mode = found.mode.clone().or(inner.mode.take());
            inner.model = found.model.clone();
            inner.problem = None;
            inner.current = Some(found);
        }
        self.emit(Event::Reset);
        Ok(())
    }

    pub fn new_conversation(&self, person: i64) -> Result<(), SendError> {
        let mut inner = self.inner();
        if let Some(other) = Self::held_by_other(&inner, person) {
            return Err(SendError::InUse(other));
        }
        if inner.terminal.is_some() {
            return Err(SendError::Paused);
        }
        if inner.running.is_some() {
            return Err(SendError::Busy);
        }
        inner.current = None;
        self.emit(Event::Reset);
        Ok(())
    }

    async fn run(self: Arc<Self>, mut setup: Setup) {
        // The site's memory is where every assistant reads it; the turn's
        // commit records the move.
        if let Err(err) = crate::site::share_memory(&self.env.site) {
            eprintln!("alogeno: the site's memory stays in CLAUDE.md: {err}");
        }
        let mut attempt = 1;
        loop {
            let mut processor = Processor::of(setup.conversation, setup.provider);
            let ended = match setup.provider {
                Provider::Claude => self.drive(&setup, &mut processor).await,
                Provider::Codex => self.drive_codex(&setup, &mut processor).await,
            };
            // The CLI ended without an answer, not stopped by anyone: it
            // starts again on the same conversation, up to three times.
            let crashed = !ended.stopped && ended.failure.is_none() && processor.outcome.is_none();
            if crashed && attempt < ATTEMPTS && !self.is_closed() {
                if !ended.errors.trim().is_empty() {
                    eprintln!(
                        "alogeno: the assistant stopped unexpectedly:\n{}",
                        ended.errors.trim_end()
                    );
                }
                processor.settle(&self).await;
                attempt += 1;
                let notice = format!(
                    "The assistant stopped unexpectedly. Trying again ({attempt} of {ATTEMPTS})\u{2026}"
                );
                self.add(setup.conversation, "notice", notice, None, None)
                    .await;
                tokio::time::sleep(Duration::from_secs(2 * attempt as u64)).await;
                setup.resume = setup.resume || processor.initialized;
                if setup.cli_session.is_none() {
                    setup.cli_session = self
                        .inner()
                        .current
                        .as_ref()
                        .filter(|c| c.id == setup.conversation)
                        .and_then(|c| c.cli_session.clone());
                }
                setup.text = CONTINUE.into();
                continue;
            }
            processor.finish(&self, ended).await;
            break;
        }
    }

    /// Runs the CLI for one turn and feeds its events to the processor.
    async fn drive(self: &Arc<Self>, setup: &Setup, processor: &mut Processor) -> Ended {
        let args = claude::turn_args(
            &setup.session,
            setup.resume,
            setup.mode,
            setup.model.as_deref(),
        );
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let spawned = self.env.claude(&args).and_then(|(mut command, filter)| {
            command.stderr(Stdio::piped());
            let child = command.spawn().map_err(|e| e.to_string());
            drop(filter);
            child
        });
        let mut child = match spawned {
            Ok(child) => child,
            Err(problem) => return Ended::failed(problem),
        };
        let (Some(mut stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Ended::failed("the assistant has no input or output".into());
        };
        // The end of the error output, to explain a crash.
        let errors = tokio::spawn(async move {
            let mut text = String::new();
            let _ = stderr.take(64 * 1024).read_to_string(&mut text).await;
            text
        });
        let message = claude::message(&setup.text);
        if let Err(err) = stdin.write_all(format!("{message}\n").as_bytes()).await {
            let _ = child.start_kill();
            return Ended::failed(format!("cannot write to the assistant: {err}"));
        }
        // One writer for the answers to the CLI's requests; `None` closes
        // the input, which lets the CLI end.
        let (out, mut outgoing) = tokio::sync::mpsc::unbounded_channel::<Option<Value>>();
        let writer = tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(Some(message)) = outgoing.recv().await {
                if stdin
                    .write_all(format!("{message}\n").as_bytes())
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut lines = BufReader::new(stdout).lines();
        let stopped = loop {
            tokio::select! {
                _ = setup.stop.notified() => {
                    let _ = child.start_kill();
                    break true;
                }
                line = lines.next_line() => match line {
                    Ok(Some(line)) => {
                        let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
                        // The CLI asks whether it may use a tool: a person
                        // answers, meanwhile its events go on.
                        if let Some(request) = claude::control_request(&value) {
                            let chat = self.clone();
                            let out = out.clone();
                            tokio::spawn(async move {
                                let answer = chat.claude_request(request).await;
                                let _ = out.send(Some(answer));
                            });
                            continue;
                        }
                        processor.handle_claude(self, &value).await;
                        if processor.outcome.is_some() {
                            let _ = out.send(None);
                        }
                    }
                    _ => break false,
                }
            }
        };
        let _ = out.send(None);
        drop(out);
        let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
        if tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .is_err()
        {
            let _ = child.start_kill();
        }
        let errors = tokio::time::timeout(Duration::from_secs(2), errors)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
        Ended {
            stopped,
            failure: None,
            errors,
        }
    }

    /// Runs Codex's app-server for one turn and feeds its events to the
    /// processor. Requests of the server, such as approvals, wait for a
    /// person meanwhile, without holding the events back.
    async fn drive_codex(self: &Arc<Self>, setup: &Setup, processor: &mut Processor) -> Ended {
        let spawned =
            self.env
                .cli(Provider::Codex, &["app-server"])
                .and_then(|(mut command, filter)| {
                    let child = command.spawn().map_err(|e| e.to_string());
                    drop(filter);
                    child
                });
        let mut child = match spawned {
            Ok(child) => child,
            Err(problem) => return Ended::failed(problem),
        };
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Ended::failed("the assistant has no input or output".into());
        };
        let errors = tokio::spawn(async move {
            let mut text = String::new();
            let _ = stderr.take(64 * 1024).read_to_string(&mut text).await;
            text
        });
        // One writer for the requests and the answers.
        let (out, mut outgoing) = tokio::sync::mpsc::unbounded_channel::<Value>();
        let writer = tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(message) = outgoing.recv().await {
                if stdin
                    .write_all(format!("{message}\n").as_bytes())
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        let send = |message: Value| {
            let _ = out.send(message);
        };
        // Requests of this connection, by id.
        const INITIALIZE: i64 = 1;
        const THREAD: i64 = 2;
        const TURN: i64 = 3;
        const INTERRUPT: i64 = 4;
        send(json!({ "id": INITIALIZE, "method": "initialize", "params": codex::initialize() }));
        let mut parser = codex::Parser::default();
        let mut resuming = setup.cli_session.is_some();
        let mut thread: Option<String> = None;
        let mut turn: Option<String> = None;
        let mut failure: Option<String> = None;
        let mut lines = BufReader::new(stdout).lines();
        // After Stop, Codex has a moment to end the turn itself.
        let mut deadline: Option<tokio::time::Instant> = None;
        // The mode of the current Codex turn, and the thread's model: going
        // ahead with a plan starts another turn in Default mode.
        let mut mode = setup.mode;
        let mut thread_model = setup.model.clone();
        let stopped = 'turns: loop {
            let stopped = loop {
                tokio::select! {
                    _ = setup.stop.notified(), if deadline.is_none() => {
                        deadline = Some(tokio::time::Instant::now() + Duration::from_secs(10));
                        match (&thread, &turn) {
                            (Some(thread), Some(turn)) => send(json!({
                                "id": INTERRUPT,
                                "method": "turn/interrupt",
                                "params": { "threadId": thread, "turnId": turn },
                            })),
                            _ => break true,
                        }
                    }
                    _ = tokio::time::sleep_until(deadline.unwrap_or_else(tokio::time::Instant::now)), if deadline.is_some() => {
                        break true;
                    }
                    line = lines.next_line() => {
                        let Ok(Some(line)) = line else { break deadline.is_some() };
                        match codex::incoming(&line) {
                            Some(codex::Incoming::Response { id, result, error }) => {
                                let result = result.unwrap_or(Value::Null);
                                match (id.as_i64(), error) {
                                    (Some(INITIALIZE), Some(error)) => {
                                        failure = Some(format!("Codex did not start: {error}"));
                                        break false;
                                    }
                                    (Some(INITIALIZE), None) => {
                                        send(json!({ "method": "initialized" }));
                                        let model = setup.model.as_deref();
                                        send(match (&setup.cli_session, resuming) {
                                            (Some(id), true) => json!({
                                                "id": THREAD,
                                                "method": "thread/resume",
                                                "params": codex::thread_params(Some(id), model),
                                            }),
                                            _ => json!({
                                                "id": THREAD,
                                                "method": "thread/start",
                                                "params": codex::thread_params(None, model),
                                            }),
                                        });
                                    }
                                    // A conversation Codex no longer has starts
                                    // again: the site keeps its memory.
                                    (Some(THREAD), Some(error)) if resuming => {
                                        eprintln!("alogeno: Codex could not resume the conversation: {error}");
                                        resuming = false;
                                        send(json!({
                                            "id": THREAD,
                                            "method": "thread/start",
                                            "params": codex::thread_params(None, setup.model.as_deref()),
                                        }));
                                    }
                                    (Some(THREAD), Some(error)) => {
                                        failure = Some(format!("Codex could not open the conversation: {error}"));
                                        break false;
                                    }
                                    (Some(THREAD), None) => {
                                        let id = result["thread"]["id"].as_str().unwrap_or_default().to_string();
                                        processor.apply(self, AgentEvent::Session { cli_id: id.clone() }).await;
                                        let mut params = json!({ "threadId": id, "input": codex::input(&setup.text) });
                                        // The mode holds for later turns too: it is
                                        // given every time.
                                        if let Some(model) = &setup.model {
                                            params["model"] = json!(model);
                                        }
                                        if thread_model.is_none() {
                                            thread_model = result["model"].as_str().map(String::from);
                                        }
                                        if let Some(model) = setup.model.as_deref().or(result["model"].as_str()) {
                                            params["collaborationMode"] = codex::collaboration(setup.mode, model);
                                        }
                                        send(json!({ "id": TURN, "method": "turn/start", "params": params }));
                                        thread = Some(id);
                                    }
                                    (Some(TURN), Some(error)) => {
                                        failure = Some(format!("Codex could not start the turn: {error}"));
                                        break false;
                                    }
                                    (Some(TURN), None) => {
                                        turn = result["turn"]["id"].as_str().map(String::from);
                                    }
                                    (_, Some(error)) => eprintln!("alogeno: Codex refused a request: {error}"),
                                    _ => {}
                                }
                            }
                            Some(codex::Incoming::Notification { method, params }) => {
                                for event in parser.notification(&method, &params) {
                                    processor.apply(self, event).await;
                                }
                                if processor.outcome.is_some() {
                                    break deadline.is_some();
                                }
                            }
                            Some(codex::Incoming::Request { id, method, params }) => {
                                let chat = self.clone();
                                let out = out.clone();
                                tokio::spawn(async move {
                                    let reply = chat.codex_request(id, &method, &params).await;
                                    let _ = out.send(reply);
                                });
                            }
                            None => {}
                        }
                    }
                }
            };
            // After a plan of plan mode, as in Codex's terminal: the person
            // goes ahead with it in Default mode, in the same turn of the
            // chat, or refines it.
            let planned = !stopped
                && mode == "plan"
                && parser.take_plan()
                && processor
                    .outcome
                    .as_ref()
                    .is_some_and(|o| o.error.is_none());
            if planned
                && let (Some(thread_id), Some((text, next))) =
                    (thread.clone(), self.implement_plan().await)
            {
                processor.outcome = None;
                mode = next;
                let mut params = json!({ "threadId": thread_id, "input": codex::input(&text) });
                if let Some(model) = &thread_model {
                    params["collaborationMode"] = codex::collaboration(mode, model);
                }
                send(json!({ "id": TURN, "method": "turn/start", "params": params }));
                turn = None;
                continue 'turns;
            }
            break stopped;
        };
        // Closing the input ends the server.
        drop(out);
        let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
        if tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .is_err()
        {
            let _ = child.start_kill();
        }
        let errors = tokio::time::timeout(Duration::from_secs(2), errors)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
        Ended {
            stopped,
            failure,
            errors,
        }
    }

    /// The answer to a request of Codex's app-server: approvals go to a
    /// person; anything else is refused.
    async fn codex_request(&self, id: Value, method: &str, params: &Value) -> Value {
        // Codex's questions (plan mode asks them) show as the chat's own.
        if method == "item/tool/requestUserInput"
            && let Ok(questions) = questions_of(&codex::questions(params))
        {
            let ids = codex::question_ids(params);
            let result = match self.ask_person(&questions).await {
                Ok(answers) => codex::answers(&ids, &answers),
                Err(_) => json!({ "answers": {} }),
            };
            return json!({ "id": id, "result": result });
        }
        if let Some((tool, reason)) = codex::approval_tool(method, params) {
            let decision = self
                .permission(params["threadId"].as_str(), &tool, reason.as_deref())
                .await;
            if let Some(answer) = codex::approval_answer(method, &decision) {
                return json!({ "id": id, "result": answer });
            }
        }
        match codex::refusal(method) {
            Ok(result) => json!({ "id": id, "result": result }),
            Err(error) => json!({ "id": id, "error": { "code": -32000, "message": error } }),
        }
    }

    /// Answers a hook event of the CLI with what the hook prints, if
    /// anything.
    pub async fn hook(&self, event: &Value) -> Option<Value> {
        if !event["alogeno_tool"].is_null() {
            return Some(self.tool(event).await);
        }
        let (session, tool) = claude::permission_request(event)?;
        // In the terminal the CLI asks on the terminal itself.
        if self.inner().terminal.is_some() {
            return None;
        }
        if let Tool::PlanReady { plan } = &tool {
            return Some(
                match self.plan_ready(session.as_deref(), plan.as_deref()).await {
                    (Decision::Allow, Some(mode)) => claude::plan_approved(mode),
                    (decision, _) => claude::permission_answer(&decision),
                },
            );
        }
        let decision = self.permission(session.as_deref(), &tool, None).await;
        Some(claude::permission_answer(&decision))
    }

    /// Codex's plan is ready: as in its terminal, the person implements it
    /// (the mode turns to Default) or stays in Plan mode, with words to
    /// refine it. The message for Codex's next turn and its mode, if any.
    async fn implement_plan(&self) -> Option<(String, &'static str)> {
        // The choices of Codex's terminal, in its words.
        const YES: &str = "Yes, implement this plan";
        const NO: &str = "No, stay in Plan mode";
        let option = |label: &str, description: &str| QuestionOption {
            label: label.into(),
            description: Some(description.into()),
        };
        let questions = [Question {
            question: "Implement this plan?".into(),
            options: vec![
                option(YES, "Switch to Default and start coding"),
                option(NO, "Write below what to change in the plan"),
            ],
            multiple: false,
        }];
        let answer = self.ask_person(&questions).await.ok()?.into_iter().next()?;
        if answer.starts_with(YES) {
            self.choose_mode("default").await;
            return Some(("Implement the plan.".into(), "default"));
        }
        let words = answer.strip_prefix(NO).unwrap_or(&answer);
        let words = words.trim_start_matches(',').trim();
        (!words.is_empty()).then(|| (words.to_string(), "plan"))
    }

    /// The answer to a request of Claude Code on its output: whether it may
    /// use a tool, as a person decides; the plan of plan mode as in its
    /// terminal.
    async fn claude_request(&self, request: claude::ControlRequest) -> Value {
        let tool = claude::tool_of(&request.tool, &request.input);
        let (decision, mode) = match &tool {
            Tool::PlanReady { plan } => self.plan_ready(None, plan.as_deref()).await,
            _ => (self.permission(None, &tool, None).await, None),
        };
        claude::control_answer(&request, &decision, mode)
    }

    /// The plan of plan mode is ready: as in Claude Code's terminal, the
    /// person goes ahead with it in another mode, or keeps planning. The
    /// mode changes for the rest of the turn and in the chat's menu.
    async fn plan_ready(
        &self,
        session: Option<&str>,
        plan: Option<&str>,
    ) -> (Decision, Option<&'static str>) {
        let Some(conversation) = self.turn_conversation(session) else {
            return (
                Decision::Deny {
                    message: "No conversation is waiting for this.".into(),
                    interrupt: true,
                },
                None,
            );
        };
        if let Some(plan) = plan.map(str::trim).filter(|p| !p.is_empty()) {
            self.add(conversation, "answer", plan.to_string(), None, None)
                .await;
        }
        // The choices of Claude Code's terminal, in its words.
        const AUTO: &str = "Yes, and use auto mode";
        const ACCEPT: &str = "Yes, auto-accept edits";
        const ASK: &str = "Yes, manually approve edits";
        let option = |label: &str, description: &str| QuestionOption {
            label: label.into(),
            description: Some(description.into()),
        };
        let questions = [Question {
            question: "Go ahead with this plan?".into(),
            options: vec![
                option(AUTO, "Claude decides what needs asking"),
                option(
                    ACCEPT,
                    "Changes files on its own, asks before other commands",
                ),
                option(ASK, "Asks before each change and command"),
                option(
                    "No, keep planning",
                    "Write below what to change in the plan",
                ),
            ],
            multiple: false,
        }];
        let answer = match self.ask_person(&questions).await {
            Ok(answers) => answers.into_iter().next().unwrap_or_default(),
            Err(message) => {
                return (
                    Decision::Deny {
                        message,
                        interrupt: true,
                    },
                    None,
                );
            }
        };
        let mode = if answer.starts_with(AUTO) {
            "auto"
        } else if answer.starts_with(ACCEPT) {
            "acceptEdits"
        } else if answer.starts_with(ASK) {
            "manual"
        } else {
            return (
                Decision::Deny {
                    message: format!("The person wants to keep planning. Their answer: {answer}"),
                    interrupt: false,
                },
                None,
            );
        };
        self.choose_mode(mode).await;
        (Decision::Allow, Some(mode))
    }

    /// Shows a permission request in the chat and waits for a person. Only
    /// the CLI of the turn in progress can ask: `session` is the one it
    /// names, if it can. `reason` is the assistant's own words for the
    /// request, when it gives them.
    async fn permission(
        &self,
        session: Option<&str>,
        tool: &Tool,
        reason: Option<&str>,
    ) -> Decision {
        let deny = |message: &str, interrupt: bool| Decision::Deny {
            message: message.into(),
            interrupt,
        };
        let Some(conversation) = self.turn_conversation(session) else {
            return deny("No conversation is waiting for this.", true);
        };
        let (mut title, detail) = request_of(tool);
        if let Some(reason) = reason.map(str::trim).filter(|r| !r.is_empty()) {
            title = clip(reason, 120);
        }
        match self
            .wait_for_person(conversation, "permission", title, detail)
            .await
        {
            Waited::Reply(Reply::Permission { allow: true, .. }) => Decision::Allow,
            Waited::Reply(_) => deny("The person did not allow this.", false),
            Waited::Stopped => deny("The person stopped the assistant.", true),
            Waited::TimedOut => deny("Nobody answered the request in time.", false),
            Waited::Unsaved => deny("The request could not be shown.", false),
        }
    }

    /// A call of a tool of alogeno's MCP server.
    async fn tool(&self, call: &Value) -> Value {
        let in_terminal = self.inner().terminal.is_some();
        let result = match call["alogeno_tool"].as_str() {
            Some("ask" | "delete_page" | "delete_component" | "show_in_panel") if in_terminal => Err(
                "In the terminal this tool is not available: ask in the conversation, delete pages and components from the chat, and say in words where to go in the panel."
                    .to_string(),
            ),
            Some("ask") => self.ask(&call["arguments"]).await,
            Some("show_in_panel") => self.show_in_panel(&call["arguments"]).await,
            Some("delete_page") => self.delete_page(&call["arguments"]).await,
            Some("delete_component") => self.delete_component(&call["arguments"]).await,
            Some(name) => self.content(name, &call["arguments"]).await,
            None => Err("unknown tool".to_string()),
        };
        match result {
            Ok(text) => json!({ "text": text }),
            Err(error) => json!({ "error": error }),
        }
    }

    /// A tool of the content: reading and changing pages and components.
    async fn content(&self, name: &str, arguments: &Value) -> Result<String, String> {
        if self.turn_conversation(None).is_none() && self.inner().terminal.is_none() {
            return Err("No conversation is waiting for this.".into());
        }
        let _one_at_a_time = self.content.lock().await;
        let site = self.env.site.clone();
        let name = name.to_string();
        let arguments = arguments.clone();
        tokio::task::spawn_blocking(move || {
            let cms = Cms::new(&site);
            let id = arguments["id"].as_str().unwrap_or_default();
            let result = match name.as_str() {
                "list_pages" => cms.list().map(|pages| json!(pages)),
                "read_page" => cms.read(id),
                "page_types" => cms.types(),
                "list_media" => crate::media::list(cms.site()).and_then(|mut items| {
                    for item in &mut items {
                        let id = item["id"].as_str().unwrap_or_default().to_string();
                        item["used_on"] = json!(crate::media::used_on(cms.site(), &id)?);
                        if let Some(object) = item.as_object_mut() {
                            object.remove("sha256");
                            object.remove("bytes");
                        }
                    }
                    Ok(json!(items))
                }),
                "update_page" => cms.update(id, &arguments["changes"]),
                "create_page" => cms.create(&arguments["page"]),
                "list_components" => cms.list_components().map(|list| json!(list)),
                "read_component" => cms.read_component(id),
                "update_component" => cms.update_component(id, &arguments["changes"]),
                "create_component" => cms.create_component(&arguments["component"]),
                _ => return Err("unknown tool".to_string()),
            };
            result
                .map(|value| value.to_string())
                .map_err(|err| err.to_string())
        })
        .await
        .map_err(|err| err.to_string())?
    }

    /// Deletes a page once a person confirms it in the chat.
    async fn delete_page(&self, arguments: &Value) -> Result<String, String> {
        let Some(conversation) = self.turn_conversation(None) else {
            return Err("No conversation is waiting for this.".into());
        };
        let id = arguments["id"].as_str().unwrap_or_default().to_string();
        let page = self
            .content_blocking(id.clone(), |cms, id| cms.check_delete(id))
            .await?;
        let title = page["title"].as_str().unwrap_or_default().to_string();
        let consequence = if page["menu"] == true {
            "It also leaves the site menu."
        } else {
            "Its address stops working."
        };
        let question = format!("Delete the page \u{201c}{title}\u{201d}?");
        match self
            .wait_for_person(
                conversation,
                "permission",
                question,
                Some(consequence.into()),
            )
            .await
        {
            Waited::Reply(Reply::Permission { allow: true, .. }) => {
                self.content_blocking(id, |cms, id| cms.delete(id)).await?;
                Ok(format!("The page \u{201c}{title}\u{201d} was deleted."))
            }
            Waited::Reply(_) => Err("The person chose to keep the page.".into()),
            Waited::Stopped => Err("The person stopped the assistant.".into()),
            Waited::TimedOut => Err("Nobody answered in time; the page was kept.".into()),
            Waited::Unsaved => Err("The request could not be shown.".into()),
        }
    }

    /// Deletes a component once a person confirms it in the chat.
    async fn delete_component(&self, arguments: &Value) -> Result<String, String> {
        let Some(conversation) = self.turn_conversation(None) else {
            return Err("No conversation is waiting for this.".into());
        };
        let id = arguments["id"].as_str().unwrap_or_default().to_string();
        let component = self
            .content_blocking(id.clone(), |cms, id| cms.read_component(id))
            .await?;
        let name = component["name"].as_str().unwrap_or_default().to_string();
        let question = format!("Delete the component \u{201c}{name}\u{201d}?");
        match self
            .wait_for_person(
                conversation,
                "permission",
                question,
                Some("It disappears from every page that shows it.".into()),
            )
            .await
        {
            Waited::Reply(Reply::Permission { allow: true, .. }) => {
                self.content_blocking(id, |cms, id| cms.delete_component(id))
                    .await?;
                Ok(format!("The component \u{201c}{name}\u{201d} was deleted."))
            }
            Waited::Reply(_) => Err("The person chose to keep the component.".into()),
            Waited::Stopped => Err("The person stopped the assistant.".into()),
            Waited::TimedOut => Err("Nobody answered in time; the component was kept.".into()),
            Waited::Unsaved => Err("The request could not be shown.".into()),
        }
    }

    /// Why the content cannot be changed from the panel now, if so: the
    /// assistant is working on the site, or the terminal is open.
    pub fn content_locked(&self) -> Option<String> {
        let inner = self.inner();
        if let Some(open) = &inner.terminal {
            return Some(format!("{} has the terminal open", open.name));
        }
        if inner.running.is_some() {
            return Some("the assistant is working on the site".into());
        }
        None
    }

    /// Reads the content for the panel, one operation at a time with the
    /// assistant's.
    pub async fn read_content<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Cms) -> Result<T, crate::cms::CmsError> + Send + 'static,
    ) -> Result<T, crate::cms::CmsError> {
        let _one_at_a_time = self.content.lock().await;
        let site = self.env.site.clone();
        tokio::task::spawn_blocking(move || work(&Cms::new(&site)))
            .await
            .map_err(|err| crate::cms::CmsError::Io(err.to_string()))?
    }

    /// A change of the content made by a person in the panel: refused while
    /// the assistant works, saved in its own version (so a going back of
    /// the chat never mixes it with the assistant's), and the preview is
    /// built again. \`work\` returns the page and what to call the change.
    pub async fn edit_content(
        &self,
        person: i64,
        work: impl FnOnce(&Cms) -> Result<(Value, String), crate::cms::CmsError> + Send + 'static,
    ) -> Result<Value, ContentError> {
        if let Some(reason) = self.content_locked() {
            return Err(ContentError::Locked(reason));
        }
        let _one_at_a_time = self.content.lock().await;
        let site = self.env.site.clone();
        let page = tokio::task::spawn_blocking(move || {
            let (page, subject) = work(&Cms::new(&site)).map_err(ContentError::Cms)?;
            if site.join(".git").is_dir() {
                let message = format!("{subject}\n\nIn the panel: person {person}.");
                crate::git::commit_all(&site, &message).map_err(ContentError::Save)?;
            }
            Ok::<_, ContentError>(page)
        })
        .await
        .map_err(|err| ContentError::Save(err.to_string()))??;
        if let Some(builder) = &self.builder {
            builder.request();
        }
        Ok(page)
    }

    /// Runs one operation of the content on the blocking pool, one at a time.
    async fn content_blocking<T: Send + 'static>(
        &self,
        id: String,
        work: impl FnOnce(&Cms, &str) -> Result<T, crate::cms::CmsError> + Send + 'static,
    ) -> Result<T, String> {
        let _one_at_a_time = self.content.lock().await;
        let site = self.env.site.clone();
        tokio::task::spawn_blocking(move || work(&Cms::new(&site), &id).map_err(|e| e.to_string()))
            .await
            .map_err(|err| err.to_string())?
    }

    /// Whether the assistant may reach `host`, which is outside the
    /// allowlist: allowed before, or allowed now by a person in the chat.
    /// Without a turn in progress the answer is no. Requests for a host
    /// already waiting share its card and its answer.
    pub async fn approve_host(&self, host: String) -> bool {
        if lock(&self.hosts).contains(&host) {
            return true;
        }
        let (sender, waiting) = {
            let mut asking = lock(&self.asking_hosts);
            match asking.get(&host) {
                Some(answer) => (None, answer.clone()),
                None => {
                    let (sender, receiver) = tokio::sync::watch::channel(None);
                    asking.insert(host.clone(), receiver.clone());
                    (Some(sender), receiver)
                }
            }
        };
        let Some(sender) = sender else {
            let mut answer = waiting;
            return answer
                .wait_for(Option::is_some)
                .await
                .map(|answer| *answer == Some(true))
                .unwrap_or(false);
        };
        let allowed = match self.turn_conversation(None) {
            None => false,
            Some(conversation) => {
                let question = format!("Let the assistant reach {host}?");
                let detail = format!(
                    "The assistant wants to connect to https://{host}. Once allowed, it can reach it in later conversations too."
                );
                match self
                    .wait_for_person(conversation, "permission", question, Some(detail))
                    .await
                {
                    Waited::Reply(Reply::Permission {
                        allow: true,
                        person,
                    }) => {
                        self.remember_host(&host, person).await;
                        true
                    }
                    _ => false,
                }
            }
        };
        lock(&self.asking_hosts).remove(&host);
        let _ = sender.send(Some(allowed));
        allowed
    }

    async fn remember_host(&self, host: &str, person: i64) {
        lock(&self.hosts).insert(host.to_string());
        let host = host.to_string();
        let subject = format!("host:{host}");
        let saved = self
            .db
            .run(move |conn| {
                conn.execute(
                    "INSERT OR IGNORE INTO allowed_host (host, approved_by) VALUES (?1, ?2)",
                    params![host, person],
                )
            })
            .await;
        if let Err(err) = saved {
            eprintln!("alogeno: cannot save an allowed host: {err}");
            return;
        }
        self.log(
            Some(person),
            crate::activity::Environment::Installation,
            "sandbox.host_allowed",
            Some(subject),
        )
        .await;
    }

    /// The hosts people allowed the assistant to reach, newest first, with
    /// who allowed each and when.
    pub async fn allowed_hosts(&self) -> Result<Vec<AllowedHost>, String> {
        self.db
            .run(|conn| -> rusqlite::Result<Vec<AllowedHost>> {
                let mut statement = conn.prepare(
                    "SELECT host, approved_by, NULL, approved_at
                     FROM allowed_host
                     ORDER BY approved_at DESC, host",
                )?;
                statement
                    .query_map([], |row| {
                        Ok(AllowedHost {
                            host: row.get(0)?,
                            by: row.get(1)?,
                            by_name: row.get(2)?,
                            at: row.get(3)?,
                        })
                    })?
                    .collect()
            })
            .await
            .map_err(|err| err.to_string())
    }

    /// Takes back the permission to reach `host`: the next request for it
    /// asks a person again. Whether it was allowed.
    pub async fn forget_host(&self, host: &str, person: i64) -> Result<bool, String> {
        lock(&self.hosts).remove(host);
        let subject = format!("host:{host}");
        let host = host.to_string();
        let removed = self
            .db
            .run(move |conn| {
                conn.execute("DELETE FROM allowed_host WHERE host = ?1", params![host])
            })
            .await
            .map_err(|err| err.to_string())?;
        if removed > 0 {
            self.log(
                Some(person),
                crate::activity::Environment::Installation,
                "sandbox.host_removed",
                Some(subject),
            )
            .await;
        }
        Ok(removed > 0)
    }

    /// Shows questions in the chat and waits for a person's answers.
    async fn ask(&self, arguments: &Value) -> Result<String, String> {
        let questions = questions_of(arguments)?;
        let answers = self.ask_person(&questions).await?;
        let mut text = String::from("The person answered:");
        for (question, answer) in questions.iter().zip(answers) {
            text.push_str(&format!("\n- {}: {answer}", question.question));
        }
        Ok(text)
    }

    /// Shows questions in the chat and waits for the answers, one per
    /// question.
    async fn ask_person(&self, questions: &[Question]) -> Result<Vec<String>, String> {
        let Some(conversation) = self.turn_conversation(None) else {
            return Err("No conversation is waiting for this.".into());
        };
        let detail = json!({ "questions": questions }).to_string();
        let title = questions[0].question.clone();
        match self
            .wait_for_person(conversation, "question", title, Some(detail))
            .await
        {
            Waited::Reply(Reply::Answers(answers)) => Ok(answers),
            Waited::Reply(_) | Waited::Unsaved => Err("The questions could not be shown.".into()),
            Waited::Stopped => Err("The person stopped the assistant.".into()),
            Waited::TimedOut => Err("Nobody answered the questions in time.".into()),
        }
    }

    /// A button in the chat that opens the panel where the person can make a
    /// change: a section, a page and its field, or a picture.
    async fn show_in_panel(&self, arguments: &Value) -> Result<String, String> {
        let Some(conversation) = self.turn_conversation(None) else {
            return Err("No conversation is waiting for this.".into());
        };
        let label = arguments["label"]
            .as_str()
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .ok_or("Say what the button opens, in a few words, in `label`.")?;
        let label = clip(label, 60);
        let place = {
            let _one_at_a_time = self.content.lock().await;
            let site = self.env.site.clone();
            let arguments = arguments.clone();
            tokio::task::spawn_blocking(move || crate::place::find(&Cms::new(&site), &arguments))
                .await
                .map_err(|err| err.to_string())??
        };
        self.add(
            conversation,
            "link",
            label,
            Some(place.to_json().to_string()),
            None,
        )
        .await
        .ok_or("The button could not be shown.")?;
        Ok(format!(
            "The person sees a button, at the end of this reply, that opens {}.",
            place.path
        ))
    }

    /// The conversation of the turn in progress, when the caller belongs to
    /// it. Callers that cannot name their session (the MCP server) pass
    /// `None`: one turn runs at a time.
    fn turn_conversation(&self, session: Option<&str>) -> Option<i64> {
        let inner = self.inner();
        match (&inner.running, &inner.current) {
            (Some(running), Some(current))
                if !running.stopping
                    && session.is_none_or(|session| {
                        session == current.session
                            || current.cli_session.as_deref() == Some(session)
                    }) =>
            {
                Some(current.id)
            }
            _ => None,
        }
    }

    /// Adds a request to the chat and waits until a person answers it, the
    /// turn stops or the time runs out.
    async fn wait_for_person(
        &self,
        conversation: i64,
        kind: &'static str,
        text: String,
        detail: Option<String>,
    ) -> Waited {
        let Some(item) = self
            .add(conversation, kind, text, detail, Some("waiting"))
            .await
        else {
            return Waited::Unsaved;
        };
        let (sender, receiver) = oneshot::channel();
        let waiting = {
            let mut inner = self.inner();
            let waiting = match inner.running.as_mut() {
                Some(running) if !running.stopping => {
                    running.requests.insert(item.id, sender);
                    running.asked = true;
                    true
                }
                _ => false,
            };
            self.emit_turn(&inner);
            waiting
        };
        if !waiting {
            self.change(item.id, None, Some("stopped")).await;
            return Waited::Stopped;
        }
        let answer = tokio::time::timeout(REQUEST_WAIT, receiver).await;
        {
            let mut inner = self.inner();
            if let Some(running) = inner.running.as_mut() {
                running.requests.remove(&item.id);
            }
            self.emit_turn(&inner);
        }
        match answer {
            Ok(Ok(reply)) => Waited::Reply(reply),
            Ok(Err(_)) => {
                self.change(item.id, None, Some("stopped")).await;
                Waited::Stopped
            }
            Err(_) => {
                self.change(item.id, None, Some("stopped")).await;
                Waited::TimedOut
            }
        }
    }

    /// A person's answer to a permission request.
    pub async fn answer_permission(&self, person: i64, id: i64, allow: bool) -> Result<(), String> {
        let sender = self.take_request(id)?;
        let state = if allow { "allowed" } else { "denied" };
        let item = self
            .db
            .run(move |conn| answer_item(conn, id, state, person, None))
            .await
            .map_err(|err| err.to_string())?;
        self.emit(Event::Item { item });
        let _ = sender.send(Reply::Permission { allow, person });
        Ok(())
    }

    /// A person's answers to questions: for each question, the chosen
    /// options and, optionally, words of their own.
    pub async fn answer_questions(
        &self,
        person: i64,
        id: i64,
        answers: Vec<Answer>,
    ) -> Result<(), String> {
        self.check_holder(person)?;
        let item = self
            .db
            .run(move |conn| get_item(conn, id))
            .await
            .map_err(|_| "this request is no longer waiting")?;
        if item.kind != "question" || item.state.as_deref() != Some("waiting") {
            return Err("this request is no longer waiting".into());
        }
        let detail: Value = serde_json::from_str(item.detail.as_deref().unwrap_or("{}"))
            .map_err(|_| "the questions cannot be read")?;
        let questions: Vec<Question> = serde_json::from_value(detail["questions"].clone())
            .map_err(|_| "the questions cannot be read")?;
        let texts = answer_texts(&questions, &answers)?;
        let sender = self.take_request(id)?;
        let detail = json!({ "questions": questions, "answers": texts }).to_string();
        let item = self
            .db
            .run(move |conn| answer_item(conn, id, "answered", person, Some(&detail)))
            .await
            .map_err(|err| err.to_string())?;
        self.emit(Event::Item { item });
        let _ = sender.send(Reply::Answers(texts));
        Ok(())
    }

    fn take_request(&self, id: i64) -> Result<oneshot::Sender<Reply>, String> {
        self.inner()
            .running
            .as_mut()
            .and_then(|running| running.requests.remove(&id))
            .ok_or_else(|| "this request is no longer waiting".to_string())
    }

    fn holder_info(inner: &Inner) -> Option<HolderInfo> {
        let holder = inner.holder.as_ref()?;
        let remaining_ms = match holder.until {
            None => None,
            Some(until) => {
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return None;
                }
                Some(left.as_millis() as u64)
            }
        };
        Some(HolderInfo {
            id: holder.person,
            name: holder.name.clone(),
            remaining_ms,
        })
    }

    /// The name of another person holding the chat, if any.
    fn held_by_other(inner: &Inner, person: i64) -> Option<String> {
        Self::holder_info(inner)
            .filter(|holder| holder.id != person)
            .map(|holder| holder.name)
    }

    /// Whether `person` may act on the chat now: answer its requests, stop
    /// the assistant, start a new conversation.
    pub fn check_holder(&self, person: i64) -> Result<(), String> {
        match Self::held_by_other(&self.inner(), person) {
            Some(name) => Err(format!("{name} is using the assistant")),
            None => Ok(()),
        }
    }

    /// Takes the chat from the person holding it, once their turn is over.
    /// Returns that person, if it was someone else.
    pub fn take_over(&self, person: i64, name: &str) -> Result<Option<i64>, SendError> {
        let mut inner = self.inner();
        if inner.running.is_some() || inner.terminal.is_some() {
            return Err(SendError::Busy);
        }
        let previous = inner
            .holder
            .as_ref()
            .map(|holder| holder.person)
            .filter(|holder| *holder != person);
        inner.holder = Some(Holder {
            person,
            name: name.to_string(),
            until: Some(Instant::now() + HOLD),
        });
        self.emit_turn(&inner);
        Ok(previous)
    }

    fn emit_turn(&self, inner: &Inner) {
        self.emit(Event::Turn {
            turn: Self::turn_info(inner),
            conversation: Self::conversation_info(inner),
            holder: Self::holder_info(inner),
            terminal: Self::terminal_info(inner),
            problem: inner.problem.clone(),
        });
    }

    fn terminal_info(inner: &Inner) -> Option<TerminalInfo> {
        inner.terminal.as_ref().map(|open| TerminalInfo {
            id: open.person,
            name: open.name.clone(),
        })
    }

    /// Answers the hooks of the CLI on `listener`, one task per connection.
    /// Answers every connection on `listener` for this site, as the server
    /// does for the site whose assistant is at work.
    #[cfg(test)]
    pub async fn serve_hooks(self: Arc<Self>, listener: tokio::net::UnixListener) {
        while let Ok((stream, _)) = listener.accept().await {
            let chat = self.clone();
            tokio::spawn(async move { chat.hook_connection(stream).await });
        }
    }

    /// Answers one connection of the CLI's hooks or of alogeno's MCP server
    /// inside the sandbox, for this site.
    pub async fn hook_connection(&self, mut stream: UnixStream) {
        let mut event = Vec::new();
        if (&mut stream)
            .take(crate::hook::LIMIT)
            .read_to_end(&mut event)
            .await
            .is_err()
        {
            return;
        }
        let Ok(event) = serde_json::from_slice::<Value>(&event) else {
            return;
        };
        if let Some(answer) = self.hook(&event).await {
            let _ = stream.write_all(answer.to_string().as_bytes()).await;
        }
        let _ = stream.shutdown().await;
    }

    /// Records what the turn changed on the site in a commit, named after
    /// the person's message, and notes it in the activity log. Nothing
    /// happens when the turn changed nothing.
    async fn save_changes(&self, conversation: i64, person: i64, request: &str, stopped: bool) {
        let site = self.env.site.clone();
        if !site.join(".git").is_dir() {
            return;
        }
        let message = commit_message(request, conversation, person, stopped);
        let committed =
            tokio::task::spawn_blocking(move || crate::git::commit_all(&site, &message)).await;
        let commit = match committed {
            Ok(Ok(Some(commit))) => commit,
            Ok(Ok(None)) => return,
            Ok(Err(err)) => {
                eprintln!("alogeno: cannot save the changes of the turn: {err}");
                return;
            }
            Err(err) => {
                eprintln!("alogeno: cannot save the changes of the turn: {err}");
                return;
            }
        };
        self.log(
            Some(person),
            crate::activity::Environment::Preview,
            "agent.changes_saved",
            Some(format!("commit:{commit}")),
        )
        .await;
    }

    /// Opens the terminal on the current conversation for `person`, or
    /// attaches again to the one they have open. The chat waits until it
    /// closes.
    pub async fn open_terminal(
        self: &Arc<Self>,
        person: i64,
        name: &str,
        columns: u16,
        rows: u16,
        theme: &str,
    ) -> Result<Arc<Terminal>, String> {
        let provider = self.env.active();
        let current = {
            let inner = self.inner();
            if let Some(open) = &inner.terminal {
                if open.person == person && !open.handle.has_exited() {
                    open.handle.resize(columns, rows);
                    return Ok(open.handle.clone());
                }
                return Err(format!("{} has the terminal open", open.name));
            }
            if let Some(other) = Self::held_by_other(&inner, person) {
                return Err(format!("{other} is using the assistant"));
            }
            if inner.running.is_some() {
                return Err("the assistant is still working".into());
            }
            if !provider.capabilities().terminal {
                return Err(format!(
                    "the terminal is not available with {} yet",
                    provider.name()
                ));
            }
            inner.current.clone().filter(|c| c.provider == provider)
        };
        let slot = self
            .slot
            .try_take(&self.site_id, crate::gate::Kind::Terminal)
            .map_err(|_| "the assistant is working on another site".to_string())?;
        let current = match current {
            Some(current) => current,
            None => {
                let session = uuid();
                let title = "Terminal".to_string();
                let saved = {
                    let (session, title) = (session.clone(), title.clone());
                    self.db
                        .run(move |conn| {
                            conn.execute(
                                "INSERT INTO conversation (session, title, created_by, provider) VALUES (?1, ?2, ?3, ?4)",
                                params![session, title, person, provider.id()],
                            )
                            .map(|_| conn.last_insert_rowid())
                        })
                        .await
                        .map_err(|err| err.to_string())?
                };
                Current {
                    id: saved,
                    session,
                    title,
                    started: false,
                    provider,
                    cli_session: None,
                    mode: None,
                    model: None,
                }
            }
        };
        let (command, filter, offset) = match provider {
            Provider::Claude => {
                let flag = if self.resumable(provider, &current.session, current.started) {
                    "--resume"
                } else {
                    "--session-id"
                };
                let (command, filter) =
                    self.env.cli_terminal(provider, &[flag, &current.session])?;
                self.env.prepare_terminal(theme)?;
                let offset = std::fs::metadata(self.transcript(&current.session))
                    .map(|meta| meta.len())
                    .unwrap_or(0);
                (command, filter, offset)
            }
            // Codex resumes the chat's thread, or starts one; its turns are
            // read back from Codex when the terminal closes.
            Provider::Codex => {
                let (command, filter) = match &current.cli_session {
                    Some(thread) => self.env.cli_terminal(provider, &["resume", thread])?,
                    None => self.env.cli_terminal(provider, &[])?,
                };
                (command, filter, unix_now() as u64)
            }
        };
        let handle =
            Terminal::spawn(command, filter, columns, rows).map_err(|err| err.to_string())?;
        {
            let mut inner = self.inner();
            inner.current = Some(current.clone());
            inner.holder = Some(Holder {
                person,
                name: name.to_string(),
                until: None,
            });
            inner.terminal = Some(OpenTerminal {
                person,
                name: name.to_string(),
                _slot: Some(slot),
                conversation: current.id,
                session: current.session.clone(),
                provider,
                cli_session: current.cli_session.clone(),
                handle: handle.clone(),
                offset,
            });
            self.emit_turn(&inner);
        }
        self.record(person, "terminal.opened", None).await;
        let chat = self.clone();
        let watched = handle.clone();
        tokio::spawn(async move {
            watched.exited().await;
            chat.terminal_closed().await;
        });
        Ok(handle)
    }

    /// Whether a started conversation can be resumed. For Claude its
    /// transcript must be in this site's folder: one written before the
    /// sites had folders of their own, or removed by hand, would make the CLI
    /// stop with an error; the conversation then goes on in a new session,
    /// without what the CLI remembered.
    fn resumable(&self, provider: Provider, session: &str, started: bool) -> bool {
        started
            && (provider != Provider::Claude
                || !self.env.keeps_transcripts()
                || self.transcript(session).exists())
    }

    /// The file where the CLI keeps a conversation, in this site's folder.
    fn transcript(&self, session: &str) -> std::path::PathBuf {
        claude::transcript(&self.env.projects(), session)
    }

    /// Closes the terminal `person` has open.
    pub fn close_terminal(&self, person: i64) -> Result<(), String> {
        let inner = self.inner();
        match &inner.terminal {
            Some(open) if open.person == person => {
                open.handle.kill();
                Ok(())
            }
            Some(open) => Err(format!("{} has the terminal open", open.name)),
            None => Ok(()),
        }
    }

    /// What was said in Claude's terminal: the part of the transcript
    /// written while it was open; and whether the session exists.
    fn claude_said(&self, open: &OpenTerminal) -> (Vec<Said>, Option<String>, bool) {
        // The CLI writes the conversation once something was said in it.
        let transcript = self.transcript(&open.session);
        let said = std::fs::read(&transcript)
            .map(|bytes| {
                bytes
                    .get(open.offset as usize..)
                    .unwrap_or_default()
                    .to_vec()
            })
            .unwrap_or_default();
        (
            claude::transcript_lines(&String::from_utf8_lossy(&said)),
            None,
            transcript.is_file(),
        )
    }

    /// What was said in Codex's terminal, as Codex tells it: the turns of
    /// the thread that started while it was open. A thread started in the
    /// terminal is found among the latest ones.
    async fn codex_said(&self, open: &OpenTerminal) -> (Vec<Said>, Option<String>, bool) {
        let read = async {
            let mut server = self.env.codex_server().await?;
            let thread = match &open.cli_session {
                Some(thread) => Some(thread.clone()),
                None => {
                    let listed = server
                        .call("thread/list", json!({ "cwd": "/site", "limit": 5 }))
                        .await?;
                    codex::newest_thread(&listed, open.offset as i64)
                }
            };
            let Some(thread) = thread else {
                server.stop().await;
                return Ok((Vec::new(), None));
            };
            let read = server
                .call(
                    "thread/read",
                    json!({ "threadId": thread, "includeTurns": true }),
                )
                .await;
            server.stop().await;
            Ok::<_, String>((codex::said_since(&read?, open.offset as i64), Some(thread)))
        };
        match read.await {
            Ok((said, thread)) => {
                let started = thread.is_some();
                (said, thread, started)
            }
            Err(err) => {
                eprintln!("alogeno: what was said in Codex's terminal could not be read: {err}");
                (
                    Vec::new(),
                    open.cli_session.clone(),
                    open.cli_session.is_some(),
                )
            }
        }
    }

    /// The terminal ended: its work is saved and shown in the chat, and the
    /// chat goes on.
    async fn terminal_closed(&self) {
        let Some(open) = self.inner().terminal.take() else {
            return;
        };
        let (said, thread, started) = match open.provider {
            Provider::Claude => self.claude_said(&open),
            Provider::Codex => self.codex_said(&open).await,
        };
        if started {
            let id = open.conversation;
            let thread = thread.clone();
            let _ = self
                .db
                .run(move |conn| {
                    conn.execute(
                        "UPDATE conversation SET started = 1, cli_session = coalesce(cli_session, ?2) WHERE id = ?1",
                        params![id, thread],
                    )
                })
                .await;
        }
        // What was said in the terminal joins the chat, so the conversation
        // reads on; a terminal closed without a word leaves nothing.
        let items = said_items(said, &self.env.site);
        if !items.is_empty() {
            self.add(
                open.conversation,
                "notice",
                format!("{} used the terminal.", open.name),
                None,
                None,
            )
            .await;
        }
        for (kind, text, detail, state) in items {
            let author = (kind == "person").then_some(open.person);
            let conversation = open.conversation;
            let saved = self
                .db
                .run(move |conn| {
                    insert_item(
                        conn,
                        conversation,
                        kind,
                        &text,
                        detail.as_deref(),
                        state,
                        author,
                    )
                })
                .await;
            if let Ok(item) = saved {
                self.emit(Event::Item { item });
            }
        }
        self.save_changes(
            open.conversation,
            open.person,
            "Changes made in the terminal",
            false,
        )
        .await;
        self.record(open.person, "terminal.closed", None).await;
        {
            let mut inner = self.inner();
            if let Some(current) = inner.current.as_mut().filter(|c| c.id == open.conversation) {
                current.started = current.started || started;
                if current.cli_session.is_none() {
                    current.cli_session = thread;
                }
            }
            if let Some(holder) = inner.holder.as_mut() {
                holder.until = Some(Instant::now() + HOLD);
            }
            self.emit_turn(&inner);
        }
        if let Some(builder) = &self.builder {
            builder.request();
        }
    }

    async fn record(&self, person: i64, action: &'static str, subject: Option<String>) {
        self.log(
            Some(person),
            crate::activity::Environment::Installation,
            action,
            subject,
        )
        .await;
    }

    /// Appends an entry about this site to the installation's activity log.
    async fn log(
        &self,
        actor: Option<i64>,
        environment: crate::activity::Environment,
        action: &'static str,
        subject: Option<String>,
    ) {
        let site = self.site_id.clone();
        let _ = self
            .installation
            .run(move |conn| {
                crate::activity::record_site(
                    conn,
                    Some(&site),
                    actor,
                    environment,
                    action,
                    subject.as_deref(),
                )
            })
            .await;
    }

    /// Sets the live text of an item and tells the panel.
    fn append(&self, id: i64, piece: &str) {
        let seq = {
            let mut inner = self.inner();
            let Some(running) = inner.running.as_mut() else {
                return;
            };
            let (text, seq) = running.live.entry(id).or_default();
            text.push_str(piece);
            *seq += 1;
            *seq
        };
        self.emit(Event::Delta {
            id,
            seq,
            text: piece.to_string(),
        });
    }

    fn live_text(&self, id: i64) -> Option<String> {
        self.inner()
            .running
            .as_ref()
            .and_then(|running| running.live.get(&id))
            .map(|(text, _)| text.clone())
    }

    fn forget_live(&self, id: i64) {
        if let Some(running) = self.inner().running.as_mut() {
            running.live.remove(&id);
        }
    }

    async fn add(
        &self,
        conversation: i64,
        kind: &'static str,
        text: String,
        detail: Option<String>,
        state: Option<&'static str>,
    ) -> Option<Item> {
        let item = self
            .db
            .run(move |conn| {
                insert_item(
                    conn,
                    conversation,
                    kind,
                    &text,
                    detail.as_deref(),
                    state,
                    None,
                )
            })
            .await
            .map_err(|err| eprintln!("alogeno: cannot save the chat: {err}"))
            .ok()?;
        self.emit(Event::Item { item: item.clone() });
        Some(item)
    }

    async fn change(&self, id: i64, text: Option<String>, state: Option<&'static str>) {
        let item = self
            .db
            .run(move |conn| update_item(conn, id, text.as_deref(), state))
            .await;
        match item {
            Ok(item) => self.emit(Event::Item { item }),
            Err(err) => eprintln!("alogeno: cannot save the chat: {err}"),
        }
    }
}

/// How the CLI process ended.
struct Ended {
    stopped: bool,
    /// The CLI could not run.
    failure: Option<String>,
    /// Its error output.
    errors: String,
}

impl Ended {
    fn failed(problem: String) -> Self {
        Self {
            stopped: false,
            failure: Some(problem),
            errors: String::new(),
        }
    }
}

/// The end of a turn as the CLI reported it.
#[derive(Debug, Default)]
struct Outcome {
    error: Option<String>,
    problem: Option<Problem>,
    /// Actions the CLI was not allowed to do.
    denied: Vec<String>,
}

/// Turns the events of one turn into chat items.
struct Processor {
    conversation: i64,
    provider: Provider,
    /// Reads Claude's events, for a turn of Claude.
    claude: claude::Parser,
    /// Item being written for each text of the assistant.
    texts: HashMap<String, i64>,
    /// Items being written, waiting for their complete text, in order.
    open: Vec<(&'static str, i64)>,
    /// Action item of each tool call.
    tools: HashMap<String, i64>,
    /// The CLI created the session.
    initialized: bool,
    outcome: Option<Outcome>,
}

impl Processor {
    #[cfg(test)]
    fn new(conversation: i64) -> Self {
        Self::of(conversation, Provider::Claude)
    }

    fn of(conversation: i64, provider: Provider) -> Self {
        Self {
            conversation,
            provider,
            claude: claude::Parser::default(),
            texts: HashMap::new(),
            open: Vec::new(),
            tools: HashMap::new(),
            initialized: false,
            outcome: None,
        }
    }

    /// An event of Claude Code's stream-json.
    async fn handle_claude(&mut self, chat: &Chat, event: &Value) {
        for event in self.claude.parse(event) {
            self.apply(chat, event).await;
        }
    }

    async fn apply(&mut self, chat: &Chat, event: AgentEvent) {
        match event {
            AgentEvent::Session { cli_id } if !self.initialized => {
                self.initialized = true;
                let id = self.conversation;
                let saved = Some(cli_id.clone()).filter(|c| !c.is_empty());
                let _ = chat
                    .db
                    .run(move |conn| {
                        conn.execute(
                            "UPDATE conversation SET started = 1, cli_session = coalesce(cli_session, ?2) WHERE id = ?1",
                            params![id, saved],
                        )
                    })
                    .await;
                if let Some(current) = chat.inner().current.as_mut().filter(|c| c.id == id) {
                    current.started = true;
                    if current.cli_session.is_none() && !cli_id.is_empty() {
                        current.cli_session = Some(cli_id);
                    }
                }
            }
            AgentEvent::Session { .. } => {}
            AgentEvent::TextStart { item, kind } => {
                let kind = kind.item();
                if let Some(added) = chat
                    .add(self.conversation, kind, String::new(), None, None)
                    .await
                {
                    self.texts.insert(item, added.id);
                    self.open.push((kind, added.id));
                }
            }
            AgentEvent::TextDelta { item, text } => {
                if let Some(id) = self.texts.get(&item) {
                    chat.append(*id, &text);
                }
            }
            AgentEvent::TextDone { item, kind, text } => {
                let kind = kind.item();
                let named = item
                    .and_then(|item| self.texts.get(&item).copied())
                    .and_then(|id| self.open.iter().position(|(_, open)| *open == id));
                match named.or_else(|| self.open.iter().position(|(open, _)| *open == kind)) {
                    Some(position) => {
                        let (_, id) = self.open.remove(position);
                        chat.forget_live(id);
                        chat.change(id, Some(text), None).await;
                    }
                    None if !text.trim().is_empty() => {
                        chat.add(self.conversation, kind, text, None, None).await;
                    }
                    None => {}
                }
            }
            AgentEvent::ToolStart { id, tool } => {
                if tool.hidden() {
                    return;
                }
                let (label, detail) = action_of(&tool, &chat.env.site);
                if let Some(item) = chat
                    .add(
                        self.conversation,
                        "activity",
                        label,
                        detail,
                        Some("running"),
                    )
                    .await
                    && !id.is_empty()
                {
                    self.tools.insert(id, item.id);
                }
            }
            AgentEvent::ToolEnd { id, failed } => {
                if let Some(item) = self.tools.remove(&id) {
                    let state = if failed { "failed" } else { "done" };
                    chat.change(item, None, Some(state)).await;
                }
            }
            AgentEvent::Limits(limits) => {
                chat.inner().limits = Some(limits.clone());
                chat.emit(Event::Limits { limits });
            }
            AgentEvent::Usage(usage) => {
                let (conversation, provider) = (self.conversation, self.provider.id());
                let site = chat.site_id.clone();
                let saved = chat
                    .installation
                    .run(move |conn| {
                        conn.execute(
                            "INSERT INTO turn_usage (site_id, conversation, provider, input_tokens, cached_tokens, output_tokens, cost)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                            params![
                                site,
                                conversation,
                                provider,
                                usage.input as i64,
                                usage.cached as i64,
                                usage.output as i64,
                                usage.cost
                            ],
                        )
                    })
                    .await;
                if let Err(err) = saved {
                    eprintln!("alogeno: cannot save the usage of the turn: {err}");
                }
            }
            AgentEvent::Finished {
                error,
                problem,
                denied,
            } => {
                let denied = denied
                    .iter()
                    .map(|tool| {
                        let (label, detail) = describe(tool);
                        // A command without a description is named by the
                        // command itself.
                        match (tool, detail) {
                            (
                                Tool::Command {
                                    description: None, ..
                                },
                                Some(command),
                            ) => command,
                            _ => label,
                        }
                    })
                    .collect();
                self.outcome = Some(Outcome {
                    error,
                    problem,
                    denied,
                });
            }
        }
    }

    /// Saves what is left of the turn and tells how it ended.
    /// Saves the text written so far and marks the actions left running.
    async fn settle(&mut self, chat: &Chat) {
        for (_, id) in std::mem::take(&mut self.open) {
            let text = chat.live_text(id).unwrap_or_default();
            chat.change(id, Some(text), None).await;
        }
        for (_, id) in std::mem::take(&mut self.tools) {
            chat.change(id, None, Some("stopped")).await;
        }
    }

    async fn finish(mut self, chat: &Chat, ended: Ended) {
        self.settle(chat).await;
        let conversation = self.conversation;
        let mut problem = None;
        if ended.stopped && chat.is_closed() {
            let text = "Stopped: alogeno was restarting. Send the message again to continue.";
            chat.add(conversation, "notice", text.into(), None, None)
                .await;
        } else if ended.stopped {
            chat.add(conversation, "notice", "Stopped.".into(), None, None)
                .await;
        } else if let Some(failure) = ended.failure {
            chat.add(
                conversation,
                "error",
                "The assistant could not start.".into(),
                Some(failure),
                None,
            )
            .await;
            problem = Some(ProblemInfo {
                kind: "error",
                until: None,
            });
        } else if let Some(outcome) = self.outcome.take() {
            if let Some(error) = outcome.error {
                let found = ProblemInfo::of(outcome.problem.unwrap_or(Problem::Other));
                let name = self.provider.name();
                let text = match found.kind {
                    "limit" => format!("The {name} plan is used up for now."),
                    "login" => format!("{name} is disconnected."),
                    _ => error.clone(),
                };
                let detail = (found.kind != "error").then_some(error);
                chat.add(conversation, "error", text, detail, None).await;
                problem = Some(found);
            }
            // Refusals the person decided are already on their cards; others
            // (the request could not reach the panel) are listed here.
            let asked = chat
                .inner()
                .running
                .as_ref()
                .is_some_and(|running| running.asked);
            if !outcome.denied.is_empty() && !asked {
                let text = "Some actions were not allowed.";
                chat.add(
                    conversation,
                    "notice",
                    text.into(),
                    Some(outcome.denied.join(" · ")),
                    None,
                )
                .await;
            }
        } else {
            let last = ended
                .errors
                .lines()
                .rev()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(|line| clip(line, DETAIL_LENGTH));
            if !ended.errors.trim().is_empty() {
                eprintln!(
                    "alogeno: the assistant stopped unexpectedly:\n{}",
                    ended.errors.trim_end()
                );
            }
            chat.add(
                conversation,
                "error",
                "The assistant stopped unexpectedly.".into(),
                last,
                None,
            )
            .await;
            problem = Some(ProblemInfo {
                kind: "error",
                until: None,
            });
        }
        chat.inner().problem = problem;
        let _ = chat
            .db
            .run(move |conn| {
                conn.execute(
                    "UPDATE conversation SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?1",
                    [conversation],
                )
            })
            .await;
        let asker = chat
            .inner()
            .running
            .as_ref()
            .map(|running| (running.person, running.request.clone()));
        if let Some((person, request)) = asker {
            chat.save_changes(conversation, person, &request, ended.stopped)
                .await;
        }
        // The turn ends with the preview showing its changes, or with the
        // reason it cannot.
        if let Some(builder) = chat.builder.clone().filter(|_| !chat.is_closed()) {
            {
                let mut inner = chat.inner();
                if let Some(running) = inner.running.as_mut() {
                    running.updating = true;
                }
                chat.emit_turn(&inner);
            }
            match tokio::time::timeout(BUILD_WAIT, builder.build_and_wait()).await {
                Ok(status) if status.state == "failed" => {
                    let detail = status.error.map(|error| clip(&error, DETAIL_LENGTH));
                    chat.add(
                        conversation,
                        "error",
                        "The preview could not be updated.".into(),
                        detail,
                        None,
                    )
                    .await;
                }
                Ok(_) => {}
                Err(_) => {
                    chat.add(
                        conversation,
                        "notice",
                        "The preview is still being updated.".into(),
                        None,
                        None,
                    )
                    .await;
                }
            }
        }
        {
            let mut inner = chat.inner();
            inner.running = None;
            // The person keeps the chat for a while, to go on with it.
            if let Some(holder) = inner.holder.as_mut() {
                holder.until = Some(Instant::now() + HOLD);
            }
            chat.emit_turn(&inner);
        }
    }
}

/// A person's reply to a request of the assistant.
enum Reply {
    /// Whether the person allowed it, and who answered.
    Permission { allow: bool, person: i64 },
    /// One text per question.
    Answers(Vec<String>),
}

/// How waiting for a person ended.
enum Waited {
    Reply(Reply),
    Stopped,
    TimedOut,
    /// The request could not be saved.
    Unsaved,
}

/// A question of the assistant, as the panel shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Question {
    pub question: String,
    pub options: Vec<QuestionOption>,
    #[serde(default)]
    pub multiple: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A person's answer to one question.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct Answer {
    #[serde(default)]
    pub choices: Vec<String>,
    /// Words of the person's own, instead of or besides the options.
    #[serde(default)]
    pub other: Option<String>,
}

/// The questions of a call of the `ask` tool, checked.
fn questions_of(arguments: &Value) -> Result<Vec<Question>, String> {
    let text = |value: &Value, limit: usize, what: &str| -> Result<String, String> {
        let text = value.as_str().map(str::trim).unwrap_or("");
        if text.is_empty() {
            return Err(format!("every {what} needs text"));
        }
        Ok(clip(text, limit))
    };
    let list = arguments["questions"]
        .as_array()
        .ok_or("questions must be a list")?;
    if list.is_empty() || list.len() > 4 {
        return Err("ask one to four questions".into());
    }
    list.iter()
        .map(|question| {
            let options = question["options"]
                .as_array()
                .ok_or("options must be a list")?;
            if options.len() < 2 || options.len() > 6 {
                return Err("every question needs two to six options".to_string());
            }
            Ok(Question {
                question: text(&question["question"], 300, "question")?,
                options: options
                    .iter()
                    .map(|option| {
                        Ok(QuestionOption {
                            label: text(&option["label"], 100, "option")?,
                            description: option["description"]
                                .as_str()
                                .map(str::trim)
                                .filter(|d| !d.is_empty())
                                .map(|d| clip(d, 200)),
                        })
                    })
                    .collect::<Result<_, String>>()?,
                multiple: question["multiSelect"] == true,
            })
        })
        .collect()
}

/// The answers as text, one per question, checked against the questions.
fn answer_texts(questions: &[Question], answers: &[Answer]) -> Result<Vec<String>, String> {
    if answers.len() != questions.len() {
        return Err("answer every question".into());
    }
    questions
        .iter()
        .zip(answers)
        .map(|(question, answer)| {
            let other = answer
                .other
                .as_deref()
                .map(str::trim)
                .filter(|o| !o.is_empty());
            if answer.choices.is_empty() && other.is_none() {
                return Err("answer every question".to_string());
            }
            if !question.multiple && answer.choices.len() > 1 {
                return Err("choose one option".to_string());
            }
            if other.is_some_and(|o| o.chars().count() > 2000) {
                return Err("the answer is too long".to_string());
            }
            for choice in &answer.choices {
                if !question.options.iter().any(|o| &o.label == choice) {
                    return Err("that option is not one of the question's".to_string());
                }
            }
            let mut parts: Vec<String> = answer.choices.clone();
            if let Some(other) = other {
                parts.push(other.to_string());
            }
            Ok(parts.join(", "))
        })
        .collect()
}

/// The chat lines of what was said in the terminal: the person's messages,
/// the answers and the actions, in order.
fn said_items(
    said: Vec<Said>,
    site: &Path,
) -> Vec<(&'static str, String, Option<String>, Option<&'static str>)> {
    said.into_iter()
        .filter_map(|said| match said {
            Said::Person(text) => Some(("person", text, None, None)),
            Said::Answer(text) => Some(("answer", text, None, None)),
            Said::Tool(tool) if tool.hidden() => None,
            Said::Tool(tool) => {
                let (label, detail) = action_of(&tool, site);
                Some(("activity", label, detail, Some("done")))
            }
        })
        .collect()
}

/// The line of an action: named after the page or the component for the
/// tools of the content, described otherwise.
fn action_of(tool: &Tool, site: &Path) -> (String, Option<String>) {
    content_activity(tool, site).unwrap_or_else(|| describe(tool))
}

/// The line of an action on the content, named after the page.
fn content_activity(tool: &Tool, site: &Path) -> Option<(String, Option<String>)> {
    let Tool::Alogeno { name, input } = tool else {
        return None;
    };
    let title_of = |id: &Value| {
        id.as_str()
            .and_then(|id| Cms::new(site).read(id).ok())
            .and_then(|page| page["title"].as_str().map(String::from))
    };
    let named = |verb: &str, title: Option<String>| match title {
        Some(title) => format!("{verb} the page \u{201c}{title}\u{201d}"),
        None => format!("{verb} a page"),
    };
    let component_name = |id: &Value| {
        id.as_str()
            .and_then(|id| Cms::new(site).read_component(id).ok())
            .and_then(|component| component["name"].as_str().map(String::from))
    };
    let component = |verb: &str, name: Option<String>| match name {
        Some(name) => format!("{verb} the component \u{201c}{name}\u{201d}"),
        None => format!("{verb} a component"),
    };
    Some(match name.as_str() {
        "list_pages" => ("Looked at the pages".into(), None),
        "page_types" => ("Looked at the page types".into(), None),
        "list_media" => ("Looked at the pictures".into(), None),
        "read_page" => (named("Read", title_of(&input["id"])), None),
        "update_page" => {
            let keys = input["changes"]
                .as_object()
                .map(|changes| changes.keys().cloned().collect::<Vec<_>>().join(", "))
                .filter(|keys| !keys.is_empty());
            // Named after its new title, when the title changes.
            let title = input["changes"]["title"]
                .as_str()
                .map(String::from)
                .or_else(|| title_of(&input["id"]));
            (named("Changed", title), keys)
        }
        "create_page" => (
            named("Created", input["page"]["title"].as_str().map(String::from)),
            None,
        ),
        "list_components" => ("Looked at the components".into(), None),
        "read_component" => (component("Read", component_name(&input["id"])), None),
        "update_component" => {
            let keys = input["changes"]
                .as_object()
                .map(|changes| changes.keys().cloned().collect::<Vec<_>>().join(", "))
                .filter(|keys| !keys.is_empty());
            let name = input["changes"]["name"]
                .as_str()
                .map(String::from)
                .or_else(|| component_name(&input["id"]));
            (component("Changed", name), keys)
        }
        "create_component" => (
            component(
                "Created",
                input["component"]["name"].as_str().map(String::from),
            ),
            None,
        ),
        _ => return None,
    })
}

/// A path as the person sees it: inside the site, without `/site/`.
fn shown_path(path: &Option<String>, otherwise: &str) -> String {
    path.as_deref()
        .map(|path| path.strip_prefix("/site/").unwrap_or(path).to_string())
        .unwrap_or_else(|| otherwise.to_string())
}

/// A text of a tool call for the second line of an action.
fn shown_text(text: &Option<String>) -> Option<String> {
    text.as_deref()
        .map(|value| clip(value.trim(), DETAIL_LENGTH))
}

/// A short description of a tool call, and a second line with its argument.
fn describe(tool: &Tool) -> (String, Option<String>) {
    match tool {
        Tool::Read { path } => (format!("Read {}", shown_path(path, "a file")), None),
        Tool::Write { path } => (format!("Wrote {}", shown_path(path, "a file")), None),
        Tool::Edit { path } => (format!("Edited {}", shown_path(path, "a file")), None),
        Tool::EditNotebook { path } => (format!("Edited {}", shown_path(path, "a notebook")), None),
        Tool::FindFiles { pattern } => ("Looked for files".into(), shown_text(pattern)),
        Tool::SearchFiles { pattern } => ("Searched the files".into(), shown_text(pattern)),
        Tool::Command {
            command,
            description,
        } => (
            shown_text(description).unwrap_or_else(|| "Ran a command".into()),
            shown_text(command),
        ),
        Tool::WebFetch { url } => ("Read a web page".into(), shown_text(url)),
        Tool::WebSearch { query } => ("Searched the web".into(), shown_text(query)),
        Tool::Helper { description } => ("Asked a helper".into(), shown_text(description)),
        Tool::Plan => ("Updated its plan".into(), None),
        Tool::PlanReady { plan } => ("Proposed a plan".into(), shown_text(plan)),
        Tool::Skill { name } => ("Used a skill".into(), shown_text(name)),
        Tool::Alogeno { name, .. } => (format!("Used {name}"), None),
        Tool::Internal => ("Used a tool".into(), None),
        Tool::Other(name) if name.is_empty() => ("Used a tool".into(), None),
        Tool::Other(name) => (format!("Used {name}"), None),
    }
}

/// A permission request as a question, and its technical detail.
fn request_of(tool: &Tool) -> (String, Option<String>) {
    let (_, detail) = describe(tool);
    let title = match tool {
        Tool::Command { description, .. } => {
            match description
                .as_deref()
                .map(str::trim)
                .filter(|d| !d.is_empty())
            {
                Some(description) => {
                    let description = clip(description.trim_end_matches('.'), 120);
                    let mut letters = description.chars();
                    let first = letters
                        .next()
                        .map(|c| c.to_uppercase().to_string())
                        .unwrap_or_default();
                    format!("{first}{}?", letters.as_str())
                }
                None => "Run a command?".into(),
            }
        }
        Tool::Read { path } => format!("Read {}?", shown_path(path, "a file")),
        Tool::Write { path } => format!("Write {}?", shown_path(path, "a file")),
        Tool::Edit { path } | Tool::EditNotebook { path } => {
            format!("Change {}?", shown_path(path, "a file"))
        }
        Tool::FindFiles { .. } | Tool::SearchFiles { .. } => "Search the files?".into(),
        Tool::WebFetch { .. } => "Open a web page?".into(),
        Tool::WebSearch { .. } => "Search the web?".into(),
        Tool::Helper { .. } => "Start a helper?".into(),
        Tool::Plan => "Update its plan?".into(),
        Tool::PlanReady { .. } => "Go ahead with this plan?".into(),
        Tool::Skill { .. } => "Use a skill?".into(),
        Tool::Alogeno { name, .. } => format!("Use {name}?"),
        Tool::Internal => "Use a tool?".into(),
        Tool::Other(name) if name.is_empty() => "Use a tool?".into(),
        Tool::Other(name) => format!("Use {name}?"),
    };
    // A command is the detail even when its description is the title.
    let detail = match tool {
        Tool::Command { command, .. } => command.as_deref().map(|c| clip(c.trim(), DETAIL_LENGTH)),
        // The whole plan: it is what the person decides on.
        Tool::PlanReady { plan } => plan.as_deref().map(|p| clip(p.trim(), PLAN_LENGTH)),
        _ => detail,
    };
    (title, detail)
}

/// The first `length` characters of `text`, with an ellipsis when cut.
fn clip(text: &str, length: usize) -> String {
    match text.char_indices().nth(length) {
        Some((end, _)) => format!("{}…", text[..end].trim_end()),
        None => text.to_string(),
    }
}

/// The message of the commit of a turn: the person's request as the
/// subject, and where it came from.
fn commit_message(request: &str, conversation: i64, person: i64, stopped: bool) -> String {
    let subject = clip(
        &request.split_whitespace().collect::<Vec<_>>().join(" "),
        72,
    );
    let mut message =
        format!("{subject}\n\nAsked in the chat: conversation {conversation}, person {person}.");
    if stopped {
        message.push_str("\nThe assistant was stopped before the end.");
    }
    message
}

fn title_of(message: &str) -> String {
    let line = message.split_whitespace().collect::<Vec<_>>().join(" ");
    clip(&line, TITLE_LENGTH)
}

/// Seconds since the Unix epoch.
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// A random UUID (version 4), as the CLI expects for a session id.
fn uuid() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the operating system random source");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

const ITEM_COLUMNS: &str = "i.id, i.kind, i.text, i.detail, i.state, NULL, i.created_at, i.version
     FROM chat_item i";

fn item_of(row: &rusqlite::Row<'_>) -> rusqlite::Result<Item> {
    Ok(Item {
        id: row.get(0)?,
        kind: row.get(1)?,
        text: row.get(2)?,
        detail: row.get(3)?,
        state: row.get(4)?,
        author: row.get(5)?,
        at: row.get(6)?,
        seq: 0,
        back: row.get::<_, Option<String>>(7)?.is_some(),
    })
}

/// The assistant the chat uses.
pub fn assistant_in_use(conn: &Connection) -> rusqlite::Result<Provider> {
    let id: String = conn.query_row("SELECT assistant FROM installation", [], |row| row.get(0))?;
    Ok(Provider::parse(&id).unwrap_or(Provider::Claude))
}

/// The latest conversation with `provider`, the assistant in use.
fn latest(conn: &Connection, provider: &str) -> rusqlite::Result<Option<Current>> {
    conn.query_row(
        "SELECT id, session, title, started, provider, cli_session, mode, model FROM conversation
         WHERE provider = ?1
         ORDER BY updated_at DESC, id DESC LIMIT 1",
        [provider],
        |row| {
            Ok(Current {
                id: row.get(0)?,
                session: row.get(1)?,
                title: row.get(2)?,
                started: row.get::<_, i64>(3)? == 1,
                provider: Provider::parse(&row.get::<_, String>(4)?).unwrap_or(Provider::Claude),
                cli_session: row.get(5)?,
                mode: row.get(6)?,
                model: row.get(7)?,
            })
        },
    )
    .optional()
}

fn items(conn: &Connection, conversation: i64) -> rusqlite::Result<Vec<Item>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {ITEM_COLUMNS} WHERE i.conversation_id = ?1 ORDER BY i.id"
    ))?;
    statement.query_map([conversation], item_of)?.collect()
}

fn get_item(conn: &Connection, id: i64) -> rusqlite::Result<Item> {
    conn.query_row(
        &format!("SELECT {ITEM_COLUMNS} WHERE i.id = ?1"),
        [id],
        item_of,
    )
}

fn insert_item(
    conn: &Connection,
    conversation: i64,
    kind: &str,
    text: &str,
    detail: Option<&str>,
    state: Option<&str>,
    author: Option<i64>,
) -> rusqlite::Result<Item> {
    conn.execute(
        "INSERT INTO chat_item (conversation_id, kind, text, detail, state, author_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![conversation, kind, text, detail, state, author],
    )?;
    get_item(conn, conn.last_insert_rowid())
}

/// Records a person's answer to a request that is still waiting.
fn answer_item(
    conn: &Connection,
    id: i64,
    state: &str,
    person: i64,
    detail: Option<&str>,
) -> rusqlite::Result<Item> {
    let changed = conn.execute(
        "UPDATE chat_item SET state = ?2, author_id = ?3, detail = COALESCE(?4, detail)
         WHERE id = ?1 AND state = 'waiting'",
        params![id, state, person, detail],
    )?;
    if changed == 0 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    get_item(conn, id)
}

fn update_item(
    conn: &Connection,
    id: i64,
    text: Option<&str>,
    state: Option<&str>,
) -> rusqlite::Result<Item> {
    conn.execute(
        "UPDATE chat_item SET text = COALESCE(?2, text), state = COALESCE(?3, state) WHERE id = ?1",
        params![id, text, state],
    )?;
    get_item(conn, id)
}

/// Locks a mutex, also after a panic in another holder.
/// A host a person allowed the assistant to reach.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AllowedHost {
    pub host: String,
    pub by: Option<i64>,
    /// The name of the person, when they still have one.
    pub by_name: Option<String>,
    pub at: String,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A chat whose assistant cannot run, for tests of other modules.
#[cfg(test)]
pub fn idle(installation: db::Handle, site: db::Handle) -> Arc<Chat> {
    Chat::with(
        crate::agent::idle().environment(),
        installation,
        site,
        "01TESTSITE".into(),
        crate::gate::AgentSlot::new(),
        None,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The installation and site databases of a test, and the owner's id.
    fn databases() -> (db::Handle, db::Handle, i64) {
        (
            db::Handle::new(db::memory_installation()),
            db::Handle::new(db::memory_site()),
            1,
        )
    }

    async fn chat() -> (Arc<Chat>, i64) {
        let (installation, site, person) = databases();
        (idle(installation, site), person)
    }

    fn events(lines: &str) -> Vec<Value> {
        lines
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// One turn as the CLI reports it: a command, a file read, an answer
    /// written in two pieces.
    const TURN: &str = r#"
{"type":"system","subtype":"init","session_id":"s"}
{"type":"stream_event","event":{"type":"message_start"},"parent_tool_use_id":null}
{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","name":"Bash"}},"parent_tool_use_id":null}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls","description":"List the files"}}]},"parent_tool_use_id":null}
{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":false}]},"parent_tool_use_id":null}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Read","input":{"file_path":"/site/alogeno.json"}}]},"parent_tool_use_id":null}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2"}]},"parent_tool_use_id":null}
{"type":"stream_event","event":{"type":"message_start"},"parent_tool_use_id":null}
{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text"}},"parent_tool_use_id":null}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"The site "}},"parent_tool_use_id":null}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"is ready."}},"parent_tool_use_id":"helper"}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"is ready."}},"parent_tool_use_id":null}
{"type":"assistant","message":{"content":[{"type":"text","text":"The site is ready."}]},"parent_tool_use_id":null}
{"type":"result","subtype":"success","is_error":false,"result":"The site is ready.","total_cost_usd":0.0125,"usage":{"input_tokens":10,"cache_creation_input_tokens":200,"cache_read_input_tokens":3000,"output_tokens":120},"permission_denials":[{"tool_name":"Bash","tool_input":{"command":"rm x","description":"Remove x"}},{"tool_name":"Bash","tool_input":{"command":"npx astro build"}}]}
"#;

    #[tokio::test]
    async fn a_turn_becomes_chat_items() {
        let (chat, person) = chat().await;
        let mut feed = chat.subscribe();
        let setup = chat
            .prepare(person, "Ada", "  What is the site called?  ")
            .await
            .unwrap();
        assert!(!setup.resume);
        assert_eq!(
            chat.prepare(person, "Ada", "again").await.err(),
            Some(SendError::Busy)
        );

        let mut processor = Processor::new(setup.conversation);
        for event in events(TURN) {
            processor.handle_claude(&chat, &event).await;
        }
        let snapshot = chat.snapshot().await;
        let answer = snapshot.items.iter().find(|i| i.kind == "answer").unwrap();
        assert_eq!(answer.text, "The site is ready.");
        assert_eq!(answer.seq, 0);
        processor
            .finish(
                &chat,
                Ended {
                    stopped: false,
                    failure: None,
                    errors: String::new(),
                },
            )
            .await;

        let snapshot = chat.snapshot().await;
        let summary: Vec<(&str, &str, Option<&str>)> = snapshot
            .items
            .iter()
            .map(|i| (i.kind.as_str(), i.text.as_str(), i.state.as_deref()))
            .collect();
        assert_eq!(
            summary,
            [
                ("person", "What is the site called?", None),
                ("activity", "List the files", Some("done")),
                ("activity", "Read alogeno.json", Some("done")),
                ("answer", "The site is ready.", None),
                ("notice", "Some actions were not allowed.", None),
            ]
        );
        // Authors are not named: the owner is the only person.
        assert_eq!(snapshot.items[0].author, None);
        assert_eq!(snapshot.items[1].detail.as_deref(), Some("ls"));
        assert_eq!(
            snapshot.items[4].detail.as_deref(),
            Some("Remove x · npx astro build")
        );
        assert!(snapshot.turn.is_none());
        assert_eq!(snapshot.limits, Some(json!({ "status": "allowed" })));
        assert_eq!(
            snapshot.conversation.map(|c| c.title),
            Some("What is the site called?".to_string())
        );

        // The pieces of the answer reached the panel in order, without the
        // helper's.
        let mut pieces = Vec::new();
        while let Ok(event) = feed.try_recv() {
            if let Event::Delta { seq, text, .. } = event {
                pieces.push((seq, text));
            }
        }
        assert_eq!(
            pieces,
            [(1, "The site ".to_string()), (2, "is ready.".to_string())]
        );

        // What the turn used is kept for Settings.
        let used = usage(&chat.installation, Provider::Claude).await.unwrap();
        assert_eq!(
            used.today,
            UsageTotal {
                turns: 1,
                input: 3210,
                cached: 3000,
                output: 120,
                cost: Some(0.0125)
            }
        );
        assert_eq!(used.month, used.today);
        assert_eq!(
            usage(&chat.installation, Provider::Codex).await.unwrap(),
            UsageSummary::default()
        );

        // The next turn resumes the session. The CLI stood in for here keeps
        // no transcript.
        *chat.env.fake.lock().unwrap() = Some("/nonexistent/cli".into());
        let next = chat.prepare(person, "Ada", "Thanks").await.unwrap();
        assert!(next.resume);
        assert_eq!(next.session, setup.session);
        assert_eq!(next.conversation, setup.conversation);
    }

    #[tokio::test]
    async fn a_stopped_turn_keeps_its_partial_answer() {
        let (chat, person) = chat().await;
        let setup = chat
            .prepare(person, "Ada", "Write a long page")
            .await
            .unwrap();
        let mut processor = Processor::new(setup.conversation);
        for event in events(
            r#"
{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text"}},"parent_tool_use_id":null}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Here is"}},"parent_tool_use_id":null}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Write","input":{"file_path":"/site/content/pages/a.json"}}]},"parent_tool_use_id":null}
"#,
        ) {
            processor.handle_claude(&chat, &event).await;
        }
        chat.stop();
        assert!(chat.snapshot().await.turn.unwrap().stopping);
        processor
            .finish(
                &chat,
                Ended {
                    stopped: true,
                    failure: None,
                    errors: String::new(),
                },
            )
            .await;
        let items = chat.snapshot().await.items;
        let summary: Vec<(&str, &str, Option<&str>)> = items
            .iter()
            .map(|i| (i.kind.as_str(), i.text.as_str(), i.state.as_deref()))
            .collect();
        assert_eq!(
            summary,
            [
                ("person", "Write a long page", None),
                ("answer", "Here is", None),
                ("activity", "Wrote content/pages/a.json", Some("stopped")),
                ("notice", "Stopped.", None),
            ]
        );
        // Nothing was created by the CLI: the next turn starts the session.
        assert!(!chat.prepare(person, "Ada", "again").await.unwrap().resume);
    }

    #[tokio::test]
    async fn a_crash_and_a_cli_error_are_shown() {
        let (chat, person) = chat().await;
        let setup = chat.prepare(person, "Ada", "Hello").await.unwrap();
        Processor::new(setup.conversation)
            .finish(
                &chat,
                Ended {
                    stopped: false,
                    failure: None,
                    errors: "warming up\nError: out of memory\n\n".into(),
                },
            )
            .await;
        let setup = chat.prepare(person, "Ada", "Hello again").await.unwrap();
        let mut processor = Processor::new(setup.conversation);
        processor
            .handle_claude(
                &chat,
                &json!({ "type": "result", "subtype": "error_during_execution", "is_error": true }),
            )
            .await;
        processor
            .finish(
                &chat,
                Ended {
                    stopped: false,
                    failure: None,
                    errors: String::new(),
                },
            )
            .await;
        let errors: Vec<(String, Option<String>)> = chat
            .snapshot()
            .await
            .items
            .into_iter()
            .filter(|i| i.kind == "error")
            .map(|i| (i.text, i.detail))
            .collect();
        assert_eq!(
            errors,
            [
                (
                    "The assistant stopped unexpectedly.".to_string(),
                    Some("Error: out of memory".to_string())
                ),
                (
                    "the assistant stopped with an error (error_during_execution)".to_string(),
                    None
                ),
            ]
        );
    }

    #[tokio::test]
    async fn messages_are_checked_and_conversations_change() {
        let (chat, person) = chat().await;
        assert_eq!(
            chat.prepare(person, "Ada", "   ").await.err(),
            Some(SendError::Empty)
        );
        let long = "a".repeat(MESSAGE_LIMIT + 1);
        assert_eq!(
            chat.prepare(person, "Ada", &long).await.err(),
            Some(SendError::TooLong)
        );

        let setup = chat.prepare(person, "Ada", "First").await.unwrap();
        assert_eq!(chat.new_conversation(person), Err(SendError::Busy));
        Processor::new(setup.conversation)
            .finish(&chat, Ended::failed("no sandbox".into()))
            .await;
        chat.new_conversation(person).unwrap();
        assert!(chat.snapshot().await.conversation.is_none());
        let other = chat.prepare(person, "Ada", "Second").await.unwrap();
        assert_ne!(other.conversation, setup.conversation);
        assert_ne!(other.session, setup.session);
    }

    #[tokio::test]
    async fn sending_without_a_sandbox_reports_it() {
        let (chat, person) = chat().await;
        chat.send(person, "Ada", "Hello").await.unwrap();
        for _ in 0..100 {
            if chat.snapshot().await.turn.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let last = chat.snapshot().await.items.pop().unwrap();
        assert_eq!(last.kind, "error");
        assert_eq!(last.detail.as_deref(), Some("bwrap is not installed"));
    }

    #[tokio::test]
    async fn a_shutdown_stops_the_turn_and_says_so() {
        let (chat, person) = chat().await;
        let setup = chat
            .prepare(person, "Ada", "Write a long page")
            .await
            .unwrap();
        chat.close();
        assert!(chat.snapshot().await.turn.unwrap().stopping);
        Processor::new(setup.conversation)
            .finish(
                &chat,
                Ended {
                    stopped: true,
                    failure: None,
                    errors: String::new(),
                },
            )
            .await;
        chat.wait_idle(Duration::from_secs(1)).await;
        let snapshot = chat.snapshot().await;
        assert!(snapshot.turn.is_none());
        assert!(
            snapshot
                .items
                .last()
                .unwrap()
                .text
                .starts_with("Stopped: alogeno was restarting")
        );
    }

    fn permission_event(session: &str) -> Value {
        json!({
            "hook_event_name": "PermissionRequest",
            "session_id": session,
            "tool_name": "Bash",
            "tool_input": { "command": "npm install leaflet", "description": "install the map add-on." },
        })
    }

    async fn waiting_request(chat: &Chat) -> Item {
        for _ in 0..200 {
            if let Some(item) = chat
                .snapshot()
                .await
                .items
                .into_iter()
                .find(|i| i.state.as_deref() == Some("waiting"))
            {
                return item;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("no request is waiting");
    }

    /// A stand-in for `codex app-server`: answers the three requests of a
    /// turn, asks to run a command, writes the answer it gets into
    /// `answer`, then completes the turn.
    fn fake_codex(dir: &Path) -> std::path::PathBuf {
        let script = dir.join("codex");
        let answer = dir.join("answer");
        std::fs::write(
            &script,
            format!(
                r#"#!/bin/sh
read line; echo '{{"id":1,"result":{{}}}}'
read line
read line; echo "$line" > {thread}
echo '{{"id":2,"result":{{"thread":{{"id":"th1"}},"model":"gpt-test"}}}}'
read line; echo "$line" > {turn}; echo '{{"id":3,"result":{{"turn":{{"id":"tu1"}}}}}}'
echo '{{"method":"item/started","params":{{"item":{{"type":"commandExecution","id":"c1","command":"npm install leaflet","commandActions":[]}}}}}}'
echo '{{"id":"r1","method":"item/commandExecution/requestApproval","params":{{"threadId":"th1","itemId":"c1","command":"npm install leaflet","reason":"Install the map add-on"}}}}'
read line; echo "$line" > {answer}
echo '{{"method":"item/completed","params":{{"item":{{"type":"commandExecution","id":"c1","status":"completed","exitCode":0}}}}}}'
echo '{{"method":"item/agentMessage/delta","params":{{"itemId":"m1","delta":"The map "}}}}'
echo '{{"method":"item/agentMessage/delta","params":{{"itemId":"m1","delta":"is on the page."}}}}'
echo '{{"method":"item/completed","params":{{"item":{{"type":"agentMessage","id":"m1","text":"The map is on the page."}}}}}}'
echo '{{"method":"turn/completed","params":{{"turn":{{"status":"completed"}}}}}}'
read line
"#,
                thread = dir.join("thread").display(),
                turn = dir.join("turn").display(),
                answer = answer.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        script
    }

    async fn turn_over(chat: &Chat) {
        for _ in 0..400 {
            if chat.snapshot().await.turn.is_none() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("the turn did not end");
    }

    #[tokio::test]
    async fn a_turn_of_codex_runs_through_its_app_server() {
        let (chat, person) = chat().await;
        let dir = tempfile::tempdir().unwrap();
        *chat.env.fake.lock().unwrap() = Some(fake_codex(dir.path()));
        chat.env.set_active(Provider::Codex);
        // Plan mode, chosen before the conversation, goes with its turns.
        assert_eq!(chat.snapshot().await.mode, "default");
        assert!(chat.set_mode(person, "acceptEdits").await.is_err());
        assert_eq!(chat.set_mode(person, "plan").await, Ok("plan"));
        chat.set_model(person, Some("gpt-x".into())).await.unwrap();

        chat.send(person, "Ada", "Add a map").await.unwrap();
        let request = waiting_request(&chat).await;
        assert_eq!(request.kind, "permission");
        assert_eq!(request.text, "Install the map add-on");
        assert_eq!(request.detail.as_deref(), Some("npm install leaflet"));
        chat.answer_permission(person, request.id, true)
            .await
            .unwrap();
        turn_over(&chat).await;

        let answer: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("answer")).unwrap())
                .unwrap();
        assert_eq!(
            answer,
            json!({ "id": "r1", "result": { "decision": "accept" } })
        );
        let turn: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("turn")).unwrap())
                .unwrap();
        assert_eq!(turn["params"]["collaborationMode"]["mode"], "plan");
        assert_eq!(
            turn["params"]["collaborationMode"]["settings"]["model"],
            "gpt-x"
        );
        assert_eq!(turn["params"]["model"], "gpt-x");
        let thread: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("thread")).unwrap())
                .unwrap();
        assert_eq!(thread["method"], "thread/start");
        assert_eq!(thread["params"]["model"], "gpt-x");
        assert_eq!(chat.snapshot().await.model.as_deref(), Some("gpt-x"));
        assert_eq!(chat.snapshot().await.mode, "plan");
        let snapshot = chat.snapshot().await;
        let summary: Vec<(&str, &str, Option<&str>)> = snapshot
            .items
            .iter()
            .map(|i| (i.kind.as_str(), i.text.as_str(), i.state.as_deref()))
            .collect();
        assert_eq!(
            summary,
            [
                ("person", "Add a map", None),
                ("activity", "Ran a command", Some("done")),
                ("permission", "Install the map add-on", Some("allowed")),
                ("answer", "The map is on the page.", None),
            ]
        );
        assert!(snapshot.problem.is_none());

        // The conversation is Codex's, under the thread id it chose; the next
        // turn resumes that thread.
        let saved = chat
            .db
            .run(|conn| {
                conn.query_row(
                    "SELECT provider, cli_session, started FROM conversation",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
            })
            .await
            .unwrap();
        assert_eq!(saved, ("codex".into(), Some("th1".into()), 1));
        let next = chat.prepare(person, "Ada", "Thanks").await.unwrap();
        assert_eq!(next.cli_session.as_deref(), Some("th1"));
        assert_eq!(next.provider, Provider::Codex);
        chat.stop();
    }

    #[tokio::test]
    async fn codex_s_plan_is_implemented_in_the_same_turn() {
        let (chat, person) = chat().await;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("codex");
        let second = dir.path().join("second");
        std::fs::write(
            &script,
            format!(
                r#"#!/bin/sh
read line; echo '{{"id":1,"result":{{}}}}'
read line
read line; echo '{{"id":2,"result":{{"thread":{{"id":"th1"}},"model":"gpt-test"}}}}'
read line; echo '{{"id":3,"result":{{"turn":{{"id":"tu1"}}}}}}'
echo '{{"method":"item/started","params":{{"item":{{"type":"plan","id":"p1","text":""}}}}}}'
echo '{{"method":"item/completed","params":{{"item":{{"type":"plan","id":"p1","text":"1. Add the page"}}}}}}'
echo '{{"method":"turn/completed","params":{{"turn":{{"status":"completed"}}}}}}'
read line; echo "$line" > {second}
echo '{{"id":3,"result":{{"turn":{{"id":"tu2"}}}}}}'
echo '{{"method":"item/completed","params":{{"item":{{"type":"agentMessage","id":"m2","text":"The page is there."}}}}}}'
echo '{{"method":"turn/completed","params":{{"turn":{{"status":"completed"}}}}}}'
read line
"#,
                second = second.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        *chat.env.fake.lock().unwrap() = Some(script);
        chat.env.set_active(Provider::Codex);
        chat.set_mode(person, "plan").await.unwrap();

        chat.send(person, "Ada", "Plan a page").await.unwrap();
        let request = waiting_request(&chat).await;
        assert_eq!(request.text, "Implement this plan?");
        chat.answer_questions(
            person,
            request.id,
            vec![Answer {
                choices: vec!["Yes, implement this plan".into()],
                other: None,
            }],
        )
        .await
        .unwrap();
        turn_over(&chat).await;

        let next: Value = serde_json::from_str(&std::fs::read_to_string(&second).unwrap()).unwrap();
        assert_eq!(next["method"], "turn/start");
        assert_eq!(next["params"]["input"][0]["text"], "Implement the plan.");
        assert_eq!(next["params"]["collaborationMode"]["mode"], "default");
        let snapshot = chat.snapshot().await;
        assert_eq!(snapshot.mode, "default");
        let texts: Vec<&str> = snapshot
            .items
            .iter()
            .filter(|i| i.kind == "answer")
            .map(|i| i.text.as_str())
            .collect();
        assert_eq!(texts, ["1. Add the page", "The page is there."]);
        chat.stop();
    }

    async fn finished(chat: &Arc<Chat>, conversation: i64) {
        Processor::new(conversation)
            .finish(
                chat,
                Ended {
                    stopped: false,
                    failure: None,
                    errors: String::new(),
                },
            )
            .await;
    }

    #[tokio::test]
    async fn past_conversations_are_listed_and_opened_again() {
        let (chat, person) = chat().await;
        let first = chat.prepare(person, "Ada", "Make the home").await.unwrap();
        finished(&chat, first.conversation).await;
        chat.set_mode(person, "plan").await.unwrap();
        chat.new_conversation(person).unwrap();
        let second = chat.prepare(person, "Ada", "Add a page").await.unwrap();
        finished(&chat, second.conversation).await;

        let list = chat.conversations().await.unwrap();
        assert_eq!(list.len(), 2);
        assert!(
            list.iter()
                .any(|c| c.id == second.conversation && c.current)
        );
        assert!(
            list.iter()
                .all(|c| c.provider == "claude" && c.messages == 1)
        );

        chat.open_conversation(person, first.conversation)
            .await
            .unwrap();
        let snapshot = chat.snapshot().await;
        assert_eq!(snapshot.conversation.unwrap().id, first.conversation);
        assert_eq!(snapshot.items[0].text, "Make the home");
        // The next message continues it, in the mode it had.
        assert_eq!(snapshot.mode, "plan");
        let again = chat.prepare(person, "Ada", "And now").await.unwrap();
        assert_eq!(again.conversation, first.conversation);
        assert_eq!(again.session, first.session);
        finished(&chat, again.conversation).await;

        // A conversation of another assistant stays as it is.
        chat.env.set_active(Provider::Codex);
        assert!(
            chat.open_conversation(person, second.conversation)
                .await
                .unwrap_err()
                .contains("Claude")
        );
    }

    #[tokio::test]
    async fn the_last_mode_chosen_stays_for_new_conversations() {
        let (chat, person) = chat().await;
        assert_eq!(chat.snapshot().await.mode, "auto");
        chat.set_mode(person, "plan").await.unwrap();
        // After a restart, without a conversation.
        let again = Chat::start(
            crate::agent::idle().environment(),
            chat.installation.clone(),
            chat.db.clone(),
            chat.site_id.clone(),
            chat.slot.clone(),
            None,
        )
        .await;
        assert_eq!(again.snapshot().await.mode, "plan");
        // Another assistant has its own; back to Claude, Claude's.
        again.env.set_active(Provider::Codex);
        again.switched().await;
        assert_eq!(again.snapshot().await.mode, "default");
        again.env.set_active(Provider::Claude);
        again.switched().await;
        assert_eq!(again.snapshot().await.mode, "plan");
    }

    #[tokio::test]
    async fn going_ahead_with_a_plan_changes_the_mode() {
        let (chat, person) = chat().await;
        chat.set_mode(person, "plan").await.unwrap();
        let setup = chat.prepare(person, "Ada", "Plan a page").await.unwrap();
        let event = json!({
            "hook_event_name": "PermissionRequest",
            "session_id": setup.session,
            "tool_name": "ExitPlanMode",
            "tool_input": { "plan": "1. Add the page" },
        });
        let asking = {
            let chat = chat.clone();
            tokio::spawn(async move { chat.hook(&event).await })
        };
        let request = waiting_request(&chat).await;
        assert_eq!(request.kind, "question");
        assert_eq!(request.text, "Go ahead with this plan?");
        let shown = chat.snapshot().await;
        assert!(
            shown
                .items
                .iter()
                .any(|i| i.kind == "answer" && i.text == "1. Add the page")
        );
        chat.answer_questions(
            person,
            request.id,
            vec![Answer {
                choices: vec!["Yes, auto-accept edits".into()],
                other: None,
            }],
        )
        .await
        .unwrap();
        let answer = asking.await.unwrap().unwrap();
        let decision = &answer["hookSpecificOutput"]["decision"];
        assert_eq!(decision["behavior"], "allow");
        assert_eq!(decision["updatedPermissions"][0]["mode"], "acceptEdits");
        assert_eq!(chat.snapshot().await.mode, "acceptEdits");
    }

    #[tokio::test]
    async fn a_permission_waits_for_a_person() {
        let (chat, person) = chat().await;
        let setup = chat.prepare(person, "Ada", "Add a map").await.unwrap();
        let asking = {
            let chat = chat.clone();
            let event = permission_event(&setup.session);
            tokio::spawn(async move { chat.hook(&event).await })
        };
        let request = waiting_request(&chat).await;
        assert_eq!(request.kind, "permission");
        assert_eq!(request.text, "Install the map add-on?");
        assert_eq!(request.detail.as_deref(), Some("npm install leaflet"));
        assert!(chat.snapshot().await.turn.unwrap().waiting);

        chat.answer_permission(person, request.id, true)
            .await
            .unwrap();
        let answer = asking.await.unwrap().unwrap();
        assert_eq!(
            answer["hookSpecificOutput"]["decision"]["behavior"],
            "allow"
        );
        let snapshot = chat.snapshot().await;
        assert!(!snapshot.turn.unwrap().waiting);
        let item = snapshot.items.iter().find(|i| i.id == request.id).unwrap();
        assert_eq!(
            (item.state.as_deref(), item.author.as_deref()),
            (Some("allowed"), None)
        );
        // Answered once only.
        assert!(
            chat.answer_permission(person, request.id, false)
                .await
                .is_err()
        );

        // A refusal.
        let asking = {
            let chat = chat.clone();
            let event = permission_event(&setup.session);
            tokio::spawn(async move { chat.hook(&event).await })
        };
        let request = waiting_request(&chat).await;
        chat.answer_permission(person, request.id, false)
            .await
            .unwrap();
        let answer = asking.await.unwrap().unwrap();
        assert_eq!(answer["hookSpecificOutput"]["decision"]["behavior"], "deny");
        assert_eq!(answer["hookSpecificOutput"]["decision"]["interrupt"], false);
    }

    #[tokio::test]
    async fn stopping_refuses_a_waiting_permission() {
        let (chat, person) = chat().await;
        let setup = chat.prepare(person, "Ada", "Add a map").await.unwrap();
        let asking = {
            let chat = chat.clone();
            let event = permission_event(&setup.session);
            tokio::spawn(async move { chat.hook(&event).await })
        };
        let request = waiting_request(&chat).await;
        chat.stop();
        let answer = asking.await.unwrap().unwrap();
        assert_eq!(answer["hookSpecificOutput"]["decision"]["behavior"], "deny");
        assert_eq!(answer["hookSpecificOutput"]["decision"]["interrupt"], true);
        let item = chat
            .snapshot()
            .await
            .items
            .into_iter()
            .find(|i| i.id == request.id)
            .unwrap();
        assert_eq!(item.state.as_deref(), Some("stopped"));
    }

    #[tokio::test]
    async fn only_the_turn_in_progress_can_ask() {
        let (chat, person) = chat().await;
        let answer = chat.hook(&permission_event("nobody")).await.unwrap();
        assert_eq!(answer["hookSpecificOutput"]["decision"]["behavior"], "deny");
        chat.prepare(person, "Ada", "Hello").await.unwrap();
        let answer = chat
            .hook(&permission_event("another-session"))
            .await
            .unwrap();
        assert_eq!(answer["hookSpecificOutput"]["decision"]["behavior"], "deny");
        assert!(
            chat.snapshot()
                .await
                .items
                .iter()
                .all(|i| i.kind != "permission")
        );
        assert!(
            chat.hook(&json!({ "hook_event_name": "Stop" }))
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn hooks_pass_through_the_socket() {
        let (chat, person) = chat().await;
        let setup = chat.prepare(person, "Ada", "Add a map").await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks.sock");
        tokio::spawn(
            chat.clone()
                .serve_hooks(tokio::net::UnixListener::bind(&path).unwrap()),
        );
        let asking = {
            let event = permission_event(&setup.session).to_string();
            tokio::spawn(async move { crate::hook::exchange(&path, event.as_bytes()).await })
        };
        let request = waiting_request(&chat).await;
        chat.answer_permission(person, request.id, true)
            .await
            .unwrap();
        let answer: Value = serde_json::from_slice(&asking.await.unwrap().unwrap()).unwrap();
        assert_eq!(
            answer["hookSpecificOutput"]["decision"]["behavior"],
            "allow"
        );
    }

    fn ask_call() -> Value {
        json!({
            "alogeno_tool": "ask",
            "arguments": { "questions": [
                { "question": "Where should the map go?", "options": [
                    { "label": "Below the form" }, { "label": "At the top", "description": "Before everything" }
                ] },
                { "question": "Which pages go in the menu?", "multiSelect": true, "options": [
                    { "label": "Home" }, { "label": "Studio" }, { "label": "Contact" }
                ] }
            ] }
        })
    }

    #[tokio::test]
    async fn questions_wait_for_answers() {
        let (chat, person) = chat().await;
        assert!(chat.hook(&ask_call()).await.unwrap()["error"].is_string());
        chat.prepare(person, "Ada", "Add a contact page")
            .await
            .unwrap();
        let asking = {
            let chat = chat.clone();
            tokio::spawn(async move { chat.hook(&ask_call()).await })
        };
        let request = waiting_request(&chat).await;
        assert_eq!(request.kind, "question");
        assert_eq!(request.text, "Where should the map go?");
        let detail: Value = serde_json::from_str(request.detail.as_deref().unwrap()).unwrap();
        assert_eq!(detail["questions"][1]["multiple"], true);
        assert_eq!(
            detail["questions"][0]["options"][1]["description"],
            "Before everything"
        );

        let answer = |choices: &[&str], other: Option<&str>| Answer {
            choices: choices.iter().map(|c| c.to_string()).collect(),
            other: other.map(String::from),
        };
        for wrong in [
            vec![answer(&["Below the form"], None)],
            vec![
                answer(&["Below the form", "At the top"], None),
                answer(&["Home"], None),
            ],
            vec![answer(&["Somewhere"], None), answer(&["Home"], None)],
            vec![answer(&[], Some("  ")), answer(&["Home"], None)],
        ] {
            assert!(
                chat.answer_questions(person, request.id, wrong)
                    .await
                    .is_err()
            );
        }
        chat.answer_questions(
            person,
            request.id,
            vec![
                answer(&[], Some("Small, below the form")),
                answer(&["Home", "Contact"], None),
            ],
        )
        .await
        .unwrap();
        let result = asking.await.unwrap().unwrap();
        assert_eq!(
            result["text"],
            "The person answered:\n- Where should the map go?: Small, below the form\n- Which pages go in the menu?: Home, Contact"
        );
        let item = chat
            .snapshot()
            .await
            .items
            .into_iter()
            .find(|i| i.id == request.id)
            .unwrap();
        assert_eq!(item.state.as_deref(), Some("answered"));
        let detail: Value = serde_json::from_str(item.detail.as_deref().unwrap()).unwrap();
        assert_eq!(detail["answers"][1], "Home, Contact");
    }

    #[test]
    fn questions_are_checked() {
        assert!(questions_of(&json!({ "questions": [] })).is_err());
        assert!(
            questions_of(
                &json!({ "questions": [{ "question": "A?", "options": [{ "label": "x" }] }] })
            )
            .is_err()
        );
        assert!(questions_of(&json!({ "questions": [{ "question": " ", "options": [{ "label": "x" }, { "label": "y" }] }] })).is_err());
        let ok = questions_of(&json!({ "questions": [{ "question": "A?", "options": [{ "label": "x" }, { "label": "y" }] }] })).unwrap();
        assert!(!ok[0].multiple);
    }

    /// A chat whose assistant works on a real site directory.
    fn chat_on_site() -> (Arc<Chat>, i64, tempfile::TempDir, String) {
        let (dir, site, home) = crate::cms::test_site();
        let (installation, site_db, person) = databases();
        let env = Arc::new(Environment::new(
            site,
            dir.path().join("site-state"),
            dir.path().join("agent"),
            crate::runtime::Runtimes {
                node: "/nonexistent/node".into(),
                claude: "/nonexistent/claude".into(),
                codex: None,
                managed: false,
                codex_managed: false,
            },
            None,
            "/nonexistent/alogeno".into(),
            "/nonexistent/agent.sock".into(),
            "/nonexistent/hooks.sock".into(),
            "/nonexistent/policy".into(),
            None,
            Provider::Claude,
        ));
        (
            Chat::with(
                env,
                installation,
                site_db,
                "01TESTSITE".into(),
                crate::gate::AgentSlot::new(),
                None,
                None,
            ),
            person,
            dir,
            home,
        )
    }

    #[tokio::test]
    async fn the_assistant_changes_pages_through_its_tools() {
        let (chat, person, _dir, home) = chat_on_site();
        let call =
            |name: &str, arguments: Value| json!({ "alogeno_tool": name, "arguments": arguments });
        let refused = chat.hook(&call("list_pages", json!({}))).await.unwrap();
        assert!(refused["error"].is_string());

        chat.prepare(person, "Ada", "Rename the site")
            .await
            .unwrap();
        let changed = chat
            .hook(&call(
                "update_page",
                json!({ "id": home, "changes": { "title": "Studio Mancini" } }),
            ))
            .await
            .unwrap();
        assert!(changed["text"].as_str().unwrap().contains("Studio Mancini"));
        let invalid = chat
            .hook(&call(
                "update_page",
                json!({ "id": home, "changes": { "slug": "Not Valid" } }),
            ))
            .await
            .unwrap();
        assert!(
            invalid["error"]
                .as_str()
                .unwrap()
                .starts_with("the page was not saved")
        );

        let created = chat
            .hook(&call(
                "create_page",
                json!({ "page": { "inside": home, "title": "Eventi 2025" } }),
            ))
            .await
            .unwrap();
        let created: Value = serde_json::from_str(created["text"].as_str().unwrap()).unwrap();
        let id = created["id"].as_str().unwrap().to_string();
        let list = chat.hook(&call("list_pages", json!({}))).await.unwrap();
        assert!(list["text"].as_str().unwrap().contains("/eventi-2025"));

        // Deleting waits for a person.
        let deleting = {
            let chat = chat.clone();
            let request = call("delete_page", json!({ "id": id }));
            tokio::spawn(async move { chat.hook(&request).await })
        };
        let card = waiting_request(&chat).await;
        assert_eq!(card.text, "Delete the page \u{201c}Eventi 2025\u{201d}?");
        assert_eq!(card.detail.as_deref(), Some("Its address stops working."));
        chat.answer_permission(person, card.id, true).await.unwrap();
        let deleted = deleting.await.unwrap().unwrap();
        assert!(deleted["text"].as_str().unwrap().ends_with("was deleted."));
        let gone = chat
            .hook(&call("read_page", json!({ "id": id })))
            .await
            .unwrap();
        assert_eq!(gone["error"], "no page has this id");
        // The home cannot go, and no card is shown for it.
        let home_delete = chat
            .hook(&call("delete_page", json!({ "id": home })))
            .await
            .unwrap();
        assert_eq!(home_delete["error"], "the home page cannot be deleted");
    }

    #[tokio::test]
    async fn the_assistant_points_to_a_field_of_the_panel() {
        let (chat, person, _dir, home) = chat_on_site();
        let call =
            |name: &str, arguments: Value| json!({ "alogeno_tool": name, "arguments": arguments });
        chat.prepare(person, "Ada", "Where do I change the description?")
            .await
            .unwrap();
        let shown = chat
            .hook(&call(
                "show_in_panel",
                json!({ "label": "Open the description", "section": "pages", "page": home, "field": "seo.description" }),
            ))
            .await
            .unwrap();
        assert_eq!(
            shown["text"],
            "The person sees a button, at the end of this reply, that opens Pages \u{203a} Studio \u{203a} Description in search results."
        );
        let link = chat
            .snapshot()
            .await
            .items
            .into_iter()
            .find(|item| item.kind == "link")
            .unwrap();
        assert_eq!(link.text, "Open the description");
        let detail: Value = serde_json::from_str(link.detail.as_deref().unwrap()).unwrap();
        assert_eq!(
            detail["href"],
            format!("/pages/{home}?field=seo.description")
        );
        // A place that does not exist is refused, and nothing is shown.
        let wrong = chat
            .hook(&call(
                "show_in_panel",
                json!({ "label": "Open", "section": "pages", "page": home, "field": "colour" }),
            ))
            .await
            .unwrap();
        assert!(
            wrong["error"]
                .as_str()
                .unwrap()
                .contains("there is no field")
        );
        let links = chat
            .snapshot()
            .await
            .items
            .iter()
            .filter(|i| i.kind == "link")
            .count();
        assert_eq!(links, 1);
    }

    #[tokio::test]
    async fn hosts_are_allowed_once_and_remembered() {
        let (chat, person) = chat().await;
        assert!(!chat.approve_host("example.org".into()).await);
        chat.prepare(person, "Ada", "Use the weather service")
            .await
            .unwrap();
        // Two connections to the same host share one card.
        let first = {
            let chat = chat.clone();
            tokio::spawn(async move { chat.approve_host("api.example.org".into()).await })
        };
        let card = waiting_request(&chat).await;
        let second = {
            let chat = chat.clone();
            tokio::spawn(async move { chat.approve_host("api.example.org".into()).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(card.text, "Let the assistant reach api.example.org?");
        chat.answer_permission(person, card.id, true).await.unwrap();
        assert!(first.await.unwrap());
        assert!(second.await.unwrap());
        let cards = chat
            .snapshot()
            .await
            .items
            .iter()
            .filter(|i| i.kind == "permission")
            .count();
        assert_eq!(cards, 1);
        // Allowed from now on, without asking, also after a restart.
        assert!(chat.approve_host("api.example.org".into()).await);
        let saved: Vec<(String, i64)> = chat
            .db
            .run(|conn| {
                conn.prepare("SELECT host, approved_by FROM allowed_host")?
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()
            })
            .await
            .unwrap();
        assert_eq!(saved, [("api.example.org".to_string(), person)]);
        let restarted = Chat::start(
            crate::agent::idle().environment(),
            chat.installation.clone(),
            chat.db.clone(),
            chat.site_id.clone(),
            chat.slot.clone(),
            None,
        )
        .await;
        assert!(restarted.approve_host("api.example.org".into()).await);
    }

    #[tokio::test]
    async fn each_turn_is_saved_in_a_commit() {
        let (chat, person, _dir, home) = chat_on_site();
        let site = chat.env.site.clone();
        crate::git::run(&site, &["init", "-q", "-b", "master"]).unwrap();
        crate::git::commit_all(&site, "chore: new site").unwrap();
        let finish = |chat: Arc<Chat>, conversation: i64, stopped: bool| async move {
            Processor::new(conversation)
                .finish(
                    &chat,
                    Ended {
                        stopped,
                        failure: None,
                        errors: String::new(),
                    },
                )
                .await;
        };

        // A turn that changes nothing leaves no commit.
        let setup = chat
            .prepare(person, "Ada", "What is on the home page?")
            .await
            .unwrap();
        finish(chat.clone(), setup.conversation, false).await;
        let count = || crate::git::run(&site, &["rev-list", "--count", "HEAD"]).unwrap();
        assert_eq!(count().trim(), "1");

        let setup = chat
            .prepare(
                person,
                "Ada",
                "Rename the home  page\nto Studio Mancini, please",
            )
            .await
            .unwrap();
        Cms::new(&site)
            .update(&home, &json!({ "title": "Studio Mancini" }))
            .unwrap();
        finish(chat.clone(), setup.conversation, true).await;
        assert_eq!(count().trim(), "2");
        let message = crate::git::run(&site, &["log", "-1", "--format=%B"]).unwrap();
        assert_eq!(
            message.trim(),
            format!(
                "Rename the home page to Studio Mancini, please\n\nAsked in the chat: conversation {}, person {person}.\nThe assistant was stopped before the end.",
                setup.conversation
            )
        );
        let logged: String = chat
            .installation
            .run(|conn| {
                conn.query_row(
                    "SELECT action || ' ' || subject FROM activity ORDER BY id DESC",
                    [],
                    |r| r.get(0),
                )
            })
            .await
            .unwrap();
        assert!(logged.starts_with("agent.changes_saved commit:"));
    }

    #[tokio::test]
    async fn a_session_without_its_transcript_starts_again() {
        let (chat, person, _dir, _home) = chat_on_site();
        let first = chat.prepare(person, "Ada", "Hello").await.unwrap();
        assert!(!first.resume);
        // The CLI started the session, then its transcript went away (it was
        // written before each site had its own folder).
        {
            let mut inner = chat.inner();
            inner.running = None;
            inner.current.as_mut().unwrap().started = true;
        }
        let again = chat.prepare(person, "Ada", "Hello again").await.unwrap();
        assert!(!again.resume, "no transcript: a new session");
        assert_eq!(again.session, first.session);
        // With its transcript in the site's folder, the session resumes.
        chat.inner().running = None;
        let transcript = chat.transcript(&first.session);
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(&transcript, "{}\n").unwrap();
        assert!(
            chat.prepare(person, "Ada", "Once more")
                .await
                .unwrap()
                .resume
        );
    }

    #[tokio::test]
    async fn one_person_at_a_time() {
        let (chat, ada) = chat().await;
        let bob = ada + 1;
        let setup = chat.prepare(ada, "Ada", "Make it blue").await.unwrap();
        let in_use = Err(SendError::InUse("Ada".into()));
        assert_eq!(
            chat.prepare(bob, "Bob", "Make it red").await.map(|_| ()),
            in_use
        );
        assert_eq!(chat.stop_by(bob), Err("Ada is using the assistant".into()));
        assert_eq!(chat.take_over(bob, "Bob"), Err(SendError::Busy));
        let holder = chat.snapshot().await.holder.unwrap();
        assert_eq!((holder.id, holder.remaining_ms), (ada, None));

        Processor::new(setup.conversation)
            .finish(
                &chat,
                Ended {
                    stopped: false,
                    failure: None,
                    errors: String::new(),
                },
            )
            .await;
        // Ada keeps the chat for a while after her turn.
        assert!(chat.snapshot().await.holder.unwrap().remaining_ms.unwrap() > 0);
        assert_eq!(
            chat.prepare(bob, "Bob", "Make it red").await.map(|_| ()),
            in_use
        );
        assert_eq!(
            chat.new_conversation(bob),
            Err(SendError::InUse("Ada".into()))
        );
        assert_eq!(chat.take_over(bob, "Bob"), Ok(Some(ada)));
        assert_eq!(
            chat.check_holder(ada),
            Err("Bob is using the assistant".into())
        );
        chat.prepare(bob, "Bob", "Make it red").await.unwrap();
    }

    #[tokio::test]
    async fn the_terminal_pauses_the_chat() {
        let (chat, person, _dir, home) = chat_on_site();
        let site = chat.env.site.clone();
        crate::git::run(&site, &["init", "-q", "-b", "master"]).unwrap();
        crate::git::commit_all(&site, "chore: new site").unwrap();
        // Without a sandbox the terminal cannot open, and nothing changes.
        let refused = chat
            .open_terminal(person, "Ada", 80, 24, "dark")
            .await
            .err()
            .unwrap();
        assert_eq!(refused, "bwrap is not installed");
        assert!(chat.snapshot().await.terminal.is_none());

        // A terminal as open_terminal leaves it.
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "read line"]);
        let filter = std::os::fd::OwnedFd::from(std::fs::File::open("/dev/null").unwrap());
        let handle = Terminal::spawn(command, filter, 80, 24).unwrap();
        let setup = chat.prepare(person, "Ada", "Hello").await.unwrap();
        Processor::new(setup.conversation)
            .finish(
                &chat,
                Ended {
                    stopped: false,
                    failure: None,
                    errors: String::new(),
                },
            )
            .await;
        {
            let mut inner = chat.inner();
            inner.terminal = Some(OpenTerminal {
                person,
                name: "Ada".into(),
                _slot: None,
                conversation: setup.conversation,
                session: setup.session.clone(),
                provider: Provider::Claude,
                cli_session: None,
                handle: handle.clone(),
                offset: 0,
            });
        }
        assert_eq!(
            chat.prepare(person, "Ada", "Hi").await.map(|_| ()),
            Err(SendError::Paused)
        );
        assert_eq!(chat.snapshot().await.terminal.unwrap().name, "Ada");
        let request =
            json!({ "hook_event_name": "PermissionRequest", "session_id": setup.session });
        assert!(chat.hook(&request).await.is_none());
        let tools = json!({ "alogeno_tool": "update_page", "arguments": { "id": home, "changes": { "title": "Studio" } } });
        assert!(chat.hook(&tools).await.unwrap()["text"].is_string());
        let ask = json!({ "alogeno_tool": "ask", "arguments": {} });
        assert!(chat.hook(&ask).await.unwrap()["error"].is_string());

        // What the CLI wrote in its transcript meanwhile.
        let transcript = chat.transcript(&setup.session);
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        let lines = [
            json!({ "type": "user", "message": { "content": "<command-name>/clear</command-name>" } }),
            json!({ "type": "user", "message": { "content": [{ "type": "text", "text": "Make the title bigger" }] } }),
            json!({ "type": "assistant", "message": { "content": [{ "type": "tool_use", "name": "Bash", "input": { "command": "ls", "description": "List the files" } }] } }),
            json!({ "type": "assistant", "isSidechain": true, "message": { "content": [{ "type": "text", "text": "helper" }] } }),
            json!({ "type": "user", "message": { "content": [{ "type": "tool_result", "content": "ok" }] } }),
            json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": "Done." }] } }),
        ];
        let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
        std::fs::write(&transcript, text).unwrap();
        handle.write(b"bye\n").unwrap();
        handle.exited().await;
        chat.terminal_closed().await;
        let snapshot = chat.snapshot().await;
        assert!(snapshot.terminal.is_none());
        let tail: Vec<(&str, &str)> = snapshot.items[snapshot.items.len() - 4..]
            .iter()
            .map(|i| (i.kind.as_str(), i.text.as_str()))
            .collect();
        assert_eq!(
            tail,
            [
                ("notice", "Ada used the terminal."),
                ("person", "Make the title bigger"),
                ("activity", "List the files"),
                ("answer", "Done."),
            ]
        );
        assert_eq!(snapshot.items[snapshot.items.len() - 3].author, None);
        let subject = crate::git::run(&site, &["log", "-1", "--format=%s"]).unwrap();
        assert_eq!(subject.trim(), "Changes made in the terminal");
        chat.prepare(person, "Ada", "Back in the chat")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn going_back_to_before_a_message_and_forward_again() {
        let (chat, person, _dir, home) = chat_on_site();
        let site = chat.env.site.clone();
        crate::git::run(&site, &["init", "-q", "-b", "master"]).unwrap();
        crate::git::commit_all(&site, "chore: new site").unwrap();
        let title = || Cms::new(&site).read(&home).unwrap()["title"].clone();
        let finish = |chat: Arc<Chat>, conversation: i64| async move {
            Processor::new(conversation)
                .finish(
                    &chat,
                    Ended {
                        stopped: false,
                        failure: None,
                        errors: String::new(),
                    },
                )
                .await;
        };

        let first = chat
            .prepare(person, "Ada", "Call it Studio Mancini")
            .await
            .unwrap();
        Cms::new(&site)
            .update(&home, &json!({ "title": "Studio Mancini" }))
            .unwrap();
        finish(chat.clone(), first.conversation).await;
        let second = chat
            .prepare(person, "Ada", "Call it Studio Rossi")
            .await
            .unwrap();
        Cms::new(&site)
            .update(&home, &json!({ "title": "Studio Rossi" }))
            .unwrap();
        finish(chat.clone(), second.conversation).await;
        assert_eq!(title(), "Studio Rossi");

        let items = chat.snapshot().await.items;
        let message = items
            .iter()
            .find(|i| i.text == "Call it Studio Rossi")
            .unwrap();
        assert!(message.back);
        let text = chat.go_back(person, message.id).await.unwrap();
        assert_eq!(text, "Call it Studio Rossi");
        assert_eq!(title(), "Studio Mancini");
        let line = chat.snapshot().await.items.pop().unwrap();
        assert_eq!(
            line.text,
            "Went back to before \u{201c}Call it Studio Rossi\u{201d}."
        );
        assert!(line.back);
        // The assistant hears of it with the next message.
        let next = chat
            .prepare(person, "Ada", "Now make it bigger")
            .await
            .unwrap();
        assert!(next.text.starts_with("[alogeno: the site went back"));
        assert!(next.text.ends_with("Now make it bigger"));
        finish(chat.clone(), next.conversation).await;

        // Undone, once.
        chat.undo_back(person, line.id).await.unwrap();
        assert_eq!(title(), "Studio Rossi");
        assert!(chat.undo_back(person, line.id).await.is_err());
        // Nothing to go back to when nothing changed.
        let first_message = items
            .iter()
            .find(|i| i.text == "Call it Studio Mancini")
            .unwrap();
        chat.go_back(person, first_message.id).await.unwrap();
        assert_eq!(title(), "Studio");
        assert!(chat.go_back(person, first_message.id).await.is_err());
    }

    #[tokio::test]
    async fn people_change_pages_in_their_own_versions() {
        let (chat, person, _dir, home) = chat_on_site();
        let site = chat.env.site.clone();
        crate::git::run(&site, &["init", "-q", "-b", "master"]).unwrap();
        crate::git::commit_all(&site, "chore: new site").unwrap();
        let rename = |title: &'static str| {
            let home = home.clone();
            move |cms: &Cms| {
                let page = cms.update(&home, &json!({ "title": title }))?;
                Ok((page, format!("Edited the page \u{201c}{title}\u{201d}")))
            }
        };
        let page = chat
            .edit_content(person, rename("Studio Bianchi"))
            .await
            .unwrap();
        assert_eq!(page["title"], "Studio Bianchi");
        let log = crate::git::run(&site, &["log", "-1", "--format=%s%n%b"]).unwrap();
        assert!(log.starts_with("Edited the page \u{201c}Studio Bianchi\u{201d}"));
        assert!(log.contains(&format!("In the panel: person {person}.")));
        // Read-only while the assistant works.
        chat.prepare(person, "Ada", "Hello").await.unwrap();
        assert!(chat.content_locked().is_some());
        let locked = chat.edit_content(person, rename("Studio Verdi")).await;
        assert!(matches!(locked, Err(crate::chat::ContentError::Locked(_))));
        let pages = chat.read_content(|cms| cms.list()).await.unwrap();
        assert_eq!(pages[0].title, "Studio Bianchi");
    }

    #[tokio::test]
    async fn a_problem_shows_until_the_next_turn() {
        let (chat, person) = chat().await;
        assert!(chat.retry(person, "Ada").await.is_err());
        let setup = chat.prepare(person, "Ada", "Hello").await.unwrap();
        let mut processor = Processor::new(setup.conversation);
        processor
            .handle_claude(&chat, &json!({ "type": "rate_limit_event", "rate_limit_info": { "status": "rejected", "resetsAt": 1794852600 } }))
            .await;
        processor
            .handle_claude(&chat, &json!({ "type": "result", "subtype": "success", "is_error": true, "result": "You've hit your limit" }))
            .await;
        processor
            .finish(
                &chat,
                Ended {
                    stopped: false,
                    failure: None,
                    errors: String::new(),
                },
            )
            .await;
        let snapshot = chat.snapshot().await;
        assert_eq!(
            snapshot.problem,
            Some(ProblemInfo {
                kind: "limit",
                until: Some(1794852600)
            })
        );
        assert_eq!(
            snapshot.items.last().unwrap().text,
            "The Claude plan is used up for now."
        );
        // A limit is waited out, not tried again.
        assert!(chat.retry(person, "Ada").await.is_err());

        // Something else went wrong: trying again sends no message of the
        // person, and the problem goes.
        let setup = chat.prepare(person, "Ada", "Hello again").await.unwrap();
        assert!(chat.snapshot().await.problem.is_none());
        Processor::new(setup.conversation)
            .finish(
                &chat,
                Ended {
                    stopped: false,
                    failure: None,
                    errors: "boom".into(),
                },
            )
            .await;
        assert_eq!(chat.snapshot().await.problem.unwrap().kind, "error");
        let before = chat.snapshot().await.items.len();
        let again = chat
            .prepare_turn(person, "Ada", CONTINUE, false)
            .await
            .unwrap();
        assert!(again.resume || !setup.resume);
        assert_eq!(chat.snapshot().await.items.len(), before);
        assert!(chat.snapshot().await.problem.is_none());
    }

    #[test]
    fn requests_read_as_questions() {
        let request = |name: &str, input: Value| request_of(&claude::tool_of(name, &input));
        assert_eq!(
            request("Bash", json!({ "command": "ls" })).0,
            "Run a command?"
        );
        assert_eq!(
            request("Write", json!({ "file_path": "/site/src/a.astro" })).0,
            "Write src/a.astro?"
        );
        assert_eq!(
            request("WebFetch", json!({ "url": "https://x.org" }))
                .1
                .as_deref(),
            Some("https://x.org")
        );
    }

    #[tokio::test]
    async fn closing_completes_after_close() {
        let (chat, _) = chat().await;
        let closing = chat.closing();
        chat.close();
        tokio::time::timeout(Duration::from_secs(1), closing)
            .await
            .unwrap();
        // A stream opened after the close ends at once.
        tokio::time::timeout(Duration::from_secs(1), chat.closing())
            .await
            .unwrap();
    }

    #[test]
    fn texts_are_described_and_clipped() {
        assert_eq!(
            title_of("Make  the\nhome page brighter"),
            "Make the home page brighter"
        );
        assert_eq!(clip("abcdef", 3), "abc…");
        assert_eq!(clip("àèì", 3), "àèì");
        let id = uuid();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        let describe = |name: &str, input: Value| describe(&claude::tool_of(name, &input));
        assert_eq!(
            describe("Grep", json!({ "pattern": "title" })),
            ("Searched the files".to_string(), Some("title".to_string()))
        );
        assert_eq!(describe("Mystery", json!({})).0, "Used Mystery");
    }
}
