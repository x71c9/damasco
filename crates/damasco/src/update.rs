//! Updates of damasco to a newer release.
//!
//! `damasco update`, run as root, downloads the installer of a release from
//! GitHub and runs it: the installer replaces the binary and the units and
//! restarts every installation on the machine, keeping configuration and
//! data. The panel cannot
//! do that itself, since the service runs unprivileged on a read-only
//! system: it leaves an empty request file in the data directory, and
//! `damasco-update.path` (`damasco-update@<name>.path` for a named
//! installation) starts `damasco-update.service`, which runs
//! `damasco update` as root. The request carries no data, so nothing the
//! service writes reaches what root runs.

use std::path::{Path, PathBuf};
use std::time::Duration;

const REPOSITORY: &str = "x71c9/damasco";

/// The file the panel creates to ask for an update.
pub const REQUEST: &str = "update-request";

/// The units that turn a request into an update, for the default
/// installation and for the named ones; installed by the installer.
const PATH_UNIT: &str = "/etc/systemd/system/damasco-update.path";
const PATH_TEMPLATE: &str = "/etc/systemd/system/damasco-update@.path";

/// Where installations keep their data: `damasco` for the default one,
/// `damasco-<name>` for the others.
const STATE: &str = "/var/lib";

/// The version of this binary.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .build()
        .into()
}

/// In development builds only: `DAMASCO_DEV_LATEST=v9.9.9` stands for the
/// latest release and makes the panel offer it, to see the update in
/// Settings without a newer release and without the update units. A press
/// of the button leaves `update-request` in the data directory: removing it
/// ends the wait.
fn dev_latest() -> Option<String> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var("DAMASCO_DEV_LATEST")
        .ok()
        .filter(|tag| version(tag).is_some())
}

/// The tag of the latest release with the major version of this binary,
/// such as `v1.2.3`. A new major version needs a new installation, so it is
/// never offered: the releases are listed instead of asking for GitHub's
/// "latest", which may belong to another major version.
pub fn latest() -> Result<String, String> {
    if let Some(tag) = dev_latest() {
        return Ok(tag);
    }
    let url = format!("https://api.github.com/repos/{REPOSITORY}/releases?per_page=30");
    let text = agent()
        .get(&url)
        .header("User-Agent", "damasco")
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|err| format!("GitHub: {err}"))?
        .body_mut()
        .read_to_string()
        .map_err(|err| format!("GitHub: {err}"))?;
    let releases: serde_json::Value =
        serde_json::from_str(&text).map_err(|err| format!("GitHub: {err}"))?;
    pick(&releases, CURRENT).ok_or_else(|| {
        let (major, _, _) = version(CURRENT).unwrap_or_default();
        format!("GitHub: no published release of damasco {major}")
    })
}

/// Of the releases GitHub lists, the highest published one (neither a draft
/// nor a pre-release) with the major version of `current`.
fn pick(releases: &serde_json::Value, current: &str) -> Option<String> {
    let (major, _, _) = version(current)?;
    releases
        .as_array()?
        .iter()
        .filter(|release| {
            !release["draft"].as_bool().unwrap_or(true)
                && !release["prerelease"].as_bool().unwrap_or(true)
        })
        .filter_map(|release| release["tag_name"].as_str())
        .filter_map(|tag| Some((version(tag)?, tag)))
        .filter(|((tag_major, _, _), _)| *tag_major == major)
        .max_by_key(|(parsed, _)| *parsed)
        .map(|(_, tag)| tag.to_string())
}

/// Whether `tag` has the major version of this binary.
fn same_major(tag: &str) -> bool {
    matches!((version(tag), version(CURRENT)), (Some(a), Some(b)) if a.0 == b.0)
}

/// `v1.2.3` or `1.2.3` as numbers, for comparing.
pub fn version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.trim().trim_start_matches('v').split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let parsed = (next()?, next()?, next()?);
    parts.next().is_none().then_some(parsed)
}

