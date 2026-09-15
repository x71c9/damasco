//! Product rules for the agent: instructions and Claude Code settings that
//! the agent can read but never change.
//!
//! Claude Code reads `/etc/claude-code/managed-settings.json` and
//! `/etc/claude-code/CLAUDE.md` at the highest precedence, above the user's
//! and the project's files. axioma writes both into a directory of its own
//! and mounts it read-only at that path in the agent's sandbox, so the agent
//! cannot loosen them, not even by editing the site's `.claude/settings.json`.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

const CLAUDE_MD: &str = include_str!("../policy/CLAUDE.md");
const MANAGED_SETTINGS: &str = include_str!("../policy/managed-settings.json");

/// Path of the directory inside the sandbox.
pub const IN_SANDBOX: &str = "/etc/claude-code";

/// Writes the policy files into `dir`, read-only.
pub fn write(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for (name, content) in [
        ("CLAUDE.md", CLAUDE_MD),
        ("managed-settings.json", MANAGED_SETTINGS),
    ] {
        let path = dir.join(name);
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
    fn writes_read_only_files_and_can_rewrite_them() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path()).unwrap();
        write(dir.path()).unwrap();
        let file = dir.path().join("CLAUDE.md");
        assert!(
            std::fs::read_to_string(&file)
                .unwrap()
                .starts_with("# Rules of axioma")
        );
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o444
        );
    }
}
