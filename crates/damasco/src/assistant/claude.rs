//! Claude Code in its structured mode: `claude -p` reads the message as
//! stream-json and writes its events as stream-json. The first turn of a
//! conversation creates the CLI session with `--session-id`; later turns
//! resume it with `--resume`, so the CLI keeps the memory of the
//! conversation in the agent's home. Permission requests reach damasco as
//! control requests on the output, answered on the input.

use serde_json::{Value, json};

use super::{AgentEvent, Decision, Problem, Said, TextKind, Tool, Usage};

/// The tokens and the price of a turn, from its `result` event.
fn usage_of(event: &Value) -> Usage {
    let usage = &event["usage"];
    let count = |key: &str| usage[key].as_u64().unwrap_or(0);
    let cached = count("cache_read_input_tokens");
    Usage {
        input: count("input_tokens") + count("cache_creation_input_tokens") + cached,
        cached,
        output: count("output_tokens"),
        cost: event["total_cost_usd"].as_f64(),
    }
}

/// The arguments of the CLI for one turn of the chat.
pub fn turn_args(session: &str, resume: bool, mode: &str, model: Option<&str>) -> Vec<String> {
    let session_flag = if resume { "--resume" } else { "--session-id" };
    let mut args: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--permission-mode",
        mode,
        // Requests for permission come on the output and are answered on
        // the input, as in Claude's SDK: the tools that need someone to
        // answer, such as the one that presents a plan, are on.
        "--permission-prompt-tool",
        "stdio",
        // Questions go through damasco's own tool; the mode is the person's
        // choice.
        "--disallowedTools",
        "AskUserQuestion,EnterPlanMode",
        session_flag,
        session,
    ]
    .into_iter()
    .map(String::from)
    .collect();
    if let Some(model) = model {
        args.extend(["--model".to_string(), model.to_string()]);
    }
    args
}

/// The arguments of the CLI to ask for its models, without a message.
pub const MODELS_ARGS: &[&str] = &[
    "-p",
    "--input-format",
    "stream-json",
    "--output-format",
    "stream-json",
    "--verbose",
];

/// The request the CLI answers with its models, among what it can do, as
/// in the menu `/model` of its terminal: the same for the account.
pub fn models_request() -> Value {
    json!({ "type": "control_request", "request_id": "damasco-models", "request": { "subtype": "initialize" } })
}

