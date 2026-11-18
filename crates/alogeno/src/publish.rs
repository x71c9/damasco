//! Publishing: the release the preview shows goes online as it is, without
//! building again. A release can go online only when it was built from the
//! last saved version of the site, with nothing else in the files, so what
//! goes online is exactly what was seen in the preview and what the list
//! of changes describes.

use std::path::Path;

use rusqlite::{Connection, params};
use serde::Serialize;

use crate::build;
use crate::changes;

/// The release of the preview, ready to go online.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Ready {
    pub release: String,
    /// The saved version it was built from.
    pub commit: String,
    /// When it was built, in seconds since 1970.
    pub built_at: u64,
}

/// What a publication did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Published {
    pub release: String,
    pub commit: String,
    /// The release that was online before, if any.
    pub previous: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PublishError {
    /// The preview does not show the last saved version yet.
    NotReady(String),
    /// The site changed since the person looked at the list.
    Changed,
    /// Everything is online already.
    Nothing,
    Failed(String),
}

/// The release of the preview, when it shows the last saved version; why
/// not, otherwise.
pub fn ready(site: &Path, releases: &Path) -> Result<Ready, String> {
    let head =
        crate::git::head(site).map_err(|_| "the site has no saved version yet".to_string())?;
    let release =
        build::current_release(releases).ok_or("the preview has no version of the site yet")?;
    let manifest: serde_json::Value = std::fs::read(releases.join(&release).join(".manifest.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or("the preview's version cannot be read")?;
    let built_from = manifest["commit"].as_str().unwrap_or_default();
    if built_from != head || manifest["uncommitted_changes"].as_bool() != Some(false) {
        return Err(
            "the preview is not up to date with the last changes yet: it updates in a moment"
                .into(),
        );
    }
    Ok(Ready {
        release,
        commit: head,
        built_at: manifest["built_at_unix"].as_u64().unwrap_or(0),
    })
}

/// Puts the release of the preview online. `expected` is the saved version
/// the person saw the list of changes for: when the site moved on since,
/// nothing happens.
pub fn publish(site: &Path, releases: &Path, expected: &str) -> Result<Published, PublishError> {
    let head = crate::git::head(site).map_err(PublishError::Failed)?;
    if head != expected {
        return Err(PublishError::Changed);
    }
    if changes::pending(site)
        .map_err(PublishError::Failed)?
        .count()
        == 0
    {
        return Err(PublishError::Nothing);
    }
    let ready = ready(site, releases).map_err(PublishError::NotReady)?;
    go_live(site, releases, &ready.release, &ready.commit).map_err(PublishError::Failed)
}

/// Puts `release`, built from `commit`, online: the release that was
/// online is kept as the previous one, and `commit` becomes the published
/// version.
pub fn go_live(
    site: &Path,
    releases: &Path,
    release: &str,
    commit: &str,
) -> Result<Published, String> {
    let previous = build::pointed(releases, "live");
    if let Some(previous) = &previous
        && previous != release
    {
        build::point(releases, "previous", previous).map_err(|err| err.to_string())?;
    }
    build::point(releases, "live", release).map_err(|err| err.to_string())?;
    changes::mark_published(site, commit)?;
    Ok(Published {
        release: release.to_string(),
        commit: commit.to_string(),
        previous,
    })
}

/// Puts the previous publication back online, and with it its version as
/// the published one: what came after it is pending again. Going back
/// again returns to where it was.
pub fn back(site: &Path, releases: &Path) -> Result<Published, String> {
    let previous = build::pointed(releases, "previous")
        .ok_or("there is no earlier publication to go back to")?;
    let commit = commit_of(releases, &previous).ok_or("the earlier publication cannot be read")?;
    go_live(site, releases, &previous, &commit)
}

/// The saved version a release was built from.
pub fn commit_of(releases: &Path, release: &str) -> Option<String> {
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(releases.join(release).join(".manifest.json")).ok()?)
            .ok()?;
    manifest["commit"]
        .as_str()
        .filter(|c| !c.is_empty())
        .map(String::from)
}

/// Keeps the publication in the history.
pub fn record(
    conn: &Connection,
    person: i64,
    published: &Published,
    changes: usize,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO publication (release, commit_id, previous_release, person_id, changes)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            published.release,
            published.commit,
            published.previous,
            person,
            changes as i64
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// The last publication, for the panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Last {
    pub at: String,
    pub by: Option<i64>,
    pub changes: i64,
}

pub fn last(conn: &Connection) -> rusqlite::Result<Option<Last>> {
    let mut statement = conn.prepare(
        "SELECT created_at, person_id, changes FROM publication ORDER BY id DESC LIMIT 1",
    )?;
    let mut rows = statement.query_map([], |row| {
        Ok(Last {
            at: row.get(0)?,
            by: row.get(1)?,
            changes: row.get(2)?,
        })
    })?;
    rows.next().transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{commit_all, head};

    /// A release as the build writes it, made from `commit`.
    fn release(releases: &Path, id: &str, commit: &str, dirty: bool) {
        let dir = releases.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), id).unwrap();
        std::fs::write(
            dir.join(".manifest.json"),
            serde_json::json!({ "id": id, "commit": commit, "uncommitted_changes": dirty, "built_at_unix": 1 })
                .to_string(),
        )
        .unwrap();
        build::point(releases, "preview", id).unwrap();
    }

    #[test]
    fn only_the_preview_of_the_last_version_goes_online() {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().join("site");
        let releases = dir.path().join("releases");
        std::fs::create_dir_all(site.join("content/pages")).unwrap();
        std::fs::create_dir_all(&releases).unwrap();
        crate::git::run(&site, &["init", "-q", "-b", "master"]).unwrap();
        std::fs::write(site.join("content/pages/HOME.json"), r#"{"title":"Home"}"#).unwrap();
        let first = commit_all(&site, "chore: new site").unwrap().unwrap();

        // No build yet, then a build with changes not saved.
        assert!(matches!(
            publish(&site, &releases, &first),
            Err(PublishError::NotReady(_))
        ));
        release(&releases, "R1", &first, true);
        assert!(matches!(
            publish(&site, &releases, &first),
            Err(PublishError::NotReady(_))
        ));

        // The build of the last version goes online.
        release(&releases, "R2", &first, false);
        let published = publish(&site, &releases, &first).unwrap();
        assert_eq!(published.release, "R2");
        assert_eq!(published.previous, None);
        assert_eq!(build::pointed(&releases, "live").as_deref(), Some("R2"));
        assert_eq!(changes::published(&site), Some(first.clone()));
        assert_eq!(
            publish(&site, &releases, &first),
            Err(PublishError::Nothing)
        );

        // A change saved after the list was read: refused.
        std::fs::write(
            site.join("content/pages/HOME.json"),
            r#"{"title":"Welcome"}"#,
        )
        .unwrap();
        let second = commit_all(&site, "Edited\n\nIn the panel: person 1.")
            .unwrap()
            .unwrap();
        assert_eq!(
            publish(&site, &releases, &first),
            Err(PublishError::Changed)
        );
        // The preview still shows the first version: not yet.
        assert!(matches!(
            publish(&site, &releases, &second),
            Err(PublishError::NotReady(_))
        ));
        release(&releases, "R3", &head(&site).unwrap(), false);
        let again = publish(&site, &releases, &second).unwrap();
        assert_eq!(again.previous.as_deref(), Some("R2"));
        assert_eq!(build::pointed(&releases, "previous").as_deref(), Some("R2"));
        assert_eq!(build::pointed(&releases, "live").as_deref(), Some("R3"));

        // Back to the publication before, and forward again.
        let back_once = back(&site, &releases).unwrap();
        assert_eq!(back_once.release, "R2");
        assert_eq!(changes::published(&site), Some(first.clone()));
        assert_eq!(changes::pending(&site).unwrap().count(), 1);
        assert_eq!(back(&site, &releases).unwrap().release, "R3");
        assert_eq!(changes::published(&site), Some(second));
    }

    #[test]
    fn publications_are_kept() {
        let conn = crate::db::memory();
        conn.execute(
            "INSERT INTO person (id, email, name, role) VALUES (1, 'a@example.com', 'Ada', 'owner')",
            [],
        )
        .unwrap();
        assert_eq!(last(&conn).unwrap(), None);
        let published = Published {
            release: "R1".into(),
            commit: "abc".into(),
            previous: None,
        };
        record(&conn, 1, &published, 3).unwrap();
        let found = last(&conn).unwrap().unwrap();
        assert_eq!((found.by, found.changes), (Some(1), 3));
    }
}
