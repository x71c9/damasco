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

#[cfg(test)]
mod tests {
    use super::*;

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
