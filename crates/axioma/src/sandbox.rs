//! The sandbox where untrusted code runs: the agent and the site's builds.
//!
//! Each process starts in bubblewrap with new user, PID, IPC, UTS, cgroup and
//! network namespaces. It sees the system read-only, the site repository
//! read-write at `/site`, the runtimes read-only under `/opt`, and nothing of
//! axioma's data directory. Its environment is rebuilt from scratch, so no
//! variable of the service leaks in.

use std::ffi::OsString;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::runtime::Runtimes;

/// What the sandbox is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// The coding agent: the site plus a persistent home for its sessions.
    Agent,
    /// A build of the site: the site and a throwaway home.
    Build,
}

impl Profile {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "agent" => Some(Profile::Agent),
            "build" => Some(Profile::Build),
            _ => None,
        }
    }
}

/// System directories visible read-only, when they exist. Merged-/usr
/// distributions have symlinks for the top-level ones; NixOS keeps
/// everything under /nix/store and /run/current-system.
const SYSTEM_DIRS: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib32",
    "/lib64",
    "/nix/store",
    "/run/current-system",
];

/// Files of /etc that programs need without revealing the machine: TLS
/// certificates, time zones, name service switch, locale aliases.
const ETC_ENTRIES: &[&str] = &[
    "/etc/ssl",
    "/etc/ca-certificates",
    "/etc/pki",
    "/etc/static/ssl",
    "/etc/localtime",
    "/etc/zoneinfo",
    "/etc/nsswitch.conf",
    "/etc/locale.alias",
];

pub struct Sandbox<'a> {
    pub profile: Profile,
    pub bwrap: PathBuf,
    /// The site repository, read-write at /site.
    pub site: &'a Path,
    /// Persistent home of the agent, read-write at /home/agent. Ignored by
    /// the build profile, which gets a throwaway home.
    pub agent_home: &'a Path,
    pub runtimes: &'a Runtimes,
    /// Egress through the proxy; without it the sandbox has no network.
    pub egress: Option<Egress<'a>>,
    /// Directory with the product rules, mounted read-only at
    /// /etc/claude-code for the agent profile.
    pub policy: Option<&'a Path>,
    /// The `cgroup.procs` of the group with the limits; without it the
    /// sandbox runs without limits (development only).
    pub cgroup: Option<&'a Path>,
}

/// How the sandbox reaches the egress proxy: the axioma binary runs inside
/// as a bridge from 127.0.0.1:3128 to the proxy's Unix socket.
pub struct Egress<'a> {
    pub axioma: &'a Path,
    pub socket: &'a Path,
    /// Socket where axioma answers the hooks of the CLI, for the agent.
    pub hooks: Option<&'a Path>,
}

const AXIOMA_IN_SANDBOX: &str = "/opt/axioma/axioma";
const SOCKET_IN_SANDBOX: &str = "/run/axioma/proxy.sock";
/// Where the hooks of the CLI reach axioma.
pub const HOOKS_IN_SANDBOX: &str = "/run/axioma/hooks.sock";

const NODE_DIR: &str = "/opt/node";
const CLAUDE_DIR: &str = "/opt/claude";

