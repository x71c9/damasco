//! OpenAI Codex through `codex app-server`: JSON-RPC on standard input and
//! output, one message per line, without the `jsonrpc` field. A turn starts
//! the server, starts or resumes the thread, starts the turn and follows its
//! notifications until `turn/completed`.
//!
//! Codex's own sandbox needs user namespaces, which damasco's sandbox does
//! not give: Codex runs with `danger-full-access` inside damasco's, and its
//! commands run as Codex decides, within damasco's sandbox (the site only,
//! the network through the proxy, a commit per turn). Should Codex ask for
//! an approval, a person answers it in the chat.
//!
//! The rules live where the agent cannot change them: `/etc/codex`
//! (requirements and managed configuration), plus read-only files over the
//! ones of the agent's home that would loosen them.

use std::collections::HashSet;

use serde_json::{Value, json};

use super::{AgentEvent, Decision, Problem, Said, TextKind, Tool, Usage};

/// The thread's tokens so far, from a `tokenUsage` breakdown.
fn tokens_of(breakdown: &Value) -> Usage {
    let count = |key: &str| breakdown[key].as_u64().unwrap_or(0);
    Usage {
        input: count("inputTokens"),
        cached: count("cachedInputTokens"),
        output: count("outputTokens"),
        cost: None,
    }
}

fn minus(total: Usage, before: Usage) -> Usage {
    Usage {
        input: total.input.saturating_sub(before.input),
        cached: total.cached.saturating_sub(before.cached),
        output: total.output.saturating_sub(before.output),
        cost: None,
    }
}

/// Where Codex reads its managed files, inside the sandbox.
pub const MANAGED_DIR: &str = "/etc/codex";

/// The `initialize` request of a connection. The turns of the chat need
/// the experimental part of the protocol for plan mode.
pub fn initialize() -> Value {
    json!({
        "clientInfo": { "name": "damasco", "title": "damasco", "version": env!("CARGO_PKG_VERSION") },
        "capabilities": { "experimentalApi": true, "requestAttestation": false },
    })
}

/// The collaboration mode of a turn: Codex's own "default" or "plan", with
/// the model of the thread.
pub fn collaboration(mode: &str, model: &str) -> Value {
    json!({
        "mode": if mode == "plan" { "plan" } else { "default" },
        "settings": { "model": model, "reasoning_effort": null, "developer_instructions": null },
    })
}

/// The parameters of `thread/start` (no `thread`) or `thread/resume`.
pub fn thread_params(thread: Option<&str>, model: Option<&str>) -> Value {
    let mut params = json!({ "cwd": "/site" });
    if let Some(thread) = thread {
        params["threadId"] = json!(thread);
    }
    if let Some(model) = model {
        params["model"] = json!(model);
    }
    params
}

/// The person's message as a turn's input: the words, then the pictures,
/// by their paths in the sandbox, which Codex reads and scales down itself.
pub fn input(text: &str, pictures: &[String]) -> Value {
    let mut input = vec![json!({ "type": "text", "text": text, "text_elements": [] })];
    input.extend(
        pictures
            .iter()
            .map(|path| json!({ "type": "localImage", "path": path })),
    );
    Value::Array(input)
}

/// A message of the server.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Response {
        id: Value,
        result: Option<Value>,
        error: Option<String>,
    },
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
}

/// Reads one line of the server, if it is a message.
pub fn incoming(line: &str) -> Option<Incoming> {
    let message: Value = serde_json::from_str(line).ok()?;
    let method = message["method"].as_str().map(String::from);
    let id = message.get("id").cloned().filter(|id| !id.is_null());
    Some(match (id, method) {
        (Some(id), None) => Incoming::Response {
            id,
            result: message.get("result").cloned(),
            error: message
                .get("error")
                .map(|e| e["message"].as_str().unwrap_or("error").to_string()),
        },
        (Some(id), Some(method)) => Incoming::Request {
            id,
            method,
            params: message["params"].clone(),
        },
        (None, Some(method)) => Incoming::Notification {
            method,
            params: message["params"].clone(),
        },
        (None, None) => return None,
    })
}

