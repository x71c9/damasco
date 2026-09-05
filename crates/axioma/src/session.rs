//! Panel sessions and login.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::activity::{self, Environment};
use crate::people::{self, PeopleError};
use crate::token;

/// Sessions last 30 days from the last request.
const LIFETIME: &str = "+30 days";

/// The person behind a session, as the panel sees them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Person {
    pub id: i64,
    pub login: String,
    pub name: String,
    pub role: String,
}

/// Opens a session for `person` and returns its token.
pub fn create(conn: &Connection, person: i64) -> rusqlite::Result<String> {
    let token = token::generate();
    conn.execute(
        "INSERT INTO session (token_hash, person_id, expires_at)
         VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?3))",
        params![token::hash(&token), person, LIFETIME],
    )?;
    Ok(token)
}

/// Returns the person of a valid session and extends it.
pub fn lookup(conn: &Connection, token: &str) -> rusqlite::Result<Option<Person>> {
    let hash = token::hash(token);
    let person = conn
        .query_row(
            "SELECT person.id, person.login, person.name, person.role FROM session
             JOIN person ON person.id = session.person_id
             WHERE session.token_hash = ?1
               AND session.expires_at > strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
               AND person.revoked_at IS NULL
               AND person.password_hash IS NOT NULL",
            [&hash],
            |row| {
                Ok(Person {
                    id: row.get(0)?,
                    login: row.get(1)?,
                    name: row.get(2)?,
                    role: row.get(3)?,
                })
            },
        )
        .optional()?;
    if person.is_some() {
        conn.execute(
            "UPDATE session SET
                last_seen_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
                expires_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?2)
             WHERE token_hash = ?1",
            params![hash, LIFETIME],
        )?;
    }
    Ok(person)
}

/// Closes one session.
pub fn delete(conn: &Connection, token: &str) -> rusqlite::Result<Option<i64>> {
    let hash = token::hash(token);
    let person: Option<i64> = conn
        .query_row(
            "SELECT person_id FROM session WHERE token_hash = ?1",
            [&hash],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(person) = person {
        conn.execute("DELETE FROM session WHERE token_hash = ?1", [&hash])?;
        activity::record(
            conn,
            Some(person),
            Environment::Installation,
            "session.logout",
            Some(&format!("person:{person}")),
        )?;
    }
    Ok(person)
}

/// Checks login and password and opens a session. Unknown logins take as
/// long as wrong passwords, so that the answer time does not reveal which
/// logins exist.
pub fn login(conn: &Connection, login: &str, password: &str) -> Result<String, PeopleError> {
    let found: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, password_hash FROM person
             WHERE login = ?1 AND revoked_at IS NULL AND password_hash IS NOT NULL",
            [login.trim()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((person, hash)) = found else {
        people::verify_password(password, dummy_hash());
        return Err(PeopleError::WrongCredentials);
    };
    if !people::verify_password(password, &hash) {
        activity::record(
            conn,
            None,
            Environment::Installation,
            "session.login_failed",
            Some(&format!("person:{person}")),
        )?;
        return Err(PeopleError::WrongCredentials);
    }
    let token = create(conn, person)?;
    activity::record(
        conn,
        Some(person),
        Environment::Installation,
        "session.login",
        Some(&format!("person:{person}")),
    )?;
    Ok(token)
}

/// Argon2id hash of a random password, verified when the login is unknown,
/// with the same parameters as real hashes.
fn dummy_hash() -> &'static str {
    static HASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HASH.get_or_init(|| people::hash_password(&token::generate()).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::people::{Redeem, owner_link, redeem};

    fn with_owner() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let (conn, _) = crate::db::open(dir.path()).unwrap();
        let (token, _) = owner_link(&conn).unwrap();
        let input = Redeem {
            login: Some("giulia"),
            name: Some("Giulia"),
            password: "correct horse battery",
        };
        redeem(&conn, &token, input).unwrap();
        (dir, conn)
    }

    #[test]
    fn login_opens_a_session_and_logout_closes_it() {
        let (_dir, conn) = with_owner();
        let token = login(&conn, "Giulia", "correct horse battery").unwrap();
        let person = lookup(&conn, &token).unwrap().unwrap();
        assert_eq!(
            (person.login.as_str(), person.role.as_str()),
            ("giulia", "owner")
        );
        assert_eq!(delete(&conn, &token).unwrap(), Some(person.id));
        assert_eq!(lookup(&conn, &token).unwrap(), None);
    }

    #[test]
    fn wrong_password_and_unknown_login_give_the_same_error() {
        let (_dir, conn) = with_owner();
        let wrong = login(&conn, "giulia", "not the password").unwrap_err();
        let unknown = login(&conn, "nobody", "not the password").unwrap_err();
        assert_eq!(wrong.to_string(), unknown.to_string());
    }

    #[test]
    fn expired_sessions_and_revoked_people_are_out() {
        let (_dir, conn) = with_owner();
        let token = login(&conn, "giulia", "correct horse battery").unwrap();
        conn.execute("UPDATE session SET expires_at = '2000-01-01T00:00:00Z'", [])
            .unwrap();
        assert_eq!(lookup(&conn, &token).unwrap(), None);
        let token = login(&conn, "giulia", "correct horse battery").unwrap();
        conn.execute("UPDATE person SET revoked_at = '2026-01-01T00:00:00Z'", [])
            .unwrap();
        assert_eq!(lookup(&conn, &token).unwrap(), None);
    }

    #[test]
    fn dummy_hash_is_a_valid_argon2_hash() {
        assert!(dummy_hash().starts_with("$argon2id$"));
    }
}
