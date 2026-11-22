//! Product rules for the agent: instructions and settings of each
//! assistant's CLI that the agent can read but never change.
//!
//! Each CLI reads managed files at the highest precedence, above the user's
//! and the project's: Claude Code in `/etc/claude-code`
//! (`managed-settings.json`, `managed-mcp.json`, `CLAUDE.md`), Codex in
//! `/etc/codex` (`requirements.toml`, `managed_config.toml`). damasco writes
//! them into a directory of its own and mounts the assistant's one
//! read-only in the agent's sandbox, so the agent cannot loosen them, not
//! even by editing the site's `.claude/` or `.codex/`. The rules themselves
//! are one text for every assistant.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::assistant::{Provider, codex};

const RULES: &str = include_str!("../policy/rules.md");
const MANAGED_SETTINGS: &str = include_str!("../policy/managed-settings.json");
const MANAGED_MCP: &str = include_str!("../policy/managed-mcp.json");

/// What only Claude Code is told, after the rules.
const CLAUDE_RULES: &str = "\n## Plan mode\n\n- In plan mode, the plan ends with the ExitPlanMode tool, never as a plain reply: the person then goes ahead with it, or keeps planning, from the chat.\n\n## The terminal\n\n- A few things work only in the terminal of the chat, where Claude Code runs interactively with the same account and site: slash commands such as a sign-in (`/design-login`), and anything that needs the interactive CLI. When a request needs one, the reply says what to type there and shows the `show_in_panel` tool of damasco with section `terminal`, a button that opens it. Afterwards the person comes back to the chat with the button at the top.\n";

/// Folders of Claude's home loaded by every session, kept empty.
const CLAUDE_LOCKED_DIRS: &[&str] = &[
    ".claude/agents",
    ".claude/commands",
    ".claude/skills",
    ".claude/output-styles",
    ".claude/plugins",
];

/// Path of Claude Code's managed directory inside the sandbox.
pub const IN_SANDBOX: &str = "/etc/claude-code";

/// The directory of `provider`'s managed files.
pub fn managed(dir: &Path, provider: Provider) -> PathBuf {
    dir.join(provider.id())
}

/// Files mounted read-only over the agent's home for `provider`.
pub fn locked(dir: &Path, provider: Provider) -> Vec<(PathBuf, &'static str)> {
    match provider {
        // What a session would load in every later session of every site:
        // instructions, agents, commands, skills, output styles and plugins.
        // Empty and read-only, so that no site can leave them for another.
        Provider::Claude => {
            let home = dir.join("claude-home");
            let mut locked = vec![(home.join("empty"), ".claude/CLAUDE.md")];
            for path in CLAUDE_LOCKED_DIRS {
                locked.push((home.join("empty-dir"), *path));
            }
            locked
        }
        Provider::Codex => {
            let home = dir.join("codex-home");
            let mut locked = vec![
                (home.join("config.toml"), ".codex/config.toml"),
                (home.join("rules"), ".codex/rules"),
            ];
            for path in codex::LOCKED_IN_HOME {
                locked.push((home.join("empty"), *path));
            }
            locked
        }
    }
}

/// Directories mounted read-only (empty) over the site for `provider`, by
/// their path in the site: the project configuration a trusted folder would
/// give the CLI.
pub fn locked_in_site(dir: &Path, provider: Provider) -> Vec<(PathBuf, &'static str)> {
    match provider {
        Provider::Claude => Vec::new(),
        Provider::Codex => vec![(dir.join("codex-site"), ".codex")],
    }
}

/// The CLI's own tools that reach the claude.ai account, refused with the
/// connectors.
const ACCOUNT_TOOLS: &[&str] = &["Artifact", "ArtifactComments", "ArtifactData", "DesignSync"];

/// Claude's managed settings. With `connectors`, the connectors of the
/// account (the MCP servers added in claude.ai) are offered to the CLI; a
/// hook of the settings asks a person before each is first used.
fn managed_settings(connectors: bool) -> String {
    let mut settings: serde_json::Value =
        serde_json::from_str(MANAGED_SETTINGS).expect("managed-settings.json is JSON");
    settings["allowAllClaudeAiMcps"] = connectors.into();
    if !connectors {
        settings["permissions"]["deny"] = ACCOUNT_TOOLS.into();
    }
    serde_json::to_string_pretty(&settings).expect("JSON serializes")
}