impl Sandbox<'_> {
    /// The bubblewrap command that runs `program` with `args` inside the
    /// sandbox, in /site. Keep the returned value until the process is
    /// spawned: it owns the descriptor bubblewrap reads the seccomp filter
    /// from.
    pub fn command(&self, program: &str, args: &[OsString]) -> std::io::Result<Prepared> {
        let filter = seccomp_fd()?;
        let mut command = Command::new(&self.bwrap);
        command.env_clear();
        command.args(self.arguments()?);
        command.arg("--seccomp").arg(filter.as_raw_fd().to_string());
        command.arg("--");
        if self.egress.is_some() {
            command
                .arg(AXIOMA_IN_SANDBOX)
                .arg("sandbox-bridge")
                .arg("--socket")
                .arg(SOCKET_IN_SANDBOX)
                .arg("--");
        }
        command.arg(program).args(args);
        inherit_fd(&mut command, filter.as_raw_fd());
        if let Some(procs) = self.cgroup {
            join_cgroup(&mut command, procs)?;
        }
        Ok(Prepared { command, filter })
    }

    fn arguments(&self) -> std::io::Result<Vec<OsString>> {
        let mut a = Arguments::default();
        a.flag("--die-with-parent");
        a.flag("--new-session");
        a.flag("--unshare-all");
        // --unshare-all only tries the user namespace; it is required here.
        a.flag("--unshare-user");
        // No new user namespaces inside: the layout set here is final.
        a.flag("--disable-userns");
        a.flag("--assert-userns-disabled");
        a.flag("--cap-drop").arg("ALL");
        a.flag("--hostname").arg("sandbox");

        for dir in SYSTEM_DIRS {
            a.flag("--ro-bind-try").arg(*dir).arg(*dir);
        }
        for entry in ETC_ENTRIES {
            a.flag("--ro-bind-try").arg(*entry).arg(*entry);
        }
        a.flag("--proc").arg("/proc");
        a.flag("--dev").arg("/dev");
        a.flag("--tmpfs").arg("/tmp");

        // Runtimes, wherever they live on the host, appear at fixed paths.
        // The directory around bin/node as named, not as resolved: some
        // distributions link node from a package that does not contain npm,
        // while the named directory has both.
        let node = std::path::absolute(&self.runtimes.node)?;
        let node_home = node
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| std::io::Error::other("node is not inside a bin directory"))?;
        let claude = std::fs::canonicalize(&self.runtimes.claude)?;
        a.flag("--ro-bind").arg(node_home).arg(NODE_DIR);
        a.flag("--ro-bind")
            .arg(&claude)
            .arg(format!("{CLAUDE_DIR}/claude"));
        if let Some(egress) = &self.egress {
            let axioma = std::fs::canonicalize(egress.axioma)?;
            a.flag("--ro-bind").arg(axioma).arg(AXIOMA_IN_SANDBOX);
            a.flag("--bind").arg(egress.socket).arg(SOCKET_IN_SANDBOX);
            if let Some(hooks) = egress.hooks {
                a.flag("--bind").arg(hooks).arg(HOOKS_IN_SANDBOX);
            }
        }

        a.flag("--bind").arg(self.site).arg("/site");
        // The history belongs to axioma: the agent reads it but cannot change
        // it, and cannot plant hooks or configuration that git would run.
        let git = self.site.join(".git");
        if git.is_dir() {
            a.flag("--ro-bind").arg(&git).arg("/site/.git");
        }
        // Files generated by axioma, such as the schema of the pages.
        let generated = self.site.join(".axioma");
        if generated.is_dir() {
            a.flag("--ro-bind").arg(&generated).arg("/site/.axioma");
        }
        if let (Profile::Agent, Some(policy)) = (self.profile, self.policy) {
            a.flag("--ro-bind")
                .arg(policy)
                .arg(crate::policy::IN_SANDBOX);
        }
        let home = match self.profile {
            Profile::Agent => {
                a.flag("--bind").arg(self.agent_home).arg("/home/agent");
                "/home/agent"
            }
            Profile::Build => {
                a.flag("--tmpfs").arg("/home/build");
                "/home/build"
            }
        };
        a.flag("--chdir").arg("/site");

        // /run/current-system/sw/bin holds the system tools on NixOS.
        let path = format!(
            "{NODE_DIR}/bin:{CLAUDE_DIR}:/usr/local/bin:/usr/bin:/bin:/run/current-system/sw/bin"
        );
        for (key, value) in [
            ("PATH", path.as_str()),
            ("HOME", home),
            ("LANG", "C.UTF-8"),
            ("TMPDIR", "/tmp"),
            ("NODE_ENV", "development"),
            ("NPM_CONFIG_UPDATE_NOTIFIER", "false"),
            ("ASTRO_TELEMETRY_DISABLED", "1"),
        ] {
            a.flag("--setenv").arg(key).arg(value);
        }
        if self.profile == Profile::Agent {
            // The CLI version is fixed by axioma: no self-update, and no
            // traffic beyond the model and the login.
            a.flag("--setenv")
                .arg("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC")
                .arg("1");
            a.flag("--setenv").arg("DISABLE_AUTOUPDATER").arg("1");
        }
        Ok(a.0)
    }
}

/// A sandbox command ready to spawn, with the descriptor of its seccomp
/// filter.
pub struct Prepared {
    pub command: Command,
    filter: std::os::fd::OwnedFd,
}

