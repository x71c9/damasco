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
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Notify, broadcast, oneshot};

use crate::agent::Environment;
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
    conversation: i64,
    session: String,
    handle: Arc<Terminal>,
    /// Size of the CLI's transcript when the terminal opened: what follows
    /// was said in the terminal.
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
}

pub struct Chat {
    env: Arc<Environment>,
    db: db::Handle,
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
    text: String,
    stop: Arc<Notify>,
}

impl Chat {
    /// Opens the chat on the latest conversation. Actions left running by a
    /// previous process are marked as stopped.
    pub async fn start(
        env: Arc<Environment>,
        db: db::Handle,
        builder: Option<Arc<Builder>>,
    ) -> Arc<Self> {
        let current = db
            .run(|conn| {
                conn.execute(
                    "UPDATE chat_item SET state = 'stopped' WHERE state = 'running'",
                    [],
                )?;
                latest(conn)
            })
            .await
            .unwrap_or_else(|err| {
                eprintln!("axioma: cannot read the conversations: {err}");
                None
            });
        let hosts = db
            .run(|conn| -> rusqlite::Result<Vec<String>> {
                let mut statement = conn.prepare("SELECT host FROM allowed_host")?;
                statement.query_map([], |row| row.get(0))?.collect()
            })
            .await
            .unwrap_or_default();
        let chat = Self::with(env, db, builder, current);
        lock(&chat.hosts).extend(hosts);
        chat
    }