/// Writes the policy files of every assistant into `dir`, read-only.
/// `connectors`: whether Claude may use the connectors of its account.
pub fn write(dir: &Path, connectors: bool) -> std::io::Result<()> {
    std::fs::create_dir_all(dir.join("codex-site"))?;
    // Before Codex, Claude's files were at the top of the directory.
    for old in ["CLAUDE.md", "managed-settings.json", "managed-mcp.json"] {
        let path = dir.join(old);
        if path.is_file() {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
            std::fs::remove_file(&path)?;
        }
    }
    let [requirements, managed_config] = codex::managed_files(RULES);
    let claude_rules = format!("{RULES}{CLAUDE_RULES}");
    let claude_settings = managed_settings(connectors);
    let empty_dir = dir.join("claude-home/empty-dir");
    std::fs::create_dir_all(&empty_dir)?;
    std::fs::set_permissions(&empty_dir, std::fs::Permissions::from_mode(0o555))?;
    let files: [(PathBuf, &str); 9] = [
        (
            managed(dir, Provider::Claude).join("CLAUDE.md"),
            &claude_rules,
        ),
        (
            managed(dir, Provider::Claude).join("managed-settings.json"),
            &claude_settings,
        ),
        (
            managed(dir, Provider::Claude).join("managed-mcp.json"),
            MANAGED_MCP,
        ),
        (
            managed(dir, Provider::Codex).join(requirements.0),
            &requirements.1,
        ),
        (
            managed(dir, Provider::Codex).join(managed_config.0),
            &managed_config.1,
        ),
        (dir.join("codex-home/config.toml"), codex::HOME_CONFIG),
        (dir.join("codex-home/empty"), ""),
        (dir.join("claude-home/empty"), ""),
        (dir.join("codex-home/rules/.damasco"), ""),
    ];
    for (path, content) in files {
        std::fs::create_dir_all(path.parent().expect("files are in a directory"))?;
        if path.exists() {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
        }
        std::fs::write(&path, content)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_settings_are_valid_json_and_strict() {
        let settings: serde_json::Value = serde_json::from_str(MANAGED_SETTINGS).unwrap();
        assert_eq!(
            settings["permissions"]["disableBypassPermissionsMode"],
            "disable"
        );
        assert_eq!(settings["allowManagedPermissionRulesOnly"], true);
        assert_eq!(settings["allowManagedHooksOnly"], true);
    }

    #[test]
    fn connectors_are_offered_only_when_allowed_and_asked_for_by_a_hook() {
        let dir = tempfile::tempdir().unwrap();
        let settings = |connectors| {
            write(dir.path(), connectors).unwrap();
            let path = managed(dir.path(), Provider::Claude).join("managed-settings.json");
            serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(path).unwrap())
                .unwrap()
        };
        let on = settings(true);
        assert_eq!(on["allowAllClaudeAiMcps"], true);
        assert_eq!(on["allowManagedMcpServersOnly"], true);
        let hook = &on["hooks"]["PreToolUse"][0];
        assert!(
            hook["matcher"]
                .as_str()
                .unwrap()
                .starts_with("mcp__claude_ai_.*|Artifact|")
        );
        assert_eq!(hook["hooks"][0]["command"], "/opt/damasco/damasco hook");
        assert!(on["permissions"]["deny"].is_null());
        let off = settings(false);
        assert_eq!(off["allowAllClaudeAiMcps"], false);
        assert_eq!(off["permissions"]["deny"][0], "Artifact");
    }

    #[test]
    fn writes_read_only_files_and_can_rewrite_them() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), true).unwrap();
        write(dir.path(), true).unwrap();
        let file = dir.path().join("claude/CLAUDE.md");
        assert!(
            std::fs::read_to_string(&file)
                .unwrap()
                .starts_with("# Rules of damasco")
        );
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o444
        );
        let requirements =
            std::fs::read_to_string(dir.path().join("codex/requirements.toml")).unwrap();
        assert!(requirements.contains("# Rules of damasco"));
        assert!(!requirements.contains("ExitPlanMode"));
        assert!(
            std::fs::read_to_string(&file)
                .unwrap()
                .contains("ExitPlanMode")
        );
        for provider in Provider::ALL {
            for (source, _) in locked(dir.path(), provider) {
                assert!(source.exists(), "{}", source.display());
            }
        }
        let claude: Vec<&str> = locked(dir.path(), Provider::Claude)
            .iter()
            .map(|(_, path)| *path)
            .collect();
        for path in [
            ".claude/CLAUDE.md",
            ".claude/agents",
            ".claude/skills",
            ".claude/plugins",
        ] {
            assert!(claude.contains(&path), "{path}");
        }
        assert!(dir.path().join("claude-home/empty-dir").is_dir());
    }

    #[test]
    fn the_rules_name_no_single_assistant() {
        assert!(!RULES.contains("Anthropic"));
        assert!(!RULES.contains("/site/CLAUDE.md"));
    }
}