/// The models of the answer to [`models_request`], or `None` if `line` is
/// not that answer. The CLI's "default" entry stays out: it marks the
/// model it stands for.
pub fn models_of(line: &str) -> Option<Vec<super::Model>> {
    let answer: Value = serde_json::from_str(line).ok()?;
    if answer["type"] != "control_response" || answer["response"]["request_id"] != "damasco-models"
    {
        return None;
    }
    let list = answer["response"]["response"]["models"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let default = list
        .iter()
        .find(|m| m["value"] == "default")
        .and_then(|m| m["resolvedModel"].as_str())
        .map(String::from);
    let models: Vec<super::Model> = list
        .iter()
        .filter(|m| m["value"] != "default")
        .filter_map(|m| {
            Some(super::Model {
                id: m["value"].as_str()?.to_string(),
                name: short_name(m["displayName"].as_str().unwrap_or_default()),
                detail: m["description"].as_str().unwrap_or_default().to_string(),
                default: default.is_some() && m["resolvedModel"].as_str() == default.as_deref(),
            })
        })
        .collect();
    // Without the context size, `opus` and `opus[1m]` both read "Opus": one
    // stays, the one the CLI uses by default, otherwise the first listed.
    let mut kept: Vec<super::Model> = Vec::new();
    for model in models {
        match kept.iter_mut().find(|k| k.name == model.name) {
            Some(twin) if model.default && !twin.default => *twin = model,
            Some(_) => {}
            None => kept.push(model),
        }
    }
    Some(kept)
}

/// The CLI's name of a model without the size of its context, which the
/// chat does not need: "Opus (1M context)" is "Opus".
fn short_name(name: &str) -> String {
    match name.find(" (") {
        Some(start) if name.ends_with(" context)") => name[..start].to_string(),
        _ => name.to_string(),
    }
}

/// The message of the person, as the CLI reads it on its input.
pub fn message(text: &str) -> Value {
    json!({
        "type": "user",
        "message": { "role": "user", "content": [{ "type": "text", "text": text }] },
    })
}

/// Reads the events of one run of the CLI.
#[derive(Debug, Default)]
pub struct Parser {
    /// Messages started so far: the blocks of each are numbered from 0.
    message: u64,
    /// The plan's limits as last reported, to tell a used-up plan.
    limits: Option<Value>,
}

impl Parser {
    pub fn parse(&mut self, event: &Value) -> Vec<AgentEvent> {
        // Events of helpers started by the assistant stay out of the chat:
        // their call already shows as one action.
        if !event["parent_tool_use_id"].is_null() {
            return Vec::new();
        }
        match event["type"].as_str() {
            Some("system") if event["subtype"] == "init" => vec![AgentEvent::Session {
                cli_id: event["session_id"].as_str().unwrap_or_default().to_string(),
            }],
            Some("stream_event") => self.stream(&event["event"]).into_iter().collect(),
            Some("assistant") => blocks(event)
                .iter()
                .filter_map(|block| match block["type"].as_str() {
                    Some("text") => Some(AgentEvent::TextDone {
                        item: None,
                        kind: TextKind::Answer,
                        text: block["text"].as_str().unwrap_or("").to_string(),
                    }),
                    Some("thinking") => Some(AgentEvent::TextDone {
                        item: None,
                        kind: TextKind::Thinking,
                        text: block["thinking"].as_str().unwrap_or("").to_string(),
                    }),
                    Some("tool_use") => Some(AgentEvent::ToolStart {
                        id: block["id"].as_str().unwrap_or_default().to_string(),
                        tool: tool_of(block["name"].as_str().unwrap_or(""), &block["input"]),
                    }),
                    _ => None,
                })
                .collect(),
            Some("user") => blocks(event)
                .iter()
                .filter(|block| block["type"] == "tool_result")
                .filter_map(|block| {
                    Some(AgentEvent::ToolEnd {
                        id: block["tool_use_id"].as_str()?.to_string(),
                        failed: block["is_error"] == true,
                    })
                })
                .collect(),
            Some("rate_limit_event") => {
                let limits = event["rate_limit_info"].clone();
                self.limits = Some(limits.clone());
                vec![AgentEvent::Limits(limits)]
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
                let problem = error
                    .as_deref()
                    .map(|error| classify(error, self.limits.as_ref()));
                let denied = event["permission_denials"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|d| tool_of(d["tool_name"].as_str().unwrap_or(""), &d["tool_input"]))
                    .collect();
                let usage = usage_of(event);
                let mut events = Vec::new();
                if !usage.is_empty() {
                    events.push(AgentEvent::Usage(usage));
                }
                events.push(AgentEvent::Finished {
                    error,
                    problem,
                    denied,
                });
                events
            }
            _ => Vec::new(),
        }
    }

    fn stream(&mut self, event: &Value) -> Option<AgentEvent> {
        let item = |index: &Value| index.as_u64().map(|i| format!("{}:{i}", self.message));
        match event["type"].as_str() {
            Some("message_start") => {
                self.message += 1;
                None
            }
            Some("content_block_start") => {
                let kind = match event["content_block"]["type"].as_str() {
                    Some("text") => TextKind::Answer,
                    Some("thinking") => TextKind::Thinking,
                    _ => return None,
                };
                Some(AgentEvent::TextStart {
                    item: item(&event["index"])?,
                    kind,
                })
            }
            Some("content_block_delta") => {
                let delta = &event["delta"];
                let piece = match delta["type"].as_str() {
                    Some("text_delta") => delta["text"].as_str(),
                    Some("thinking_delta") => delta["thinking"].as_str(),
                    _ => None,
                }?;
                Some(AgentEvent::TextDelta {
                    item: item(&event["index"])?,
                    text: piece.to_string(),
                })
            }
            _ => None,
        }
    }
}

fn blocks(event: &Value) -> Vec<Value> {
    event["message"]["content"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// A tool of Claude Code, by its name and input.
pub fn tool_of(name: &str, input: &Value) -> Tool {
    let text = |key: &str| input[key].as_str().map(String::from);
    if let Some(name) = name.strip_prefix("mcp__damasco__") {
        return Tool::Damasco {
            name: name.to_string(),
            input: input.clone(),
        };
    }
    match name {
        "ToolSearch" => Tool::Internal,
        "Read" => Tool::Read {
            path: text("file_path"),
        },
        "Write" => Tool::Write {
            path: text("file_path"),
        },
        "Edit" | "MultiEdit" => Tool::Edit {
            path: text("file_path"),
        },
        "NotebookEdit" => Tool::EditNotebook {
            path: text("notebook_path"),
        },
        "Glob" => Tool::FindFiles {
            pattern: text("pattern"),
        },
        "Grep" => Tool::SearchFiles {
            pattern: text("pattern"),
        },
        "Bash" => Tool::Command {
            command: text("command"),
            description: text("description"),
        },
        "WebFetch" => Tool::WebFetch { url: text("url") },
        "WebSearch" => Tool::WebSearch {
            query: text("query"),
        },
        "Task" | "Agent" => Tool::Helper {
            description: text("description"),
        },
        "TodoWrite" => Tool::Plan,
        "ExitPlanMode" => Tool::PlanReady { plan: text("plan") },
        "Skill" => Tool::Skill {
            name: text("skill"),
        },
        other => Tool::Other(other.to_string()),
    }
}

/// A permission request of the CLI's hook: the session that asks, and what
/// for. `None` for other hook events.
pub fn permission_request(event: &Value) -> Option<(Option<String>, Tool)> {
    (event["hook_event_name"] == "PermissionRequest").then(|| {
        (
            event["session_id"].as_str().map(String::from),
            tool_of(
                event["tool_name"].as_str().unwrap_or(""),
                &event["tool_input"],
            ),
        )
    })
}

/// The prefix of the tools of the connectors of a claude.ai account, the
/// MCP servers added in claude.ai, as Claude Code names them.
const CONNECTOR_PREFIX: &str = "mcp__claude_ai_";

/// The CLI's own tools that reach the claude.ai account: its artifacts,
/// design systems included, and standalone Claude Design. Asked of a person
/// as the connectors are.
const ACCOUNT_TOOLS: &[(&str, &str)] = &[
    ("Artifact", "Claude artifacts"),
    ("ArtifactComments", "Claude artifacts"),
    ("ArtifactData", "Claude artifacts"),
    ("DesignSync", "Claude Design"),
];

/// The connector whose tool the CLI is about to use, by its name in
/// claude.ai (`Google Drive` for `mcp__claude_ai_Google_Drive__search`),
/// or the part of the account one of the CLI's own tools reaches. `None`
/// for other hook events and other tools.
pub fn connector_use(event: &Value) -> Option<String> {
    if event["hook_event_name"] != "PreToolUse" {
        return None;
    }
    let tool = event["tool_name"].as_str()?;
    if let Some((_, name)) = ACCOUNT_TOOLS.iter().find(|(own, _)| *own == tool) {
        return Some((*name).to_string());
    }
    let server = tool
        .strip_prefix(CONNECTOR_PREFIX)?
        .split("__")
        .next()
        .filter(|server| !server.is_empty())?;
    Some(server.replace('_', " "))
}

/// What the hook prints before a connector's tool: allowed, refused, or,
/// with `None`, the CLI asks on its terminal.
pub fn connector_answer(allowed: Option<bool>, connector: &str) -> Value {
    let (decision, reason) = match allowed {
        Some(true) => ("allow", format!("{connector} is allowed in damasco.")),
        Some(false) => (
            "deny",
            format!(
                "The person did not allow {connector}. Go on without it, or ask them to allow it when it comes up again."
            ),
        ),
        None => ("ask", format!("{connector} is not allowed in damasco yet.")),
    };
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": decision,
            "permissionDecisionReason": reason,
        }
    })
}

