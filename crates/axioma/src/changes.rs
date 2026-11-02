//! Changes not online yet: what differs between the last published version
//! of the site and the current one, item by item, with who made each change
//! and when.
//!
//! There is no draft state in the files: a change is pending until the
//! commit that holds it is published. The published version is the ref
//! `refs/axioma/published`; before the first publication everything is new.

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

/// Where axioma keeps the version that is online.
pub const PUBLISHED: &str = "refs/axioma/published";

/// The tree with nothing in it, a git constant.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Created,
    Changed,
    Deleted,
}

/// One page or picture that is not online as it is now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Item {
    pub id: String,
    pub status: Status,
    /// The page's title or the picture's name; for a deleted item, as it was.
    pub title: String,
    /// The person who made the last change, when it says so.
    pub by: Option<i64>,
    /// When the last change was saved, in seconds since 1970.
    pub at: i64,
}

/// A file of code, schema or settings that is not online as it is now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct File {
    pub path: String,
    pub status: Status,
}

/// A saved version that changed code, schema or settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Version {
    pub commit: String,
    /// The first line of its message: the request, or what was done.
    pub subject: String,
    pub by: Option<i64>,
    pub at: i64,
}

/// Everything not online yet.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Pending {
    /// The version that is online, if the site was ever published.
    pub published: Option<String>,
    /// The last saved version: publishing confirms against it.
    pub head: Option<String>,
    pub pages: Vec<Item>,
    pub media: Vec<Item>,
    /// Code, schema and settings go online together.
    pub files: Vec<File>,
    pub versions: Vec<Version>,
}

impl Pending {
    /// How many changes the panel counts: each page, each picture, and the
    /// code as one.
    pub fn count(&self) -> usize {
        self.pages.len() + self.media.len() + usize::from(!self.files.is_empty())
    }
}

/// The version that is online, if any.
pub fn published(site: &Path) -> Option<String> {
    crate::git::run(
        site,
        &["rev-parse", "--verify", "-q", &format!("{PUBLISHED}^{{commit}}")],
    )
    .ok()
    .map(|commit| commit.trim().to_string())
}

/// Records `commit` as the version that is online.
pub fn mark_published(site: &Path, commit: &str) -> Result<(), String> {
    crate::git::run(site, &["update-ref", PUBLISHED, commit])?;
    Ok(())
}

enum Kind {
    Page(String),
    Media(String),
    Code,
}

fn kind_of(path: &str) -> Kind {
    let item = |dir: &str| {
        path.strip_prefix(dir)
            .and_then(|rest| rest.strip_suffix(".json"))
            .filter(|id| !id.is_empty() && !id.contains('/'))
            .map(str::to_string)
    };
    if let Some(id) = item("content/pages/") {
        Kind::Page(id)
    } else if let Some(id) = item("content/media/") {
        Kind::Media(id)
    } else {
        Kind::Code
    }
}

