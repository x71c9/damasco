//! Checks of the machine damasco runs on.
//!
//! The agent and the builds will run inside bubblewrap, with memory, CPU and
//! process limits applied through cgroups delegated by systemd. These checks
//! say whether the machine allows that. A failed check never stops the
//! panel: features that need the sandbox stay disabled until it passes.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(match self {
            Level::Ok => "ok",
            Level::Warn => "warn",
            Level::Fail => "FAIL",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub name: &'static str,
    pub level: Level,
    pub detail: String,
}

impl Item {
    fn new(name: &'static str, level: Level, detail: impl Into<String>) -> Self {
        Self {
            name,
            level,
            detail: detail.into(),
        }
    }
}

/// Where the checks look. `root` is `/` on a real machine and a temporary
/// directory in tests.
pub struct System {
    pub root: PathBuf,
    /// Directories searched for `bwrap`.
    pub path: Option<OsString>,
    /// True when running as a systemd service (`INVOCATION_ID` is set).
    pub as_service: bool,
}

impl System {
    pub fn current() -> Self {
        Self {
            root: PathBuf::from("/"),
            path: std::env::var_os("PATH"),
            as_service: std::env::var_os("INVOCATION_ID").is_some(),
        }
    }

    fn file(&self, path: &str) -> PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }

    fn read(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(self.file(path)).ok()
    }
}

/// Runs every check, in order.
pub fn run(system: &System) -> Vec<Item> {
    vec![
        systemd(system),
        cgroup_v2(system),
        delegation(system),
        user_namespaces(system),
        bubblewrap(system),
        git(system),
    ]
}

fn systemd(system: &System) -> Item {
    if system.file("/run/systemd/system").is_dir() {
        Item::new("systemd", Level::Ok, "systemd is the init system")
    } else {
        Item::new(
            "systemd",
            Level::Fail,
            "systemd is not the init system; damasco runs as a systemd service",
        )
    }
}

fn cgroup_v2(system: &System) -> Item {
    match system.read("/sys/fs/cgroup/cgroup.controllers") {
        Some(_) => Item::new("cgroup v2", Level::Ok, "unified hierarchy mounted"),
        None => Item::new(
            "cgroup v2",
            Level::Fail,
            "the unified cgroup hierarchy (cgroup v2) is not mounted at /sys/fs/cgroup",
        ),
    }
}

/// The service needs its own cgroup subtree, writable and with the memory,
/// cpu and pids controllers, to place the sandboxes under limits.
fn delegation(system: &System) -> Item {
    const NAME: &str = "delegation";
    const NEEDED: [&str; 3] = ["memory", "cpu", "pids"];
    let missing_level = if system.as_service {
        Level::Fail
    } else {
        Level::Warn
    };
    let Some(own) = system.read("/proc/self/cgroup").and_then(|text| {
        text.lines()
            .find_map(|l| l.strip_prefix("0::").map(String::from))
    }) else {
        return Item::new(
            NAME,
            missing_level,
            "own cgroup not found in /proc/self/cgroup",
        );
    };
    // In the service damasco sits in the `main` leaf (DelegateSubgroup=main);
    // the delegated cgroup is its parent, where damasco enables the
    // controllers for the sandbox groups next to `main`.
    let (own, control) = match own.strip_suffix("/main") {
        Some(parent) => (parent.to_string(), "cgroup.subtree_control"),
        None => (own, "cgroup.procs"),
    };
    let dir = system.file(&format!("/sys/fs/cgroup{own}"));
    let controllers = std::fs::read_to_string(dir.join("cgroup.controllers")).unwrap_or_default();
    let missing: Vec<&str> = NEEDED
        .into_iter()
        .filter(|c| !controllers.split_whitespace().any(|have| have == *c))
        .collect();
    let writable = std::fs::OpenOptions::new()
        .write(true)
        .open(dir.join(control))
        .is_ok();
    let outside = if system.as_service {
        ""
    } else {
        " (expected outside the systemd service)"
    };
    if !writable {
        Item::new(
            NAME,
            missing_level,
            format!("cgroup {own} is not delegated to damasco{outside}"),
        )
    } else if !missing.is_empty() {
        Item::new(
            NAME,
            missing_level,
            format!(
                "cgroup {own} lacks controllers: {}{outside}",
                missing.join(", ")
            ),
        )
    } else {
        Item::new(
            NAME,
            Level::Ok,
            format!("cgroup {own} with memory, cpu and pids"),
        )
    }
}

