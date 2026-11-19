//! Updates of alogeno to a newer release.
//!
//! `alogeno update`, run as root, downloads the installer of a release from
//! GitHub and runs it: the installer replaces the binary and the units and
//! restarts every installation on the machine, keeping configuration and
//! data. The panel cannot
//! do that itself, since the service runs unprivileged on a read-only
//! system: it leaves an empty request file in the data directory, and
//! `alogeno-update.path` (`alogeno-update@<name>.path` for a named
//! installation) starts `alogeno-update.service`, which runs
//! `alogeno update` as root. The request carries no data, so nothing the
//! service writes reaches what root runs.

use std::path::{Path, PathBuf};
use std::time::Duration;

const REPOSITORY: &str = "x71c9/alogeno";

/// The file the panel creates to ask for an update.
pub const REQUEST: &str = "update-request";

/// The units that turn a request into an update, for the default
/// installation and for the named ones; installed by the installer.
const PATH_UNIT: &str = "/etc/systemd/system/alogeno-update.path";
const PATH_TEMPLATE: &str = "/etc/systemd/system/alogeno-update@.path";

/// Where installations keep their data: `alogeno` for the default one,
/// `alogeno-<name>` for the others.
const STATE: &str = "/var/lib";

/// The version of this binary.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .build()
        .into()
}

/// In development builds only: `ALOGENO_DEV_LATEST=v9.9.9` stands for the
/// latest release and makes the panel offer it, to see the update in
/// Settings without a newer release and without the update units. A press
/// of the button leaves `update-request` in the data directory: removing it
/// ends the wait.
fn dev_latest() -> Option<String> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var("ALOGENO_DEV_LATEST")
        .ok()
        .filter(|tag| version(tag).is_some())
}

/// The tag of the latest release, such as `v0.1.2`.
pub fn latest() -> Result<String, String> {
    if let Some(tag) = dev_latest() {
        return Ok(tag);
    }
    let url = format!("https://api.github.com/repos/{REPOSITORY}/releases/latest");
    let text = agent()
        .get(&url)
        .header("User-Agent", "alogeno")
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|err| format!("GitHub: {err}"))?
        .body_mut()
        .read_to_string()
        .map_err(|err| format!("GitHub: {err}"))?;
    let release: serde_json::Value =
        serde_json::from_str(&text).map_err(|err| format!("GitHub: {err}"))?;
    release["tag_name"]
        .as_str()
        .filter(|tag| version(tag).is_some())
        .map(String::from)
        .ok_or_else(|| "GitHub: the latest release has no version tag".into())
}

/// `v1.2.3` or `1.2.3` as numbers, for comparing.
pub fn version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.trim().trim_start_matches('v').split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let parsed = (next()?, next()?, next()?);
    parts.next().is_none().then_some(parsed)
}

/// `tag` when it is newer than this binary.
pub fn newer(tag: &str) -> Option<String> {
    (version(tag)? > version(CURRENT)?).then(|| tag.to_string())
}

/// Whether this installation can update from the panel: the unit that runs
/// requests is installed.
pub fn from_panel() -> bool {
    if dev_latest().is_some() {
        return true;
    }
    if by_nix() {
        return false;
    }
    // Set by alogeno@.service for a named installation.
    let named = std::env::var_os("ALOGENO_INSTANCE").is_some_and(|name| !name.is_empty());
    Path::new(if named { PATH_TEMPLATE } else { PATH_UNIT }).is_file()
}

/// Whether this binary was installed with Nix (the flake's package or
/// module): then the system configuration decides its version, and neither
/// the panel nor `alogeno update` replace it.
pub fn by_nix() -> bool {
    std::env::current_exe().is_ok_and(|exe| exe.starts_with("/nix/store"))
}

pub fn request_path(data_dir: &Path) -> PathBuf {
    data_dir.join(REQUEST)
}

/// The request files of every installation under `state`, present or not.
fn requests(state: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(state) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name == "alogeno" || name.starts_with("alogeno-")
        })
        .map(|entry| entry.path().join(REQUEST))
        .collect();
    found.sort();
    found
}

/// Downloads the installer of `tag` (the latest release when `None`) and
/// runs it as root in upgrade mode, for every installation on the machine.
/// Removes the pending requests of the panels first, so that the path units
/// do not start again.
pub fn run(tag: Option<&str>) -> Result<(), String> {
    if by_nix() {
        return Err(
            "installed with Nix: update the alogeno input of the system flake and rebuild".into(),
        );
    }
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        return Err("run as root, for example: sudo alogeno update".into());
    }
    for path in requests(Path::new(STATE)) {
        // A symbolic link in its place is removed, not followed.
        match std::fs::remove_file(&path) {
            Ok(()) => eprintln!(
                "alogeno: update requested from the panel ({})",
                path.display()
            ),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(format!("{}: {err}", path.display())),
        }
    }
    let tag = match tag {
        Some(tag) if version(tag).is_some() => format!("v{}", tag.trim_start_matches('v')),
        Some(tag) => return Err(format!("{tag} is not a version such as v0.1.2")),
        None => latest()?,
    };
    if version(&tag) == version(CURRENT) {
        eprintln!("alogeno: {tag} is already installed; installing it again");
    }
    let url = format!("https://github.com/{REPOSITORY}/releases/download/{tag}/install.sh");
    eprintln!("alogeno: downloading the installer of {tag}");
    let script = agent()
        .get(&url)
        .call()
        .map_err(|err| format!("{url}: {err}"))?
        .body_mut()
        .read_to_string()
        .map_err(|err| format!("{url}: {err}"))?;
    let status = std::process::Command::new("bash")
        .arg("-s")
        .env("ALOGENO_VERSION", &tag)
        .env("ALOGENO_UPGRADE", "1")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .take()
                .expect("piped stdin")
                .write_all(script.as_bytes())?;
            child.wait()
        })
        .map_err(|err| format!("running the installer: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("the installer of {tag} failed ({status})"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_numbers() {
        assert_eq!(version("v0.1.10"), Some((0, 1, 10)));
        assert_eq!(version("0.2.0"), Some((0, 2, 0)));
        assert_eq!(version("v1.2"), None);
        assert_eq!(version("v1.2.3.4"), None);
        assert_eq!(version("latest"), None);
        assert!(version("v0.1.10") > version("v0.1.9"));
    }

    #[test]
    fn requests_of_every_installation() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["alogeno", "alogeno-blog", "alogenoish", "other"] {
            std::fs::create_dir(dir.path().join(name)).unwrap();
        }
        assert_eq!(
            requests(dir.path()),
            vec![
                dir.path().join("alogeno").join(REQUEST),
                dir.path().join("alogeno-blog").join(REQUEST),
            ]
        );
        assert!(requests(&dir.path().join("missing")).is_empty());
    }

    #[test]
    fn only_newer_releases_are_offered() {
        let (major, minor, patch) = version(CURRENT).unwrap();
        assert_eq!(newer(&format!("v{major}.{minor}.{patch}")), None);
        assert_eq!(newer("v0.0.1"), None);
        let next = format!("v{major}.{minor}.{}", patch + 1);
        assert_eq!(newer(&next), Some(next.clone()));
        assert_eq!(newer("nonsense"), None);
    }
}