    fn with(
        env: Arc<Environment>,
        db: db::Handle,
        builder: Option<Arc<Builder>>,
        current: Option<Current>,
    ) -> Arc<Self> {
        let (events, _) = broadcast::channel(1024);
        let (closed, _) = tokio::sync::watch::channel(false);
        Arc::new(Self {
            env,
            db,
            builder,
            inner: Mutex::new(Inner {
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
        }
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
            });
            inner.current.clone()
        };
        // The site before the message: going back returns to it.
        let version = if shown {
            self.site_version().await
        } else {
            None
        };
        let created = {
            let text = text.clone();
            self.db
                .run(move |conn| -> rusqlite::Result<(Current, Option<Item>)> {
                    let current = match current {
                        Some(current) => current,
                        None => {
                            let session = uuid();
                            let title = title_of(&text);
                            conn.execute(
                                "INSERT INTO conversation (session, title, created_by) VALUES (?1, ?2, ?3)",
                                params![session, title, person],
                            )?;
                            Current {
                                id: conn.last_insert_rowid(),
                                session,
                                title,
                                started: false,
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
        Ok(Setup {
            conversation: current.id,
            session: current.session,
            resume: current.started,
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
        let undo = self.switch_site(target.clone(), subject).await?;
        let line = format!("Went back to before \u{201c}{}\u{201d}.", title_of(&text));
        self.add_versioned(conversation, line, undo).await;
        self.inner().note = Some(format!(
            "axioma: the site went back to how it was before the message \u{201c}{}\u{201d}; everything done after it was undone",
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
        self.switch_site(version.clone(), "Undid going back".into())
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
        self.inner().note =
            Some("axioma: the going back was undone; the site is again as it was before it".into());
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
    async fn switch_site(&self, version: String, subject: String) -> Result<String, String> {
        let site = self.env.site.clone();
        if !site.join(".git").is_dir() {
            return Err("the site has no history".into());
        }
        let before = tokio::task::spawn_blocking(move || -> Result<String, String> {
            crate::git::commit_all(&site, "Saved before going back")?;
            let before = crate::git::head(&site)?;
            if crate::git::same_tree(&site, &before, &version)? {
                return Err("the site is already like that".into());
            }
            crate::git::restore(&site, &version)?;
            crate::git::commit_all(&site, &subject)?;
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
        let mut attempt = 1;
        loop {
            let mut processor = Processor::new(setup.conversation);
            let ended = self.drive(&setup, &mut processor).await;
            // The CLI ended without an answer, not stopped by anyone: it
            // starts again on the same conversation, up to three times.
            let crashed = !ended.stopped && ended.failure.is_none() && processor.outcome.is_none();
            if crashed && attempt < ATTEMPTS && !self.is_closed() {
                if !ended.errors.trim().is_empty() {
                    eprintln!(
                        "axioma: the assistant stopped unexpectedly:\n{}",
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
                setup.text = CONTINUE.into();
                continue;
            }
            processor.finish(&self, ended).await;
            break;
        }
    }

    /// Runs the CLI for one turn and feeds its events to the processor.
    async fn drive(&self, setup: &Setup, processor: &mut Processor) -> Ended {
        let session_flag = if setup.resume {
            "--resume"
        } else {
            "--session-id"
        };
        let args = [
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--permission-mode",
            "acceptEdits",
            session_flag,
            setup.session.as_str(),
        ];
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
        let message = json!({
            "type": "user",
            "message": { "role": "user", "content": [{ "type": "text", "text": setup.text }] },
        });
        if let Err(err) = stdin.write_all(format!("{message}\n").as_bytes()).await {
            let _ = child.start_kill();
            return Ended::failed(format!("cannot write to the assistant: {err}"));
        }
        let mut stdin = Some(stdin);
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
                        processor.handle(self, &value).await;
                        // The answer is complete: closing the input lets the
                        // CLI end.
                        if processor.outcome.is_some() {
                            stdin = None;
                        }
                    }
                    _ => break false,
                }
            }
        };
        drop(stdin);
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

    /// Answers a hook event of the CLI with what the hook prints, if
    /// anything.
    pub async fn hook(&self, event: &Value) -> Option<Value> {
        if !event["axioma_tool"].is_null() {
            return Some(self.tool(event).await);
        }
        match event["hook_event_name"].as_str() {
            // In the terminal the CLI asks on the terminal itself.
            Some("PermissionRequest") if self.inner().terminal.is_some() => None,
            Some("PermissionRequest") => Some(self.permission(event).await),
            _ => None,
        }
    }

    /// Shows a permission request in the chat and waits for a person.
    async fn permission(&self, event: &Value) -> Value {
        let decision = |allow: bool, message: &str, interrupt: bool| {
            let mut decision = json!({ "behavior": if allow { "allow" } else { "deny" } });
            if !allow {
                decision["message"] = json!(message);
                decision["interrupt"] = json!(interrupt);
            }
            json!({
                "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": decision }
            })
        };
        // Only the CLI of the turn in progress can ask.
        let Some(conversation) = self.turn_conversation(event["session_id"].as_str()) else {
            return decision(false, "No conversation is waiting for this.", true);
        };
        let (title, detail) = request_of(
            event["tool_name"].as_str().unwrap_or(""),
            &event["tool_input"],
        );
        match self
            .wait_for_person(conversation, "permission", title, detail)
            .await
        {
            Waited::Reply(Reply::Permission { allow: true, .. }) => decision(true, "", false),
            Waited::Reply(_) => decision(false, "The person did not allow this.", false),
            Waited::Stopped => decision(false, "The person stopped the assistant.", true),
            Waited::TimedOut => decision(false, "Nobody answered the request in time.", false),
            Waited::Unsaved => decision(false, "The request could not be shown.", false),
        }
    }

    /// A call of a tool of axioma's MCP server.
    async fn tool(&self, call: &Value) -> Value {
        let in_terminal = self.inner().terminal.is_some();
        let result = match call["axioma_tool"].as_str() {
            Some("ask" | "delete_page") if in_terminal => Err(
                "In the terminal this tool is not available: ask in the conversation, and delete pages from the chat."
                    .to_string(),
            ),
            Some("ask") => self.ask(&call["arguments"]).await,
            Some("delete_page") => self.delete_page(&call["arguments"]).await,
            Some(name) => self.content(name, &call["arguments"]).await,
            None => Err("unknown tool".to_string()),
        };
        match result {
            Ok(text) => json!({ "text": text }),
            Err(error) => json!({ "error": error }),
        }
    }

    /// A tool of the content: reading and changing pages.
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
        let saved = self
            .db
            .run(move |conn| -> rusqlite::Result<()> {
                conn.execute(
                    "INSERT OR IGNORE INTO allowed_host (host, approved_by) VALUES (?1, ?2)",
                    params![host, person],
                )?;
                crate::activity::record(
                    conn,
                    Some(person),
                    crate::activity::Environment::Installation,
                    "sandbox.host_allowed",
                    Some(&format!("host:{host}")),
                )
            })
            .await;
        if let Err(err) = saved {
            eprintln!("axioma: cannot save an allowed host: {err}");
        }
    }

    /// Shows questions in the chat and waits for a person's answers.
    async fn ask(&self, arguments: &Value) -> Result<String, String> {
        let questions = questions_of(arguments)?;
        let Some(conversation) = self.turn_conversation(None) else {
            return Err("No conversation is waiting for this.".into());
        };
        let detail = json!({ "questions": questions }).to_string();
        let title = questions[0].question.clone();
        match self
            .wait_for_person(conversation, "question", title, Some(detail))
            .await
        {
            Waited::Reply(Reply::Answers(answers)) => {
                let mut text = String::from("The person answered:");
                for (question, answer) in questions.iter().zip(answers) {
                    text.push_str(&format!("\n- {}: {answer}", question.question));
                }
                Ok(text)
            }
            Waited::Reply(_) | Waited::Unsaved => Err("The questions could not be shown.".into()),
            Waited::Stopped => Err("The person stopped the assistant.".into()),
            Waited::TimedOut => Err("Nobody answered the questions in time.".into()),
        }
    }

    /// The conversation of the turn in progress, when the caller belongs to
    /// it. Callers that cannot name their session (the MCP server) pass
    /// `None`: one turn runs at a time.
    fn turn_conversation(&self, session: Option<&str>) -> Option<i64> {
        let inner = self.inner();
        match (&inner.running, &inner.current) {
            (Some(running), Some(current))
                if !running.stopping
                    && session.is_none_or(|session| session == current.session) =>
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
    pub async fn serve_hooks(self: Arc<Self>, listener: UnixListener) {
        loop {
            let stream = match listener.accept().await {
                Ok((stream, _)) => stream,
                Err(err) => {
                    eprintln!("axioma: hook connection refused: {err}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let chat = self.clone();
            tokio::spawn(async move { chat.hook_connection(stream).await });
        }
    }

    async fn hook_connection(&self, mut stream: UnixStream) {
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
                eprintln!("axioma: cannot save the changes of the turn: {err}");
                return;
            }
            Err(err) => {
                eprintln!("axioma: cannot save the changes of the turn: {err}");
                return;
            }
        };
        let _ = self
            .db
            .run(move |conn| {
                crate::activity::record(
                    conn,
                    Some(person),
                    crate::activity::Environment::Preview,
                    "agent.changes_saved",
                    Some(&format!("commit:{commit}")),
                )
            })
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
            inner.current.clone()
        };
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
                                "INSERT INTO conversation (session, title, created_by) VALUES (?1, ?2, ?3)",
                                params![session, title, person],
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
                }
            }
        };
        let flag = if current.started {
            "--resume"
        } else {
            "--session-id"
        };
        let (command, filter) = self.env.claude_terminal(&[flag, &current.session])?;
        self.env.prepare_terminal(theme)?;
        let offset = std::fs::metadata(self.transcript(&current.session))
            .map(|meta| meta.len())
            .unwrap_or(0);
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
                conversation: current.id,
                session: current.session.clone(),
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

    /// The file where the CLI keeps a conversation, in the agent's home.
    fn transcript(&self, session: &str) -> std::path::PathBuf {
        self.env
            .home
            .join(".claude/projects/-site")
            .join(format!("{session}.jsonl"))
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

    /// The terminal ended: its work is saved and shown in the chat, and the
    /// chat goes on.
    async fn terminal_closed(&self) {
        let Some(open) = self.inner().terminal.take() else {
            return;
        };
        // The CLI writes the conversation once something was said in it.
        let transcript = self.transcript(&open.session);
        let started = transcript.is_file();
        if started {
            let id = open.conversation;
            let _ = self
                .db
                .run(move |conn| {
                    conn.execute("UPDATE conversation SET started = 1 WHERE id = ?1", [id])
                })
                .await;
        }
        // What was said in the terminal joins the chat, so the conversation
        // reads on; a terminal closed without a word leaves nothing.
        let said = std::fs::read(&transcript)
            .map(|bytes| {
                bytes
                    .get(open.offset as usize..)
                    .unwrap_or_default()
                    .to_vec()
            })
            .unwrap_or_default();
        let items = transcript_items(&String::from_utf8_lossy(&said), &self.env.site);
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
        let _ = self
            .db
            .run(move |conn| {
                crate::activity::record(
                    conn,
                    Some(person),
                    crate::activity::Environment::Installation,
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
            .map_err(|err| eprintln!("axioma: cannot save the chat: {err}"))
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
            Err(err) => eprintln!("axioma: cannot save the chat: {err}"),
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
    /// Actions the CLI was not allowed to do.
    denied: Vec<String>,
}

/// Turns the events of one turn into chat items.
struct Processor {
    conversation: i64,
    /// Item being written for each content block of the current message.
    blocks: HashMap<u64, i64>,
    /// Items being written, waiting for their complete text, in order.
    open: Vec<(&'static str, i64)>,
    /// Action item of each tool call.
    tools: HashMap<String, i64>,
    /// The CLI created the session.
    initialized: bool,
    outcome: Option<Outcome>,
}

impl Processor {
    fn new(conversation: i64) -> Self {
        Self {
            conversation,
            blocks: HashMap::new(),
            open: Vec::new(),
            tools: HashMap::new(),
            initialized: false,
            outcome: None,
        }
    }

    async fn handle(&mut self, chat: &Chat, event: &Value) {
        // Events of helpers started by the assistant stay out of the chat:
        // their call already shows as one action.
        if !event["parent_tool_use_id"].is_null() {
            return;
        }
        match event["type"].as_str() {
            Some("system") if event["subtype"] == "init" && !self.initialized => {
                self.initialized = true;
                let id = self.conversation;
                let _ = chat
                    .db
                    .run(move |conn| {
                        conn.execute("UPDATE conversation SET started = 1 WHERE id = ?1", [id])
                    })
                    .await;
                if let Some(current) = chat.inner().current.as_mut().filter(|c| c.id == id) {
                    current.started = true;
                }
            }
            Some("stream_event") => self.stream(chat, &event["event"]).await,
            Some("assistant") => {
                let blocks = event["message"]["content"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                for block in blocks {
                    self.block(chat, &block).await;
                }
            }
            Some("user") => {
                let blocks = event["message"]["content"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                for block in blocks.iter().filter(|b| b["type"] == "tool_result") {
                    let Some(id) = block["tool_use_id"]
                        .as_str()
                        .and_then(|t| self.tools.remove(t))
                    else {
                        continue;
                    };
                    let state = if block["is_error"] == true {
                        "failed"
                    } else {
                        "done"
                    };
                    chat.change(id, None, Some(state)).await;
                }
            }
            Some("rate_limit_event") => {
                let limits = event["rate_limit_info"].clone();
                chat.inner().limits = Some(limits.clone());
                chat.emit(Event::Limits { limits });
            }
            Some("result") => {
                let failed = event["is_error"] == true || event["subtype"] != "success";
                let error = failed.then(|| {
                    event["result"]
                        .as_str()
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                        .map(String::from)
                        .unwrap_or_else(|| {
                            format!(
                                "the assistant stopped with an error ({})",
                                event["subtype"].as_str().unwrap_or("unknown")
                            )
                        })
                });
                let denied = event["permission_denials"]
                    .as_array()
                    .map(|denials| {
                        denials
                            .iter()
                            .map(|d| {
                                let tool = d["tool_name"].as_str().unwrap_or("");
                                let (label, detail) = describe(tool, &d["tool_input"]);
                                // A command without a description is named
                                // by the command itself.
                                match detail {
                                    Some(command)
                                        if tool == "Bash"
                                            && d["tool_input"]["description"].is_null() =>
                                    {
                                        command
                                    }
                                    _ => label,
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                self.outcome = Some(Outcome { error, denied });
            }
            _ => {}
        }
    }

    async fn stream(&mut self, chat: &Chat, event: &Value) {
        match event["type"].as_str() {
            Some("message_start") => self.blocks.clear(),
            Some("content_block_start") => {
                let kind = match event["content_block"]["type"].as_str() {
                    Some("text") => "answer",
                    Some("thinking") => "thinking",
                    _ => return,
                };
                let Some(index) = event["index"].as_u64() else {
                    return;
                };
                if let Some(item) = chat
                    .add(self.conversation, kind, String::new(), None, None)
                    .await
                {
                    self.blocks.insert(index, item.id);
                    self.open.push((kind, item.id));
                }
            }
            Some("content_block_delta") => {
                let delta = &event["delta"];
                let piece = match delta["type"].as_str() {
                    Some("text_delta") => delta["text"].as_str(),
                    Some("thinking_delta") => delta["thinking"].as_str(),
                    _ => None,
                };
                if let (Some(piece), Some(id)) = (
                    piece,
                    event["index"].as_u64().and_then(|i| self.blocks.get(&i)),
                ) {
                    chat.append(*id, piece);
                }
            }
            _ => {}
        }
    }

    /// A complete block of a message of the assistant.
    async fn block(&mut self, chat: &Chat, block: &Value) {
        let (kind, text) = match block["type"].as_str() {
            Some("text") => ("answer", block["text"].as_str().unwrap_or("")),
            Some("thinking") => ("thinking", block["thinking"].as_str().unwrap_or("")),
            // Loading more tools is internal to the CLI; a question shows as
            // its own card.
            Some("tool_use")
                if block["name"] == "ToolSearch" || block["name"] == "mcp__axioma__ask" =>
            {
                return;
            }
            // Deleting a page shows as its confirmation card.
            Some("tool_use") if block["name"] == "mcp__axioma__delete_page" => return,
            Some("tool_use") => {
                let name = block["name"].as_str().unwrap_or("");
                let (label, detail) = content_activity(name, &block["input"], &chat.env.site)
                    .unwrap_or_else(|| describe(name, &block["input"]));
                if let Some(item) = chat
                    .add(
                        self.conversation,
                        "activity",
                        label,
                        detail,
                        Some("running"),
                    )
                    .await
                    && let Some(id) = block["id"].as_str()
                {
                    self.tools.insert(id.to_string(), item.id);
                }
                return;
            }
            _ => return,
        };
        let text = text.to_string();
        match self.open.iter().position(|(open, _)| *open == kind) {
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
            let text = "Stopped: axioma was restarting. Send the message again to continue.";
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
                let limits = chat.inner().limits.clone();
                let found = classify(&error, limits.as_ref());
                let text = match found.kind {
                    "limit" => "The Claude plan is used up for now.".to_string(),
                    "login" => "Claude is disconnected.".to_string(),
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
                    "axioma: the assistant stopped unexpectedly:\n{}",
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

/// What an error of the CLI means for the person: the plan used up, the
/// account signed out, or something else.
fn classify(error: &str, limits: Option<&Value>) -> ProblemInfo {
    let text = error.to_lowercase();
    let rejected = limits.is_some_and(|l| l["status"] == "rejected");
    if rejected
        || text.contains("usage limit")
        || text.contains("hit your limit")
        || text.contains("limit reached")
    {
        let until = limits
            .and_then(|l| l["resetsAt"].as_i64())
            .or_else(|| error.rsplit('|').next().and_then(|t| t.trim().parse().ok()));
        return ProblemInfo {
            kind: "limit",
            until,
        };
    }
    if text.contains("/login")
        || text.contains("not logged in")
        || text.contains("oauth token")
        || text.contains("invalid api key")
        || text.contains("authentication")
    {
        return ProblemInfo {
            kind: "login",
            until: None,
        };
    }
    ProblemInfo {
        kind: "error",
        until: None,
    }
}

/// The chat lines of a part of a CLI transcript: the person's messages,
/// the answers and the actions, in order. Helpers' lines, commands of the
/// CLI and notes it adds itself stay out.
fn transcript_items(
    text: &str,
    site: &Path,
) -> Vec<(&'static str, String, Option<String>, Option<&'static str>)> {
    let mut items = Vec::new();
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if entry["isSidechain"] == true || entry["isMeta"] == true {
            continue;
        }
        let content = &entry["message"]["content"];
        match entry["type"].as_str() {
            Some("user") => {
                let text = match content {
                    Value::String(text) => text.clone(),
                    Value::Array(blocks) => blocks
                        .iter()
                        .filter(|b| b["type"] == "text")
                        .filter_map(|b| b["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n"),
                    _ => String::new(),
                };
                let text = text.trim();
                if !text.is_empty()
                    && !text.starts_with('<')
                    && !text.starts_with("[Request interrupted")
                {
                    items.push(("person", text.to_string(), None, None));
                }
            }
            Some("assistant") => {
                for block in content.as_array().into_iter().flatten() {
                    match block["type"].as_str() {
                        Some("text") => {
                            let text = block["text"].as_str().unwrap_or_default().trim();
                            if !text.is_empty() {
                                items.push(("answer", text.to_string(), None, None));
                            }
                        }
                        Some("tool_use") => {
                            let name = block["name"].as_str().unwrap_or_default();
                            if name == "ToolSearch" || name == "mcp__axioma__ask" {
                                continue;
                            }
                            let (label, detail) = content_activity(name, &block["input"], site)
                                .unwrap_or_else(|| describe(name, &block["input"]));
                            items.push(("activity", label, detail, Some("done")));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    items
}

/// The line of an action on the content, named after the page.
fn content_activity(tool: &str, input: &Value, site: &Path) -> Option<(String, Option<String>)> {
    let title_of = |id: &Value| {
        id.as_str()
            .and_then(|id| Cms::new(site).read(id).ok())
            .and_then(|page| page["title"].as_str().map(String::from))
    };
    let named = |verb: &str, title: Option<String>| match title {
        Some(title) => format!("{verb} the page \u{201c}{title}\u{201d}"),
        None => format!("{verb} a page"),
    };
    Some(match tool {
        "mcp__axioma__list_pages" => ("Looked at the pages".into(), None),
        "mcp__axioma__page_types" => ("Looked at the page types".into(), None),
        "mcp__axioma__list_media" => ("Looked at the pictures".into(), None),
        "mcp__axioma__read_page" => (named("Read", title_of(&input["id"])), None),
        "mcp__axioma__update_page" => {
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
        "mcp__axioma__create_page" => (
            named("Created", input["page"]["title"].as_str().map(String::from)),
            None,
        ),
        _ => return None,
    })
}

/// A short description of a tool call, and a second line with its argument.
fn describe(tool: &str, input: &Value) -> (String, Option<String>) {
    let text = |key: &str| {
        input[key]
            .as_str()
            .map(|value| clip(value.trim(), DETAIL_LENGTH))
    };
    let file = |key: &str| {
        input[key]
            .as_str()
            .map(|path| path.strip_prefix("/site/").unwrap_or(path).to_string())
    };
    let (label, detail) = match tool {
        "Read" => (
            format!("Read {}", file("file_path").unwrap_or("a file".into())),
            None,
        ),
        "Write" => (
            format!("Wrote {}", file("file_path").unwrap_or("a file".into())),
            None,
        ),
        "Edit" | "MultiEdit" => (
            format!("Edited {}", file("file_path").unwrap_or("a file".into())),
            None,
        ),
        "NotebookEdit" => (
            format!(
                "Edited {}",
                file("notebook_path").unwrap_or("a notebook".into())
            ),
            None,
        ),
        "Glob" => ("Looked for files".into(), text("pattern")),
        "Grep" => ("Searched the files".into(), text("pattern")),
        "Bash" => (
            text("description").unwrap_or_else(|| "Ran a command".into()),
            text("command"),
        ),
        "WebFetch" => ("Read a web page".into(), text("url")),
        "WebSearch" => ("Searched the web".into(), text("query")),
        "Task" | "Agent" => ("Asked a helper".into(), text("description")),
        "TodoWrite" => ("Updated its plan".into(), None),
        "Skill" => ("Used a skill".into(), text("skill")),
        "" => ("Used a tool".into(), None),
        other => (format!("Used {other}"), None),
    };
    (label, detail)
}

/// A permission request as a question, and its technical detail.
fn request_of(tool: &str, input: &Value) -> (String, Option<String>) {
    let (_, detail) = describe(tool, input);
    let file = |key: &str| {
        input[key]
            .as_str()
            .map(|path| path.strip_prefix("/site/").unwrap_or(path).to_string())
            .unwrap_or_else(|| "a file".into())
    };
    let title = match tool {
        "Bash" => match input["description"]
            .as_str()
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
        },
        "Read" => format!("Read {}?", file("file_path")),
        "Write" => format!("Write {}?", file("file_path")),
        "Edit" | "MultiEdit" => format!("Change {}?", file("file_path")),
        "NotebookEdit" => format!("Change {}?", file("notebook_path")),
        "Glob" | "Grep" => "Search the files?".into(),
        "WebFetch" => "Open a web page?".into(),
        "WebSearch" => "Search the web?".into(),
        "Task" | "Agent" => "Start a helper?".into(),
        "" => "Use a tool?".into(),
        other => format!("Use {other}?"),
    };
    // A command is the detail even when its description is the title.
    let detail = match tool {
        "Bash" => input["command"]
            .as_str()
            .map(|c| clip(c.trim(), DETAIL_LENGTH)),
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

const ITEM_COLUMNS: &str =
    "i.id, i.kind, i.text, i.detail, i.state, p.name, i.created_at, i.version
     FROM chat_item i LEFT JOIN person p ON p.id = i.author_id";

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

fn latest(conn: &Connection) -> rusqlite::Result<Option<Current>> {
    conn.query_row(
        "SELECT id, session, title, started FROM conversation ORDER BY updated_at DESC, id DESC LIMIT 1",
        [],
        |row| {
            Ok(Current {
                id: row.get(0)?,
                session: row.get(1)?,
                title: row.get(2)?,
                started: row.get::<_, i64>(3)? == 1,
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
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A chat whose assistant cannot run, for tests of other modules.
#[cfg(test)]
pub fn idle(db: db::Handle) -> Arc<Chat> {
    Chat::with(crate::agent::idle().environment(), db, None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn chat() -> (Arc<Chat>, i64) {
        let conn = db::memory();
        conn.execute(
            "INSERT INTO person (email, name, role) VALUES ('a@example.org', 'Ada', 'owner')",
            [],
        )
        .unwrap();
        let person = conn.last_insert_rowid();
        (idle(db::Handle::new(conn)), person)
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
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Read","input":{"file_path":"/site/axioma.json"}}]},"parent_tool_use_id":null}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2"}]},"parent_tool_use_id":null}
{"type":"stream_event","event":{"type":"message_start"},"parent_tool_use_id":null}
{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text"}},"parent_tool_use_id":null}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"The site "}},"parent_tool_use_id":null}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"is ready."}},"parent_tool_use_id":"helper"}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"is ready."}},"parent_tool_use_id":null}
{"type":"assistant","message":{"content":[{"type":"text","text":"The site is ready."}]},"parent_tool_use_id":null}
{"type":"result","subtype":"success","is_error":false,"result":"The site is ready.","permission_denials":[{"tool_name":"Bash","tool_input":{"command":"rm x","description":"Remove x"}},{"tool_name":"Bash","tool_input":{"command":"npx astro build"}}]}
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
            processor.handle(&chat, &event).await;
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
                ("activity", "Read axioma.json", Some("done")),
                ("answer", "The site is ready.", None),
                ("notice", "Some actions were not allowed.", None),
            ]
        );
        assert_eq!(snapshot.items[0].author.as_deref(), Some("Ada"));
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

        // The next turn resumes the session.
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
            processor.handle(&chat, &event).await;
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
            .handle(
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
                .starts_with("Stopped: axioma was restarting")
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
            (Some("allowed"), Some("Ada"))
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
        tokio::spawn(chat.clone().serve_hooks(UnixListener::bind(&path).unwrap()));
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
            "axioma_tool": "ask",
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
        let conn = db::memory();
        conn.execute(
            "INSERT INTO person (email, name, role) VALUES ('a@example.org', 'Ada', 'owner')",
            [],
        )
        .unwrap();
        let person = conn.last_insert_rowid();
        let env = Arc::new(Environment {
            site,
            home: dir.path().join("agent"),
            runtimes: crate::runtime::Runtimes {
                node: "/nonexistent/node".into(),
                claude: "/nonexistent/claude".into(),
                managed: false,
            },
            bwrap: None,
            axioma: "/nonexistent/axioma".into(),
            proxy: "/nonexistent/agent.sock".into(),
            hooks: "/nonexistent/hooks.sock".into(),
            policy: "/nonexistent/policy".into(),
            cgroup: None,
        });
        (
            Chat::with(env, db::Handle::new(conn), None, None),
            person,
            dir,
            home,
        )
    }

    #[tokio::test]
    async fn the_assistant_changes_pages_through_its_tools() {
        let (chat, person, _dir, home) = chat_on_site();
        let call =
            |name: &str, arguments: Value| json!({ "axioma_tool": name, "arguments": arguments });
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
        let restarted =
            Chat::start(crate::agent::idle().environment(), chat.db.clone(), None).await;
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
            .db
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
    async fn one_person_at_a_time() {
        let (chat, ada) = chat().await;
        let bob = chat
            .db
            .run(|conn| {
                conn.execute(
                    "INSERT INTO person (email, name, role) VALUES ('b@example.org', 'Bob', 'admin')",
                    [],
                )
                .map(|_| conn.last_insert_rowid())
            })
            .await
            .unwrap();
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
                conversation: setup.conversation,
                session: setup.session.clone(),
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
        let tools = json!({ "axioma_tool": "update_page", "arguments": { "id": home, "changes": { "title": "Studio" } } });
        assert!(chat.hook(&tools).await.unwrap()["text"].is_string());
        let ask = json!({ "axioma_tool": "ask", "arguments": {} });
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
        assert_eq!(
            snapshot.items[snapshot.items.len() - 3].author.as_deref(),
            Some("Ada")
        );
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
        assert!(next.text.starts_with("[axioma: the site went back"));
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

    #[test]
    fn errors_of_the_cli_are_told_apart() {
        let rejected = json!({ "status": "rejected", "resetsAt": 1794852600 });
        assert_eq!(
            classify("You've hit your limit", Some(&rejected)),
            ProblemInfo {
                kind: "limit",
                until: Some(1794852600)
            }
        );
        assert_eq!(
            classify("Claude AI usage limit reached|1794852600", None).until,
            Some(1794852600)
        );
        assert_eq!(
            classify("Invalid API key \u{b7} Please run /login", None).kind,
            "login"
        );
        assert_eq!(classify("Not logged in", None).kind, "login");
        assert_eq!(
            classify("Overloaded", Some(&json!({ "status": "allowed" }))).kind,
            "error"
        );
    }

    #[tokio::test]
    async fn a_problem_shows_until_the_next_turn() {
        let (chat, person) = chat().await;
        assert!(chat.retry(person, "Ada").await.is_err());
        let setup = chat.prepare(person, "Ada", "Hello").await.unwrap();
        let mut processor = Processor::new(setup.conversation);
        processor
            .handle(&chat, &json!({ "type": "rate_limit_event", "rate_limit_info": { "status": "rejected", "resetsAt": 1794852600 } }))
            .await;
        processor
            .handle(&chat, &json!({ "type": "result", "subtype": "success", "is_error": true, "result": "You've hit your limit" }))
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
        assert_eq!(
            request_of("Bash", &json!({ "command": "ls" })).0,
            "Run a command?"
        );
        assert_eq!(
            request_of("Write", &json!({ "file_path": "/site/src/a.astro" })).0,
            "Write src/a.astro?"
        );
        assert_eq!(
            request_of("WebFetch", &json!({ "url": "https://x.org" }))
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
        assert_eq!(
            describe("Grep", &json!({ "pattern": "title" })),
            ("Searched the files".to_string(), Some("title".to_string()))
        );
        assert_eq!(describe("Mystery", &json!({})).0, "Used Mystery");
    }
}