/// Reads the notifications of one turn.
#[derive(Debug, Default)]
pub struct Parser {
    /// Reasoning items whose summary started: those without one stay out.
    thinking: HashSet<String>,
    /// The last error that ended the turn, with what it means.
    error: Option<(String, Problem)>,
    limits: Option<Value>,
    /// The turn gave a plan (plan mode).
    planned: bool,
    /// The thread's tokens before the turn and now: the turn used the
    /// difference. Codex reports the thread's total after each request.
    tokens: Option<(Usage, Usage)>,
}

impl Parser {
    /// Whether a plan came since the last call.
    pub fn take_plan(&mut self) -> bool {
        std::mem::take(&mut self.planned)
    }

    pub fn notification(&mut self, method: &str, params: &Value) -> Vec<AgentEvent> {
        match method {
            "thread/started" => params["thread"]["id"]
                .as_str()
                .map(|id| AgentEvent::Session {
                    cli_id: id.to_string(),
                })
                .into_iter()
                .collect(),
            "item/started" => {
                let item = &params["item"];
                let id = item["id"].as_str().unwrap_or_default().to_string();
                match item["type"].as_str() {
                    // The plan of plan mode reads as an answer.
                    Some("agentMessage" | "plan") => vec![AgentEvent::TextStart {
                        item: id,
                        kind: TextKind::Answer,
                    }],
                    _ => tool_of_item(item)
                        .map(|tool| AgentEvent::ToolStart { id, tool })
                        .into_iter()
                        .collect(),
                }
            }
            "item/agentMessage/delta" | "item/plan/delta" => vec![AgentEvent::TextDelta {
                item: params["itemId"].as_str().unwrap_or_default().to_string(),
                text: params["delta"].as_str().unwrap_or_default().to_string(),
            }],
            "item/reasoning/summaryTextDelta" => {
                let item = params["itemId"].as_str().unwrap_or_default().to_string();
                let mut events = Vec::new();
                if self.thinking.insert(item.clone()) {
                    events.push(AgentEvent::TextStart {
                        item: item.clone(),
                        kind: TextKind::Thinking,
                    });
                }
                events.push(AgentEvent::TextDelta {
                    item,
                    text: params["delta"].as_str().unwrap_or_default().to_string(),
                });
                events
            }
            "item/reasoning/summaryPartAdded" => {
                let item = params["itemId"].as_str().unwrap_or_default().to_string();
                if self.thinking.contains(&item) {
                    vec![AgentEvent::TextDelta {
                        item,
                        text: "\n\n".into(),
                    }]
                } else {
                    Vec::new()
                }
            }
            "item/completed" => {
                let item = &params["item"];
                let id = item["id"].as_str().unwrap_or_default().to_string();
                if item["type"] == "plan" {
                    self.planned = true;
                }
                match item["type"].as_str() {
                    Some("agentMessage" | "plan") => vec![AgentEvent::TextDone {
                        item: Some(id),
                        kind: TextKind::Answer,
                        text: item["text"].as_str().unwrap_or_default().to_string(),
                    }],
                    Some("reasoning") => {
                        let text = strings(&item["summary"]).join("\n\n");
                        if self.thinking.contains(&id) || !text.trim().is_empty() {
                            vec![AgentEvent::TextDone {
                                item: Some(id),
                                kind: TextKind::Thinking,
                                text,
                            }]
                        } else {
                            Vec::new()
                        }
                    }
                    _ if tool_of_item(item).is_some() => {
                        let status = item["status"].as_str().unwrap_or_default();
                        let failed = matches!(status, "failed" | "declined")
                            || item["exitCode"].as_i64().is_some_and(|code| code != 0);
                        vec![AgentEvent::ToolEnd { id, failed }]
                    }
                    _ => Vec::new(),
                }
            }
            "thread/tokenUsage/updated" => {
                let usage = &params["tokenUsage"];
                let total = tokens_of(&usage["total"]);
                let before = match self.tokens {
                    Some((before, _)) => before,
                    None => minus(total, tokens_of(&usage["last"])),
                };
                self.tokens = Some((before, total));
                Vec::new()
            }
            "account/rateLimits/updated" => {
                let limits = params["rateLimits"].clone();
                self.limits = Some(limits.clone());
                vec![AgentEvent::Limits(limits)]
            }
            "error" if params["willRetry"] != true => {
                let error = &params["error"];
                self.error = Some((message_of(error), self.problem_of(error)));
                Vec::new()
            }
            "turn/completed" => {
                let turn = &params["turn"];
                let (error, problem) = match turn["status"].as_str() {
                    Some("failed") => {
                        let (message, problem) = match &turn["error"] {
                            Value::Null => self
                                .error
                                .take()
                                .unwrap_or_else(|| ("the turn failed".into(), Problem::Other)),
                            error => (message_of(error), self.problem_of(error)),
                        };
                        (Some(message), Some(problem))
                    }
                    _ => (None, None),
                };
                let mut events = Vec::new();
                if let Some((before, total)) = self.tokens.take() {
                    let used = minus(total, before);
                    if !used.is_empty() {
                        events.push(AgentEvent::Usage(used));
                    }
                }
                events.push(AgentEvent::Finished {
                    error,
                    problem,
                    denied: Vec::new(),
                });
                events
            }
            _ => Vec::new(),
        }
    }

