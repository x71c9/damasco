//! The databases: `installation.db` in the data directory (the owner, the
//! sessions, the sites, the assistant in use, the activity log) and one
//! `site.db` in each site's folder (its conversations, publications and
//! preview sessions).
//!
//! The schema version is stored in SQLite's `user_version`. Migrations are SQL
//! files compiled into the binary and applied in order, each in its own
//! transaction. Before upgrading an existing database, a copy is written to
//! `backups/` next to it. A database newer than the binary is refused:
//! downgrades are not supported.

use std::fmt;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

/// Migrations of `installation.db` in order; migration `n` (1-based) brings
/// the schema to version `n`.
pub const INSTALLATION_MIGRATIONS: &[&str] = &[
    include_str!("../migrations/installation/0001_installation.sql"),
    include_str!("../migrations/installation/0002_domain.sql"),
];

/// Migrations of each site's `site.db`.
pub const SITE_MIGRATIONS: &[&str] = &[
    include_str!("../migrations/site/0001_site.sql"),
    include_str!("../migrations/site/0002_connector.sql"),
    include_str!("../migrations/site/0003_attachment.sql"),
    include_str!("../migrations/site/0004_effort.sql"),
];

const INSTALLATION_FILE: &str = "installation.db";
const SITE_FILE: &str = "site.db";
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

/// Outcome of opening a database.
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

/// Creates the data directory if needed, opens `installation.db` and brings
/// its schema to the latest version.
pub fn open_installation(data_dir: &Path) -> Result<(Connection, Opened), DbError> {
    let path = data_dir.join(INSTALLATION_FILE);
    open(data_dir, &path, INSTALLATION_MIGRATIONS, "installation")
}

/// Opens the `site.db` of the site whose folder is `root`, creating the
/// folder if needed, and brings its schema to the latest version.
pub fn open_site(root: &Path) -> Result<(Connection, Opened), DbError> {
    open(root, &root.join(SITE_FILE), SITE_MIGRATIONS, "site")
}

/// Opens the database at `path` in `dir`, applying `migrations`; backups go
/// to `dir/backups`, named after `name`.
fn open(
    dir: &Path,
    path: &Path,
    migrations: &[&str],
    name: &str,
) -> Result<(Connection, Opened), DbError> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| err(format!("cannot create {}", dir.display()), e))?;
    let conn = Connection::open(path).map_err(|e| err(path.display(), e))?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;",
    )
    .map_err(|e| err(path.display(), e))?;
    let (version, backup) = migrate(&conn, migrations, &dir.join(BACKUP_DIR), name)
        .map_err(|e| DbError(format!("{}: {e}", path.display())))?;
    Ok((
        conn,
        Opened {
            path: path.to_path_buf(),
            version,
            backup,
        },
    ))
}

#[cfg(test)]
fn memory(migrations: &[&str]) -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory database");
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .expect("pragma");
    migrate(&conn, migrations, Path::new("/nonexistent"), "memory").expect("migrations");
    conn
}

/// A fresh in-memory installation database, for tests.
#[cfg(test)]
pub fn memory_installation() -> Connection {
    memory(INSTALLATION_MIGRATIONS)
}

/// A fresh in-memory site database, for tests.
#[cfg(test)]
pub fn memory_site() -> Connection {
    memory(SITE_MIGRATIONS)
}

/// Applies the missing `migrations` and returns the resulting version and the
/// backup written before upgrading, if any.
fn migrate(
    conn: &Connection,
    migrations: &[&str],
    backup_dir: &Path,
    name: &str,
) -> Result<(usize, Option<PathBuf>), DbError> {
    let stored: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|e| err("cannot read schema version", e))?;
    let current =
        usize::try_from(stored).map_err(|_| DbError(format!("invalid schema version {stored}")))?;
    let latest = migrations.len();
    if current > latest {
        return Err(DbError(format!(
            "schema version {current} is newer than this damasco supports ({latest}); \
             downgrades are not supported, install a newer release"
        )));
    }
    if current == latest {
        return Ok((current, None));
    }
    let backup = if current > 0 {
        Some(backup(conn, current, backup_dir, name)?)
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
fn backup(conn: &Connection, version: usize, dir: &Path, name: &str) -> Result<PathBuf, DbError> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| err(format!("cannot create {}", dir.display()), e))?;
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("{name}-v{version}-{seconds}.db"));
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
    fn creates_a_new_installation_at_the_latest_version() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let (conn, opened) = open_installation(&data).unwrap();
        assert_eq!(opened.version, INSTALLATION_MIGRATIONS.len());
        assert_eq!(opened.backup, None);
        assert_eq!(opened.path, data.join("installation.db"));
        let (created, instance): (String, String) = conn
            .query_row(
                "SELECT created_at, instance_id FROM installation",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(created.ends_with('Z'), "{created}");
        assert_eq!(instance.len(), 32, "{instance}");
        let mode = std::fs::metadata(&data).unwrap().permissions();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
            0o700
        );
    }

    #[test]
    fn a_site_gets_its_own_database_in_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sites").join("01SITE");
        let (conn, opened) = open_site(&root).unwrap();
        assert_eq!(opened.path, root.join("site.db"));
        let tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name IN ('conversation', 'publication')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 2);
    }

    #[test]
    fn reopening_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        open_installation(dir.path()).unwrap();
        let (_, opened) = open_installation(dir.path()).unwrap();
        assert_eq!(opened.backup, None);
        assert!(!dir.path().join(BACKUP_DIR).exists());
    }

    #[test]
    fn upgrade_writes_a_backup_named_after_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open(dir.path().join(SITE_FILE)).unwrap();
        let backups = dir.path().join(BACKUP_DIR);
        assert_eq!(migrate(&conn, &[M1], &backups, "site").unwrap(), (1, None));
        let (version, backup) = migrate(&conn, &[M1, M2], &backups, "site").unwrap();
        assert_eq!(version, 2);
        let backup = backup.unwrap();
        let name = backup.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("site-v1-"), "{name}");
        let backup = Connection::open(backup).unwrap();
        assert_eq!(super::tests::version(&backup), 1);
        let rows: i64 = backup
            .query_row("SELECT count(*) FROM a", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn failed_migration_leaves_the_version_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open(dir.path().join(SITE_FILE)).unwrap();
        let backups = dir.path().join(BACKUP_DIR);
        migrate(&conn, &[M1], &backups, "site").unwrap();
        let broken = "CREATE TABLE c (z INTEGER); THIS IS NOT SQL;";
        let message = migrate(&conn, &[M1, broken], &backups, "site")
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
        let conn = Connection::open(dir.path().join(SITE_FILE)).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        let message = migrate(&conn, &[M1], &dir.path().join(BACKUP_DIR), "site")
            .unwrap_err()
            .to_string();
        assert!(message.contains("newer than this damasco"), "{message}");
        assert_eq!(version(&conn), 99);
    }
}
