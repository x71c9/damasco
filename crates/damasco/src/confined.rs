//! Files in the folders the agent and the builds write (a site, a home of
//! the assistant), reached by damasco from outside the sandbox.
//!
//! What is there may be a link to anywhere on the machine, put in place at
//! any moment. So a path under such a folder is walked one name at a time,
//! each opened in the folder already open and never through a link, and
//! only regular files are read or copied. A path checked first and used
//! later would not do: it can change in between.

use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{Error, ErrorKind, Read, Result, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

/// A folder, held open.
pub struct Dir(File);

impl Dir {
    /// Opens a folder that is damasco's own: the root of what is confined.
    pub fn root(path: &Path) -> Result<Self> {
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(path)
            .map(Self)
    }

    /// The path of `name` in this very folder, wherever it was moved
    /// meanwhile. Only the last name is left to resolve, which each use
    /// does without following a link.
    fn at(&self, name: &OsStr) -> Result<PathBuf> {
        let mut parts = Path::new(name).components();
        match (parts.next(), parts.next()) {
            (Some(Component::Normal(_)), None) => Ok(self.path().join(name)),
            _ => Err(Error::new(
                ErrorKind::InvalidInput,
                "not the name of a file",
            )),
        }
    }

    fn path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.0.as_raw_fd()))
    }

    /// The folder `name` in this one.
    pub fn dir(&self, name: impl AsRef<OsStr>) -> Result<Self> {
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(self.at(name.as_ref())?)
            .map(Self)
    }

    /// The regular file `name` in this folder.
    pub fn file(&self, name: impl AsRef<OsStr>) -> Result<File> {
        // Non-blocking: a pipe in the place of a file would wait forever.
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(self.at(name.as_ref())?)?;
        if file.metadata()?.is_file() {
            Ok(file)
        } else {
            Err(Error::new(ErrorKind::InvalidData, "not a regular file"))
        }
    }

    /// What `name` is, itself.
    pub fn kind(&self, name: impl AsRef<OsStr>) -> Result<std::fs::FileType> {
        Ok(std::fs::symlink_metadata(self.at(name.as_ref())?)?.file_type())
    }

    /// The names in this folder, with what each one is.
    pub fn entries(&self) -> Result<Vec<(OsString, std::fs::FileType)>> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(self.path())? {
            let entry = entry?;
            // Gone meanwhile: not there.
            if let Ok(kind) = entry.file_type() {
                found.push((entry.file_name(), kind));
            }
        }
        Ok(found)
    }

    /// The folders of `parts` in this one, made with `mode` where missing.
    fn made(self, parts: &[&OsStr], mode: u32) -> Result<Self> {
        let mut dir = self;
        for part in parts {
            dir = match dir.dir(part) {
                Err(err) if err.kind() == ErrorKind::NotFound => {
                    match std::fs::DirBuilder::new().mode(mode).create(dir.at(part)?) {
                        Err(err) if err.kind() != ErrorKind::AlreadyExists => return Err(err),
                        _ => dir.dir(part)?,
                    }
                }
                found => found?,
            };
        }
        Ok(dir)
    }

    /// Puts `bytes` at `name` through a new file renamed over it: a link
    /// there is replaced, not followed.
    fn write(&self, name: &OsStr, bytes: &[u8], mode: u32) -> Result<()> {
        let temporary = self.at(OsStr::new(&format!(".damasco-{}.new", crate::site::ulid())))?;
        let target = self.at(name)?;
        let written = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(mode)
                .open(&temporary)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            std::fs::rename(&temporary, &target)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        written
    }

    /// Moves `name` to `target`, which is damasco's own. A link moves as
    /// itself.
    pub fn move_out(&self, name: impl AsRef<OsStr>, target: &Path) -> Result<()> {
        std::fs::rename(self.at(name.as_ref())?, target)
    }

    /// Which folder this is, so that two can be told apart.
    pub fn identity(&self) -> Result<(u64, u64)> {
        let meta = self.0.metadata()?;
        Ok((meta.dev(), meta.ino()))
    }
}

/// The names of a relative path: no `..`, nothing absolute.
fn names(path: &Path) -> Result<Vec<&OsStr>> {
    path.components()
        .map(|part| match part {
            Component::Normal(name) => Ok(name),
            _ => Err(Error::new(ErrorKind::InvalidInput, "not a path inside")),
        })
        .collect()
}

/// The folder `path` under `root`.
pub fn dir(root: &Path, path: impl AsRef<Path>) -> Result<Dir> {
    names(path.as_ref())?
        .into_iter()
        .try_fold(Dir::root(root)?, |dir, name| dir.dir(name))
}

/// The folder `path` under `root`, made with `mode` where missing.
pub fn make_dir(root: &Path, path: impl AsRef<Path>, mode: u32) -> Result<Dir> {
    Dir::root(root)?.made(&names(path.as_ref())?, mode)
}

/// The folder that holds `path` under `root`, and the last name of it.
fn parent<'a>(root: &Path, path: &'a Path) -> Result<(Dir, &'a OsStr)> {
    let mut parts = names(path)?;
    let name = parts
        .pop()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "no name"))?;
    let dir = parts
        .into_iter()
        .try_fold(Dir::root(root)?, |dir, part| dir.dir(part))?;
    Ok((dir, name))
}

/// The regular file `path` under `root`.
pub fn file(root: &Path, path: impl AsRef<Path>) -> Result<File> {
    let (dir, name) = parent(root, path.as_ref())?;
    dir.file(name)
}

pub fn read(root: &Path, path: impl AsRef<Path>) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file(root, path)?.read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub fn read_to_string(root: &Path, path: impl AsRef<Path>) -> Result<String> {
    let mut text = String::new();
    file(root, path)?.read_to_string(&mut text)?;
    Ok(text)
}

