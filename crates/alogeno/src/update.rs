//! Updates of alogeno to a newer release.
//!
//! `alogeno update`, run as root, downloads the installer of a release from
//! GitHub and runs it: the installer replaces the binary and the units and
//! restarts the service, keeping configuration and data. The panel cannot
//! do that itself, since the service runs unprivileged on a read-only
//! system: it leaves an empty request file in the data directory, and
//! `alogeno-update.path` starts `alogeno-update.service`, which runs
//! `alogeno update` as root. The request carries no data, so nothing the
//! service writes reaches what root runs.

use std::path::{Path, PathBuf};
use std::time::Duration;

const REPOSITORY: &str = "x71c9/alogeno";

/// The file the panel creates to ask for an update.
pub const REQUEST: &str = "update-request";

/// The unit that turns a request into an update; installed by the installer.
const PATH_UNIT: &str = "/etc/systemd/system/alogeno-update.path";

/// The version of this binary.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .build()
        .into()
}

/// The tag of the latest release, such as `v0.1.2`.
pub fn latest() -> Result<String, String> {
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
    Path::new(PATH_UNIT).is_file()
}

pub fn request_path(data_dir: &Path) -> PathBuf {
    data_dir.join(REQUEST)
}

/// Downloads the installer of `tag` (the latest release when `None`) and
/// runs it as root. Removes a pending request from the panel first, so that
/// the path unit does not start again.
pub fn run(data_dir: &Path, tag: Option<&str>) -> Result<(), String> {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        return Err("run as root, for example: sudo alogeno update".into());
    }
    // A symbolic link in its place is removed, not followed.
    match std::fs::remove_file(request_path(data_dir)) {
        Ok(()) => eprintln!("alogeno: update requested from the panel"),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(format!("{}: {err}", request_path(data_dir).display())),
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
    fn only_newer_releases_are_offered() {
        let (major, minor, patch) = version(CURRENT).unwrap();
        assert_eq!(newer(&format!("v{major}.{minor}.{patch}")), None);
        assert_eq!(newer("v0.0.1"), None);
        let next = format!("v{major}.{minor}.{}", patch + 1);
        assert_eq!(newer(&next), Some(next.clone()));
        assert_eq!(newer("nonsense"), None);
    }
}