/// `tag` when it is newer than this binary and has the same major version.
pub fn newer(tag: &str) -> Option<String> {
    (same_major(tag) && version(tag)? > version(CURRENT)?).then(|| tag.to_string())
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
    // Set by damasco@.service for a named installation.
    let named = std::env::var_os("DAMASCO_INSTANCE").is_some_and(|name| !name.is_empty());
    Path::new(if named { PATH_TEMPLATE } else { PATH_UNIT }).is_file()
}

/// Whether this binary was installed with Nix (the flake's package or
/// module): then the system configuration decides its version, and neither
/// the panel nor `damasco update` replace it.
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
            name == "damasco" || name.starts_with("damasco-")
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
            "installed with Nix: update the damasco input of the system flake and rebuild".into(),
        );
    }
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        return Err("run as root, for example: sudo damasco update".into());
    }
    for path in requests(Path::new(STATE)) {
        // A symbolic link in its place is removed, not followed.
        match std::fs::remove_file(&path) {
            Ok(()) => eprintln!(
                "damasco: update requested from the panel ({})",
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
    if !same_major(&tag) {
        let (major, _, _) = version(&tag).unwrap_or_default();
        return Err(format!(
            "{tag}: damasco {major} needs a new installation, see \
             https://github.com/{REPOSITORY}/blob/master/docs/install.md"
        ));
    }
    if version(&tag) == version(CURRENT) {
        eprintln!("damasco: {tag} is already installed; installing it again");
    }
    let url = format!("https://github.com/{REPOSITORY}/releases/download/{tag}/install.sh");
    eprintln!("damasco: downloading the installer of {tag}");
    let script = agent()
        .get(&url)
        .call()
        .map_err(|err| format!("{url}: {err}"))?
        .body_mut()
        .read_to_string()
        .map_err(|err| format!("{url}: {err}"))?;
    let status = std::process::Command::new("bash")
        .arg("-s")
        .env("DAMASCO_VERSION", &tag)
        .env("DAMASCO_UPGRADE", "1")
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
        for name in ["damasco", "damasco-blog", "damascoish", "other"] {
            std::fs::create_dir(dir.path().join(name)).unwrap();
        }
        assert_eq!(
            requests(dir.path()),
            vec![
                dir.path().join("damasco").join(REQUEST),
                dir.path().join("damasco-blog").join(REQUEST),
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

    #[test]
    fn another_major_version_is_never_offered() {
        let (major, minor, patch) = version(CURRENT).unwrap();
        assert_eq!(newer(&format!("v{}.0.0", major + 1)), None);
        let next = format!("v{major}.{minor}.{}", patch + 1);
        assert_eq!(newer(&next), Some(next.clone()));
    }

    fn release(tag: &str, draft: bool, prerelease: bool) -> serde_json::Value {
        serde_json::json!({ "tag_name": tag, "draft": draft, "prerelease": prerelease })
    }

    #[test]
    fn the_latest_release_keeps_the_major_version() {
        let releases = serde_json::json!([
            release("v2.0.0", false, false),
            release("v1.2.1", false, false),
            release("v1.2.0", false, false),
            release("v1.1.1", false, false),
        ]);
        assert_eq!(pick(&releases, "1.2.0").as_deref(), Some("v1.2.1"));
        assert_eq!(pick(&releases, "1.1.1").as_deref(), Some("v1.2.1"));
        assert_eq!(pick(&releases, "2.0.0").as_deref(), Some("v2.0.0"));
        let only_next_major = serde_json::json!([release("v2.0.0", false, false)]);
        assert_eq!(pick(&only_next_major, "1.2.0"), None);
    }

    #[test]
    fn drafts_and_pre_releases_are_skipped() {
        let releases = serde_json::json!([
            release("v1.3.0", true, false),
            release("v1.2.2", false, true),
            release("v1.2.1", false, false),
            serde_json::json!({ "tag_name": "nightly", "draft": false, "prerelease": false }),
        ]);
        assert_eq!(pick(&releases, "1.2.0").as_deref(), Some("v1.2.1"));
        assert_eq!(pick(&serde_json::json!({}), "1.2.0"), None);
    }
}
