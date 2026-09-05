//! People with access to the panel and their one-time links.
//!
//! Accounts work without email: whoever creates an account, or the
//! installation for the owner, gets a link that is valid once and hands it
//! over. Redeeming the link sets the email, the name and the password.

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
    /// First password, together with the email and the name.
    Setup,
    /// New password for an existing account.
    Reset,
}

impl Purpose {
    fn as_str(self) -> &'static str {
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
pub enum PeopleError {
    /// Unknown, used, expired or revoked link. One message for all cases, so
    /// that a link cannot be probed.
    InvalidLink,
    EmailTaken,
    WrongCredentials,
    /// A field of the form is not acceptable.
    Invalid {
        field: &'static str,
        message: String,
    },
    Database(rusqlite::Error),
    Hash(String),
}

impl fmt::Display for PeopleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PeopleError::InvalidLink => {
                f.write_str("the link is not valid: it may be used or expired")
            }
            PeopleError::EmailTaken => f.write_str("the email is already in use"),
            PeopleError::WrongCredentials => f.write_str("wrong email or password"),
            PeopleError::Invalid { message, .. } => f.write_str(message),
            PeopleError::Database(err) => write!(f, "database: {err}"),
            PeopleError::Hash(err) => write!(f, "password hashing: {err}"),
        }
    }
}

impl PeopleError {
    /// The form field the error is about, if any.
    pub fn field(&self) -> Option<&'static str> {
        match self {
            PeopleError::EmailTaken => Some("email"),
            PeopleError::Invalid { field, .. } => Some(field),
            _ => None,
        }
    }
}

impl std::error::Error for PeopleError {}

impl From<rusqlite::Error> for PeopleError {
    fn from(err: rusqlite::Error) -> Self {
        PeopleError::Database(err)
    }
}

/// Creates a link for the owner: a setup link when the owner has no
/// password yet (creating the owner on a new installation), otherwise a
/// reset link. With `replace`, older unused links of the owner stop working;
/// without it they stay valid until they expire, as for the link printed at
/// every start of a new installation.
pub fn owner_link(conn: &Connection, replace: bool) -> Result<(String, Purpose), PeopleError> {
    let tx = conn.unchecked_transaction()?;
    let owner: Option<(i64, bool)> = tx
        .query_row(
            "SELECT id, password_hash IS NOT NULL FROM person
             WHERE role = 'owner' AND revoked_at IS NULL",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (person, purpose) = match owner {
        Some((id, true)) => (id, Purpose::Reset),
        Some((id, false)) => (id, Purpose::Setup),
        None => {
            tx.execute("INSERT INTO person (role) VALUES ('owner')", [])?;
            (tx.last_insert_rowid(), Purpose::Setup)
        }
    };
    let token = new_link(&tx, person, purpose, "+1 day", replace)?;
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

/// True when the owner has completed the account.
pub fn has_owner(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM person
         WHERE role = 'owner' AND revoked_at IS NULL AND password_hash IS NOT NULL)",
        [],
        |row| row.get(0),
    )
}

fn new_link(
    conn: &Connection,
    person: i64,
    purpose: Purpose,
    validity: &str,
    replace: bool,
) -> rusqlite::Result<String> {
    if replace {
        conn.execute(
            "UPDATE link SET used_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
             WHERE person_id = ?1 AND used_at IS NULL",
            [person],
        )?;
    }
    let token = token::generate();
    conn.execute(
        "INSERT INTO link (token_hash, person_id, purpose, expires_at)
         VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?4))",
        params![token::hash(&token), person, purpose.as_str(), validity],
    )?;
    Ok(token)
}

/// A link that can still be redeemed.
#[derive(Debug, PartialEq, Eq)]
pub struct Link {
    pub person: i64,
    pub purpose: Purpose,
}

/// Looks up a link without using it.
pub fn find_link(conn: &Connection, token: &str) -> Result<Link, PeopleError> {
    let found: Option<(i64, String)> = conn
        .query_row(
            "SELECT link.person_id, link.purpose FROM link
             JOIN person ON person.id = link.person_id
             WHERE link.token_hash = ?1
               AND link.used_at IS NULL
               AND link.expires_at > strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
               AND person.revoked_at IS NULL",
            [token::hash(token)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (person, purpose) = found.ok_or(PeopleError::InvalidLink)?;
    let purpose = Purpose::parse(&purpose).ok_or(PeopleError::InvalidLink)?;
    Ok(Link { person, purpose })
}

/// What the person types when redeeming a link. Email and name are needed
/// only for a setup link.
pub struct Redeem<'a> {
    pub email: Option<&'a str>,
    pub name: Option<&'a str>,
    pub password: &'a str,
}

/// Uses a link: sets the password (and, for a setup link, email and name)
/// and returns the person's id.
pub fn redeem(conn: &Connection, token: &str, input: Redeem<'_>) -> Result<i64, PeopleError> {
    let tx = conn.unchecked_transaction()?;
    let link = find_link(&tx, token)?;
    check_password(input.password)?;
    let hash = hash_password(input.password)?;
    if link.purpose == Purpose::Setup {
        let email = check_email(input.email.unwrap_or_default())?;
        let name = check_name(input.name.unwrap_or_default())?;
        let updated = tx.execute(
            "UPDATE person SET email = ?1, name = ?2, password_hash = ?3 WHERE id = ?4",
            params![email, name, hash, link.person],
        );
        match updated {
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                return Err(PeopleError::EmailTaken);
            }
            other => other?,
        };
    } else {
        tx.execute(
            "UPDATE person SET password_hash = ?1 WHERE id = ?2",
            params![hash, link.person],
        )?;
        // A new password closes every open session of the account.
        tx.execute("DELETE FROM session WHERE person_id = ?1", [link.person])?;
    }
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

fn invalid(field: &'static str, message: impl Into<String>) -> PeopleError {
    PeopleError::Invalid {
        field,
        message: message.into(),
    }
}

fn check_password(password: &str) -> Result<(), PeopleError> {
    let length = password.chars().count();
    if length < PASSWORD_MIN {
        return Err(invalid(
            "password",
            format!("at least {PASSWORD_MIN} characters"),
        ));
    }
    if length > PASSWORD_MAX {
        return Err(invalid("password", "too long"));
    }
    Ok(())
}

/// A plausible email address: one `@`, something before it, a dot after it,
/// no spaces. The address is never verified, since axioma sends no mail.
pub fn check_email(email: &str) -> Result<String, PeopleError> {
    let email = email.trim().to_lowercase();
    let valid = email.len() <= 254
        && !email.chars().any(char::is_whitespace)
        && email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && !domain.contains('@')
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
        });
    if valid {
        Ok(email)
    } else {
        Err(invalid("email", "not a valid email address"))
    }
}