/// Whether something is at `path` under `root`, whatever it is.
pub fn present(root: &Path, path: impl AsRef<Path>) -> bool {
    parent(root, path.as_ref()).is_ok_and(|(dir, name)| dir.kind(name).is_ok())
}

/// Puts `bytes` at `path` under `root`, making the folders missing: they
/// are open to their owner only when the file is.
pub fn write(root: &Path, path: impl AsRef<Path>, bytes: &[u8], mode: u32) -> Result<()> {
    let mut parts = names(path.as_ref())?;
    let name = parts
        .pop()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "no name"))?;
    let folders = if mode & 0o077 == 0 { 0o700 } else { 0o777 };
    Dir::root(root)?
        .made(&parts, folders)?
        .write(name, bytes, mode)
}

/// Removes the file or the link `path` under `root`.
pub fn remove(root: &Path, path: impl AsRef<Path>) -> Result<()> {
    let (dir, name) = parent(root, path.as_ref())?;
    std::fs::remove_file(dir.at(name)?)
}

/// The names of the `.json` files in the folder `path` under `root`, in
/// order; `None` when the folder is not there.
pub fn json_files(root: &Path, path: impl AsRef<Path>) -> Result<Option<Vec<String>>> {
    let dir = match dir(root, path) {
        Ok(dir) => dir,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let mut found: Vec<String> = dir
        .entries()?
        .into_iter()
        .filter_map(|(name, _)| name.into_string().ok())
        .filter(|name| Path::new(name).extension().is_some_and(|e| e == "json"))
        .collect();
    found.sort();
    Ok(Some(found))
}

/// Takes away links, and files in the place of folders, along `path` under
/// `root`: where a sandbox mounts, a link would move the mount elsewhere.
pub fn clear_way(root: &Path, path: impl AsRef<Path>) -> Result<()> {
    let parts = names(path.as_ref())?;
    let mut here = match Dir::root(root) {
        Ok(dir) => dir,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    for (index, part) in parts.iter().enumerate() {
        let last = index + 1 == parts.len();
        let kind = match here.kind(part) {
            Ok(kind) => kind,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err),
        };
        if kind.is_symlink() || (!last && !kind.is_dir()) {
            return match std::fs::remove_file(here.at(part)?) {
                Err(err) if err.kind() != ErrorKind::NotFound => Err(err),
                _ => Ok(()),
            };
        }
        if last {
            break;
        }
        here = here.dir(part)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// A confined folder, and a place outside it.
    fn places() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let (root, outside) = (dir.path().join("root"), dir.path().join("outside"));
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("file"), "outside").unwrap();
        (dir, root, outside)
    }

    #[test]
    fn nothing_is_read_through_a_link() {
        let (_dir, root, outside) = places();
        symlink(&outside, root.join("folder")).unwrap();
        symlink(outside.join("file"), root.join("real/file")).unwrap();
        assert!(read(&root, "folder/file").is_err());
        assert!(read(&root, "real/file").is_err());
        assert!(dir(&root, "folder").is_err());
        assert!(json_files(&root, "folder").is_err());
        assert!(read(&root, "../outside/file").is_err());
        assert!(read(&root, outside.join("file")).is_err());
        assert!(present(&root, "real/file") && !present(&root, "folder/file"));
        // A folder is no file.
        assert!(read(&root, "real").is_err());
    }

    #[test]
    fn nothing_is_written_through_a_link() {
        let (_dir, root, outside) = places();
        symlink(&outside, root.join("folder")).unwrap();
        assert!(write(&root, "folder/new", b"x", 0o600).is_err());
        assert!(write(&root, "folder/deeper/new", b"x", 0o600).is_err());
        assert!(!outside.join("new").exists() && !outside.join("deeper").exists());

        // A link in the place of the file gives way to it.
        symlink(outside.join("file"), root.join("real/file")).unwrap();
        write(&root, "real/file", b"inside", 0o600).unwrap();
        assert_eq!(read(&root, "real/file").unwrap(), b"inside");
        // So does one to a file that is not there.
        symlink(outside.join("absent"), root.join("real/absent")).unwrap();
        write(&root, "real/absent", b"inside", 0o600).unwrap();
        assert!(!outside.join("absent").exists());
        assert_eq!(
            std::fs::read_to_string(outside.join("file")).unwrap(),
            "outside"
        );

        assert!(remove(&root, "folder/file").is_err());
        assert!(outside.join("file").exists());
    }

    #[test]
    fn a_folder_open_stays_the_same_when_its_name_is_swapped() {
        let (_dir, root, outside) = places();
        std::fs::write(root.join("real/file"), "inside").unwrap();
        let held = dir(&root, "real").unwrap();
        std::fs::rename(root.join("real"), root.join("aside")).unwrap();
        symlink(&outside, root.join("real")).unwrap();
        let mut text = String::new();
        held.file("file")
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "inside");
        assert!(read(&root, "real/file").is_err());
    }

    #[test]
    fn the_way_to_a_mount_is_cleared_of_links() {
        let (_dir, root, outside) = places();
        symlink(&outside, root.join("home")).unwrap();
        clear_way(&root, "home/file").unwrap();
        assert!(!present(&root, "home"));
        assert!(
            outside.join("file").exists(),
            "the link goes, not what it named"
        );
        symlink(outside.join("file"), root.join("real/file")).unwrap();
        clear_way(&root, "real/file").unwrap();
        assert!(!present(&root, "real/file") && outside.join("file").exists());
        std::fs::write(root.join("real/file"), "inside").unwrap();
        clear_way(&root, "real/file").unwrap();
        assert!(present(&root, "real/file"), "what is in its place stays");
    }
}
