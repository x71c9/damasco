//! Access to the preview. The preview shows changes that are not online,
//! so only people of the panel see it. Its host is not the panel's, and the
//! panel's cookie never reaches it: the panel hands out a code that works
//! once, for a minute, and the preview exchanges it for a session of its
//! own, in a cookie of the preview's host.

use rusqlite::{Connection, OptionalExtension, params};

use crate::token;

/// How long a code works.
const CODE_LIFETIME: &str = "+60 seconds";
/// How long a session of the preview lasts; the panel opens a new one each
/// time it loads.
const SESSION_LIFETIME: &str = "+12 hours";

/// A new one-time code for `person`.
pub fn code(conn: &Connection, person: i64) -> rusqlite::Result<String> {
    let code = token::generate();
    conn.execute(
        "INSERT INTO preview_code (token_hash, person_id, expires_at)
         VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?3))",
        params![token::hash(&code), person, CODE_LIFETIME],
    )?;
    // Old codes and sessions go.
    conn.execute(
        "DELETE FROM preview_code WHERE expires_at < strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-1 day')",
        [],
    )?;
    conn.execute(
        "DELETE FROM preview_session WHERE expires_at < strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
        [],
    )?;
    Ok(code)
}

/// Exchanges a code for a session token, once.
pub fn redeem(conn: &Connection, code: &str) -> rusqlite::Result<Option<String>> {
    let person: Option<i64> = conn
        .query_row(
            "UPDATE preview_code SET used_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
             WHERE token_hash = ?1 AND used_at IS NULL
               AND expires_at > strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
             RETURNING person_id",
            [token::hash(code)],
            |row| row.get(0),
        )
        .optional()?;
    let Some(person) = person else {
        return Ok(None);
    };
    let session = token::generate();
    conn.execute(
        "INSERT INTO preview_session (token_hash, person_id, expires_at)
         VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?3))",
        params![token::hash(&session), person, SESSION_LIFETIME],
    )?;
    Ok(Some(session))
}

/// Whether a session token opens the preview: known and not expired. A new
/// password of the owner closes the panel's sessions, not these: they end
/// with their lifetime.
pub fn valid(conn: &Connection, session: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM preview_session
           WHERE token_hash = ?1
             AND expires_at > strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))",
        [token::hash(session)],
        |row| row.get(0),
    )
}

/// Where to go after entering: a path of the preview, never another host.
pub fn safe_path(to: Option<&str>) -> String {
    match to {
        Some(path) if path.starts_with('/') && !path.starts_with("//") && !path.contains('\\') => {
            path.to_string()
        }
        _ => "/".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_opens_one_session_once() {
        let conn = crate::db::memory_site();
        let code = code(&conn, 1).unwrap();
        let session = redeem(&conn, &code).unwrap().unwrap();
        assert!(valid(&conn, &session).unwrap());
        assert_eq!(redeem(&conn, &code).unwrap(), None, "a code works once");
        assert!(!valid(&conn, "made-up").unwrap());
        conn.execute(
            "UPDATE preview_session SET expires_at = '2000-01-01T00:00:00Z'",
            [],
        )
        .unwrap();
        assert!(!valid(&conn, &session).unwrap(), "expired sessions are out");
    }

    #[test]
    fn only_paths_of_the_preview() {
        assert_eq!(
            safe_path(Some("/projects/?build=R1")),
            "/projects/?build=R1"
        );
        for wrong in [
            None,
            Some("https://evil.example"),
            Some("//evil.example"),
            Some("/\\evil"),
        ] {
            assert_eq!(safe_path(wrong), "/");
        }
    }
}
