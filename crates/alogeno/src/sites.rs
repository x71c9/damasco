//! The sites of the installation: a row of `installation.db` each, and a
//! folder `data_dir/sites/<id>` laid out as the data directory of alogeno
//! 1.x was for its only site (`site/`, `media/`, `releases/`, `build/`),
//! plus the site's own database, `site.db`.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};

/// Names a slug may not take: they are parts of the panel's addresses.
const RESERVED: &[&str] = &["preview", "admin", "www", "api"];

/// One site as `installation.db` lists it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Site {
    /// ULID, never changes; names the folder.
    pub id: String,
    /// Names the preview and the panel's addresses; never changes either.
    pub slug: String,
    pub name: String,
}

/// The folder of the site `id`.
pub fn root(data_dir: &Path, id: &str) -> PathBuf {
    data_dir.join("sites").join(id)
}

/// A slug: 1 to 32 of `a-z`, `0-9` and `-`, starting and ending with a
/// letter or digit, without `--` (and so without `xn--`), and none of the
/// reserved names.
pub fn check_slug(slug: &str) -> Result<(), String> {
    let valid_chars = slug
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    let ends = |b: Option<u8>| b.is_some_and(|b| b != b'-');
    if slug.is_empty()
        || slug.len() > 32
        || !valid_chars
        || !ends(slug.bytes().next())
        || !ends(slug.bytes().last())
        || slug.contains("--")
    {
        return Err(format!(
            "{slug:?}: 1 to 32 lowercase letters, digits and single hyphens, \
             starting and ending with a letter or digit"
        ));
    }
    if RESERVED.contains(&slug) {
        return Err(format!("{slug:?} is reserved"));
    }
    Ok(())
}

/// Every site, oldest first.
pub fn list(conn: &Connection) -> rusqlite::Result<Vec<Site>> {
    let mut statement = conn.prepare("SELECT id, slug, name FROM site ORDER BY created_at, id")?;
    statement
        .query_map([], |row| {
            Ok(Site {
                id: row.get(0)?,
                slug: row.get(1)?,
                name: row.get(2)?,
            })
        })?
        .collect()
}

/// Creates a site: first its folder, with the repository made from the
/// template, then its row. A creation cut short leaves a folder without a
/// row, which `sweep` removes at the next start; never a row without its
/// folder.
pub fn create(conn: &Connection, data_dir: &Path, name: &str, slug: &str) -> Result<Site, String> {
    check_slug(slug)?;
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err("the name of a site has 1 to 80 characters".into());
    }
    let taken: bool = conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM site WHERE slug = ?1)",
            [slug],
            |row| row.get(0),
        )
        .map_err(|err| err.to_string())?;
    if taken {
        return Err(format!("another site is already called {slug:?}"));
    }
    let id = crate::site::ulid();
    let root = root(data_dir, &id);
    crate::site::Site::at(&root).create(name, "en")?;
    conn.execute(
        "INSERT INTO site (id, slug, name) VALUES (?1, ?2, ?3)",
        params![id, slug, name],
    )
    .map_err(|err| err.to_string())?;
    Ok(Site {
        id,
        slug: slug.to_string(),
        name: name.to_string(),
    })
}

/// Removes the site's row: its addresses go with it. The folder goes
/// next, with `remove_tree`, or with `sweep` at the next start.
pub fn delete(conn: &Connection, id: &str) -> rusqlite::Result<bool> {
    Ok(conn.execute("DELETE FROM site WHERE id = ?1", [id])? > 0)
}

/// Removes the folders under `data_dir/sites` that no site owns, left by a
/// creation cut short. Returns what it removed.
pub fn sweep(conn: &Connection, data_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let dir = data_dir.join("sites");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let owned: Vec<String> = list(conn)
        .map_err(|err| err.to_string())?
        .into_iter()
        .map(|site| site.id)
        .collect();
    let mut removed = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if owned.contains(&name) || !entry.path().is_dir() {
            continue;
        }
        remove_tree(&entry.path()).map_err(|err| format!("{}: {err}", entry.path().display()))?;
        removed.push(entry.path());
    }
    Ok(removed)
}

/// Removes a folder whose builds left read-only directories (releases are
/// sealed): every directory becomes writable first.
pub fn remove_tree(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fn writable(path: &Path) -> std::io::Result<()> {
        let meta = std::fs::symlink_metadata(path)?;
        if meta.is_dir() {
            let mut permissions = meta.permissions();
            permissions.set_mode(permissions.mode() | 0o700);
            std::fs::set_permissions(path, permissions)?;
            for entry in std::fs::read_dir(path)? {
                writable(&entry?.path())?;
            }
        }
        Ok(())
    }
    writable(path)?;
    std::fs::remove_dir_all(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_follow_the_rules() {
        for good in ["blog", "a", "shop-2", "x1"] {
            assert!(check_slug(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "Blog",
            "-a",
            "a-",
            "a--b",
            "xn--caff-dsa",
            "a.b",
            "a b",
            "preview",
            "admin",
            "www",
            "api",
        ] {
            assert!(check_slug(bad).is_err(), "{bad}");
        }
        assert!(check_slug(&"a".repeat(32)).is_ok());
        assert!(check_slug(&"a".repeat(33)).is_err());
    }

    #[test]
    fn two_sites_get_separate_folders_and_slugs_stay_unique() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::memory_installation();
        let blog = create(&conn, dir.path(), "Blog", "blog").unwrap();
        let shop = create(&conn, dir.path(), "Shop", "shop").unwrap();
        assert_ne!(blog.id, shop.id);
        for site in [&blog, &shop] {
            assert!(root(dir.path(), &site.id).join("site/.git").is_dir());
        }
        assert_eq!(list(&conn).unwrap(), [blog, shop]);
        assert!(create(&conn, dir.path(), "Again", "blog").is_err());
        assert!(create(&conn, dir.path(), "  ", "other").is_err());
    }

    #[test]
    fn a_folder_without_a_site_is_swept() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::memory_installation();
        let kept = create(&conn, dir.path(), "Blog", "blog").unwrap();
        // A creation cut short, with a read-only folder as a sealed release.
        let orphan = root(dir.path(), "01ORPHAN");
        let sealed = orphan.join("releases/R1");
        std::fs::create_dir_all(&sealed).unwrap();
        std::fs::write(sealed.join("index.html"), "x").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o555)).unwrap();
        assert_eq!(
            sweep(&conn, dir.path()).unwrap(),
            std::slice::from_ref(&orphan)
        );
        assert!(!orphan.exists());
        assert!(root(dir.path(), &kept.id).is_dir());
    }
}
