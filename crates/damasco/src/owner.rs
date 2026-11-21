//! The owner of the installation, the only account of the panel, and its
//! one-time links.
//!
//! The account has a password and nothing else: no email, no name. The
//! installation prints a link that is valid once (`damasco owner-link`); the
//! first one sets the password, later ones reset it. Whoever runs the
//! command on the server can always take the account back.

use std::fmt;

use argon2::Argon2;
use argon2::password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash};
use rusqlite::{Connection, OptionalExtension, params};

use crate::activity::{self, Environment};
use crate::token;

pub const PASSWORD_MIN: usize = 10;
const PASSWORD_MAX: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// The first password of the owner.
    Setup,
    /// A new password, replacing a forgotten one.
    Reset,
}

impl Purpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Purpose::Setup => "setup",
            Purpose::Reset => "reset",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "setup" => Some(Purpose::Setup),
            "reset" => Some(Purpose::Reset),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum OwnerError {
    /// Unknown, used or expired link. One message for all cases, so that a
    /// link cannot be probed.
    InvalidLink,
    WrongPassword,
    /// A field of the form is not acceptable.
    Invalid {
        field: &'static str,
        message: String,
    },
    Database(rusqlite::Error),
    Hash(String),
}

impl fmt::Display for OwnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OwnerError::InvalidLink => {
                f.write_str("the link is not valid: it may be used or expired")
            }
            OwnerError::WrongPassword => f.write_str("wrong password"),
            OwnerError::Invalid { message, .. } => f.write_str(message),
            OwnerError::Database(err) => write!(f, "database: {err}"),
            OwnerError::Hash(err) => write!(f, "password hashing: {err}"),
        }
    }
}

impl OwnerError {
    /// The form field the error is about, if any.
    pub fn field(&self) -> Option<&'static str> {
        match self {
            OwnerError::Invalid { field, .. } => Some(field),
            _ => None,
        }
    }
}

impl std::error::Error for OwnerError {}

impl From<rusqlite::Error> for OwnerError {
    fn from(err: rusqlite::Error) -> Self {
        OwnerError::Database(err)
    }
}

/// Creates a link for the owner: a setup link when the owner has no
/// password yet (creating the owner on a new installation), otherwise a
/// reset link. With `replace`, older unused links stop working; without it
/// they stay valid until they expire, as for the link printed at every start
/// of a new installation.
pub fn owner_link(conn: &Connection, replace: bool) -> Result<(String, Purpose), OwnerError> {
    let tx = conn.unchecked_transaction()?;
    let owner: Option<(i64, bool)> = tx
        .query_row(
            "SELECT id, password_hash IS NOT NULL FROM person WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (person, purpose) = match owner {
        Some((id, true)) => (id, Purpose::Reset),
        Some((id, false)) => (id, Purpose::Setup),
        None => {
            tx.execute("INSERT INTO person (id) VALUES (1)", [])?;
            (tx.last_insert_rowid(), Purpose::Setup)
        }
    };
    if replace {
        tx.execute(
            "UPDATE link SET used_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
             WHERE person_id = ?1 AND used_at IS NULL",
            [person],
        )?;
    }
    let token = token::generate();
    tx.execute(
        "INSERT INTO link (token_hash, person_id, purpose, expires_at)
         VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '+1 day'))",
        params![token::hash(&token), person, purpose.as_str()],
    )?;
    activity::record(
        &tx,
        None,
        Environment::Installation,
        "owner.link_created",
        Some(&format!("person:{person}")),
    )?;
    tx.commit()?;
    Ok((token, purpose))
}

/// True when the owner has a password.
pub fn has_owner(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM person WHERE id = 1 AND password_hash IS NOT NULL)",
        [],
        |row| row.get(0),
    )
}