/// What the hook prints for a decision.
pub fn permission_answer(decision: &Decision) -> Value {
    let decision = match decision {
        Decision::Allow => json!({ "behavior": "allow" }),
        Decision::Deny { message, interrupt } => {
            json!({ "behavior": "deny", "message": message, "interrupt": interrupt })
        }
    };
    json!({
        "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": decision }
    })
}

/// A request of the CLI to use a tool, on its output.
#[derive(Debug, Clone, PartialEq)]
pub struct ControlRequest {
    pub id: Value,
    pub tool: String,
    pub input: Value,
}

/// The request in `event`, if it is one to use a tool.
pub fn control_request(event: &Value) -> Option<ControlRequest> {
    (event["type"] == "control_request" && event["request"]["subtype"] == "can_use_tool").then(
        || ControlRequest {
            id: event["request_id"].clone(),
            tool: event["request"]["tool_name"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            input: event["request"]["input"].clone(),
        },
    )
}

/// The answer to a [`ControlRequest`]; going ahead with a plan moves the
/// session to `mode`.
pub fn control_answer(request: &ControlRequest, decision: &Decision, mode: Option<&str>) -> Value {
    let response = match decision {
        Decision::Allow => {
            let mut allow = json!({ "behavior": "allow", "updatedInput": request.input });
            if let Some(mode) = mode {
                allow["updatedPermissions"] =
                    json!([{ "type": "setMode", "mode": mode, "destination": "session" }]);
            }
            allow
        }
        Decision::Deny { message, interrupt } => {
            json!({ "behavior": "deny", "message": message, "interrupt": interrupt })
        }
    };
    json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": request.id, "response": response },
    })
}

