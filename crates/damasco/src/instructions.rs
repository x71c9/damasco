//! The owner's instructions for the assistant: those for every site, in
//! `data_dir/instructions.md`, and those for one, in the site's
//! `agent/instructions.md`. They are outside the repository: going back
//! leaves them, and they never go online. The agent gets the two as one
//! read-only file, where each CLI reads the user's own instructions
//! (`~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`): both read it at every
//! start, resumed conversations included, and take a changed copy in place
//! of the earlier one.

use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Characters each of the two texts holds at most.
pub const LIMIT: usize = 20_000;

const FILE: &str = "instructions.md";
/// What the agent reads, in the site's state.
const COMBINED: &str = "instructions.combined.md";

/// One write at a time: each checks what it replaces.
static WRITING: Mutex<()> = Mutex::new(());

#[derive(Debug, PartialEq, Eq)]
pub enum SaveError {
    TooLong,
    /// They are no longer as the person read them.
    Changed,
    Io(String),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::TooLong => write!(f, "instructions hold up to {LIMIT} characters"),
            SaveError::Changed => write!(f, "the instructions were changed elsewhere meanwhile"),
            SaveError::Io(problem) => write!(f, "{problem}"),
        }
    }
}

/// The instructions for every site.
pub fn general(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE)
}

/// The instructions for the site whose state of the assistants is `state`.
pub fn of_site(state: &Path) -> PathBuf {
    state.join(FILE)
}

/// The text at `path`; empty when there is none.
pub fn read(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(format!("{}: {err}", path.display())),
    }
}

/// Saves `text` at `path` in place of `was`, which the person read there.
pub fn save(path: &Path, text: &str, was: &str) -> Result<String, SaveError> {
    let text = text.replace("\r\n", "\n").replace('\0', "");
    let text = text.trim();
    if text.chars().count() > LIMIT {
        return Err(SaveError::TooLong);
    }
    let _one_at_a_time = WRITING.lock().unwrap_or_else(|err| err.into_inner());
    if read(path).map_err(SaveError::Io)? != was {
        return Err(SaveError::Changed);
    }
    replace(path, text, 0o600).map_err(SaveError::Io)?;
    Ok(text.to_string())
}

/// Writes what the agent of a site reads and returns its path: empty
/// without instructions, so that the CLI takes earlier ones away.
pub fn combined(general: &Path, state: &Path) -> Result<PathBuf, String> {
    let text = combine(&read(general)?, &read(&of_site(state))?);
    let path = state.join(COMBINED);
    if std::fs::read_to_string(&path).is_ok_and(|old| old == text) {
        return Ok(path);
    }
    replace(&path, &text, 0o444)?;
    Ok(path)
}

fn combine(general: &str, site: &str) -> String {
    let (general, site) = (general.trim(), site.trim());
    if general.is_empty() && site.is_empty() {
        return String::new();
    }
    let mut text = String::from(
        "# Instructions of the owner\n\nThe owner of this installation wrote them in the panel, under Settings › Instructions. They are followed without being repeated in the chat. damasco's rules come first; these come before the site's notes in /site/AGENTS.md.",
    );
    if !general.is_empty() && !site.is_empty() {
        text.push_str(" Where the two parts differ, those for this site win.");
    }
    for (title, part) in [("For every site", general), ("For this site", site)] {
        if !part.is_empty() {
            text.push_str(&format!("\n\n## {title}\n\n{part}"));
        }
    }
    text.push('\n');
    text
}

/// Puts `text` at `path` through a rename, so it is never read half
/// written, and a sandbox already running keeps the copy it started with.
fn replace(path: &Path, text: &str, mode: u32) -> Result<(), String> {
    let failed = |err: std::io::Error| format!("{}: {err}", path.display());
    let dir = path.parent().ok_or("instructions are in a folder")?;
    std::os::unix::fs::DirBuilderExt::mode(std::fs::DirBuilder::new().recursive(true), 0o700)
        .create(dir)
        .map_err(failed)?;
    let temporary = dir.join(format!(".instructions-{}.new", crate::site::ulid()));
    std::fs::write(&temporary, text)
        .and_then(|()| std::fs::set_permissions(&temporary, PermissionsExt::from_mode(mode)))
        .and_then(|()| std::fs::rename(&temporary, path))
        .map_err(|err| {
            let _ = std::fs::remove_file(&temporary);
            failed(err)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_only_in_place_of_what_was_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = of_site(&dir.path().join("agent"));
        assert_eq!(read(&path).unwrap(), "");
        assert_eq!(save(&path, "  One\r\nTwo\n", "").unwrap(), "One\nTwo");
        assert_eq!(save(&path, "Three", ""), Err(SaveError::Changed));
        assert_eq!(read(&path).unwrap(), "One\nTwo");
        let long = "è".repeat(LIMIT + 1);
        assert_eq!(save(&path, &long, "One\nTwo"), Err(SaveError::TooLong));
        let most = "è".repeat(LIMIT);
        assert_eq!(save(&path, &most, "One\nTwo").unwrap(), most);
    }

    #[test]
    fn the_agent_reads_both_as_one() {
        let dir = tempfile::tempdir().unwrap();
        let (all, state) = (general(dir.path()), dir.path().join("sites/A/agent"));
        let reading = || read(&combined(&all, &state).unwrap()).unwrap();
        assert_eq!(reading(), "");

        save(&all, "Texts in Italian.", "").unwrap();
        let only_general = reading();
        assert!(only_general.contains("## For every site\n\nTexts in Italian."));
        assert!(!only_general.contains("For this site") && !only_general.contains("differ"));

        save(&of_site(&state), "Texts in English.", "").unwrap();
        let both = reading();
        assert!(both.find("Texts in Italian.") < both.find("Texts in English."));
        assert!(both.contains("those for this site win"));

        // Another site hears only those for every site.
        let other = combined(&all, &dir.path().join("sites/B/agent")).unwrap();
        assert!(!read(&other).unwrap().contains("English"));

        save(&all, "", "Texts in Italian.").unwrap();
        save(&of_site(&state), " ", "Texts in English.").unwrap();
        assert_eq!(reading(), "");
    }
}
