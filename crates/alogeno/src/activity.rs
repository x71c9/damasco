//! The activity log: an append-only table, in `installation.db`, of what
//! happened on the installation and on its sites.

use rusqlite::{Connection, params};

/// Where an action happened. Accounts and sessions belong to the whole
/// installation; content and code will belong to preview or production.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Environment {
    Installation,
    #[allow(dead_code)]
    Preview,
    #[allow(dead_code)]
    Production,
}

impl Environment {
    fn as_str(self) -> &'static str {
        match self {
            Environment::Installation => "installation",
            Environment::Preview => "preview",
            Environment::Production => "production",
        }
    }
}

/// Appends one entry about the installation. `actor` is the owner who
/// acted, `None` for alogeno itself or the command line. `subject`
/// identifies the object (for example `person:1`) and never carries
/// personal values.
pub fn record(
    conn: &Connection,
    actor: Option<i64>,
    environment: Environment,
    action: &str,
    subject: Option<&str>,
) -> rusqlite::Result<()> {
    record_site(conn, None, actor, environment, action, subject)
}

/// Appends one entry about a site (`site`, its id), or about the
/// installation (`None`).
pub fn record_site(
    conn: &Connection,
    site: Option<&str>,
    actor: Option<i64>,
    environment: Environment,
    action: &str,
    subject: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO activity (site_id, actor_id, environment, action, subject)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![site, actor, environment.as_str(), action, subject],
    )?;
    Ok(())
}

/// One entry as the panel shows it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Entry {
    pub id: i64,
    pub at: String,
    pub site_id: Option<String>,
    pub environment: String,
    pub action: String,
    pub actor_id: Option<i64>,
    pub subject: Option<String>,
}

/// Newest entries first, at most `limit`, older than `before` when given.
pub fn list(conn: &Connection, before: Option<i64>, limit: u32) -> rusqlite::Result<Vec<Entry>> {
    let mut statement = conn.prepare(
        "SELECT id, at, site_id, environment, action, actor_id, subject
         FROM activity
         WHERE ?1 IS NULL OR id < ?1
         ORDER BY id DESC
         LIMIT ?2",
    )?;
    let rows = statement.query_map(params![before, limit], |row| {
        Ok(Entry {
            id: row.get(0)?,
            at: row.get(1)?,
            site_id: row.get(2)?,
            environment: row.get(3)?,
            action: row.get(4)?,
            actor_id: row.get(5)?,
            subject: row.get(6)?,
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_newest_first_in_pages() {
        let conn = crate::db::memory_installation();
        for _ in 0..3 {
            record_site(
                &conn,
                Some("01SITE"),
                Some(1),
                Environment::Preview,
                "content.page_changed",
                Some("page:home"),
            )
            .unwrap();
        }
        let page = list(&conn, None, 2).unwrap();
        assert_eq!(page.len(), 2);
        assert!(page[0].id > page[1].id);
        assert_eq!(page[0].site_id.as_deref(), Some("01SITE"));
        let rest = list(&conn, Some(page[1].id), 2).unwrap();
        assert_eq!(rest.len(), 1);
        assert!(rest[0].id < page[1].id);
    }

    #[test]
    fn entries_cannot_be_changed_or_removed() {
        let dir = tempfile::tempdir().unwrap();
        let (conn, _) = crate::db::open_installation(dir.path()).unwrap();
        record(
            &conn,
            None,
            Environment::Installation,
            "test.done",
            Some("x:1"),
        )
        .unwrap();
        let update = conn.execute("UPDATE activity SET action = 'other'", []);
        assert!(update.unwrap_err().to_string().contains("append-only"));
        let delete = conn.execute("DELETE FROM activity", []);
        assert!(delete.unwrap_err().to_string().contains("append-only"));
    }
}
