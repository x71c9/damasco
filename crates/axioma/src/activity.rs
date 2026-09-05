//! The activity log: an append-only table of what happened on the site.

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

/// Appends one entry. `actor` is the person who acted, `None` for the
/// command line. `subject` identifies the object (for example `person:3`)
/// and never carries personal values.
pub fn record(
    conn: &Connection,
    actor: Option<i64>,
    environment: Environment,
    action: &str,
    subject: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO activity (actor_id, environment, action, subject) VALUES (?1, ?2, ?3, ?4)",
        params![actor, environment.as_str(), action, subject],
    )?;
    Ok(())
}

/// One entry as the panel shows it. Names are looked up at reading time, so
/// the log itself never stores them.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Entry {
    pub id: i64,
    pub at: String,
    pub environment: String,
    pub action: String,
    pub actor_id: Option<i64>,
    pub actor_name: Option<String>,
    pub subject: Option<String>,
    /// Name of the person the entry is about, when the subject is a person.
    pub subject_name: Option<String>,
}

/// Newest entries first, at most `limit`, older than `before` when given.
pub fn list(conn: &Connection, before: Option<i64>, limit: u32) -> rusqlite::Result<Vec<Entry>> {
    let mut statement = conn.prepare(
        "SELECT a.id, a.at, a.environment, a.action, a.actor_id, actor.name, a.subject, target.name
         FROM activity a
         LEFT JOIN person actor ON actor.id = a.actor_id
         LEFT JOIN person target ON a.subject = 'person:' || target.id
         WHERE ?1 IS NULL OR a.id < ?1
         ORDER BY a.id DESC
         LIMIT ?2",
    )?;
    let rows = statement.query_map(params![before, limit], |row| {
        Ok(Entry {
            id: row.get(0)?,
            at: row.get(1)?,
            environment: row.get(2)?,
            action: row.get(3)?,
            actor_id: row.get(4)?,
            actor_name: row.get(5)?,
            subject: row.get(6)?,
            subject_name: row.get(7)?,
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_newest_first_with_names_and_pages() {
        let conn = crate::db::memory();
        conn.execute(
            "INSERT INTO person (name, role) VALUES ('Giulia', 'owner')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO person (name, role) VALUES ('Marco', 'editor')",
            [],
        )
        .unwrap();
        for _ in 0..3 {
            record(
                &conn,
                Some(1),
                Environment::Installation,
                "person.invited",
                Some("person:2"),
            )
            .unwrap();
        }
        let page = list(&conn, None, 2).unwrap();
        assert_eq!(page.len(), 2);
        assert!(page[0].id > page[1].id);
        assert_eq!(page[0].actor_name.as_deref(), Some("Giulia"));
        assert_eq!(page[0].subject_name.as_deref(), Some("Marco"));
        let rest = list(&conn, Some(page[1].id), 2).unwrap();
        assert_eq!(rest.len(), 1);
        assert!(rest[0].id < page[1].id);
    }

    #[test]
    fn entries_cannot_be_changed_or_removed() {
        let dir = tempfile::tempdir().unwrap();
        let (conn, _) = crate::db::open(dir.path()).unwrap();
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
