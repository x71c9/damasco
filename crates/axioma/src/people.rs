//! People with access to the panel and their one-time links.
//!
//! Accounts work without email: whoever creates an account, or the
//! installation for the owner, gets a link that is valid once and hands it
//! over. Redeeming the link sets the email, the name and the password.

use std::fmt;

use argon2::Argon2;
use argon2::password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash};
use rusqlite::{Connection, OptionalExtension, params};

use crate::access::{self, Action, Resource, Role, Scope};
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
    /// The person acting may not do this.
    Forbidden(String),
    NotFound,
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
            PeopleError::Forbidden(reason) => f.write_str(reason),
            PeopleError::NotFound => f.write_str("no such person"),
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
    /// Name given in the invitation, offered as the default.
    pub name: Option<String>,
}

/// Looks up a link without using it.
pub fn find_link(conn: &Connection, token: &str) -> Result<Link, PeopleError> {
    let found: Option<(i64, String, Option<String>)> = conn
        .query_row(
            "SELECT link.person_id, link.purpose, person.name FROM link
             JOIN person ON person.id = link.person_id
             WHERE link.token_hash = ?1
               AND link.used_at IS NULL
               AND link.expires_at > strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
               AND person.revoked_at IS NULL",
            [token::hash(token)],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let (person, purpose, name) = found.ok_or(PeopleError::InvalidLink)?;
    let purpose = Purpose::parse(&purpose).ok_or(PeopleError::InvalidLink)?;
    Ok(Link {
        person,
        purpose,
        name,
    })
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

/// Invitations last a week; reset links a day.
const INVITATION_VALIDITY: &str = "+7 days";
const RESET_VALIDITY: &str = "+1 day";

/// One row of the People section.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Listed {
    pub id: i64,
    pub email: Option<String>,
    pub name: Option<String>,
    pub role: String,
    /// "active", "invited" (link not used yet) or "revoked".
    pub status: &'static str,
    pub created_at: String,
}

/// Everyone who has or had access: the owner first, then active people,
/// pending invitations and revoked accounts.
pub fn list(conn: &Connection) -> rusqlite::Result<Vec<Listed>> {
    let mut statement = conn.prepare(
        "SELECT id, email, name, role, password_hash IS NOT NULL, revoked_at IS NOT NULL, created_at
         FROM person
         ORDER BY revoked_at IS NOT NULL, role != 'owner', password_hash IS NULL,
                  name COLLATE NOCASE, id",
    )?;
    let rows = statement.query_map([], |row| {
        let active: bool = row.get(4)?;
        let revoked: bool = row.get(5)?;
        Ok(Listed {
            id: row.get(0)?,
            email: row.get(1)?,
            name: row.get(2)?,
            role: row.get(3)?,
            status: if revoked {
                "revoked"
            } else if active {
                "active"
            } else {
                "invited"
            },
            created_at: row.get(6)?,
        })
    })?;
    rows.collect()
}

/// The person acting on the People section.
#[derive(Debug, Clone, Copy)]
pub struct Actor {
    pub id: i64,
    pub role: Role,
}

struct Target {
    role: Role,
    active: bool,
    revoked: bool,
}

fn target(conn: &Connection, id: i64) -> Result<Target, PeopleError> {
    let found: Option<(String, bool, bool)> = conn
        .query_row(
            "SELECT role, password_hash IS NOT NULL, revoked_at IS NOT NULL FROM person WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let (role, active, revoked) = found.ok_or(PeopleError::NotFound)?;
    Ok(Target {
        role: Role::parse(&role).ok_or(PeopleError::NotFound)?,
        active,
        revoked,
    })
}

/// Checks shared by every change to someone else's account.
fn check_target(actor: Actor, id: i64, target: &Target) -> Result<(), PeopleError> {
    if !access::allows(
        actor.role,
        Scope::Installation,
        Resource::People,
        Action::Write,
    ) {
        return Err(PeopleError::Forbidden(
            "managing people needs the owner or an administrator".into(),
        ));
    }
    if id == actor.id {
        return Err(PeopleError::Forbidden(
            "this action does not apply to one's own account".into(),
        ));
    }
    if target.role == Role::Owner {
        return Err(PeopleError::Forbidden(
            "nobody can change the owner's account from here".into(),
        ));
    }
    if target.revoked {
        return Err(PeopleError::Forbidden(
            "the access of this person was revoked".into(),
        ));
    }
    Ok(())
}

fn invitable(role: Role) -> Result<Role, PeopleError> {
    match role {
        Role::Admin | Role::Editor => Ok(role),
        Role::Owner => Err(invalid(
            "role",
            "a site has one owner; choose administrator or editor",
        )),
    }
}

/// Invites a person: creates the account with a name and a role and returns
/// its id with a setup link valid for a week.
pub fn invite(
    conn: &Connection,
    actor: Actor,
    name: &str,
    role: Role,
) -> Result<(i64, String), PeopleError> {
    if !access::allows(
        actor.role,
        Scope::Installation,
        Resource::People,
        Action::Write,
    ) {
        return Err(PeopleError::Forbidden(
            "managing people needs the owner or an administrator".into(),
        ));
    }
    let name = check_name(name)?;
    let role = invitable(role)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO person (name, role) VALUES (?1, ?2)",
        params![name, role.as_str()],
    )?;
    let person = tx.last_insert_rowid();
    let token = new_link(&tx, person, Purpose::Setup, INVITATION_VALIDITY, true)?;
    activity::record(
        &tx,
        Some(actor.id),
        Environment::Installation,
        "person.invited",
        Some(&format!("person:{person}")),
    )?;
    tx.commit()?;
    Ok((person, token))
}

pub fn change_role(
    conn: &Connection,
    actor: Actor,
    id: i64,
    role: Role,
) -> Result<(), PeopleError> {
    let found = target(conn, id)?;
    check_target(actor, id, &found)?;
    let role = invitable(role)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE person SET role = ?1 WHERE id = ?2",
        params![role.as_str(), id],
    )?;
    activity::record(
        &tx,
        Some(actor.id),
        Environment::Installation,
        "person.role_changed",
        Some(&format!("person:{id}")),
    )?;
    tx.commit()?;
    Ok(())
}