/// The owner's id and password hash, once a password is set.
pub fn credentials(conn: &Connection) -> rusqlite::Result<Option<(i64, String)>> {
    conn.query_row(
        "SELECT id, password_hash FROM person WHERE id = 1 AND password_hash IS NOT NULL",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
}

/// A link that can still be redeemed.
#[derive(Debug, PartialEq, Eq)]
pub struct Link {
    pub person: i64,
    pub purpose: Purpose,
}

/// Looks up a link without using it.
pub fn find_link(conn: &Connection, token: &str) -> Result<Link, OwnerError> {
    let found: Option<(i64, String)> = conn
        .query_row(
            "SELECT person_id, purpose FROM link
             WHERE token_hash = ?1
               AND used_at IS NULL
               AND expires_at > strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
            [token::hash(token)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (person, purpose) = found.ok_or(OwnerError::InvalidLink)?;
    let purpose = Purpose::parse(&purpose).ok_or(OwnerError::InvalidLink)?;
    Ok(Link { person, purpose })
}

/// Uses a link: sets the password, closes every open session (a reset
/// follows a lost password, or a stolen one) and returns the owner's id.
pub fn redeem(conn: &Connection, token: &str, password: &str) -> Result<i64, OwnerError> {
    let tx = conn.unchecked_transaction()?;
    let link = find_link(&tx, token)?;
    check_password(password)?;
    let hash = hash_password(password)?;
    tx.execute(
        "UPDATE person SET password_hash = ?1 WHERE id = ?2",
        params![hash, link.person],
    )?;
    tx.execute("DELETE FROM session WHERE person_id = ?1", [link.person])?;
    tx.execute(
        "UPDATE link SET used_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE token_hash = ?1",
        [token::hash(token)],
    )?;
    let action = match link.purpose {
        Purpose::Setup => "person.setup",
        Purpose::Reset => "person.password_reset",
    };
    activity::record(
        &tx,
        Some(link.person),
        Environment::Installation,
        action,
        Some(&format!("person:{}", link.person)),
    )?;
    tx.commit()?;
    Ok(link.person)
}

fn check_password(password: &str) -> Result<(), OwnerError> {
    let invalid = |message: String| OwnerError::Invalid {
        field: "password",
        message,
    };
    let length = password.chars().count();
    if length < PASSWORD_MIN {
        return Err(invalid(format!("at least {PASSWORD_MIN} characters")));
    }
    if length > PASSWORD_MAX {
        return Err(invalid("too long".into()));
    }
    Ok(())
}

pub fn hash_password(password: &str) -> Result<String, OwnerError> {
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|err| OwnerError::Hash(err.to_string()))
}

pub fn verify_password(password: &str, stored: &str) -> bool {
    PasswordHash::new(stored)
        .map(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let (conn, _) = crate::db::open_installation(dir.path()).unwrap();
        (dir, conn)
    }

    #[test]
    fn first_owner_link_creates_the_owner() {
        let (_dir, conn) = db();
        assert!(!has_owner(&conn).unwrap());
        let (token, purpose) = owner_link(&conn, true).unwrap();
        assert_eq!(purpose, Purpose::Setup);
        let id = redeem(&conn, &token, "correct horse battery").unwrap();
        let (found, hash) = credentials(&conn).unwrap().unwrap();
        assert_eq!(found, id);
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery", &hash));
        assert!(!verify_password("wrong password", &hash));
        assert!(has_owner(&conn).unwrap());
    }

    #[test]
    fn a_link_works_once() {
        let (_dir, conn) = db();
        let (token, _) = owner_link(&conn, true).unwrap();
        redeem(&conn, &token, "correct horse battery").unwrap();
        let again = redeem(&conn, &token, "correct horse battery");
        assert!(matches!(again, Err(OwnerError::InvalidLink)));
    }

    #[test]
    fn a_reset_link_sets_a_new_password_and_closes_sessions() {
        let (_dir, conn) = db();
        let (first, _) = owner_link(&conn, true).unwrap();
        let id = redeem(&conn, &first, "correct horse battery").unwrap();
        conn.execute(
            "INSERT INTO session (token_hash, person_id, expires_at) VALUES (x'00', ?1, '2999-01-01T00:00:00Z')",
            [id],
        )
        .unwrap();
        let (token, purpose) = owner_link(&conn, true).unwrap();
        assert_eq!(purpose, Purpose::Reset);
        // Creating the link leaves the sessions open: redeeming it closes them.
        let count = |conn: &Connection| -> i64 {
            conn.query_row("SELECT count(*) FROM session", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(count(&conn), 1);
        assert_eq!(redeem(&conn, &token, "another long password").unwrap(), id);
        assert_eq!(count(&conn), 0);
        let (_, hash) = credentials(&conn).unwrap().unwrap();
        assert!(verify_password("another long password", &hash));
        let owners: i64 = conn
            .query_row("SELECT count(*) FROM person", [], |r| r.get(0))
            .unwrap();
        assert_eq!(owners, 1);
    }

    #[test]
    fn a_new_link_disables_the_previous_one() {
        let (_dir, conn) = db();
        let (old, _) = owner_link(&conn, true).unwrap();
        let (new, _) = owner_link(&conn, true).unwrap();
        assert!(matches!(
            find_link(&conn, &old),
            Err(OwnerError::InvalidLink)
        ));
        assert!(find_link(&conn, &new).is_ok());
    }

    #[test]
    fn links_printed_at_startup_leave_earlier_ones_valid() {
        let (_dir, conn) = db();
        let (first, _) = owner_link(&conn, false).unwrap();
        let (second, _) = owner_link(&conn, false).unwrap();
        assert!(find_link(&conn, &first).is_ok());
        assert!(find_link(&conn, &second).is_ok());
    }

    #[test]
    fn expired_links_are_refused() {
        let (_dir, conn) = db();
        let (token, _) = owner_link(&conn, true).unwrap();
        conn.execute("UPDATE link SET expires_at = '2000-01-01T00:00:00Z'", [])
            .unwrap();
        assert!(matches!(
            find_link(&conn, &token),
            Err(OwnerError::InvalidLink)
        ));
    }

    #[test]
    fn rejects_short_passwords_without_using_the_link() {
        let (_dir, conn) = db();
        let (token, _) = owner_link(&conn, true).unwrap();
        let short = redeem(&conn, &token, "short").unwrap_err();
        assert_eq!(short.field(), Some("password"));
        assert!(short.to_string().contains("at least 10"));
        assert!(find_link(&conn, &token).is_ok());
    }

    #[test]
    fn activity_records_identifiers_only() {
        let (_dir, conn) = db();
        let (token, _) = owner_link(&conn, true).unwrap();
        redeem(&conn, &token, "correct horse battery").unwrap();
        let entries: Vec<(String, String)> = conn
            .prepare("SELECT action, subject FROM activity ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            entries,
            [
                ("owner.link_created".to_string(), "person:1".to_string()),
                ("person.setup".to_string(), "person:1".to_string()),
            ]
        );
    }
}
