//! Resource limits for the sandbox, through cgroup v2.
//!
//! systemd delegates a cgroup subtree to the service (`Delegate=yes`) and
//! keeps damasco itself in the `main` leaf (`DelegateSubgroup=main`). Next to
//! it damasco creates `agent` and `build`, each with a memory ceiling, a lower
//! CPU weight and a process cap. When a group runs out of memory the kernel
//! stops that group only: the panel and the published site keep running.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::config::LimitsConfig;
use crate::sandbox::Profile;

const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;

/// Under load, damasco (weight 100, the default) goes first; when the machine
/// is idle the sandbox uses all of the CPU.
const SANDBOX_CPU_WEIGHT: &str = "50";
/// Enough for npm, Astro and the agent's tools; stops fork bombs.
const SANDBOX_PIDS: &str = "512";
/// Memory the kernel keeps for damasco itself under pressure.
const DAMASCO_MEMORY_LOW: u64 = 128 * MIB;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub agent_memory: u64,
    pub build_memory: u64,
}

impl Limits {
    /// Defaults from the machine's memory: 40% for the agent (at most 2 GiB)
    /// and 50% for builds (at most 3 GiB). The configuration may set either.
    pub fn compute(config: &LimitsConfig, total: u64) -> Result<Self, String> {
        let pick = |field: &str, value: &Option<String>, share: u64, cap: u64| {
            match value {
            Some(text) => parse_size(text)
                .filter(|bytes| *bytes >= 64 * MIB)
                .ok_or_else(|| {
                    format!("limits.{field}: {text:?} is not a size of at least 64M (for example 512M or 2G)")
                }),
            None => Ok((total / 100 * share).min(cap)),
        }
        };
        Ok(Self {
            agent_memory: pick("agent_memory", &config.agent_memory, 40, 2 * GIB)?,
            build_memory: pick("build_memory", &config.build_memory, 50, 3 * GIB)?,
        })
    }
}

/// Sizes such as `512M`, `2G`, `1536M`, `2048K` or plain bytes.
pub fn parse_size(text: &str) -> Option<u64> {
    let text = text.trim();
    let (number, unit) = match text.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
        Some((index, _)) => text.split_at(index),
        None => (text, ""),
    };
    let factor = match unit.trim().to_ascii_uppercase().as_str() {
        "" | "B" => 1,
        "K" | "KB" | "KIB" => 1 << 10,
        "M" | "MB" | "MIB" => MIB,
        "G" | "GB" | "GIB" => GIB,
        _ => return None,
    };
    number.parse::<u64>().ok()?.checked_mul(factor)
}

/// A size for people: `1.5 GiB`, `512 MiB`.
pub struct Size(pub u64);

impl fmt::Display for Size {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 >= GIB {
            write!(f, "{:.1} GiB", self.0 as f64 / GIB as f64)
        } else {
            write!(f, "{} MiB", self.0 / MIB)
        }
    }
}

/// Total memory of the machine, from /proc/meminfo.
pub fn total_memory() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

/// The two groups of the sandbox, ready to receive processes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cgroups {
    agent: PathBuf,
    build: PathBuf,
}

impl Cgroups {
    /// Sets up the groups under the cgroup systemd delegated to damasco.
    pub fn prepare(limits: &Limits) -> Result<Self, String> {
        let own = std::fs::read_to_string("/proc/self/cgroup")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find_map(|l| l.strip_prefix("0::").map(String::from))
            })
            .ok_or("own cgroup not found in /proc/self/cgroup")?;
        Self::prepare_at(Path::new("/sys/fs/cgroup"), &own, limits)
    }

    fn prepare_at(sysfs: &Path, own: &str, limits: &Limits) -> Result<Self, String> {
        let Some(parent) = own.strip_suffix("/main") else {
            return Err(format!(
                "damasco runs in {own}, not in the main leaf of a delegated cgroup \
                 (expected outside the systemd service)"
            ));
        };
        let root = sysfs.join(parent.trim_start_matches('/'));
        write(&root.join("cgroup.subtree_control"), "+memory +cpu +pids")?;
        let mut groups = Vec::new();
        for (name, memory) in [
            ("agent", limits.agent_memory),
            ("build", limits.build_memory),
        ] {
            let dir = root.join(name);
            match std::fs::create_dir(&dir) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(err) => return Err(format!("{}: {err}", dir.display())),
            }
            write(&dir.join("memory.max"), &memory.to_string())?;
            // Out of memory, the whole group stops, not a random process in it.
            write(&dir.join("memory.oom.group"), "1")?;
            write(&dir.join("cpu.weight"), SANDBOX_CPU_WEIGHT)?;
            write(&dir.join("pids.max"), SANDBOX_PIDS)?;
            groups.push(dir);
        }
        // A reservation for damasco itself: best effort.
        let _ = write(
            &sysfs.join(own.trim_start_matches('/')).join("memory.low"),
            &DAMASCO_MEMORY_LOW.to_string(),
        );
        let build = groups.pop().expect("two groups");
        let agent = groups.pop().expect("two groups");
        Ok(Self { agent, build })
    }

    /// The `cgroup.procs` file a sandbox of `profile` joins.
    pub fn procs(&self, profile: Profile) -> PathBuf {
        match profile {
            Profile::Agent => self.agent.join("cgroup.procs"),
            Profile::Build => self.build.join("cgroup.procs"),
        }
    }
}

