//! The site repository: an Astro project in `data_dir/site`, created from
//! the template embedded in the binary and versioned with git.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rust_embed::RustEmbed;

use crate::git;

/// `site-template/` of the axioma repository (MIT-0).
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
            "src/content.config.ts",
        ] {
            assert!(site.path.join(file).is_file(), "{file}");
        }
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(site.path.join("axioma.json")).unwrap())
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
}