fn check_name(name: &str) -> Result<String, PeopleError> {
    let name = name.trim();
    if (1..=80).contains(&name.chars().count()) {
        Ok(name.to_string())
    } else {
        Err(invalid("name", "1 to 80 characters"))
    }
}

pub fn hash_password(password: &str) -> Result<String, PeopleError> {
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|err| PeopleError::Hash(err.to_string()))
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
        let (conn, _) = crate::db::open(dir.path()).unwrap();
        (dir, conn)
    }

    fn setup<'a>(email: &'a str, password: &'a str) -> Redeem<'a> {
        Redeem {
            email: Some(email),
            name: Some("Giulia Mancini"),
            password,
        }
    }

    #[test]
    fn first_owner_link_creates_the_owner() {
        let (_dir, conn) = db();
        let (token, purpose) = owner_link(&conn, true).unwrap();
        assert_eq!(purpose, Purpose::Setup);
        let id = redeem(
            &conn,
            &token,
            setup("Giulia@Example.com", "correct horse battery"),
        )
        .unwrap();
        let (email, hash): (String, String) = conn
            .query_row(
                "SELECT email, password_hash FROM person WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(email, "giulia@example.com");
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery", &hash));
        assert!(!verify_password("wrong password", &hash));
    }

    #[test]
    fn a_link_works_once() {
        let (_dir, conn) = db();
        let (token, _) = owner_link(&conn, true).unwrap();
        redeem(
            &conn,
            &token,
            setup("giulia@example.com", "correct horse battery"),
        )
        .unwrap();
        let again = redeem(&conn, &token, setup("other", "correct horse battery"));
        assert!(matches!(again, Err(PeopleError::InvalidLink)));
    }

    #[test]
    fn later_owner_links_reset_the_password_and_close_sessions() {
        let (_dir, conn) = db();
        let (first, _) = owner_link(&conn, true).unwrap();
        let id = redeem(
            &conn,
            &first,
            setup("giulia@example.com", "correct horse battery"),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session (token_hash, person_id, expires_at) VALUES (x'00', ?1, '2999-01-01T00:00:00Z')",
            [id],
        )
        .unwrap();
        let (token, purpose) = owner_link(&conn, true).unwrap();
        assert_eq!(purpose, Purpose::Reset);
        let reset = Redeem {
            email: None,
            name: None,
            password: "another long password",
        };
        assert_eq!(redeem(&conn, &token, reset).unwrap(), id);
        let sessions: i64 = conn
            .query_row("SELECT count(*) FROM session", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sessions, 0);
        let owners: i64 = conn
            .query_row(
                "SELECT count(*) FROM person WHERE role = 'owner'",
                [],
                |r| r.get(0),
            )
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
            Err(PeopleError::InvalidLink)
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
            Err(PeopleError::InvalidLink)
        ));
    }

    #[test]
    fn rejects_weak_passwords_and_bad_emails() {
        let (_dir, conn) = db();
        let (token, _) = owner_link(&conn, true).unwrap();
        let short = redeem(&conn, &token, setup("giulia@example.com", "short"));
        let short = short.unwrap_err();
        assert_eq!(short.field(), Some("password"));
        assert!(short.to_string().contains("at least 10"));
        let email = redeem(
            &conn,
            &token,
            setup("giulia at example", "correct horse battery"),
        );
        let email = email.unwrap_err();
        assert_eq!(email.field(), Some("email"));
        // Failed attempts do not use the link.
        assert!(find_link(&conn, &token).is_ok());
    }

    #[test]
    fn activity_records_identifiers_only() {
        let (_dir, conn) = db();
        let (token, _) = owner_link(&conn, true).unwrap();
        redeem(
            &conn,
            &token,
            setup("giulia@example.com", "correct horse battery"),
        )
        .unwrap();
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
