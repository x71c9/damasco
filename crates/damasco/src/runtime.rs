//! Runtimes that run untrusted code inside the sandbox: Node.js, for the
//! site's builds and the agent's commands, and the CLI of each assistant
//! (Claude Code, Codex). Only the CLI of the assistant in use is needed.
//!
//! damasco downloads them itself, at a version fixed in this file and checked
//! against a SHA-256 also fixed here, into `data_dir/runtimes`. The binary
//! never runs them outside the sandbox, and never needs them for itself.
//! Where the downloaded builds cannot run (NixOS), the configuration can name
//! runtimes already on the machine instead.

use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::assistant::Provider;
use crate::config::RuntimeConfig;

pub const NODE_VERSION: &str = "26.13.1";
pub const CLAUDE_VERSION: &str = "2.1.325";
pub const CODEX_VERSION: &str = "0.170.0";

/// One file to download for one architecture.
struct Download {
    url: String,
    sha256: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arch {
    X64,
    Arm64,
}

impl Arch {
    fn current() -> Option<Self> {
        match std::env::consts::ARCH {
            "x86_64" => Some(Arch::X64),
            "aarch64" => Some(Arch::Arm64),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Arch::X64 => "x64",
            Arch::Arm64 => "arm64",
        }
    }

    /// The target of Codex's static builds.
    fn musl(self) -> &'static str {
        match self {
            Arch::X64 => "x86_64-unknown-linux-musl",
            Arch::Arm64 => "aarch64-unknown-linux-musl",
        }
    }
}

/// Official Node.js archive, checksums from SHASUMS256.txt of the release.
fn node_download(arch: Arch) -> Download {
    let sha256 = match arch {
        Arch::X64 => "518674de2b621728a5e4486f5fb3b7d2900bb3cc4aa97969edaa463db99f9778",
        Arch::Arm64 => "80a330b35415382878df18d41afbd106ea89bfb3be0087d6608e289a14c5cc7d",
    };
    Download {
        url: format!(
            "https://nodejs.org/dist/v{NODE_VERSION}/node-v{NODE_VERSION}-linux-{}.tar.gz",
            arch.name()
        ),
        sha256,
    }
}

/// Official Claude Code build, checksums from the release's manifest.json.
fn claude_download(arch: Arch) -> Download {
    let sha256 = match arch {
        Arch::X64 => "0dc4f4a9a165fc60746d7ae8489712e5fc352f1e7ad4eb22e7141bd0812eb0cf",
        Arch::Arm64 => "7b6e484b1989a5a14a4b354709ccc8672518a3ec5b9ccd7981fc6254327fde67",
    };
    Download {
        url: format!(
            "https://downloads.claude.ai/claude-code-releases/{CLAUDE_VERSION}/linux-{}/claude",
            arch.name()
        ),
        sha256,
    }
}

/// Official Codex build: an archive with one static executable, checksums
/// from the digests of the GitHub release `rust-v{CODEX_VERSION}`.
fn codex_download(arch: Arch) -> Download {
    let sha256 = match arch {
        Arch::X64 => "d054916ccd28a53820376b8d2f7c875184ca8f40fcd44bbabeb8ee240ce247f5",
        Arch::Arm64 => "0da6b061f6b5223901a5a73e841bd1466596e2ef620d68823360febed2ff7ebc",
    };
    Download {
        url: format!(
            "https://github.com/openai/codex/releases/download/rust-v{CODEX_VERSION}/codex-{}.tar.gz",
            arch.musl()
        ),
        sha256,
    }
}

/// The program Codex runs its tools through ("code mode"), which it looks
/// for next to its own executable.
pub const CODEX_HOST: &str = "codex-code-mode-host";