fn user_namespaces(system: &System) -> Item {
    const NAME: &str = "user namespaces";
    let number = |path: &str| {
        system
            .read(path)
            .and_then(|text| text.trim().parse::<u64>().ok())
    };
    if number("/proc/sys/user/max_user_namespaces") == Some(0) {
        return Item::new(NAME, Level::Fail, "disabled: user.max_user_namespaces is 0");
    }
    if number("/proc/sys/kernel/unprivileged_userns_clone") == Some(0) {
        return Item::new(
            NAME,
            Level::Fail,
            "disabled for unprivileged users: kernel.unprivileged_userns_clone is 0",
        );
    }
    if number("/proc/sys/kernel/apparmor_restrict_unprivileged_userns") == Some(1) {
        if system.file("/etc/apparmor.d/damasco-bwrap").is_file() {
            return Item::new(
                NAME,
                Level::Ok,
                "restricted by AppArmor; the damasco-bwrap profile lets bubblewrap use them",
            );
        }
        return Item::new(
            NAME,
            Level::Warn,
            "restricted by AppArmor: bubblewrap needs the damasco AppArmor profile",
        );
    }
    Item::new(NAME, Level::Ok, "available to unprivileged users")
}

pub fn find_in_path(path: Option<&OsString>, name: &str) -> Option<PathBuf> {
    std::env::split_paths(path?)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Looks for `bwrap` and runs it once in an empty sandbox.
fn bubblewrap(system: &System) -> Item {
    const NAME: &str = "bubblewrap";
    let Some(bwrap) = find_in_path(system.path.as_ref(), "bwrap") else {
        return Item::new(
            NAME,
            Level::Fail,
            "bwrap not found in PATH; install the bubblewrap package",
        );
    };
    let version = Command::new(&bwrap)
        .arg("--version")
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default();
    match try_sandbox(&bwrap) {
        Ok(()) => Item::new(NAME, Level::Ok, format!("{version} starts a sandbox")),
        Err(reason) => Item::new(
            NAME,
            Level::Fail,
            format!("{} cannot start a sandbox: {reason}", bwrap.display()),
        ),
    }
}

fn try_sandbox(bwrap: &Path) -> Result<(), String> {
    let output = Command::new(bwrap)
        .args([
            "--unshare-all",
            "--die-with-parent",
            "--ro-bind",
            "/",
            "/",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "true",
        ])
        .output()
        .map_err(|err| err.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(stderr.lines().next().unwrap_or("unknown error").to_string())
    }
}

/// Looks for `git`, which damasco runs outside the sandbox on the site
/// repository.
fn git(system: &System) -> Item {
    const NAME: &str = "git";
    let Some(git) = find_in_path(system.path.as_ref(), "git") else {
        return Item::new(
            NAME,
            Level::Fail,
            "git not found in PATH; install the git package",
        );
    };
    match Command::new(&git).arg("--version").output() {
        Ok(out) if out.status.success() => Item::new(
            NAME,
            Level::Ok,
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ),
        _ => Item::new(
            NAME,
            Level::Fail,
            format!("{} --version does not run", git.display()),
        ),
    }
}

/// Formats the result for the terminal, one line per check.
pub fn report(items: &[Item]) -> String {
    items
        .iter()
        .map(|item| format!("{:<4}  {:<16} {}\n", item.level, item.name, item.detail))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(root: &Path, path: &str, text: &str) {
        let file = root.join(path.trim_start_matches('/'));
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }

    /// A machine with systemd, cgroup v2, a delegated cgroup and user
    /// namespaces, but without bwrap.
    fn healthy() -> (tempfile::TempDir, System) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("run/systemd/system")).unwrap();
        write(
            root,
            "/sys/fs/cgroup/cgroup.controllers",
            "cpu io memory pids\n",
        );
        write(
            root,
            "/proc/self/cgroup",
            "0::/system.slice/damasco.service\n",
        );
        let own = "/sys/fs/cgroup/system.slice/damasco.service";
        write(
            root,
            &format!("{own}/cgroup.controllers"),
            "cpu io memory pids\n",
        );
        write(root, &format!("{own}/cgroup.procs"), "");
        write(root, "/proc/sys/user/max_user_namespaces", "62124\n");
        let system = System {
            root: root.to_path_buf(),
            path: Some(OsString::new()),
            as_service: true,
        };
        (dir, system)
    }

    fn level(items: &[Item], name: &str) -> Level {
        items.iter().find(|item| item.name == name).unwrap().level
    }

    #[test]
    fn healthy_machine_passes_everything_but_bwrap_and_git() {
        let (_dir, system) = healthy();
        let items = run(&system);
        for name in ["systemd", "cgroup v2", "delegation", "user namespaces"] {
            assert_eq!(level(&items, name), Level::Ok, "{name}: {items:?}");
        }
        assert_eq!(level(&items, "bubblewrap"), Level::Fail);
        assert_eq!(level(&items, "git"), Level::Fail);
    }

    #[test]
    fn git_in_path_passes() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, mut system) = healthy();
        let bin = system.root.join("bin");
        write(
            &system.root,
            "/bin/git",
            "#!/bin/sh\necho git version 2.47.0\n",
        );
        fs::set_permissions(bin.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
        system.path = Some(bin.into_os_string());
        let item = git(&system);
        assert_eq!(item.level, Level::Ok, "{item:?}");
        assert_eq!(item.detail, "git version 2.47.0");
    }

    #[test]
    fn missing_systemd_and_cgroup_v2_fail() {
        let (_dir, system) = healthy();
        fs::remove_dir(system.root.join("run/systemd/system")).unwrap();
        fs::remove_file(system.root.join("sys/fs/cgroup/cgroup.controllers")).unwrap();
        let items = run(&system);
        assert_eq!(level(&items, "systemd"), Level::Fail);
        assert_eq!(level(&items, "cgroup v2"), Level::Fail);
    }

    #[test]
    fn missing_controllers_fail_in_the_service_and_warn_outside() {
        let (_dir, mut system) = healthy();
        write(
            &system.root,
            "/sys/fs/cgroup/system.slice/damasco.service/cgroup.controllers",
            "cpu io\n",
        );
        let item = delegation(&system);
        assert_eq!(item.level, Level::Fail);
        assert!(item.detail.contains("memory, pids"), "{}", item.detail);
        system.as_service = false;
        assert_eq!(delegation(&system).level, Level::Warn);
    }

    #[test]
    fn the_main_leaf_is_checked_through_its_delegated_parent() {
        let (_dir, system) = healthy();
        let service = "/sys/fs/cgroup/system.slice/damasco.service";
        write(
            &system.root,
            "/proc/self/cgroup",
            "0::/system.slice/damasco.service/main\n",
        );
        // The leaf has no controllers until damasco enables them on the parent.
        write(
            &system.root,
            &format!("{service}/main/cgroup.controllers"),
            "",
        );
        write(
            &system.root,
            &format!("{service}/cgroup.subtree_control"),
            "",
        );
        let item = delegation(&system);
        assert_eq!(item.level, Level::Ok, "{item:?}");
        assert!(
            item.detail.contains("/system.slice/damasco.service "),
            "{}",
            item.detail
        );
        fs::remove_file(
            system
                .root
                .join(&service[1..])
                .join("cgroup.subtree_control"),
        )
        .unwrap();
        assert_eq!(delegation(&system).level, Level::Fail);
    }

    #[test]
    fn the_apparmor_profile_lifts_the_restriction() {
        let (_dir, system) = healthy();
        write(
            &system.root,
            "/proc/sys/kernel/apparmor_restrict_unprivileged_userns",
            "1\n",
        );
        assert_eq!(user_namespaces(&system).level, Level::Warn);
        write(&system.root, "/etc/apparmor.d/damasco-bwrap", "");
        assert_eq!(user_namespaces(&system).level, Level::Ok);
    }

    #[test]
    fn user_namespace_switches() {
        let (_dir, system) = healthy();
        write(
            &system.root,
            "/proc/sys/kernel/apparmor_restrict_unprivileged_userns",
            "1\n",
        );
        assert_eq!(user_namespaces(&system).level, Level::Warn);
        write(
            &system.root,
            "/proc/sys/kernel/unprivileged_userns_clone",
            "0\n",
        );
        assert_eq!(user_namespaces(&system).level, Level::Fail);
        write(&system.root, "/proc/sys/user/max_user_namespaces", "0\n");
        assert!(
            user_namespaces(&system)
                .detail
                .contains("max_user_namespaces")
        );
    }

    #[test]
    fn report_has_one_line_per_check() {
        let (_dir, system) = healthy();
        let text = report(&run(&system));
        assert_eq!(text.lines().count(), 6);
        assert!(text.starts_with("ok    systemd"), "{text}");
    }
}