    /// What an error of a turn means for the person.
    fn problem_of(&self, error: &Value) -> Problem {
        let info = &error["codexErrorInfo"];
        let text = message_of(error).to_lowercase();
        if info == "usageLimitExceeded"
            || info == "rateLimitExceeded"
            || text.contains("usage limit")
        {
            return Problem::Limit(self.resets_at());
        }
        if info == "unauthorized"
            || text.contains("log in")
            || text.contains("login")
            || text.contains("unauthorized")
        {
            return Problem::Login;
        }
        Problem::Other
    }

    /// When the used-up window of the plan opens again.
    fn resets_at(&self) -> Option<i64> {
        let limits = self.limits.as_ref()?;
        ["primary", "secondary"]
            .iter()
            .map(|window| &limits[*window])
            .filter(|window| {
                window["usedPercent"]
                    .as_f64()
                    .is_some_and(|used| used >= 100.0)
            })
            .filter_map(|window| window["resetsAt"].as_i64())
            .max()
    }
}

fn message_of(error: &Value) -> String {
    error["message"]
        .as_str()
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .unwrap_or("the turn failed")
        .to_string()
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(String::from))
        .collect()
}

/// The tool behind an item of a thread, for the items that are actions.
pub fn tool_of_item(item: &Value) -> Option<Tool> {
    Some(match item["type"].as_str()? {
        "commandExecution" => command_tool(
            item["command"].as_str().unwrap_or_default(),
            &item["commandActions"],
        ),
        "fileChange" => {
            let changes = item["changes"].as_array().cloned().unwrap_or_default();
            let path = changes
                .first()
                .and_then(|c| c["path"].as_str())
                .map(String::from);
            let added = changes.len() == 1 && changes[0]["kind"]["type"] == "add";
            if added {
                Tool::Write { path }
            } else {
                Tool::Edit { path }
            }
        }
        "mcpToolCall" => {
            let tool = item["tool"].as_str().unwrap_or_default();
            match item["server"].as_str() {
                Some("damasco") => Tool::Damasco {
                    name: tool.to_string(),
                    input: item["arguments"].clone(),
                },
                Some(server) => Tool::Other(format!("{server} {tool}")),
                None => Tool::Other(tool.to_string()),
            }
        }
        "webSearch" => Tool::WebSearch {
            query: item["query"].as_str().map(String::from),
        },
        "imageView" => Tool::Read {
            path: item["path"].as_str().map(String::from),
        },
        "collabAgentToolCall" => Tool::Helper {
            description: item["prompt"].as_str().map(String::from),
        },
        "dynamicToolCall" => Tool::Other(item["tool"].as_str().unwrap_or_default().to_string()),
        _ => return None,
    })
}

/// A command, named after what it does when Codex could tell: reading a
/// file, listing or searching files.
pub fn command_tool(command: &str, actions: &Value) -> Tool {
    let actions = actions.as_array().cloned().unwrap_or_default();
    if let [action] = actions.as_slice() {
        match action["type"].as_str() {
            Some("read") => {
                return Tool::Read {
                    path: action["path"].as_str().map(String::from),
                };
            }
            Some("listFiles") => {
                return Tool::FindFiles {
                    pattern: action["path"].as_str().map(String::from),
                };
            }
            Some("search") => {
                return Tool::SearchFiles {
                    pattern: action["query"].as_str().map(String::from),
                };
            }
            _ => {}
        }
    }
    Tool::Command {
        command: Some(unwrapped(command).to_string()),
        description: None,
    }
}

