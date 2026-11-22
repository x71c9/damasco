//! The assistants damasco can drive, and what the chat needs from each.
//!
//! An assistant is an official vendor CLI run in the agent sandbox. The chat
//! does not read the CLI's own events: each assistant turns them into the
//! events of this module, and its tool calls into [`Tool`], so that the
//! chat, its items and its phrases are the same whatever the assistant.

pub mod claude;
pub mod codex;

use serde::Serialize;
use serde_json::Value;

/// The vendor CLI behind the assistant. One is active per installation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Claude,
    Codex,
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::Claude, Provider::Codex];

    /// The id kept in the database and in the API.
    pub fn id(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.id() == id)
    }

    /// The name people know the assistant by.
    pub fn name(self) -> &'static str {
        match self {
            Provider::Claude => "Claude",
            Provider::Codex => "ChatGPT",
        }
    }

    /// The program of the CLI, in the sandbox's PATH.
    pub fn program(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
        }
    }

    /// The modes of the CLI the chat offers, as the CLI names them; the
    /// first is the default. Claude Code's `bypassPermissions` stays out:
    /// damasco's rules refuse it.
    pub fn modes(self) -> &'static [&'static str] {
        match self {
            Provider::Claude => &["auto", "acceptEdits", "manual", "plan", "dontAsk"],
            Provider::Codex => &["default", "plan"],
        }
    }

    pub fn default_mode(self) -> &'static str {
        self.modes()[0]
    }

    /// `mode` if the CLI has it, otherwise its default.
    pub fn mode(self, mode: Option<&str>) -> &'static str {
        self.modes()
            .iter()
            .find(|m| Some(**m) == mode)
            .copied()
            .unwrap_or_else(|| self.default_mode())
    }

    pub fn capabilities(self) -> Capabilities {
        match self {
            Provider::Claude => Capabilities {
                subscription: true,
                api_key: true,
                terminal: true,
            },
            Provider::Codex => Capabilities {
                subscription: true,
                api_key: true,
                terminal: true,
            },
        }
    }

    /// Hosts the CLI needs, beyond those of every assistant (npm).
    pub fn hosts(self) -> &'static [&'static str] {
        match self {
            Provider::Claude => &[
                "*.anthropic.com",
                "claude.ai",
                "*.claude.ai",
                "claude.com",
                "*.claude.com",
                // The content of artifacts, read with Claude Code's Artifact tool.
                "*.claudeusercontent.com",
            ],
            Provider::Codex => &["*.openai.com", "chatgpt.com", "*.chatgpt.com"],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    /// Signing in with the person's plan.
    pub subscription: bool,
    pub api_key: bool,
    /// The interactive CLI in the panel's terminal, on the chat's
    /// conversation.
    pub terminal: bool,
}

/// Whether a text is part of the answer or of the reasoning before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    Answer,
    Thinking,
}

impl TextKind {
    /// The kind of the chat item.
    pub fn item(self) -> &'static str {
        match self {
            TextKind::Answer => "answer",
            TextKind::Thinking => "thinking",
        }
    }
}

/// Why a turn did not finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// The plan is used up until then (Unix seconds), when known.
    Limit(Option<i64>),
    /// The account must sign in again.
    Login,
    /// Anything else: the turn can be tried again.
    Other,
}

/// What happens in a turn, as the chat needs it.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    /// The CLI has the conversation: the id it knows it by.
    Session {
        cli_id: String,
    },
    /// A text starts: its pieces follow with the same `item`.
    TextStart {
        item: String,
        kind: TextKind,
    },
    TextDelta {
        item: String,
        text: String,
    },
    /// A text is complete. Without `item`, it completes the oldest open text
    /// of its kind, or stands alone.
    TextDone {
        item: Option<String>,
        kind: TextKind,
        text: String,
    },
    /// The assistant uses a tool; `id` names it until it ends.
    ToolStart {
        id: String,
        tool: Tool,
    },
    ToolEnd {
        id: String,
        failed: bool,
    },
    /// The plan's limits as the CLI reports them, passed to the panel.
    Limits(Value),
    /// What the turn used, once it is over.
    Usage(Usage),
    /// The turn is over.
    Finished {
        /// The error the CLI reported, if the turn failed.
        error: Option<String>,
        problem: Option<Problem>,
        /// Actions refused without a person deciding.
        denied: Vec<Tool>,
    },
}

/// A tool call of the assistant, whatever the CLI calls it.
#[derive(Debug, Clone, PartialEq)]
pub enum Tool {
    /// A tool of damasco's MCP server, by its name, with its arguments.
    Damasco {
        name: String,
        input: Value,
    },
    /// Internal to the CLI, never shown (loading more tools).
    Internal,
    Read {
        path: Option<String>,
    },
    Write {
        path: Option<String>,
    },
    Edit {
        path: Option<String>,
    },
    EditNotebook {
        path: Option<String>,
    },
    FindFiles {
        pattern: Option<String>,
    },
    SearchFiles {
        pattern: Option<String>,
    },
    Command {
        command: Option<String>,
        /// What the command is for, as the assistant put it.
        description: Option<String>,
    },
    WebFetch {
        url: Option<String>,
    },
    WebSearch {
        query: Option<String>,
    },
    Helper {
        description: Option<String>,
    },
    Plan,
    /// The plan of plan mode, ready for the person to go ahead with.
    PlanReady {
        plan: Option<String>,
    },
    Skill {
        name: Option<String>,
    },
    /// Any other tool, by its name ("" when unknown).
    Other(String),
}

impl Tool {
    /// Tools that show as their own card, or not at all, instead of an
    /// action: questions, buttons to the panel, confirmations to delete.
    pub fn hidden(&self) -> bool {
        match self {
            Tool::Internal => true,
            Tool::Damasco { name, .. } => matches!(
                name.as_str(),
                "ask" | "show_in_panel" | "delete_page" | "delete_component"
            ),
            _ => false,
        }
    }
}

/// The tokens of a turn, and its price when the CLI tells it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    /// Every token read by the model, the cached ones included.
    pub input: u64,
    /// Of the input, the tokens read from the cache.
    pub cached: u64,
    /// Written by the model, its reasoning included.
    pub output: u64,
    /// In US dollars, as the CLI computes it from the API's prices.
    pub cost: Option<f64>,
}

impl Usage {
    pub fn is_empty(&self) -> bool {
        self.input == 0 && self.output == 0
    }
}

/// A model the assistant's CLI can use, by the name the CLI takes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub detail: String,
    /// The CLI uses it when none is chosen.
    pub default: bool,
}

/// A line of what was said in the terminal, as the chat shows it.
#[derive(Debug, Clone, PartialEq)]
pub enum Said {
    Person(String),
    Answer(String),
    Tool(Tool),
}

/// A person's decision on a request of the assistant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny {
        /// Told to the assistant.
        message: String,
        /// The turn stops too.
        interrupt: bool,
    },
}
