//! Panel sessions and login.
//!
//! Signing in takes three steps, so that the slow password check (Argon2)
//! never holds the database: read the owner's hash (`owner::credentials`),
//! check the password outside the database (`check_password`), then open the
//! session or record the failure (`finish_login`).

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::activity::{self, Environment};
use crate::owner::{self, OwnerError};
use crate::token;

/// Sessions last 30 days from the last request.
const LIFETIME: &str = "+30 days";

/// The person behind a session: always the owner, the only account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Person {
    pub id: i64,
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
            "SELECT person.id FROM session
             JOIN person ON person.id = session.person_id
             WHERE session.token_hash = ?1
               AND session.expires_at > strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
               AND person.revoked_at IS NULL
               AND person.password_hash IS NOT NULL",
            [&hash],
            |row| Ok(Person { id: row.get(0)? }),
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

/// The owner's id when `password` is the owner's, given what
/// `owner::credentials` found. Without an owner the check takes as long as
/// with one, against a hash of a random password, and fails. Slow on
/// purpose: callers run it outside the database.
pub fn check_password(credentials: Option<&(i64, String)>, password: &str) -> Option<i64> {
    match credentials {
        Some((person, hash)) => owner::verify_password(password, hash).then_some(*person),
        None => {
            owner::verify_password(password, dummy_hash());
            None
        }
    }
}

/// Opens a session after a good password (`signed_in`), or records a failed
/// attempt on the owner (`attempted`, when an owner exists) and refuses.
pub fn finish_login(
    conn: &Connection,
    signed_in: Option<i64>,
    attempted: Option<i64>,
) -> Result<(String, Person), OwnerError> {
    let Some(person) = signed_in else {
        if let Some(person) = attempted {
            activity::record(
                conn,
                None,
                Environment::Installation,
                "session.login_failed",
                Some(&format!("person:{person}")),
            )?;
        }
        return Err(OwnerError::WrongPassword);
    };
    let token = create(conn, person)?;
    activity::record(
        conn,
        Some(person),
        Environment::Installation,
        "session.login",
        Some(&format!("person:{person}")),
    )?;
    Ok((token, Person { id: person }))
}

/// Argon2id hash of a random password, checked when there is no owner, with
/// the same parameters as real hashes.
fn dummy_hash() -> &'static str {
    static HASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HASH.get_or_init(|| owner::hash_password(&token::generate()).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::owner::{owner_link, redeem};

    fn with_owner() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let (conn, _) = crate::db::open(dir.path()).unwrap();
        let (token, _) = owner_link(&conn, true).unwrap();
        redeem(&conn, &token, "correct horse battery").unwrap();
        (dir, conn)
    }

    fn login(conn: &Connection, password: &str) -> Result<(String, Person), OwnerError> {
        let found = owner::credentials(conn).unwrap();
        let signed_in = check_password(found.as_ref(), password);
        finish_login(conn, signed_in, found.map(|(id, _)| id))
    }

    #[test]
    fn login_opens_a_session_and_logout_closes_it() {
        let (_dir, conn) = with_owner();
        let (token, person) = login(&conn, "correct horse battery").unwrap();
        assert_eq!(lookup(&conn, &token).unwrap(), Some(person));
        assert_eq!(delete(&conn, &token).unwrap(), Some(person.id));
        assert_eq!(lookup(&conn, &token).unwrap(), None);
    }

    #[test]
    fn a_wrong_password_is_refused_and_recorded() {
        let (_dir, conn) = with_owner();
        let wrong = login(&conn, "not the password").unwrap_err();
        assert!(matches!(wrong, OwnerError::WrongPassword));
        let failed: i64 = conn
            .query_row(
                "SELECT count(*) FROM activity WHERE action = 'session.login_failed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(failed, 1);
    }

    #[test]
    fn without_an_owner_every_password_is_wrong() {
        let dir = tempfile::tempdir().unwrap();
        let (conn, _) = crate::db::open(dir.path()).unwrap();
        assert!(matches!(
            login(&conn, "correct horse battery"),
            Err(OwnerError::WrongPassword)
        ));
    }

    #[test]
    fn expired_sessions_and_revoked_people_are_out() {
        let (_dir, conn) = with_owner();
        let (token, _) = login(&conn, "correct horse battery").unwrap();
        conn.execute("UPDATE session SET expires_at = '2000-01-01T00:00:00Z'", [])
            .unwrap();
        assert_eq!(lookup(&conn, &token).unwrap(), None);
        let (token, _) = login(&conn, "correct horse battery").unwrap();
        conn.execute("UPDATE person SET revoked_at = '2026-01-01T00:00:00Z'", [])
            .unwrap();
        assert_eq!(lookup(&conn, &token).unwrap(), None);
    }

    #[test]
    fn dummy_hash_is_a_valid_argon2_hash() {
        assert!(dummy_hash().starts_with("$argon2id$"));
    }
}