/// A new link for someone else: a fresh invitation when the account is not
/// set up yet, otherwise a password reset. Earlier links stop working.
pub fn link_for(
    conn: &Connection,
    actor: Actor,
    id: i64,
) -> Result<(String, Purpose), PeopleError> {
    let found = target(conn, id)?;
    check_target(actor, id, &found)?;
    let (purpose, validity) = if found.active {
        (Purpose::Reset, RESET_VALIDITY)
    } else {
        (Purpose::Setup, INVITATION_VALIDITY)
    };
    let tx = conn.unchecked_transaction()?;
    let token = new_link(&tx, id, purpose, validity, true)?;
    activity::record(
        &tx,
        Some(actor.id),
        Environment::Installation,
        "person.link_created",
        Some(&format!("person:{id}")),
    )?;
    tx.commit()?;
    Ok((token, purpose))
}

/// Revokes access: closes the sessions and disables the links. The account
/// stays in the list, and in the activity log, as revoked.
pub fn revoke(conn: &Connection, actor: Actor, id: i64) -> Result<(), PeopleError> {
    let found = target(conn, id)?;
    check_target(actor, id, &found)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE person SET revoked_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?1",
        [id],
    )?;
    tx.execute("DELETE FROM session WHERE person_id = ?1", [id])?;
    tx.execute(
        "UPDATE link SET used_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
         WHERE person_id = ?1 AND used_at IS NULL",
        [id],
    )?;
    activity::record(
        &tx,
        Some(actor.id),
        Environment::Installation,
        "person.revoked",
        Some(&format!("person:{id}")),
    )?;
    tx.commit()?;
    Ok(())
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

    /// An installation with an active owner; returns the owner as actor.
    fn with_owner() -> (tempfile::TempDir, Connection, Actor) {
        let (dir, conn) = db();
        let (token, _) = owner_link(&conn, true).unwrap();
        let id = redeem(
            &conn,
            &token,
            setup("owner@example.com", "correct horse battery"),
        )
        .unwrap();
        (
            dir,
            conn,
            Actor {
                id,
                role: Role::Owner,
            },
        )
    }

    fn join(conn: &Connection, token: &str, email: &str) -> i64 {
        let input = Redeem {
            email: Some(email),
            name: Some("Marco"),
            password: "another long password",
        };
        redeem(conn, token, input).unwrap()
    }

    #[test]
    fn invited_people_join_with_their_role() {
        let (_dir, conn, owner) = with_owner();
        let (id, token) = invite(&conn, owner, "Marco", Role::Editor).unwrap();
        let link = find_link(&conn, &token).unwrap();
        assert_eq!(
            (link.purpose, link.name.as_deref()),
            (Purpose::Setup, Some("Marco"))
        );
        let row = |id| {
            list(&conn)
                .unwrap()
                .into_iter()
                .find(|p| p.id == id)
                .unwrap()
        };
        assert_eq!(
            (row(id).status, row(id).role.as_str()),
            ("invited", "editor")
        );
        join(&conn, &token, "marco@example.com");
        assert_eq!(row(id).status, "active");
        assert_eq!(list(&conn).unwrap()[0].role, "owner");
    }

    #[test]
    fn the_owner_role_cannot_be_given() {
        let (_dir, conn, owner) = with_owner();
        let err = invite(&conn, owner, "Marco", Role::Owner).unwrap_err();
        assert_eq!(err.field(), Some("role"));
    }

    #[test]
    fn nobody_acts_on_the_owner_or_on_themselves() {
        let (_dir, conn, owner) = with_owner();
        let (admin_id, token) = invite(&conn, owner, "Marco", Role::Admin).unwrap();
        join(&conn, &token, "marco@example.com");
        let admin = Actor {
            id: admin_id,
            role: Role::Admin,
        };
        for result in [
            change_role(&conn, admin, owner.id, Role::Editor),
            revoke(&conn, admin, owner.id),
            link_for(&conn, admin, owner.id).map(|_| ()),
            revoke(&conn, admin, admin.id),
            change_role(&conn, owner, owner.id, Role::Editor),
        ] {
            assert!(
                matches!(result, Err(PeopleError::Forbidden(_))),
                "{result:?}"
            );
        }
        // Administrators manage the other administrators.
        let (other, _) = invite(&conn, admin, "Sara", Role::Admin).unwrap();
        change_role(&conn, admin, other, Role::Editor).unwrap();
    }

    #[test]
    fn editors_cannot_manage_people() {
        let (_dir, conn, owner) = with_owner();
        let (editor_id, token) = invite(&conn, owner, "Marco", Role::Editor).unwrap();
        join(&conn, &token, "marco@example.com");
        let editor = Actor {
            id: editor_id,
            role: Role::Editor,
        };
        assert!(matches!(
            invite(&conn, editor, "Sara", Role::Editor),
            Err(PeopleError::Forbidden(_))
        ));
    }

    #[test]
    fn new_links_reinvite_or_reset() {
        let (_dir, conn, owner) = with_owner();
        let (id, first) = invite(&conn, owner, "Marco", Role::Editor).unwrap();
        let (second, purpose) = link_for(&conn, owner, id).unwrap();
        assert_eq!(purpose, Purpose::Setup);
        assert!(matches!(
            find_link(&conn, &first),
            Err(PeopleError::InvalidLink)
        ));
        join(&conn, &second, "marco@example.com");
        assert_eq!(link_for(&conn, owner, id).unwrap().1, Purpose::Reset);
    }

    #[test]
    fn revoking_closes_sessions_and_links() {
        let (_dir, conn, owner) = with_owner();
        let (id, token) = invite(&conn, owner, "Marco", Role::Editor).unwrap();
        join(&conn, &token, "marco@example.com");
        let session =
            crate::session::login(&conn, "marco@example.com", "another long password").unwrap();
        let (pending, _) = link_for(&conn, owner, id).unwrap();
        revoke(&conn, owner, id).unwrap();
        assert_eq!(crate::session::lookup(&conn, &session).unwrap(), None);
        assert!(matches!(
            find_link(&conn, &pending),
            Err(PeopleError::InvalidLink)
        ));
        assert!(
            crate::session::login(&conn, "marco@example.com", "another long password").is_err()
        );
        assert_eq!(list(&conn).unwrap().last().unwrap().status, "revoked");
        assert!(matches!(
            revoke(&conn, owner, id),
            Err(PeopleError::Forbidden(_))
        ));
    }
}