/// The command inside the shell Codex wraps it in (`bash -lc '…'`).
pub fn unwrapped(command: &str) -> &str {
    let command = command.trim();
    for shell in [
        "/bin/bash -lc ",
        "bash -lc ",
        "/bin/bash -c ",
        "bash -c ",
        "/bin/sh -c ",
        "sh -c ",
    ] {
        if let Some(rest) = command.strip_prefix(shell) {
            let rest = rest.trim();
            for quote in ['\'', '"'] {
                if let Some(inner) = rest
                    .strip_prefix(quote)
                    .and_then(|r| r.strip_suffix(quote))
                    .filter(|inner| !inner.contains(quote))
                {
                    return inner;
                }
            }
            return rest;
        }
    }
    command
}

/// The answer to an approval request of the server, by its method; `None`
/// when the request is not an approval of an action.
pub fn approval_answer(method: &str, decision: &Decision) -> Option<Value> {
    let accepted = matches!(decision, Decision::Allow);
    let decision = match decision {
        Decision::Allow => "accept",
        Decision::Deny {
            interrupt: true, ..
        } => "cancel",
        Decision::Deny { .. } => "decline",
    };
    match method {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            Some(json!({ "decision": decision }))
        }
        "execCommandApproval" | "applyPatchApproval" => Some(json!({
            "decision": if accepted { "approved" } else { "denied" }
        })),
        _ => None,
    }
}

/// The action an approval request is about, and the reason Codex gives.
pub fn approval_tool(method: &str, params: &Value) -> Option<(Tool, Option<String>)> {
    let reason = params["reason"].as_str().map(String::from);
    let tool = match method {
        "item/commandExecution/requestApproval" => Tool::Command {
            command: params["command"].as_str().map(|c| unwrapped(c).to_string()),
            description: None,
        },
        "execCommandApproval" => Tool::Command {
            command: Some(strings(&params["command"]).join(" ")),
            description: None,
        },
        "item/fileChange/requestApproval" | "applyPatchApproval" => Tool::Edit {
            path: params["grantRoot"].as_str().map(String::from),
        },
        _ => return None,
    };
    Some((tool, reason))
}

/// The questions of a `item/tool/requestUserInput` request, in the shape of
/// damasco's `ask` tool.
pub fn questions(params: &Value) -> Value {
    let questions: Vec<Value> = params["questions"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|q| {
            json!({
                "question": q["question"],
                "options": q["options"].as_array().cloned().unwrap_or_default(),
                "multiSelect": false,
            })
        })
        .collect();
    json!({ "questions": questions })
}

