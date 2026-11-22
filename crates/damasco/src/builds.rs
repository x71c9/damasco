//! The builds of a site, each kept as a git ref named after it that points
//! to the saved version it was built from: `refs/damasco/builds/<id>`, or
//! `refs/damasco/failed-builds/<id>` when it failed. The releases on disk
//! are few and go; the refs stay with the history of the site, so the site
//! can go back to any of its builds.

use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;

use crate::{build, git, schema};

const BUILDS: &str = "refs/damasco/builds";
const FAILED: &str = "refs/damasco/failed-builds";
/// How the subject of a going back to a build starts.
const WENT_BACK: &str = "Went back to: ";

/// Where the last saved change of a build was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Assistant,
    Panel,
    Other,
}

/// One build, as the panel lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Build {
    pub id: String,
    /// When it ended, in seconds since 1970.
    pub at: u64,
    /// The last change saved before it.
    pub title: String,
    pub origin: Origin,
    pub failed: bool,
    /// The preview shows it.
    pub in_preview: bool,
    pub online: bool,
    /// The site's files are as this build has them: nothing to go back to.
    pub current: bool,
}

/// A page of the history, newest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct History {
    pub builds: Vec<Build>,
    /// There are older ones.
    pub more: bool,
}

fn reference(id: &str, failed: bool) -> String {
    format!("{}/{id}", if failed { FAILED } else { BUILDS })
}

fn keep(site: &Path, id: &str, commit: &str, failed: bool) -> Result<(), String> {
    if !schema::is_ulid(id) {
        return Err(format!("{id:?} is not the id of a build"));
    }
    git::run(site, &["update-ref", &reference(id, failed), commit]).map(|_| ())
}

/// Keeps a release in the history, when it was built from a saved version
/// with nothing else in the files.
pub fn record_release(site: &Path, releases: &Path, release: &str) -> Result<(), String> {
    match build::manifest(releases, release) {
        Some(manifest) if manifest.clean && !manifest.commit.is_empty() => {
            keep(site, release, &manifest.commit, false)
        }
        _ => Ok(()),
    }
}

/// Keeps a build of the saved version `commit` that failed.
pub fn record_failure(site: &Path, commit: &str) -> Result<(), String> {
    keep(site, &crate::site::ulid(), commit, true)
}

/// Starts the history of a site that has none yet (made before damasco
/// kept it) with what is known: the releases still on disk and the
/// publications, as release and saved version.
pub fn adopt(site: &Path, releases: &Path, publications: &[(String, String)]) {
    let none = git::run(
        site,
        &["for-each-ref", "--count=1", "--format=%(refname)", BUILDS],
    )
    .is_ok_and(|found| found.trim().is_empty());
    if !none {
        return;
    }
    for (release, commit) in publications {
        // A version no longer in the repository has nothing to go back to.
        let _ = keep(site, release, commit, false);
    }
    for entry in std::fs::read_dir(releases).into_iter().flatten().flatten() {
        let id = entry.file_name().to_string_lossy().into_owned();
        if schema::is_ulid(&id) {
            let _ = record_release(site, releases, &id);
        }
    }
}

/// The newest `limit` builds.
pub fn list(site: &Path, releases: &Path, limit: usize) -> Result<History, String> {
    if !site.join(".git").is_dir() {
        return Ok(History::default());
    }
    let head = git::run(site, &["rev-parse", "--verify", "-q", "HEAD^{tree}"]).unwrap_or_default();
    // NUL between the parts: it is the one byte a message cannot hold.
    let out = git::run(
        site,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(tree)%00%(contents)%00",
            BUILDS,
            FAILED,
        ],
    )?;
    let parts: Vec<&str> = out.split('\0').collect();
    let mut found: Vec<(&str, bool, &str, &str)> = parts
        .as_chunks::<3>()
        .0
        .iter()
        .filter_map(|[name, tree, message]| {
            let (kind, id) = name.trim_start().rsplit_once('/')?;
            schema::is_ulid(id).then_some((id, kind == FAILED, *tree, *message))
        })
        .collect();
    // The ids are in the order of time.
    found.sort_unstable_by(|a, b| b.0.cmp(a.0));
    let more = found.len() > limit;
    let preview = build::current_release(releases);
    let live = build::pointed(releases, "live");
    let builds = found
        .into_iter()
        .take(limit)
        .map(|(id, failed, tree, message)| Build {
            id: id.to_string(),
            at: crate::site::ulid_millis(id) / 1000,
            title: title(message),
            origin: origin(message),
            failed,
            in_preview: preview.as_deref() == Some(id),
            online: live.as_deref() == Some(id),
            current: tree == head.trim(),
        })
        .collect();
    Ok(History { builds, more })
}

/// The saved version of the build `id` and what it is called.
pub fn find(site: &Path, id: &str) -> Option<(String, String)> {
    if !schema::is_ulid(id) {
        return None;
    }
    let commit = [false, true].into_iter().find_map(|failed| {
        let name = format!("{}^{{commit}}", reference(id, failed));
        git::run(site, &["rev-parse", "--verify", "-q", &name]).ok()
    })?;
    let commit = commit.trim().to_string();
    let message = git::run(site, &["log", "-1", "--format=%B", &commit]).ok()?;
    Some((commit, title(&message)))
}

