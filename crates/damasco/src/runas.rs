//! The commands that act on an installation (`owner-link`, `check`,
//! `runtimes`, `build`), started as root, run as the installation's own
//! user, the owner of its data directory, as `runuser` would: what they
//! write stays readable by the service, and `check` tests the sandbox as the
//! service runs it. So `sudo damasco owner-link` is enough.

use std::ffi::{CStr, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// The configuration of the named installation `instance`
/// (`/etc/damasco-<name>/damasco.toml`), or `config` without one.
pub fn config_path(instance: Option<&str>, config: &Path) -> Result<PathBuf, String> {
    let Some(name) = instance else {
        return Ok(config.to_path_buf());
    };
    let valid = (1..=32).contains(&name.len())
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !valid {
        return Err(format!(
            "--instance {name}: lower-case letters, digits and -, up to 32"
        ));
    }
    Ok(PathBuf::from(format!("/etc/damasco-{name}/damasco.toml")))
}

/// The user and group that own `data_dir`: the installation's own.
pub fn owner_of(data_dir: &Path) -> Result<(u32, u32), String> {
    let meta = std::fs::metadata(data_dir).map_err(|err| {
        format!(
            "{}: {err}; is damasco installed with this configuration?",
            data_dir.display()
        )
    })?;
    service_user(data_dir, meta.uid(), meta.gid())
}

/// Refuses a data directory owned by root: running as root would leave
/// files the service cannot read.
fn service_user(data_dir: &Path, uid: u32, gid: u32) -> Result<(u32, u32), String> {
    if uid == 0 {
        return Err(format!(
            "{} belongs to root, not to the user of an installation",
            data_dir.display()
        ));
    }
    Ok((uid, gid))
}

/// Becomes the user `uid` for good, with its group, its supplementary
/// groups and its HOME, USER and LOGNAME. Runs before any other thread
/// exists: the identity and the environment change for the whole process.
pub fn become_user(uid: u32, gid: u32) -> Result<(), String> {
    let last = |what: &str| format!("{what}: {}", std::io::Error::last_os_error());
    // SAFETY: getpwuid returns static storage, copied at once and before
    // any other call that could overwrite it.
    let (name, home) = unsafe {
        let entry = libc::getpwuid(uid);
        if entry.is_null() {
            return Err(format!("no user with uid {uid}"));
        }
        (
            CStr::from_ptr((*entry).pw_name).to_owned(),
            CStr::from_ptr((*entry).pw_dir).to_owned(),
        )
    };
    // SAFETY: plain system calls on values checked above; the groups go
    // before the group, the group before the user, as root is needed for
    // each.
    unsafe {
        if libc::initgroups(name.as_ptr(), gid) != 0 {
            return Err(last("initgroups"));
        }
        if libc::setgid(gid) != 0 {
            return Err(last("setgid"));
        }
        if libc::setuid(uid) != 0 {
            return Err(last("setuid"));
        }
        // Root must be gone for good.
        if libc::setuid(0) == 0 {
            return Err("still able to become root after setuid".into());
        }
        // SAFETY: no other thread exists yet (see the function's comment).
        std::env::set_var("HOME", OsStr::from_bytes(home.to_bytes()));
        std::env::set_var("USER", OsStr::from_bytes(name.to_bytes()));
        std::env::set_var("LOGNAME", OsStr::from_bytes(name.to_bytes()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_instance_names_its_configuration() {
        let default = Path::new("/etc/damasco/damasco.toml");
        assert_eq!(config_path(None, default).unwrap(), default);
        assert_eq!(
            config_path(Some("blog"), default).unwrap(),
            Path::new("/etc/damasco-blog/damasco.toml")
        );
        for bad in ["", "-x", "Blog", "../x", "a/b", &"x".repeat(33)] {
            assert!(config_path(Some(bad), default).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_data_directory_names_the_user() {
        let dir = tempfile::tempdir().unwrap();
        let meta = std::fs::metadata(dir.path()).unwrap();
        assert_eq!(owner_of(dir.path()).unwrap(), (meta.uid(), meta.gid()));
        let missing = owner_of(&dir.path().join("none")).unwrap_err();
        assert!(missing.contains("is damasco installed"), "{missing}");
        let root = service_user(Path::new("/var/lib/damasco"), 0, 0).unwrap_err();
        assert!(root.contains("belongs to root"), "{root}");
    }
}