/// What the hook prints when the person goes ahead with the plan of plan
/// mode: the request is allowed and the session continues in `mode`, as
/// Claude Code's terminal does.
pub fn plan_approved(mode: &str) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": {
                "behavior": "allow",
                "updatedPermissions": [
                    { "type": "setMode", "mode": mode, "destination": "session" }
                ],
            },
        }
    })
}

/// What an error of the CLI means for the person: the plan used up, the
/// account signed out, or something else.
pub fn classify(error: &str, limits: Option<&Value>) -> Problem {
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
        return Problem::Limit(until);
    }
    if text.contains("/login")
        || text.contains("not logged in")
        || text.contains("oauth token")
        || text.contains("invalid api key")
        || text.contains("authentication")
    {
        return Problem::Login;
    }
    Problem::Other
}

/// The transcript of a session, written by the interactive CLI too, in the
/// site's `projects` folder (mounted at `~/.claude/projects` in the
/// sandbox, where the site is `/site`).
pub fn transcript(projects: &std::path::Path, session: &str) -> std::path::PathBuf {
    projects.join("-site").join(format!("{session}.jsonl"))
}

/// The person's messages, the answers and the actions of a part of a
/// transcript, in order. Helpers' lines, commands of the CLI and notes it
/// adds itself stay out.
pub fn transcript_lines(text: &str) -> Vec<Said> {
    let mut said = Vec::new();
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
                    said.push(Said::Person(text.to_string()));
                }
            }
            Some("assistant") => {
                for block in content.as_array().into_iter().flatten() {
                    match block["type"].as_str() {
                        Some("text") => {
                            let text = block["text"].as_str().unwrap_or_default().trim();
                            if !text.is_empty() {
                                said.push(Said::Answer(text.to_string()));
                            }
                        }
                        Some("tool_use") => said.push(Said::Tool(tool_of(
                            block["name"].as_str().unwrap_or_default(),
                            &block["input"],
                        ))),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    said
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_connector_of_a_tool_is_named_as_in_claude_ai() {
        let event = |name: &str| json!({ "hook_event_name": "PreToolUse", "tool_name": name });
        assert_eq!(
            connector_use(&event("mcp__claude_ai_Google_Drive__search_files")).as_deref(),
            Some("Google Drive")
        );
        assert_eq!(
            connector_use(&event("mcp__claude_ai_Claude_Docs__read")).as_deref(),
            Some("Claude Docs")
        );
        assert_eq!(
            connector_use(&event("ArtifactData")).as_deref(),
            Some("Claude artifacts")
        );
        assert_eq!(
            connector_use(&event("DesignSync")).as_deref(),
            Some("Claude Design")
        );
        assert_eq!(connector_use(&event("ArtifactCheck")), None);
        assert_eq!(connector_use(&event("mcp__damasco__read_page")), None);
        assert_eq!(connector_use(&event("WebFetch")), None);
        let other =
            json!({ "hook_event_name": "PermissionRequest", "tool_name": "mcp__claude_ai_X__y" });
        assert_eq!(connector_use(&other), None);
        let answer = |allowed| {
            connector_answer(allowed, "Google Drive")["hookSpecificOutput"]["permissionDecision"]
                .clone()
        };
        assert_eq!(answer(Some(true)), "allow");
        assert_eq!(answer(Some(false)), "deny");
        assert_eq!(answer(None), "ask");
    }

    #[test]
    fn errors_of_the_cli_are_told_apart() {
        let rejected = json!({ "status": "rejected", "resetsAt": 1794852600 });
        assert_eq!(
            classify("You've hit your limit", Some(&rejected)),
            Problem::Limit(Some(1794852600))
        );
        assert_eq!(
            classify("Claude AI usage limit reached|1794852600", None),
            Problem::Limit(Some(1794852600))
        );
        assert_eq!(
            classify("Invalid API key \u{b7} Please run /login", None),
            Problem::Login
        );
        assert_eq!(classify("Not logged in", None), Problem::Login);
        assert_eq!(
            classify("Overloaded", Some(&json!({ "status": "allowed" }))),
            Problem::Other
        );
    }

    #[test]
    fn the_mode_of_the_turn_is_the_cli_s_own() {
        let args = turn_args("s", true, "plan", Some("opus"));
        assert_eq!(args[args.len() - 2..], ["--model", "opus"]);
        let at = args.iter().position(|a| a == "--permission-mode").unwrap();
        assert_eq!(args[at + 1], "plan");
        assert_eq!(
            tool_of("ExitPlanMode", &json!({ "plan": "1. Add a page" })),
            Tool::PlanReady {
                plan: Some("1. Add a page".into())
            }
        );
    }

    #[test]
    fn requests_on_the_output_are_answered_on_the_input() {
        let event = json!({ "type": "control_request", "request_id": "q1", "request": { "subtype": "can_use_tool", "tool_name": "ExitPlanMode", "input": { "plan": "1. Page" } } });
        let request = control_request(&event).unwrap();
        assert_eq!(request.tool, "ExitPlanMode");
        let answer = control_answer(&request, &Decision::Allow, Some("acceptEdits"));
        assert_eq!(answer["response"]["request_id"], "q1");
        assert_eq!(
            answer["response"]["response"]["updatedInput"]["plan"],
            "1. Page"
        );
        assert_eq!(
            answer["response"]["response"]["updatedPermissions"][0]["mode"],
            "acceptEdits"
        );
        let denied = control_answer(
            &request,
            &Decision::Deny {
                message: "no".into(),
                interrupt: false,
            },
            None,
        );
        assert_eq!(denied["response"]["response"]["behavior"], "deny");
        assert!(control_request(&json!({ "type": "assistant" })).is_none());
    }

    #[test]
    fn the_models_are_the_cli_s_own() {
        let line = r#"{"type":"control_response","response":{"subtype":"success","request_id":"damasco-models","response":{"models":[{"value":"default","resolvedModel":"claude-opus-5-5","displayName":"Default (recommended)"},{"value":"opus","resolvedModel":"claude-opus-5-5","displayName":"Opus","description":"Opus 5.5"},{"value":"haiku","resolvedModel":"claude-haiku-5-5","displayName":"Haiku","description":"Haiku 5.5"}]}}}"#;
        let models = models_of(line).unwrap();
        assert_eq!(
            models
                .iter()
                .map(|m| (m.id.as_str(), m.default))
                .collect::<Vec<_>>(),
            [("opus", true), ("haiku", false)]
        );
        assert!(models_of(r#"{"type":"system","subtype":"init"}"#).is_none());
    }

    #[test]
    fn models_drop_their_context_size_and_keep_one_of_twins() {
        let line = r#"{"type":"control_response","response":{"subtype":"success","request_id":"damasco-models","response":{"models":[{"value":"default","resolvedModel":"claude-opus-5-5[1m]","displayName":"Default (recommended)"},{"value":"opus","resolvedModel":"claude-opus-5-5","displayName":"Opus"},{"value":"opus[1m]","resolvedModel":"claude-opus-5-5[1m]","displayName":"Opus (1M context)"},{"value":"sonnet[1m]","resolvedModel":"claude-sonnet-5-5[1m]","displayName":"Sonnet (1M context)"},{"value":"sonnet","resolvedModel":"claude-sonnet-5-5","displayName":"Sonnet"}]}}}"#;
        let models = models_of(line).unwrap();
        assert_eq!(
            models
                .iter()
                .map(|m| (m.id.as_str(), m.name.as_str(), m.default))
                .collect::<Vec<_>>(),
            [("opus[1m]", "Opus", true), ("sonnet[1m]", "Sonnet", false)]
        );
        assert_eq!(short_name("Custom (beta)"), "Custom (beta)");
    }

    #[test]
    fn tools_are_named_whatever_the_cli_calls_them() {
        assert_eq!(
            tool_of("mcp__damasco__ask", &json!({})),
            Tool::Damasco {
                name: "ask".into(),
                input: json!({})
            }
        );
        assert!(tool_of("ToolSearch", &json!({})).hidden());
        assert_eq!(
            tool_of("Bash", &json!({ "command": "ls" })),
            Tool::Command {
                command: Some("ls".into()),
                description: None
            }
        );
    }

    #[test]
    fn blocks_of_each_message_are_told_apart() {
        let mut parser = Parser::default();
        let start = json!({ "type": "stream_event", "event": { "type": "message_start" }, "parent_tool_use_id": null });
        let block = json!({ "type": "stream_event", "event": { "type": "content_block_start", "index": 0, "content_block": { "type": "text" } }, "parent_tool_use_id": null });
        parser.parse(&start);
        let first = parser.parse(&block);
        parser.parse(&start);
        let second = parser.parse(&block);
        assert_ne!(first, second);
        let helper = json!({ "type": "assistant", "parent_tool_use_id": "t", "message": { "content": [{ "type": "text", "text": "x" }] } });
        assert!(parser.parse(&helper).is_empty());
    }
}
