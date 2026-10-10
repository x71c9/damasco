//! The installation database, `axioma.db`.
//!
//! The schema version is stored in SQLite's `user_version`. Migrations are SQL
//! files compiled into the binary and applied in order, each in its own
//! transaction. Before upgrading an existing database, a copy is written to
//! `backups/`. A database newer than the binary is refused: downgrades are
//! not supported.

use std::fmt;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

/// Migrations in order; migration `n` (1-based) brings the schema to version `n`.
pub const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_installation.sql"),
    include_str!("../migrations/0002_people.sql"),
    include_str!("../migrations/0003_chat.sql"),
    include_str!("../migrations/0004_chat_decisions.sql"),
];

const FILE_NAME: &str = "axioma.db";
const BACKUP_DIR: &str = "backups";

/// The connection shared by the HTTP handlers. Queries run on the blocking
/// thread pool, one at a time.
#[derive(Clone)]
pub struct Handle(std::sync::Arc<std::sync::Mutex<Connection>>);

impl Handle {
    pub fn new(conn: Connection) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(conn)))
    }

    pub async fn run<T, F>(&self, work: F) -> T
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> T + Send + 'static,
    {
        let inner = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let conn = inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            work(&conn)
        })
        .await
        .expect("database task")
    }
}

/// Outcome of opening the database.
#[derive(Debug, PartialEq, Eq)]
pub struct Opened {
    pub path: PathBuf,
    pub version: usize,
    /// Copy written before the upgrade, if any migration was applied to an
    /// existing database.
    pub backup: Option<PathBuf>,
}

#[derive(Debug)]
pub struct DbError(String);

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DbError {}

fn err(context: impl fmt::Display, cause: impl fmt::Display) -> DbError {
    DbError(format!("{context}: {cause}"))
}

/// Creates the data directory if needed, opens `axioma.db` and brings its
/// schema to the latest version.
pub fn open(data_dir: &Path) -> Result<(Connection, Opened), DbError> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(data_dir)
        .map_err(|e| err(format!("cannot create {}", data_dir.display()), e))?;
    let path = data_dir.join(FILE_NAME);
    let conn = Connection::open(&path).map_err(|e| err(path.display(), e))?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;",
    )
    .map_err(|e| err(path.display(), e))?;
    let (version, backup) = migrate(&conn, MIGRATIONS, &data_dir.join(BACKUP_DIR))
        .map_err(|e| DbError(format!("{}: {e}", path.display())))?;
    Ok((
        conn,
        Opened {
            path,
            version,
            backup,
        },
    ))
}

/// A fresh in-memory database at the latest schema, for tests.
#[cfg(test)]
pub fn memory() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory database");
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .expect("pragma");
    migrate(&conn, MIGRATIONS, Path::new("/nonexistent")).expect("migrations");
    conn
}

/// Applies the missing `migrations` and returns the resulting version and the
/// backup written before upgrading, if any.
fn migrate(
    conn: &Connection,
    migrations: &[&str],
    backup_dir: &Path,
) -> Result<(usize, Option<PathBuf>), DbError> {
    let stored: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|e| err("cannot read schema version", e))?;
    let current =
        usize::try_from(stored).map_err(|_| DbError(format!("invalid schema version {stored}")))?;
    let latest = migrations.len();
    if current > latest {
        return Err(DbError(format!(
            "schema version {current} is newer than this axioma supports ({latest}); \
             downgrades are not supported, install a newer release"
        )));
    }
    if current == latest {
        return Ok((current, None));
    }
    let backup = if current > 0 {
        Some(backup(conn, current, backup_dir)?)
    } else {
        None
    };
    for (index, sql) in migrations.iter().enumerate().skip(current) {
        let version = index + 1;
        let apply = || -> rusqlite::Result<()> {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", version as i64)?;
            tx.commit()
        };
        apply().map_err(|e| err(format!("migration {version} failed, nothing changed"), e))?;
    }
    Ok((latest, backup))
}

/// Writes a consistent copy of the database with `VACUUM INTO`.
fn backup(conn: &Connection, version: usize, dir: &Path) -> Result<PathBuf, DbError> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| err(format!("cannot create {}", dir.display()), e))?;
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("axioma-v{version}-{seconds}.db"));
    let target = path
        .to_str()
        .ok_or_else(|| DbError(format!("backup path is not UTF-8: {}", path.display())))?;
    conn.execute("VACUUM INTO ?1", [target])
        .map_err(|e| err(format!("cannot write backup {}", path.display()), e))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const M1: &str = "CREATE TABLE a (x INTEGER) STRICT; INSERT INTO a VALUES (1);";
    const M2: &str = "CREATE TABLE b (y INTEGER) STRICT;";

    fn version(conn: &Connection) -> i64 {
        conn.query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn creates_a_new_database_at_the_latest_version() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let (conn, opened) = open(&data).unwrap();
        assert_eq!(opened.version, MIGRATIONS.len());
        assert_eq!(opened.backup, None);
        let created: String = conn
            .query_row("SELECT created_at FROM installation", [], |row| row.get(0))
            .unwrap();
        assert!(created.ends_with('Z'), "{created}");
        let mode = std::fs::metadata(&data).unwrap().permissions();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
            0o700
        );
    }

    #[test]
    fn reopening_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        open(dir.path()).unwrap();
        let (_, opened) = open(dir.path()).unwrap();
        assert_eq!(opened.backup, None);
        assert!(!dir.path().join(BACKUP_DIR).exists());
    }

    #[test]
    fn upgrade_writes_a_backup_first() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open(dir.path().join(FILE_NAME)).unwrap();
        let backups = dir.path().join(BACKUP_DIR);
        assert_eq!(migrate(&conn, &[M1], &backups).unwrap(), (1, None));
        let (version, backup) = migrate(&conn, &[M1, M2], &backups).unwrap();
        assert_eq!(version, 2);
        let backup = Connection::open(backup.unwrap()).unwrap();
        assert_eq!(super::tests::version(&backup), 1);
        let rows: i64 = backup
            .query_row("SELECT count(*) FROM a", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn failed_migration_leaves_the_version_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open(dir.path().join(FILE_NAME)).unwrap();
        let backups = dir.path().join(BACKUP_DIR);
        migrate(&conn, &[M1], &backups).unwrap();
        let broken = "CREATE TABLE c (z INTEGER); THIS IS NOT SQL;";
        let message = migrate(&conn, &[M1, broken], &backups)
            .unwrap_err()
            .to_string();
        assert!(message.contains("migration 2 failed"), "{message}");
        assert_eq!(version(&conn), 1);
        let tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name = 'c'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
    }

    #[test]
    fn refuses_a_newer_database() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open(dir.path().join(FILE_NAME)).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        let message = migrate(&conn, &[M1], &dir.path().join(BACKUP_DIR))
            .unwrap_err()
            .to_string();
        assert!(message.contains("newer than this axioma"), "{message}");
        assert_eq!(version(&conn), 99);
    }
}