/// Official build of Codex's code-mode host, from the same release.
fn codex_host_download(arch: Arch) -> Download {
    let sha256 = match arch {
        Arch::X64 => "18ab29d970586890085c3eba1d55068fd66262154855e51a54f3708522fa4dc5",
        Arch::Arm64 => "8ae871c5d494aa5842fff37adbbac9ca586b0c8af40d80d90feb8d1e10f86a59",
    };
    Download {
        url: format!(
            "https://github.com/openai/codex/releases/download/rust-v{CODEX_VERSION}/{CODEX_HOST}-{}.tar.gz",
            arch.musl()
        ),
        sha256,
    }
}

#[derive(Debug)]
pub struct RuntimeError(String);

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RuntimeError {}

fn err(context: impl fmt::Display, cause: impl fmt::Display) -> RuntimeError {
    RuntimeError(format!("{context}: {cause}"))
}

/// Where each runtime lives on this installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Runtimes {
    /// The `node` executable; npm sits next to it.
    pub node: PathBuf,
    pub claude: PathBuf,
    /// Codex, when this installation can use it.
    pub codex: Option<PathBuf>,
    /// True when damasco downloads and owns them.
    pub managed: bool,
    /// True when damasco downloads Codex, also next to runtimes of the
    /// machine: the configuration names Codex only to use another one.
    pub codex_managed: bool,
}

impl Runtimes {
    /// Managed runtimes under `data_dir`, unless the configuration names
    /// runtimes already on the machine. Names without a slash are looked up
    /// in PATH.
    pub fn resolve(config: &RuntimeConfig, data_dir: &Path) -> Result<Self, RuntimeError> {
        match (&config.node, &config.claude) {
            (None, None) => {
                let arch = Arch::current().ok_or_else(|| {
                    RuntimeError(format!(
                        "no managed runtimes for {}; set [runtimes] node and claude",
                        std::env::consts::ARCH
                    ))
                })?;
                let root = data_dir.join("runtimes");
                Ok(Self {
                    node: root
                        .join(format!("node-v{NODE_VERSION}-linux-{}", arch.name()))
                        .join("bin/node"),
                    claude: root.join(format!("claude-{CLAUDE_VERSION}")).join("claude"),
                    codex: Some(managed_codex(data_dir)),
                    managed: true,
                    codex_managed: true,
                })
            }
            (Some(node), Some(claude)) => Ok(Self {
                node: locate(node)?,
                claude: locate(claude)?,
                codex: match config.codex.as_deref() {
                    Some(codex) => Some(locate(codex)?),
                    None => Arch::current().map(|_| managed_codex(data_dir)),
                },
                managed: false,
                codex_managed: config.codex.is_none() && Arch::current().is_some(),
            }),
            _ => Err(RuntimeError(
                "[runtimes] needs both node and claude, or neither".into(),
            )),
        }
    }

    /// Whether damasco downloads `provider`'s CLI when it is missing.
    pub fn downloads(&self, provider: Provider) -> bool {
        match provider {
            Provider::Claude => self.managed,
            Provider::Codex => self.codex_managed,
        }
    }

    /// The executable of an assistant's CLI, if this installation has one.
    pub fn cli(&self, provider: Provider) -> Option<&Path> {
        match provider {
            Provider::Claude => Some(&self.claude),
            Provider::Codex => self.codex.as_deref(),
        }
    }

    /// The runtimes missing on disk: Node.js, and the CLI of `provider`
    /// when one is named.
    pub fn missing(&self, provider: Option<Provider>) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if !self.node.is_file() {
            missing.push("node");
        }
        if let Some(provider) = provider
            && !self.cli(provider).is_some_and(Path::is_file)
        {
            missing.push(provider.program());
        }
        // Codex needs its code-mode host next to it.
        if provider == Some(Provider::Codex)
            && let Some(codex) = self.codex.as_deref()
            && !codex.with_file_name(CODEX_HOST).is_file()
        {
            missing.push(CODEX_HOST);
        }
        missing
    }

    /// Versions reported by the runtimes themselves, or the reason they
    /// cannot run. Codex writes into its home when it runs, so outside the
    /// sandbox only its fixed version is told.
    pub fn versions(&self) -> Vec<(&'static str, Result<String, String>)> {
        let mut versions = vec![
            ("node", version(&self.node)),
            ("claude", version(&self.claude)),
        ];
        if let Some(codex) = self.codex.as_deref().filter(|codex| codex.is_file()) {
            let told = if self.managed {
                Ok(format!("codex-cli {CODEX_VERSION}"))
            } else {
                Ok(format!("{} (version not checked)", codex.display()))
            };
            versions.push(("codex", told));
        }
        versions
    }
}