impl Prepared {
    /// The command and the descriptor; keep the descriptor alive until the
    /// command is spawned.
    pub fn into_parts(self) -> (Command, std::os::fd::OwnedFd) {
        (self.command, self.filter)
    }
}

/// A memory file holding the compiled seccomp filter, closed on exec except
/// in the bubblewrap child (see `inherit_fd`).
fn seccomp_fd() -> std::io::Result<std::os::fd::OwnedFd> {
    use std::io::{Seek, Write};
    use std::os::fd::FromRawFd;
    let program = crate::seccomp::program().map_err(std::io::Error::other)?;
    // SAFETY: memfd_create returns a new descriptor owned by nobody else.
    let fd = unsafe { libc::memfd_create(c"axioma-seccomp".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fd is valid and exclusively ours.
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.write_all(program)?;
    file.rewind()?;
    Ok(file.into())
}

/// Clears close-on-exec on `fd` in the child only, so that bubblewrap can
/// read it while other children of axioma never see it.
fn inherit_fd(command: &mut Command, fd: std::os::fd::RawFd) {
    use std::os::unix::process::CommandExt;
    // SAFETY: fcntl is async-signal-safe and fd stays open until spawn.
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Moves the child into the cgroup before it executes bwrap, so that every
/// process of the sandbox is under the limits from its first instruction.
fn join_cgroup(command: &mut Command, procs: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::CommandExt;
    let path = std::ffi::CString::new(procs.as_os_str().as_bytes())?;
    // SAFETY: between fork and exec the closure only calls open, write and
    // close, which are async-signal-safe, on memory prepared before the fork.
    unsafe {
        command.pre_exec(move || {
            let fd = libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // "0" means the writing process itself.
            let written = libc::write(fd, b"0".as_ptr().cast(), 1);
            let error = std::io::Error::last_os_error();
            libc::close(fd);
            if written == 1 { Ok(()) } else { Err(error) }
        });
    }
    Ok(())
}

#[derive(Default)]
struct Arguments(Vec<OsString>);

impl Arguments {
    fn flag(&mut self, flag: &str) -> &mut Self {
        self.0.push(flag.into());
        self
    }

    fn arg(&mut self, value: impl Into<OsString>) -> &mut Self {
        self.0.push(value.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A sandbox over temporary directories, with the node and claude found
    /// in PATH; `None` when bwrap or the runtimes are missing.
    fn setup() -> Option<(tempfile::TempDir, PathBuf, PathBuf, Runtimes, PathBuf)> {
        let path = std::env::var_os("PATH")?;
        let find = |name: &str| {
            std::env::split_paths(&path)
                .map(|d| d.join(name))
                .find(|p| p.is_file())
        };
        let bwrap = find("bwrap")?;
        let node = find("node")?;
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().join("site");
        let home = dir.path().join("home");
        fs::create_dir_all(&site).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::write(dir.path().join("secret"), "axioma data").unwrap();
        // Any executable stands in for claude in these tests.
        let runtimes = Runtimes {
            node: node.clone(),
            claude: node,
            managed: false,
        };
        Some((dir, site, home, runtimes, bwrap))
    }

    fn run(sandbox: &Sandbox<'_>, program: &str, args: &[&str]) -> (i32, String) {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        let output = sandbox
            .command(program, &args)
            .unwrap()
            .command
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (output.status.code().unwrap_or(-1), text.trim().to_string())
    }

    #[test]
    fn arguments_isolate_and_map_paths() {
        let runtimes = Runtimes {
            node: PathBuf::from("/bin/sh"),
            claude: PathBuf::from("/bin/sh"),
            managed: false,
        };
        let sandbox = Sandbox {
            profile: Profile::Build,
            bwrap: PathBuf::from("bwrap"),
            site: Path::new("/srv/site"),
            agent_home: Path::new("/srv/agent"),
            runtimes: &runtimes,
            egress: None,
            policy: None,
            cgroup: None,
        };
        let Ok(args) = sandbox.arguments() else {
            return;
        };
        let text: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let joined = text.join(" ");
        assert!(joined.contains("--unshare-all"));
        assert!(joined.contains("--bind /srv/site /site"));
        assert!(
            !joined.contains("/srv/agent"),
            "build must not see the agent home"
        );
        assert!(joined.contains("--tmpfs /home/build"));
    }

    #[test]
    fn runs_node_with_the_site_writable_and_nothing_else_visible() {
        let Some((dir, site, home, runtimes, bwrap)) = setup() else {
            eprintln!("skipped: bwrap or node missing");
            return;
        };
        let sandbox = Sandbox {
            profile: Profile::Build,
            bwrap,
            site: &site,
            agent_home: &home,
            runtimes: &runtimes,
            egress: None,
            policy: None,
            cgroup: None,
        };
        let (code, out) = run(&sandbox, "node", &["-e", "console.log(process.cwd())"]);
        assert_eq!((code, out.as_str()), (0, "/site"));

        let (code, _) = run(
            &sandbox,
            "node",
            &["-e", "require('fs').writeFileSync('built.txt', 'ok')"],
        );
        assert_eq!(code, 0);
        assert_eq!(fs::read_to_string(site.join("built.txt")).unwrap(), "ok");

        let secret = dir.path().join("secret");
        let probe = format!("process.exit(require('fs').existsSync({secret:?}) ? 1 : 0)");
        let (code, _) = run(&sandbox, "node", &["-e", &probe]);
        assert_eq!(
            code, 0,
            "the host directory around the site must be invisible"
        );

        let (code, out) = run(
            &sandbox,
            "node",
            &[
                "-e",
                "console.log(Object.keys(process.env).sort().join(','))",
            ],
        );
        assert_eq!(code, 0);
        assert_eq!(
            out,
            "ASTRO_TELEMETRY_DISABLED,HOME,LANG,NODE_ENV,NPM_CONFIG_UPDATE_NOTIFIER,PATH,PWD,TMPDIR"
        );
    }

    #[test]
    fn has_no_network_and_its_own_processes() {
        let Some((_dir, site, home, runtimes, bwrap)) = setup() else {
            eprintln!("skipped: bwrap or node missing");
            return;
        };
        let sandbox = Sandbox {
            profile: Profile::Build,
            bwrap,
            site: &site,
            agent_home: &home,
            runtimes: &runtimes,
            egress: None,
            policy: None,
            cgroup: None,
        };
        let connect = "require('net').connect(443, '1.1.1.1').on('connect', () => process.exit(1)).on('error', () => process.exit(0))";
        assert_eq!(run(&sandbox, "node", &["-e", connect]).0, 0);
        let count = "console.log(require('fs').readdirSync('/proc').filter((n) => /^\\d+$/.test(n)).length)";
        let (_, out) = run(&sandbox, "node", &["-e", count]);
        let pids: usize = out.parse().unwrap();
        assert!(
            pids <= 3,
            "only the sandbox's own processes are visible, got {pids}"
        );
    }

    #[test]
    fn the_agent_keeps_its_home() {
        let Some((_dir, site, home, runtimes, bwrap)) = setup() else {
            eprintln!("skipped: bwrap or node missing");
            return;
        };
        let sandbox = Sandbox {
            profile: Profile::Agent,
            bwrap,
            site: &site,
            agent_home: &home,
            runtimes: &runtimes,
            egress: None,
            policy: None,
            cgroup: None,
        };
        let write = "require('fs').writeFileSync(require('os').homedir() + '/memory', 'kept')";
        assert_eq!(run(&sandbox, "node", &["-e", write]).0, 0);
        assert_eq!(fs::read_to_string(home.join("memory")).unwrap(), "kept");
    }

    #[test]
    fn runs_under_the_seccomp_filter() {
        let Some((_dir, site, home, runtimes, bwrap)) = setup() else {
            eprintln!("skipped: bwrap or node missing");
            return;
        };
        let sandbox = Sandbox {
            profile: Profile::Build,
            bwrap,
            site: &site,
            agent_home: &home,
            runtimes: &runtimes,
            egress: None,
            policy: None,
            cgroup: None,
        };
        let status = "const s = require('fs').readFileSync('/proc/self/status', 'utf8'); console.log(/^Seccomp:\\s+2$/m.test(s) && /^NoNewPrivs:\\s+1$/m.test(s))";
        assert_eq!(
            run(&sandbox, "node", &["-e", status]),
            (0, "true".to_string())
        );
    }
}
