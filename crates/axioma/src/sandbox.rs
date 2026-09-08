//! The sandbox where untrusted code runs: the agent and the site's builds.
//!
//! Each process starts in bubblewrap with new user, PID, IPC, UTS, cgroup and
//! network namespaces. It sees the system read-only, the site repository
//! read-write at `/site`, the runtimes read-only under `/opt`, and nothing of
//! axioma's data directory. Its environment is rebuilt from scratch, so no
//! variable of the service leaks in.

use std::ffi::OsString;
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
}

const NODE_DIR: &str = "/opt/node";
const CLAUDE_DIR: &str = "/opt/claude";

impl Sandbox<'_> {
    /// The bubblewrap command that runs `program` with `args` inside the
    /// sandbox, in /site.
    pub fn command(&self, program: &str, args: &[OsString]) -> std::io::Result<Command> {
        let mut command = Command::new(&self.bwrap);
        command.env_clear();
        command.args(self.arguments()?);
        command.arg("--").arg(program).args(args);
        Ok(command)
    }

    fn arguments(&self) -> std::io::Result<Vec<OsString>> {
        let mut a = Arguments::default();
        a.flag("--die-with-parent");
        a.flag("--new-session");
        a.flag("--unshare-all");
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
        let node = std::fs::canonicalize(&self.runtimes.node)?;
        let node_home = node
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| std::io::Error::other("node is not inside a bin directory"))?;
        let claude = std::fs::canonicalize(&self.runtimes.claude)?;
        a.flag("--ro-bind").arg(node_home).arg(NODE_DIR);
        a.flag("--ro-bind")
            .arg(&claude)
            .arg(format!("{CLAUDE_DIR}/claude"));

        a.flag("--bind").arg(self.site).arg("/site");
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
        ] {
            a.flag("--setenv").arg(key).arg(value);
        }
        Ok(a.0)
    }
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
        let output = sandbox.command(program, &args).unwrap().output().unwrap();
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
        assert_eq!(out, "HOME,LANG,NODE_ENV,PATH,PWD,TMPDIR");
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
        };
        let write = "require('fs').writeFileSync(require('os').homedir() + '/memory', 'kept')";
        assert_eq!(run(&sandbox, "node", &["-e", write]).0, 0);
        assert_eq!(fs::read_to_string(home.join("memory")).unwrap(), "kept");
    }
}
