//! The site repository: an Astro project in `data_dir/site`, created from
//! the template embedded in the binary and versioned with git.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rust_embed::RustEmbed;

use crate::git;

/// `site-template/` of the alogeno repository (MIT-0).
#[derive(RustEmbed)]
#[folder = "../../site-template"]
struct Template;

/// A new ULID: 48 bits of milliseconds and 80 random bits, in Crockford's
/// base 32. Ordered by creation time, 26 characters.
pub fn ulid() -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
        & ((1 << 48) - 1);
    let mut random = [0u8; 10];
    getrandom::fill(&mut random).expect("the operating system random source");
    let mut value: u128 = u128::from(millis) << 80;
    for (index, byte) in random.iter().enumerate() {
        value |= u128::from(*byte) << (8 * (9 - index));
    }
    (0..26)
        .rev()
        .map(|i| ALPHABET[((value >> (5 * i)) & 0x1f) as usize] as char)
        .collect()
}

/// The site repository.
pub struct Site {
    pub path: PathBuf,
}

impl Site {
    pub fn at(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join("site"),
        }
    }

    pub fn exists(&self) -> bool {
        self.path.join(".git").is_dir()
    }

    /// Creates the repository from the template, with the site's name and
    /// language and a home page, and records it in a first commit.
    pub fn create(&self, name: &str, lang: &str) -> Result<(), String> {
        if self.exists() {
            return Ok(());
        }
        let home = ulid();
        let fill = |text: &str| {
            text.replace("{{home}}", &home)
                .replace("{{name}}", &json_text(name))
                .replace("{{lang}}", &json_text(lang))
        };
        for file in Template::iter() {
            let content = Template::get(&file).expect("listed file");
            let target = self.path.join(fill(&file));
            if let Some(dir) = target.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            let bytes = match std::str::from_utf8(&content.data) {
                Ok(text) => fill(text).into_bytes(),
                Err(_) => content.data.into_owned(),
            };
            std::fs::write(&target, bytes).map_err(|e| format!("{}: {e}", target.display()))?;
        }
        // Pages are always stored in their canonical form.
        let schema = crate::schema::Schema::load(&self.path)
            .map_err(|found| format!("site template: {}", found[0]))?;
        let pages = self.path.join("content/pages");
        for entry in std::fs::read_dir(&pages)
            .map_err(|e| format!("{}: {e}", pages.display()))?
            .flatten()
        {
            let path = entry.path();
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let page: serde_json::Value =
                serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            std::fs::write(&path, crate::schema::canonical(&schema, &page))
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        git::run(&self.path, &["init", "-q", "-b", "master"])?;
        git::run(&self.path, &["add", "-A"])?;
        git::run(&self.path, &["commit", "-q", "-m", "chore: new site"])?;
        Ok(())
    }
}

/// Escapes text for a JSON string literal, without the quotes.
fn json_text(text: &str) -> String {
    let quoted = serde_json::to_string(text).expect("a string serializes");
    quoted[1..quoted.len() - 1].to_string()
}

/// What `CLAUDE.md` holds once the site's memory is in `AGENTS.md`: Claude
/// Code reads the file it points to, the other assistants read `AGENTS.md`.
const MEMORY_POINTER: &str = "@AGENTS.md\n";

/// Moves the memory of a site made before Codex from `CLAUDE.md` to
/// `AGENTS.md`, which every assistant reads, and leaves in `CLAUDE.md` the
/// line that points there. Safe to run again; true when it changed files.
pub fn share_memory(site: &Path) -> std::io::Result<bool> {
    let claude = site.join("CLAUDE.md");
    let agents = site.join("AGENTS.md");
    if agents.exists() || !claude.is_file() {
        return Ok(false);
    }
    let memory = std::fs::read_to_string(&claude)?;
    if memory.trim() == MEMORY_POINTER.trim() {
        return Ok(false);
    }
    std::fs::write(&agents, memory)?;
    std::fs::write(&claude, MEMORY_POINTER)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulids_are_26_characters_and_ordered() {
        let a = ulid();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = ulid();
        assert_eq!(a.len(), 26);
        assert!(a < b, "{a} < {b}");
        assert!(
            a.chars()
                .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
        );
    }

    #[test]
    fn creates_the_repository_from_the_template() {
        let dir = tempfile::tempdir().unwrap();
        let site = Site::at(dir.path());
        site.create("Studio \"Corte\"", "it").unwrap();
        assert!(site.exists());
        for file in [
            ".gitignore",
            "package.json",
            "package-lock.json",
            "astro.config.mjs",
            "LICENSE",
            "CLAUDE.md",
            "AGENTS.md",
            "src/content.config.ts",
        ] {
            assert!(site.path.join(file).is_file(), "{file}");
        }
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(site.path.join("alogeno.json")).unwrap())
                .unwrap();
        assert_eq!(settings["name"], "Studio \"Corte\"");
        let pages: Vec<_> = std::fs::read_dir(site.path.join("content/pages"))
            .unwrap()
            .collect();
        assert_eq!(pages.len(), 1);
        let home_file = pages[0].as_ref().unwrap().path();
        let home: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&home_file).unwrap()).unwrap();
        assert_eq!(home["id"].as_str().unwrap().len(), 26);
        assert_eq!(home_file.file_stem().unwrap().to_str(), home["id"].as_str());
        assert_eq!(home["inside"], serde_json::Value::Null);
        let log = git::run(&site.path, &["log", "--format=%s"]).unwrap();
        assert_eq!(log.trim(), "chore: new site");
        // A second call leaves the repository alone.
        site.create("Other", "en").unwrap();
        assert_eq!(
            git::run(&site.path, &["rev-list", "--count", "HEAD"])
                .unwrap()
                .trim(),
            "1"
        );
    }

    #[test]
    fn the_memory_moves_to_agents_md_once() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("CLAUDE.md"),
            "# Site memory\n- Blue headings.\n",
        )
        .unwrap();
        assert!(share_memory(dir.path()).unwrap());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap(),
            "# Site memory\n- Blue headings.\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap(),
            MEMORY_POINTER
        );
        assert!(!share_memory(dir.path()).unwrap());
    }
}
