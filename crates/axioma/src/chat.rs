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
use crate::db;

/// Longest message accepted, in characters.
pub const MESSAGE_LIMIT: usize = 20_000;
/// Characters of the first message kept as the conversation's title.
const TITLE_LENGTH: usize = 60;
/// Characters kept of the second line of an action.
const DETAIL_LENGTH: usize = 300;
/// Longest wait for the build that ends a turn.
const BUILD_WAIT: Duration = Duration::from_secs(10 * 60);
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
    },
    Limits {
        limits: Value,
    },
    /// Another conversation is shown: the panel asks for a new snapshot.
    Reset,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SendError {
    Busy,
    Empty,
    TooLong,
    Unavailable(String),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::Busy => write!(f, "the assistant is still working"),
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
}

#[derive(Default)]
struct Inner {
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
        Self::with(env, db, builder, current)
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
        }
    }

    /// Records the message and starts a turn with it.
    pub async fn send(self: &Arc<Self>, person: i64, text: &str) -> Result<(), SendError> {
        let setup = self.prepare(person, text).await?;
        let chat = self.clone();
        tokio::spawn(async move { chat.run(setup).await });
        Ok(())
    }

    async fn prepare(&self, person: i64, text: &str) -> Result<Setup, SendError> {
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
            if inner.running.is_some() {
                return Err(SendError::Busy);
            }
            inner.running = Some(Running {
                started: Instant::now(),
                stop: stop.clone(),
                stopping: false,
                updating: false,
                live: HashMap::new(),
                requests: HashMap::new(),
                asked: false,
            });
            inner.current.clone()
        };
        let created = {
            let text = text.clone();
            self.db
                .run(move |conn| -> rusqlite::Result<(Current, Item)> {
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
                    let item = insert_item(conn, current.id, "person", &text, None, None, Some(person))?;
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
            self.emit(Event::Turn {
                turn: Self::turn_info(&inner),
                conversation: Self::conversation_info(&inner),
            });
        }
        self.emit(Event::Item { item });
        Ok(Setup {
            conversation: current.id,
            session: current.session,
            resume: current.started,
            text,
            stop,
        })
    }

    /// Stops the turn in progress, if any.
    pub fn stop(&self) {
        let mut inner = self.inner();
        if let Some(running) = inner.running.as_mut() {
            running.stopping = true;
            running.stop.notify_one();
            // Requests still waiting are refused.
            running.requests.clear();
            let turn = Self::turn_info(&inner);
            let conversation = Self::conversation_info(&inner);
            self.emit(Event::Turn { turn, conversation });
        }
    }

    /// Leaves the current conversation: the next message starts a new one.
    pub fn new_conversation(&self) -> Result<(), SendError> {
        let mut inner = self.inner();
        if inner.running.is_some() {
            return Err(SendError::Busy);
        }
        inner.current = None;
        self.emit(Event::Reset);
        Ok(())
    }

    async fn run(self: Arc<Self>, setup: Setup) {
        let mut processor = Processor::new(setup.conversation);
        let ended = self.drive(&setup, &mut processor).await;
        processor.finish(&self, ended).await;
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
            Waited::Reply(Reply::Permission(true)) => decision(true, "", false),
            Waited::Reply(_) => decision(false, "The person did not allow this.", false),
            Waited::Stopped => decision(false, "The person stopped the assistant.", true),
            Waited::TimedOut => decision(false, "Nobody answered the request in time.", false),
            Waited::Unsaved => decision(false, "The request could not be shown.", false),
        }
    }

    /// A call of a tool of axioma's MCP server.
    async fn tool(&self, call: &Value) -> Value {
        let result = match call["axioma_tool"].as_str() {
            Some("ask") => self.ask(&call["arguments"]).await,
            _ => Err("unknown tool".to_string()),
        };
        match result {
            Ok(text) => json!({ "text": text }),
            Err(error) => json!({ "error": error }),
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
        let _ = sender.send(Reply::Permission(allow));
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

    fn emit_turn(&self, inner: &Inner) {
        self.emit(Event::Turn {
            turn: Self::turn_info(inner),
            conversation: Self::conversation_info(inner),
        });
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
            Some("tool_use") => {
                let (label, detail) =
                    describe(block["name"].as_str().unwrap_or(""), &block["input"]);
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
    async fn finish(mut self, chat: &Chat, ended: Ended) {
        for (_, id) in std::mem::take(&mut self.open) {
            let text = chat.live_text(id).unwrap_or_default();
            chat.change(id, Some(text), None).await;
        }
        for (_, id) in std::mem::take(&mut self.tools) {
            chat.change(id, None, Some("stopped")).await;
        }
        let conversation = self.conversation;
        if ended.stopped && chat.is_closed() {
            let text = "Stopped: axioma was restarting. Send the message again to continue.";
            chat.add(conversation, "notice", text.into(), None, None)
                .await;
        } else if ended.stopped {
            chat.add(conversation, "notice", "Stopped.".into(), None, None)
                .await;
        } else if let Some(problem) = ended.failure {
            chat.add(
                conversation,
                "error",
                "The assistant could not start.".into(),
                Some(problem),
                None,
            )
            .await;
        } else if let Some(outcome) = self.outcome.take() {
            if let Some(error) = outcome.error {
                chat.add(conversation, "error", error, None, None).await;
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
        }
        let _ = chat
            .db
            .run(move |conn| {
                conn.execute(
                    "UPDATE conversation SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?1",
                    [conversation],
                )
            })
            .await;
        // The turn ends with the preview showing its changes, or with the
        // reason it cannot.
        if let Some(builder) = chat.builder.clone().filter(|_| !chat.is_closed()) {
            {
                let mut inner = chat.inner();
                if let Some(running) = inner.running.as_mut() {
                    running.updating = true;
                }
                let turn = Chat::turn_info(&inner);
                let conversation = Chat::conversation_info(&inner);
                chat.emit(Event::Turn { turn, conversation });
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
            let conversation = Chat::conversation_info(&inner);
            chat.emit(Event::Turn {
                turn: None,
                conversation,
            });
        }
    }
}

/// A person's reply to a request of the assistant.
enum Reply {
    Permission(bool),
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

const ITEM_COLUMNS: &str = "i.id, i.kind, i.text, i.detail, i.state, p.name, i.created_at
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
            .prepare(person, "  What is the site called?  ")
            .await
            .unwrap();
        assert!(!setup.resume);
        assert_eq!(
            chat.prepare(person, "again").await.err(),
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
        let next = chat.prepare(person, "Thanks").await.unwrap();
        assert!(next.resume);
        assert_eq!(next.session, setup.session);
        assert_eq!(next.conversation, setup.conversation);
    }

    #[tokio::test]
    async fn a_stopped_turn_keeps_its_partial_answer() {
        let (chat, person) = chat().await;
        let setup = chat.prepare(person, "Write a long page").await.unwrap();
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
        assert!(!chat.prepare(person, "again").await.unwrap().resume);
    }

    #[tokio::test]
    async fn a_crash_and_a_cli_error_are_shown() {
        let (chat, person) = chat().await;
        let setup = chat.prepare(person, "Hello").await.unwrap();
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
        let setup = chat.prepare(person, "Hello again").await.unwrap();
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
            chat.prepare(person, "   ").await.err(),
            Some(SendError::Empty)
        );
        let long = "a".repeat(MESSAGE_LIMIT + 1);
        assert_eq!(
            chat.prepare(person, &long).await.err(),
            Some(SendError::TooLong)
        );

        let setup = chat.prepare(person, "First").await.unwrap();
        assert_eq!(chat.new_conversation(), Err(SendError::Busy));
        Processor::new(setup.conversation)
            .finish(&chat, Ended::failed("no sandbox".into()))
            .await;
        chat.new_conversation().unwrap();
        assert!(chat.snapshot().await.conversation.is_none());
        let other = chat.prepare(person, "Second").await.unwrap();
        assert_ne!(other.conversation, setup.conversation);
        assert_ne!(other.session, setup.session);
    }

    #[tokio::test]
    async fn sending_without_a_sandbox_reports_it() {
        let (chat, person) = chat().await;
        chat.send(person, "Hello").await.unwrap();
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
        let setup = chat.prepare(person, "Write a long page").await.unwrap();
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
        let setup = chat.prepare(person, "Add a map").await.unwrap();
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
        let setup = chat.prepare(person, "Add a map").await.unwrap();
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
        chat.prepare(person, "Hello").await.unwrap();
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
        let setup = chat.prepare(person, "Add a map").await.unwrap();
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
        chat.prepare(person, "Add a contact page").await.unwrap();
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