/// The person named in a commit message: "…person 3." as axioma writes it
/// for the chat and the panel.
fn person_of(message: &str) -> Option<i64> {
    let at = message.rfind("person ")?;
    let digits: String = message[at + 7..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

struct Commit {
    hash: String,
    at: i64,
    message: String,
    files: Vec<String>,
}

/// The commits after the published version, newest first, with the files
/// each one changed.
fn history(site: &Path, published: Option<&str>) -> Result<Vec<Commit>, String> {
    let range = match published {
        Some(commit) => format!("{commit}..HEAD"),
        None => "HEAD".to_string(),
    };
    let out = crate::git::run(
        site,
        &[
            "-c",
            "core.quotePath=false",
            "log",
            "--no-renames",
            "--name-only",
            "--format=%x1e%H%x1f%ct%x1f%B%x1f",
            &range,
        ],
    )?;
    let mut commits = Vec::new();
    for record in out.split('\u{1e}').skip(1) {
        let mut parts = record.splitn(4, '\u{1f}');
        let (Some(hash), Some(at), Some(message), Some(files)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        commits.push(Commit {
            hash: hash.to_string(),
            at: at.trim().parse().unwrap_or(0),
            message: message.trim().to_string(),
            files: files
                .lines()
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect(),
        });
    }
    Ok(commits)
}

/// The changes between the published version and the last saved one.
pub fn pending(site: &Path) -> Result<Pending, String> {
    if crate::git::run(site, &["rev-parse", "--verify", "-q", "HEAD"]).is_err() {
        return Ok(Pending::default());
    }
    let published = published(site);
    let base = published.as_deref().unwrap_or(EMPTY_TREE);
    let diff = crate::git::run(
        site,
        &["diff", "--name-status", "--no-renames", "-z", base, "HEAD"],
    )?;
    let mut fields = diff.split('\0').filter(|field| !field.is_empty());
    let mut changed = Vec::new();
    while let (Some(code), Some(path)) = (fields.next(), fields.next()) {
        let status = match code {
            "A" => Status::Created,
            "D" => Status::Deleted,
            _ => Status::Changed,
        };
        changed.push((path.to_string(), status));
    }

    let commits = history(site, published.as_deref())?;
    // The newest commit that touched each file.
    let mut last: HashMap<&str, &Commit> = HashMap::new();
    for commit in &commits {
        for file in &commit.files {
            last.entry(file.as_str()).or_insert(commit);
        }
    }

    // Titles, read from the version that has the item: the current one, or
    // the published one for a deleted item.
    let specs: Vec<String> = changed
        .iter()
        .map(|(path, status)| match status {
            Status::Deleted => format!("{base}:{path}"),
            _ => format!("HEAD:{path}"),
        })
        .collect();
    let wanted: Vec<bool> = changed
        .iter()
        .map(|(path, _)| !matches!(kind_of(path), Kind::Code))
        .collect();
    let contents = crate::git::show_many(
        site,
        &specs
            .iter()
            .zip(&wanted)
            .filter(|(_, wanted)| **wanted)
            .map(|(spec, _)| spec.clone())
            .collect::<Vec<_>>(),
    )?;
    let mut contents = contents.into_iter();

    let mut result = Pending {
        published: published.clone(),
        head: crate::git::head(site).ok(),
        ..Pending::default()
    };
    for (path, status) in changed {
        let commit = last.get(path.as_str());
        let by = commit.and_then(|commit| person_of(&commit.message));
        let at = commit.map_or(0, |commit| commit.at);
        let kind = kind_of(&path);
        let title_of = |key: &str, contents: Option<Vec<u8>>| {
            contents
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .and_then(|value| value[key].as_str().map(str::to_string))
                .unwrap_or_default()
        };
        match kind {
            Kind::Page(id) => {
                let title = title_of("title", contents.next().flatten());
                result.pages.push(Item { id, status, title, by, at });
            }
            Kind::Media(id) => {
                let title = title_of("name", contents.next().flatten());
                result.media.push(Item { id, status, title, by, at });
            }
            Kind::Code => result.files.push(File { path, status }),
        }
    }
    // Newest first.
    result.pages.sort_by(|a, b| b.at.cmp(&a.at));
    result.media.sort_by(|a, b| b.at.cmp(&a.at));

    // The versions that changed code, among those not online.
    let code: std::collections::HashSet<&str> =
        result.files.iter().map(|file| file.path.as_str()).collect();
    result.versions = commits
        .iter()
        .filter(|commit| commit.files.iter().any(|file| code.contains(file.as_str())))
        .map(|commit| Version {
            commit: commit.hash.clone(),
            subject: commit.message.lines().next().unwrap_or_default().to_string(),
            by: person_of(&commit.message),
            at: commit.at,
        })
        .collect();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{commit_all, head};
    use serde_json::json;

    fn page(site: &Path, id: &str, title: &str) {
        let value = json!({ "id": id, "title": title });
        std::fs::write(
            site.join(format!("content/pages/{id}.json")),
            value.to_string(),
        )
        .unwrap();
    }

    #[test]
    fn people_are_read_from_the_messages() {
        assert_eq!(person_of("Edited\n\nIn the panel: person 3."), Some(3));
        assert_eq!(
            person_of("Make it blue\n\nAsked in the chat: conversation 2, person 14."),
            Some(14)
        );
        assert_eq!(person_of("chore: new site"), None);
    }

    #[test]
    fn everything_is_pending_until_published_then_only_what_changed() {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path();
        crate::git::run(site, &["init", "-q", "-b", "master"]).unwrap();
        assert_eq!(pending(site).unwrap(), Pending::default());

        std::fs::create_dir_all(site.join("content/pages")).unwrap();
        std::fs::create_dir_all(site.join("content/media")).unwrap();
        std::fs::create_dir_all(site.join("src")).unwrap();
        page(site, "HOME", "Home");
        page(site, "LAKE", "Lake");
        std::fs::write(site.join("src/layout.astro"), "one").unwrap();
        commit_all(site, "chore: new site").unwrap();

        // Never published: everything is new, the code counts once.
        let first = pending(site).unwrap();
        assert_eq!(first.published, None);
        assert_eq!(first.pages.len(), 2);
        assert!(first.pages.iter().all(|item| item.status == Status::Created));
        assert_eq!(first.files.len(), 1);
        assert_eq!(first.count(), 3);
        assert_eq!(first.versions[0].subject, "chore: new site");
        assert_eq!(first.versions[0].by, None);

        mark_published(site, &head(site).unwrap()).unwrap();
        assert_eq!(published(site), Some(head(site).unwrap()));
        assert_eq!(pending(site).unwrap().count(), 0);

        // A page edited in the panel, one deleted and a picture added in the
        // chat, the code changed in the chat.
        page(site, "HOME", "Welcome");
        commit_all(site, "Edited the page “Welcome”\n\nIn the panel: person 2.").unwrap();
        std::fs::remove_file(site.join("content/pages/LAKE.json")).unwrap();
        std::fs::write(
            site.join("content/media/PIC.json"),
            json!({ "id": "PIC", "name": "lake.png" }).to_string(),
        )
        .unwrap();
        std::fs::write(site.join("src/layout.astro"), "two").unwrap();
        commit_all(
            site,
            "Make the background black\n\nAsked in the chat: conversation 1, person 1.",
        )
        .unwrap();

        let now = pending(site).unwrap();
        assert_eq!(now.count(), 4);
        let home = now.pages.iter().find(|item| item.id == "HOME").unwrap();
        assert_eq!((home.status, home.title.as_str(), home.by), (Status::Changed, "Welcome", Some(2)));
        let lake = now.pages.iter().find(|item| item.id == "LAKE").unwrap();
        // A deleted page keeps the title it has online.
        assert_eq!((lake.status, lake.title.as_str(), lake.by), (Status::Deleted, "Lake", Some(1)));
        assert_eq!(now.media[0].title, "lake.png");
        assert_eq!(now.media[0].status, Status::Created);
        assert_eq!(
            now.files,
            [File { path: "src/layout.astro".into(), status: Status::Changed }]
        );
        assert_eq!(now.versions.len(), 1);
        assert_eq!(now.versions[0].subject, "Make the background black");

        // Changed back as it is online: nothing left to publish for it.
        page(site, "HOME", "Home");
        commit_all(site, "Edited the page “Home”\n\nIn the panel: person 2.").unwrap();
        assert!(pending(site).unwrap().pages.iter().all(|item| item.id != "HOME"));
    }
}