pub fn question_ids(params: &Value) -> Vec<String> {
    params["questions"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|q| q["id"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The answer to a `item/tool/requestUserInput` request.
pub fn answers(ids: &[String], answers: &[String]) -> Value {
    let answers: serde_json::Map<String, Value> = ids
        .iter()
        .zip(answers)
        .map(|(id, answer)| (id.clone(), json!({ "answers": [answer] })))
        .collect();
    json!({ "answers": answers })
}

/// The answer to the server's other requests: nothing is granted.
pub fn refusal(method: &str) -> Result<Value, String> {
    match method {
        "mcpServer/elicitation/request" => {
            Ok(json!({ "action": "decline", "content": null, "_meta": null }))
        }
        "item/tool/requestUserInput" => Ok(json!({ "answers": {} })),
        _ => Err(format!("{method} is not available in damasco")),
    }
}

/// A time of Codex in Unix seconds, whether it gives seconds or
/// milliseconds.
fn seconds(value: &Value) -> Option<i64> {
    let time = value
        .as_i64()
        .or_else(|| value.as_f64().map(|t| t as i64))?;
    Some(if time > 100_000_000_000 {
        time / 1000
    } else {
        time
    })
}

/// The newest thread of a `thread/list` answer created at or after `since`
/// (Unix seconds): the one a terminal started.
pub fn newest_thread(listed: &Value, since: i64) -> Option<String> {
    listed["data"]
        .as_array()?
        .iter()
        .filter_map(|thread| Some((seconds(&thread["createdAt"])?, thread["id"].as_str()?)))
        .filter(|(created, _)| *created >= since)
        .max_by_key(|(created, _)| *created)
        .map(|(_, id)| id.to_string())
}

/// What was said in the turns of a `thread/read` answer that started at or
/// after `since` (Unix seconds): the person's messages, the answers and the
/// actions, in order.
pub fn said_since(read: &Value, since: i64) -> Vec<Said> {
    let mut said = Vec::new();
    let turns = read["thread"]["turns"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for turn in turns {
        if seconds(&turn["startedAt"]).is_some_and(|started| started < since) {
            continue;
        }
        for item in turn["items"].as_array().into_iter().flatten() {
            match item["type"].as_str() {
                Some("userMessage") => {
                    let text = item["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|c| c["type"] == "text")
                        .filter_map(|c| c["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !text.trim().is_empty() {
                        said.push(Said::Person(text.trim().to_string()));
                    }
                }
                Some("agentMessage" | "plan") => {
                    let text = item["text"].as_str().unwrap_or_default().trim();
                    if !text.is_empty() {
                        said.push(Said::Answer(text.to_string()));
                    }
                }
                _ => {
                    if let Some(tool) = tool_of_item(item) {
                        said.push(Said::Tool(tool));
                    }
                }
            }
        }
    }
    said
}

/// The models of a `model/list` answer people can choose.
pub fn models_of(listed: &Value) -> Vec<super::Model> {
    listed["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["hidden"] != true)
        .filter_map(|m| {
            Some(super::Model {
                id: m["model"].as_str()?.to_string(),
                name: m["displayName"].as_str().unwrap_or_default().to_string(),
                detail: m["description"].as_str().unwrap_or_default().to_string(),
                default: m["isDefault"] == true,
            })
        })
        .collect()
}

/// The account as `account/read` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub signed_in: bool,
    /// "chatgpt" or "apiKey".
    pub method: Option<String>,
    pub email: Option<String>,
    pub plan: Option<String>,
}

pub fn account_of(result: &Value) -> Account {
    let account = &result["account"];
    Account {
        signed_in: account.is_object(),
        method: account["type"].as_str().map(String::from),
        email: account["email"].as_str().map(String::from),
        plan: account["planType"].as_str().map(String::from),
    }
}

/// Codex's managed files: `requirements.toml`, which nothing can loosen,
/// and `managed_config.toml`, above the user's configuration. `rules` are
/// the product's rules, given as developer instructions.
pub fn managed_files(rules: &str) -> [(&'static str, String); 2] {
    let rules = rules.replace("'''", "' ' '");
    let requirements = format!(
        r#"# Written by damasco: the assistant reads it, nothing can loosen it.
allowed_approval_policies = ["on-request"]
allowed_sandbox_modes = ["danger-full-access"]
allow_managed_hooks_only = true
additional_developer_instructions = '''
{rules}'''

[features]
hooks = false
plugins = false
remote_plugin = false
plugin_sharing = false
apps = false
browser_use = false
browser_use_external = false
computer_use = false
tool_suggest = false
skill_mcp_dependency_install = false
daemon_auto_start = false
"#
    );
    let managed = r#"# Written by damasco: above the configuration of the agent's home.
approval_policy = "on-request"
sandbox_mode = "danger-full-access"
check_for_update_on_startup = false

[analytics]
enabled = false

[mcp_servers.damasco]
command = "/opt/damasco/damasco"
args = ["mcp"]
startup_timeout_sec = 30
tool_timeout_sec = 3600
# damasco's tools need no approval, as with Claude Code.
default_tools_approval_mode = "approve"
"#
    .to_string();
    [
        ("requirements.toml", requirements),
        ("managed_config.toml", managed),
    ]
}

/// The configuration of the agent's home, mounted read-only over the one
/// Codex would read there. `/site` is trusted, so the interactive Codex
/// opens it without asking; the site's own `.codex`, which a trusted
/// project could use to change the configuration, is an empty read-only
/// directory in the sandbox.
pub const HOME_CONFIG: &str = r#"# Written by damasco and read-only for the assistant.
[projects."/site"]
trust_level = "trusted"
"#;

/// Files of Codex's home the agent could use to change the rules of later
/// turns, mounted read-only (empty) over the home.
pub const LOCKED_IN_HOME: &[&str] = &[".codex/AGENTS.md", ".codex/AGENTS.override.md"];

/// A connection to `codex app-server`, for the requests outside a turn:
/// the account, the sign-in.
pub struct Server {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    lines: tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    next: i64,
}

/// How long a request outside a turn waits for its answer.
const ANSWER_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

impl Server {
    /// Spawns the server with `command` (`codex app-server` in the sandbox,
    /// with piped input and output) and initializes the connection.
    pub async fn start(mut command: tokio::process::Command) -> Result<Self, String> {
        use tokio::io::AsyncBufReadExt;
        command.stderr(std::process::Stdio::null());
        let mut child = command.spawn().map_err(|e| e.to_string())?;
        let stdin = child.stdin.take().ok_or("no input to Codex")?;
        let stdout = child.stdout.take().ok_or("no output from Codex")?;
        let mut server = Self {
            child,
            stdin,
            lines: tokio::io::BufReader::new(stdout).lines(),
            next: 0,
        };
        server.call("initialize", initialize()).await?;
        server.send(&json!({ "method": "initialized" })).await?;
        Ok(server)
    }

    async fn send(&mut self, message: &Value) -> Result<(), String> {
        use tokio::io::AsyncWriteExt;
        self.stdin
            .write_all(format!("{message}\n").as_bytes())
            .await
            .map_err(|e| format!("cannot write to Codex: {e}"))
    }

    /// The next message of the server; `None` once it closed.
    pub async fn next(&mut self) -> Option<Incoming> {
        loop {
            let line = self.lines.next_line().await.ok()??;
            if let Some(message) = incoming(&line) {
                return Some(message);
            }
        }
    }

    /// Sends a request and waits for its answer. Notifications meanwhile
    /// are skipped; requests of the server are refused.
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next += 1;
        let id = self.next;
        self.send(&json!({ "id": id, "method": method, "params": params }))
            .await?;
        let answer = tokio::time::timeout(ANSWER_WAIT, async {
            loop {
                match self.next().await {
                    Some(Incoming::Response {
                        id: got,
                        result,
                        error,
                    }) if got == id => {
                        return match error {
                            Some(error) => Err(error),
                            None => Ok(result.unwrap_or(Value::Null)),
                        };
                    }
                    Some(Incoming::Request { id, method, .. }) => {
                        let reply = match refusal(&method) {
                            Ok(result) => json!({ "id": id, "result": result }),
                            Err(error) => {
                                json!({ "id": id, "error": { "code": -32000, "message": error } })
                            }
                        };
                        self.send(&reply).await?;
                    }
                    Some(_) => {}
                    None => return Err("Codex stopped".to_string()),
                }
            }
        })
        .await;
        answer.unwrap_or_else(|_| Err(format!("Codex did not answer {method}")))
    }

    /// Waits for the notification `method`, up to `limit`.
    pub async fn notification(
        &mut self,
        method: &str,
        limit: std::time::Duration,
    ) -> Result<Value, String> {
        tokio::time::timeout(limit, async {
            loop {
                match self.next().await {
                    Some(Incoming::Notification {
                        method: got,
                        params,
                    }) if got == method => {
                        return Ok(params);
                    }
                    Some(Incoming::Request { id, method, .. }) => {
                        let error = refusal(&method).err().unwrap_or_default();
                        self.send(
                            &json!({ "id": id, "error": { "code": -32000, "message": error } }),
                        )
                        .await?;
                    }
                    Some(_) => {}
                    None => return Err("Codex stopped".to_string()),
                }
            }
        })
        .await
        .unwrap_or_else(|_| Err(format!("no {method} from Codex in time")))
    }

    pub async fn stop(mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_becomes_events() {
        let mut parser = Parser::default();
        let started = parser.notification(
            "item/started",
            &json!({ "item": { "type": "commandExecution", "id": "c1", "command": "/bin/bash -lc 'npm install x'", "commandActions": [{ "type": "unknown", "command": "npm install x" }], "status": "inProgress" } }),
        );
        assert_eq!(
            started,
            [AgentEvent::ToolStart {
                id: "c1".into(),
                tool: Tool::Command {
                    command: Some("npm install x".into()),
                    description: None
                }
            }]
        );
        let ended = parser.notification(
            "item/completed",
            &json!({ "item": { "type": "commandExecution", "id": "c1", "status": "completed", "exitCode": 1 } }),
        );
        assert_eq!(
            ended,
            [AgentEvent::ToolEnd {
                id: "c1".into(),
                failed: true
            }]
        );
        let read = parser.notification(
            "item/started",
            &json!({ "item": { "type": "commandExecution", "id": "c2", "command": "cat a.md", "commandActions": [{ "type": "read", "command": "cat a.md", "name": "a.md", "path": "/site/a.md" }] } }),
        );
        assert_eq!(
            read,
            [AgentEvent::ToolStart {
                id: "c2".into(),
                tool: Tool::Read {
                    path: Some("/site/a.md".into())
                }
            }]
        );
        // A reasoning without a summary stays out; one with it shows.
        assert!(
            parser
                .notification(
                    "item/completed",
                    &json!({ "item": { "type": "reasoning", "id": "r0", "summary": [], "content": [] } })
                )
                .is_empty()
        );
        let thinking = parser.notification(
            "item/reasoning/summaryTextDelta",
            &json!({ "itemId": "r1", "delta": "Looking" }),
        );
        assert_eq!(thinking.len(), 2);
        let answer = parser.notification(
            "item/agentMessage/delta",
            &json!({ "itemId": "m1", "delta": "Done." }),
        );
        assert_eq!(
            answer,
            [AgentEvent::TextDelta {
                item: "m1".into(),
                text: "Done.".into()
            }]
        );
        let tool = parser.notification(
            "item/started",
            &json!({ "item": { "type": "mcpToolCall", "id": "t", "server": "damasco", "tool": "update_page", "arguments": { "id": "x" }, "status": "inProgress" } }),
        );
        assert!(
            matches!(&tool[0], AgentEvent::ToolStart { tool: Tool::Damasco { name, .. }, .. } if name == "update_page")
        );
    }

    #[test]
    fn the_end_of_a_turn_tells_what_went_wrong() {
        let mut parser = Parser::default();
        parser.notification(
            "account/rateLimits/updated",
            &json!({ "rateLimits": { "primary": { "usedPercent": 100.0, "resetsAt": 1795000000 }, "secondary": { "usedPercent": 20.0, "resetsAt": 1796000000 } } }),
        );
        let done = parser.notification(
            "turn/completed",
            &json!({ "turn": { "status": "failed", "error": { "message": "You've hit your usage limit.", "codexErrorInfo": "usageLimitExceeded" } } }),
        );
        assert_eq!(
            done,
            [AgentEvent::Finished {
                error: Some("You've hit your usage limit.".into()),
                problem: Some(Problem::Limit(Some(1795000000))),
                denied: Vec::new()
            }]
        );
        let signed_out = parser.notification(
            "turn/completed",
            &json!({ "turn": { "status": "failed", "error": { "message": "401", "codexErrorInfo": "unauthorized" } } }),
        );
        assert!(matches!(
            &signed_out[0],
            AgentEvent::Finished {
                problem: Some(Problem::Login),
                ..
            }
        ));
        let fine = parser.notification(
            "turn/completed",
            &json!({ "turn": { "status": "completed" } }),
        );
        assert_eq!(
            fine,
            [AgentEvent::Finished {
                error: None,
                problem: None,
                denied: Vec::new()
            }]
        );
    }

    #[test]
    fn a_turn_tells_the_tokens_it_used() {
        let mut parser = Parser::default();
        let breakdown = |input: u64, cached: u64, output: u64| {
            json!({ "totalTokens": input + output, "inputTokens": input, "cachedInputTokens": cached,
                    "cacheWriteInputTokens": 0, "outputTokens": output, "reasoningOutputTokens": 0 })
        };
        // The thread had used 1000 in and 100 out in earlier turns; this
        // turn makes two requests.
        parser.notification(
            "thread/tokenUsage/updated",
            &json!({ "threadId": "t", "turnId": "u", "tokenUsage": {
                "total": breakdown(1500, 200, 150), "last": breakdown(500, 200, 50), "modelContextWindow": null } }),
        );
        parser.notification(
            "thread/tokenUsage/updated",
            &json!({ "threadId": "t", "turnId": "u", "tokenUsage": {
                "total": breakdown(2300, 900, 190), "last": breakdown(800, 700, 40), "modelContextWindow": null } }),
        );
        let done = parser.notification(
            "turn/completed",
            &json!({ "turn": { "status": "completed" } }),
        );
        assert_eq!(
            done[0],
            AgentEvent::Usage(Usage {
                input: 1300,
                cached: 900,
                output: 90,
                cost: None
            })
        );
        assert!(matches!(done[1], AgentEvent::Finished { .. }));
        // The next turn counts from where this one ended.
        let next = parser.notification(
            "turn/completed",
            &json!({ "turn": { "status": "completed" } }),
        );
        assert_eq!(next.len(), 1);
    }

    #[test]
    fn hooks_and_approvals_are_answered() {
        assert_eq!(
            approval_answer("item/commandExecution/requestApproval", &Decision::Allow),
            Some(json!({ "decision": "accept" }))
        );
        assert!(approval_answer("item/permissions/requestApproval", &Decision::Allow).is_none());
        assert_eq!(
            incoming(r#"{"id":3,"method":"item/tool/requestUserInput","params":{}}"#),
            Some(Incoming::Request {
                id: json!(3),
                method: "item/tool/requestUserInput".into(),
                params: json!({})
            })
        );
        assert!(refusal("account/chatgptAuthTokens/refresh").is_err());
    }

    #[test]
    fn the_plan_of_plan_mode_reads_as_an_answer_and_questions_are_asked() {
        let mut parser = Parser::default();
        let started = parser.notification(
            "item/started",
            &json!({ "item": { "type": "plan", "id": "p", "text": "" } }),
        );
        assert_eq!(
            started,
            [AgentEvent::TextStart {
                item: "p".into(),
                kind: TextKind::Answer
            }]
        );
        assert_eq!(
            parser.notification(
                "item/plan/delta",
                &json!({ "itemId": "p", "delta": "# Plan" })
            ),
            [AgentEvent::TextDelta {
                item: "p".into(),
                text: "# Plan".into()
            }]
        );
        let params = json!({ "questions": [{ "id": "goal", "question": "What first?", "options": [
            { "label": "Portfolio", "description": "" }, { "label": "Contacts", "description": "" }
        ] }] });
        assert_eq!(
            questions(&params)["questions"][0]["options"][1]["label"],
            "Contacts"
        );
        assert_eq!(
            answers(&question_ids(&params), &["Portfolio".into()]),
            json!({ "answers": { "goal": { "answers": ["Portfolio"] } } })
        );
    }

    #[test]
    fn the_terminal_s_turns_are_read_back() {
        let listed = json!({ "data": [
            { "id": "old", "createdAt": 1795000000 },
            { "id": "new", "createdAt": 1795000600000i64 },
        ] });
        assert_eq!(newest_thread(&listed, 1795000500).as_deref(), Some("new"));
        assert_eq!(newest_thread(&listed, 1795001000), None);
        let read = json!({ "thread": { "turns": [
            { "startedAt": 1795000000, "items": [{ "type": "userMessage", "content": [{ "type": "text", "text": "before" }] }] },
            { "startedAt": 1795000700, "items": [
                { "type": "userMessage", "content": [{ "type": "text", "text": "Make it blue" }] },
                { "type": "reasoning", "summary": [] },
                { "type": "fileChange", "id": "f", "changes": [{ "path": "/site/src/a.css", "kind": { "type": "update" } }] },
                { "type": "agentMessage", "text": "Done." },
            ] },
        ] } });
        assert_eq!(
            said_since(&read, 1795000500),
            [
                Said::Person("Make it blue".into()),
                Said::Tool(Tool::Edit {
                    path: Some("/site/src/a.css".into())
                }),
                Said::Answer("Done.".into()),
            ]
        );
    }

    #[test]
    fn the_managed_files_hold_the_rules() {
        let [(_, requirements), (_, managed)] = managed_files("Rule one.\n'''");
        assert!(requirements.contains("Rule one."));
        assert!(requirements.contains("hooks = false"));
        assert!(managed.contains("[mcp_servers.damasco]"));
        let parsed: toml::Value = toml::from_str(&requirements).unwrap();
        assert!(
            parsed["additional_developer_instructions"]
                .as_str()
                .unwrap()
                .contains("Rule one.")
        );
    }
}