/// The subject of the version that takes the site back to a build.
pub fn went_back(title: &str) -> String {
    // Going back to a going back names where both lead.
    format!("{WENT_BACK}{}", title.trim_start_matches(WENT_BACK))
}

fn title(message: &str) -> String {
    match message.lines().next().unwrap_or_default().trim() {
        "" => "Saved version".into(),
        "chore: new site" => "New site".into(),
        subject => subject.into(),
    }
}

/// Read from the last lines damasco writes in each message.
fn origin(message: &str) -> Origin {
    let body: HashSet<&str> = message
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':').map(|(start, _)| start))
        .collect();
    if body.contains("Asked in the chat") {
        Origin::Assistant
    } else if body.contains("In the panel") {
        Origin::Panel
    } else {
        Origin::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join("site")).unwrap();
            std::fs::create_dir_all(dir.path().join("releases")).unwrap();
            git::run(&dir.path().join("site"), &["init", "-q", "-b", "master"]).unwrap();
            Self { dir }
        }

        fn site(&self) -> std::path::PathBuf {
            self.dir.path().join("site")
        }

        fn releases(&self) -> std::path::PathBuf {
            self.dir.path().join("releases")
        }

        fn save(&self, text: &str, message: &str) -> String {
            std::fs::write(self.site().join("page"), text).unwrap();
            git::commit_all(&self.site(), message).unwrap().unwrap()
        }

        fn release(&self, commit: &str, dirty: bool) -> String {
            // Ids of the same millisecond have no order.
            std::thread::sleep(std::time::Duration::from_millis(2));
            let id = crate::site::ulid();
            let dir = self.releases().join(&id);
            std::fs::create_dir_all(&dir).unwrap();
            let manifest = serde_json::json!({ "commit": commit, "uncommitted_changes": dirty });
            std::fs::write(dir.join(".manifest.json"), manifest.to_string()).unwrap();
            id
        }
    }

    #[test]
    fn builds_are_listed_newest_first_with_what_they_are() {
        let it = Fixture::new();
        let (site, releases) = (it.site(), it.releases());
        assert_eq!(list(&site, &releases, 10).unwrap(), History::default());

        let first = it.save("one", "chore: new site");
        let one = it.release(&first, false);
        record_release(&site, &releases, &one).unwrap();
        let second = it.save(
            "two",
            "Make it blue\n\nAsked in the chat: conversation 1, person 1.",
        );
        let two = it.release(&second, false);
        record_release(&site, &releases, &two).unwrap();
        // Files that were not saved are no version to go back to.
        let dirty = it.release(&second, true);
        record_release(&site, &releases, &dirty).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let third = it.save("three", "Edited the page\n\nIn the panel: person 1.");
        record_failure(&site, &third).unwrap();
        build::point(&releases, "preview", &two).unwrap();
        build::point(&releases, "live", &one).unwrap();

        let history = list(&site, &releases, 10).unwrap();
        assert!(!history.more);
        let seen: Vec<_> = history
            .builds
            .iter()
            .map(|b| (b.title.as_str(), b.origin, b.failed, b.current))
            .collect();
        assert_eq!(
            seen,
            [
                ("Edited the page", Origin::Panel, true, true),
                ("Make it blue", Origin::Assistant, false, false),
                ("New site", Origin::Other, false, false),
            ]
        );
        assert!(history.builds[1].in_preview && !history.builds[1].online);
        assert!(history.builds[2].online && !history.builds[2].in_preview);
        assert_eq!(history.builds[2].at, crate::site::ulid_millis(&one) / 1000);

        let page = list(&site, &releases, 2).unwrap();
        assert_eq!((page.builds.len(), page.more), (2, true));

        assert_eq!(find(&site, &one), Some((first, "New site".into())));
        assert_eq!(find(&site, &history.builds[0].id).unwrap().0, third);
        assert_eq!(find(&site, &dirty), None);
        assert_eq!(find(&site, "../../heads/master"), None);
        // The history is the site's own, not changes waiting to go online.
        assert_eq!(git::head(&site).unwrap(), third);
    }

    #[test]
    fn a_site_without_history_starts_from_what_is_known() {
        let it = Fixture::new();
        let (site, releases) = (it.site(), it.releases());
        let first = it.save("one", "First");
        let published = crate::site::ulid();
        let second = it.save("two", "Second");
        let kept = it.release(&second, false);
        let gone = (crate::site::ulid(), "0".repeat(40));
        adopt(
            &site,
            &releases,
            &[(published.clone(), first.clone()), gone.clone()],
        );
        assert_eq!(find(&site, &published).unwrap().0, first);
        assert_eq!(find(&site, &kept).unwrap().0, second);
        assert_eq!(find(&site, &gone.0), None);

        // Once there is a history, nothing is added to it.
        let later = it.release(&second, false);
        adopt(&site, &releases, &[]);
        assert_eq!(find(&site, &later), None);
    }

    #[test]
    fn going_back_again_does_not_pile_up_names() {
        assert_eq!(went_back("Blue"), "Went back to: Blue");
        assert_eq!(went_back(&went_back("Blue")), "Went back to: Blue");
    }
}