fn managed_codex(data_dir: &Path) -> PathBuf {
    data_dir
        .join("runtimes")
        .join(format!("codex-{CODEX_VERSION}"))
        .join("codex")
}

fn locate(value: &str) -> Result<PathBuf, RuntimeError> {
    if value.contains('/') {
        return Ok(PathBuf::from(value));
    }
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join(value))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| RuntimeError(format!("[runtimes] {value} not found in PATH")))
}

fn version(executable: &Path) -> Result<String, String> {
    let output = Command::new(executable)
        .arg("--version")
        .output()
        .map_err(|e| format!("{}: {e}", executable.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.lines().next().unwrap_or("failed").to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Downloads the managed runtimes that are missing. Each file is checked
/// against its fixed SHA-256 before anything is unpacked, and appears under
/// its final name only once complete.
///
/// `node` is false next to a Node.js of the machine: then only the CLIs of
/// `providers` are fetched.
pub fn install(
    data_dir: &Path,
    node: bool,
    providers: &[Provider],
    report: &dyn Fn(&str),
) -> Result<Runtimes, RuntimeError> {
    let runtimes = Runtimes::resolve(&RuntimeConfig::default(), data_dir)?;
    let arch = Arch::current().expect("resolve checked the architecture");
    let root = data_dir.join("runtimes");
    let staging = root.join(".download");
    fs::create_dir_all(&staging).map_err(|e| err(staging.display(), e))?;

    if node && !runtimes.node.is_file() {
        let download = node_download(arch);
        report(&format!("downloading Node.js {NODE_VERSION}"));
        let archive = staging.join("node.tar.gz");
        fetch(&download, &archive)?;
        let unpacked = staging.join("node");
        let _ = fs::remove_dir_all(&unpacked);
        unpack(&archive, &unpacked)?;
        let name = format!("node-v{NODE_VERSION}-linux-{}", arch.name());
        let target = root.join(&name);
        let _ = fs::remove_dir_all(&target);
        fs::rename(unpacked.join(&name), &target).map_err(|e| err(target.display(), e))?;
        let _ = fs::remove_dir_all(&unpacked);
        let _ = fs::remove_file(&archive);
    }

    if providers.contains(&Provider::Codex)
        && let Some(codex) = runtimes.codex.as_deref()
    {
        let host = codex.with_file_name(CODEX_HOST);
        for (download, name, target) in [
            (codex_download(arch), "codex", codex),
            (codex_host_download(arch), CODEX_HOST, host.as_path()),
        ] {
            if target.is_file() {
                continue;
            }
            report(&format!("downloading {name} {CODEX_VERSION}"));
            let archive = staging.join(format!("{name}.tar.gz"));
            fetch(&download, &archive)?;
            let unpacked = staging.join(name);
            let _ = fs::remove_dir_all(&unpacked);
            unpack(&archive, &unpacked)?;
            let file = unpacked.join(format!("{name}-{}", arch.musl()));
            fs::set_permissions(&file, fs::Permissions::from_mode(0o755))
                .map_err(|e| err(file.display(), e))?;
            let dir = target.parent().expect("codex has a directory");
            fs::create_dir_all(dir).map_err(|e| err(dir.display(), e))?;
            fs::rename(&file, target).map_err(|e| err(target.display(), e))?;
            let _ = fs::remove_dir_all(&unpacked);
            let _ = fs::remove_file(&archive);
        }
    }

    if providers.contains(&Provider::Claude) && !runtimes.claude.is_file() {
        let download = claude_download(arch);
        report(&format!("downloading Claude Code {CLAUDE_VERSION}"));
        let file = staging.join("claude");
        fetch(&download, &file)?;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755))
            .map_err(|e| err(file.display(), e))?;
        let dir = runtimes.claude.parent().expect("claude has a directory");
        fs::create_dir_all(dir).map_err(|e| err(dir.display(), e))?;
        fs::rename(&file, &runtimes.claude).map_err(|e| err(runtimes.claude.display(), e))?;
    }

    let _ = fs::remove_dir(&staging);
    Ok(runtimes)
}

/// Downloads `download` into `target`, computing the SHA-256 on the way, and
/// removes the file when the checksum does not match.
fn fetch(download: &Download, target: &Path) -> Result<(), RuntimeError> {
    let response = ureq::get(&download.url)
        .call()
        .map_err(|e| err(&download.url, e))?;
    let mut body = response.into_body().into_reader();
    let mut file = fs::File::create(target).map_err(|e| err(target.display(), e))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let read = body.read(&mut buffer).map_err(|e| err(&download.url, e))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])
            .map_err(|e| err(target.display(), e))?;
    }
    file.sync_all().map_err(|e| err(target.display(), e))?;
    let actual = hex(&hasher.finalize());
    if actual != download.sha256 {
        let _ = fs::remove_file(target);
        return Err(RuntimeError(format!(
            "{}: SHA-256 {actual} does not match the expected {}",
            download.url, download.sha256
        )));
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unpack(archive: &Path, into: &Path) -> Result<(), RuntimeError> {
    let file = fs::File::open(archive).map_err(|e| err(archive.display(), e))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(io::BufReader::new(file)));
    tar.set_preserve_permissions(true);
    tar.unpack(into).map_err(|e| err(archive.display(), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_paths_live_under_the_data_dir() {
        let runtimes =
            Runtimes::resolve(&RuntimeConfig::default(), Path::new("/var/lib/damasco")).unwrap();
        assert!(runtimes.managed);
        assert!(runtimes.node.starts_with("/var/lib/damasco/runtimes"));
        assert!(runtimes.node.ends_with("bin/node"));
        assert!(
            runtimes
                .claude
                .ends_with(format!("claude-{CLAUDE_VERSION}/claude"))
        );
    }

    #[test]
    fn external_runtimes_need_both() {
        let half = RuntimeConfig {
            node: Some("/usr/bin/node".into()),
            claude: None,
            codex: None,
        };
        assert!(Runtimes::resolve(&half, Path::new("/tmp")).is_err());
        let both = RuntimeConfig {
            node: Some("/usr/bin/node".into()),
            claude: Some("/usr/bin/claude".into()),
            codex: None,
        };
        let runtimes = Runtimes::resolve(&both, Path::new("/tmp")).unwrap();
        assert!(!runtimes.managed);
        assert_eq!(runtimes.claude, Path::new("/usr/bin/claude"));
    }

    #[test]
    fn checksums_are_well_formed() {
        for arch in [Arch::X64, Arch::Arm64] {
            for download in [
                node_download(arch),
                claude_download(arch),
                codex_download(arch),
            ] {
                assert_eq!(download.sha256.len(), 64);
                assert!(download.sha256.chars().all(|c| c.is_ascii_hexdigit()));
                assert!(download.url.starts_with("https://"));
            }
        }
    }

    #[test]
    fn a_wrong_checksum_removes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        fs::write(&source, b"not the expected bytes").unwrap();
        let download = Download {
            url: format!("file://{}", source.display()),
            sha256: "0000000000000000000000000000000000000000000000000000000000000000",
        };
        let target = dir.path().join("target");
        // ureq does not speak file://; the error path is what matters here.
        assert!(fetch(&download, &target).is_err());
        assert!(!target.exists());
    }

    #[test]
    fn hex_is_lowercase() {
        assert_eq!(hex(&[0x00, 0xab, 0xff]), "00abff");
    }
}