/// How many times the kernel stopped the group for lack of memory, from
/// `memory.events` next to the given `cgroup.procs`.
pub fn oom_kills(procs: &Path) -> u64 {
    let events = procs.with_file_name("memory.events");
    std::fs::read_to_string(events)
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("oom_kill "))
                .and_then(|n| n.trim().parse().ok())
        })
        .unwrap_or(0)
}

fn write(path: &Path, value: &str) -> Result<(), String> {
    std::fs::write(path, value).map_err(|err| format!("{}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_parse() {
        assert_eq!(parse_size("512M"), Some(512 * MIB));
        assert_eq!(parse_size("2G"), Some(2 * GIB));
        assert_eq!(parse_size("2 GiB"), Some(2 * GIB));
        assert_eq!(parse_size("2048k"), Some(2 * MIB));
        assert_eq!(parse_size("1000"), Some(1000));
        assert_eq!(parse_size("2T"), None);
        assert_eq!(parse_size("lots"), None);
    }

    #[test]
    fn defaults_follow_the_machine_and_have_caps() {
        let small = Limits::compute(&LimitsConfig::default(), 2 * GIB).unwrap();
        assert_eq!(small.agent_memory, 2 * GIB / 100 * 40);
        assert_eq!(small.build_memory, 2 * GIB / 100 * 50);
        let large = Limits::compute(&LimitsConfig::default(), 64 * GIB).unwrap();
        assert_eq!((large.agent_memory, large.build_memory), (2 * GIB, 3 * GIB));
    }

    #[test]
    fn configuration_overrides_and_is_checked() {
        let config = LimitsConfig {
            agent_memory: Some("1G".into()),
            build_memory: None,
        };
        let limits = Limits::compute(&config, 8 * GIB).unwrap();
        assert_eq!(limits.agent_memory, GIB);
        let tiny = LimitsConfig {
            agent_memory: Some("10M".into()),
            build_memory: None,
        };
        assert!(
            Limits::compute(&tiny, 8 * GIB)
                .unwrap_err()
                .contains("agent_memory")
        );
    }

    #[test]
    fn prepares_groups_next_to_the_main_leaf() {
        let dir = tempfile::tempdir().unwrap();
        let service = dir.path().join("system.slice/damasco.service");
        std::fs::create_dir_all(service.join("main")).unwrap();
        let limits = Limits {
            agent_memory: GIB,
            build_memory: 2 * GIB,
        };
        let groups =
            Cgroups::prepare_at(dir.path(), "/system.slice/damasco.service/main", &limits).unwrap();
        let read = |path: &str| std::fs::read_to_string(service.join(path)).unwrap();
        assert_eq!(read("cgroup.subtree_control"), "+memory +cpu +pids");
        assert_eq!(read("agent/memory.max"), GIB.to_string());
        assert_eq!(read("build/memory.max"), (2 * GIB).to_string());
        assert_eq!(read("build/memory.oom.group"), "1");
        assert_eq!(read("build/pids.max"), "512");
        assert_eq!(read("main/memory.low"), DAMASCO_MEMORY_LOW.to_string());
        assert_eq!(
            groups.procs(Profile::Build),
            service.join("build/cgroup.procs")
        );
    }

    #[test]
    fn refuses_outside_a_delegated_main_leaf() {
        let dir = tempfile::tempdir().unwrap();
        let limits = Limits {
            agent_memory: GIB,
            build_memory: GIB,
        };
        let err =
            Cgroups::prepare_at(dir.path(), "/user.slice/session-1.scope", &limits).unwrap_err();
        assert!(err.contains("main leaf"), "{err}");
    }

    #[test]
    fn reads_the_oom_counter() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("memory.events"),
            "low 0\nhigh 0\nmax 4\noom 1\noom_kill 2\n",
        )
        .unwrap();
        assert_eq!(oom_kills(&dir.path().join("cgroup.procs")), 2);
        assert_eq!(oom_kills(Path::new("/nonexistent/cgroup.procs")), 0);
    }

    #[test]
    fn sizes_display() {
        assert_eq!(Size(3 * GIB / 2).to_string(), "1.5 GiB");
        assert_eq!(Size(512 * MIB).to_string(), "512 MiB");
    }
}
