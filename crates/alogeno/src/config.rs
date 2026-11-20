//! Installation configuration, read from a TOML file at startup.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The panel's hostname and how browsers reach it. The previews and the
/// sites' addresses follow from it and from the sites in the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hosts {
    /// Control panel; previews are `<slug>.preview.<panel>`.
    pub panel: String,
    /// How browsers reach these hostnames: "https" behind the reverse proxy,
    /// "http" in local development.
    pub scheme: String,
    /// Port in public addresses, when not the default of the scheme.
    pub port: Option<u16>,
}

/// Validated configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Local address the HTTP server listens on, behind the reverse proxy.
    pub listen: SocketAddr,
    /// Directory holding `alogeno.db` and every other file of the installation.
    /// A relative path is resolved against the directory of the configuration
    /// file.
    pub data_dir: PathBuf,
    pub hosts: Hosts,
    pub domains: Domains,
    pub proxy: Proxy,
    pub runtimes: RuntimeConfig,
    pub limits: LimitsConfig,
    pub sandbox: SandboxConfig,
}

/// Hosts the sandbox may reach on top of the built-in ones, for example
/// `github.com` or `*.githubusercontent.com`. Set by whoever runs the
/// installation, never by the agent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SandboxConfig {
    pub build_allow: Vec<String>,
    pub agent_allow: Vec<String>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSandbox {
    #[serde(default)]
    build_allow: Vec<String>,
    #[serde(default)]
    agent_allow: Vec<String>,
}

/// An allowlist entry: a hostname, or `*.` followed by a hostname.
fn allow_entry(field: &str, value: &str) -> Result<String, ConfigError> {
    let (wildcard, host) = match value.trim().strip_prefix("*.") {
        Some(rest) => (true, rest),
        None => (false, value.trim()),
    };
    let host = normalize_host(field, host)?;
    if !host.contains('.') {
        return Err(ConfigError(format!(
            "{field}: {value:?} is too broad; name a domain such as example.com"
        )));
    }
    Ok(if wildcard { format!("*.{host}") } else { host })
}

/// Memory ceilings of the sandbox, overriding the defaults computed from the
/// machine's memory. Sizes such as `512M` or `2G`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsConfig {
    pub agent_memory: Option<String>,
    pub build_memory: Option<String>,
}

/// Runtimes already on the machine, used instead of the ones alogeno
/// downloads (for example on NixOS). Both or neither; a name without a slash
/// is looked up in PATH.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    pub node: Option<String>,
    pub claude: Option<String>,
    /// Codex, optional: without it the assistant is Claude only.
    pub codex: Option<String>,
}

/// The reverse proxy in front of alogeno. Requests from these addresses may
/// carry the client address in `header`; requests from anywhere else are
/// taken at face value.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proxy {
    #[serde(default = "default_trusted")]
    pub trusted: Vec<IpAddr>,
    #[serde(default = "default_client_header")]
    pub header: String,
}

impl Default for Proxy {
    fn default() -> Self {
        Self {
            trusted: default_trusted(),
            header: default_client_header(),
        }
    }
}

fn default_trusted() -> Vec<IpAddr> {
    vec![
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(Ipv6Addr::LOCALHOST),
    ]
}

fn default_client_header() -> String {
    "x-forwarded-for".into()
}

/// How the sites' own domains reach this installation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Domains {
    /// Loopback address where the reverse proxy asks whether a domain is
    /// served here (Caddy's on-demand TLS): set for the one installation of
    /// the machine whose domains are added from the panel alone, unset for
    /// the others, whose domains are added to the proxy by hand.
    pub ask_listen: Option<SocketAddr>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDomains {
    ask_listen: Option<SocketAddr>,
}

#[derive(Debug)]
pub struct ConfigError(String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    listen: SocketAddr,
    data_dir: PathBuf,
    hosts: RawHosts,
    #[serde(default)]
    domains: RawDomains,
    #[serde(default)]
    proxy: Proxy,
    #[serde(default)]
    runtimes: RuntimeConfig,
    #[serde(default)]
    limits: LimitsConfig,
    #[serde(default)]
    sandbox: RawSandbox,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHosts {
    panel: String,
    // Keys of alogeno 1.x, refused with a message of their own.
    preview: Option<toml::Value>,
    site: Option<toml::Value>,
    #[serde(default = "default_scheme")]
    scheme: String,
    port: Option<u16>,
}

fn default_scheme() -> String {
    "https".into()
}

impl Config {
    /// Reads and validates the configuration file at `path`.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path)
            .map_err(|err| ConfigError(format!("cannot read {}: {err}", path.display())))?;
        let mut config =
            Self::parse(&text).map_err(|err| ConfigError(format!("{}: {err}", path.display())))?;
        if config.data_dir.is_relative() {
            let base = path.parent().unwrap_or(Path::new("."));
            config.data_dir = base.join(&config.data_dir);
        }
        Ok(config)
    }

    /// Parses and validates a configuration from TOML text.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let raw: RawConfig = toml::from_str(text).map_err(|err| ConfigError(err.to_string()))?;
        if raw.hosts.preview.is_some() || raw.hosts.site.is_some() {
            return Err(ConfigError(
                "hosts.preview and hosts.site belong to the configuration of alogeno 1.x: \
                 alogeno 2 needs a new installation (see \"Moving from 1.x\" in docs/install.md)"
                    .into(),
            ));
        }
        if let Some(ask) = raw.domains.ask_listen
            && !ask.ip().is_loopback()
        {
            return Err(ConfigError(format!(
                "domains.ask_listen: {ask} is not a loopback address; only the local reverse proxy asks there"
            )));
        }
        let hosts = Hosts {
            panel: normalize_host("hosts.panel", &raw.hosts.panel)?,
            scheme: match raw.hosts.scheme.as_str() {
                "https" | "http" => raw.hosts.scheme.clone(),
                other => {
                    return Err(ConfigError(format!(
                        "hosts.scheme: {other:?} is not https or http"
                    )));
                }
            },
            port: raw.hosts.port,
        };
        if raw.data_dir.as_os_str().is_empty() {
            return Err(ConfigError("data_dir: must not be empty".into()));
        }
        Ok(Self {
            listen: raw.listen,
            data_dir: raw.data_dir,
            hosts,
            domains: Domains {
                ask_listen: raw.domains.ask_listen,
            },
            proxy: Proxy {
                header: raw.proxy.header.to_ascii_lowercase(),
                ..raw.proxy
            },
            runtimes: raw.runtimes,
            limits: raw.limits,
            sandbox: SandboxConfig {
                build_allow: raw
                    .sandbox
                    .build_allow
                    .iter()
                    .map(|entry| allow_entry("sandbox.build_allow", entry))
                    .collect::<Result<_, _>>()?,
                agent_allow: raw
                    .sandbox
                    .agent_allow
                    .iter()
                    .map(|entry| allow_entry("sandbox.agent_allow", entry))
                    .collect::<Result<_, _>>()?,
            },
        })
    }

    /// Public address of the panel, for links handed to people.
    pub fn panel_url(&self) -> String {
        self.url_of(&self.hosts.panel)
    }

    /// Public address of one of the hostnames, with the scheme and port
    /// browsers use.
    pub fn url_of(&self, host: &str) -> String {
        let port = self.hosts.port.map(|p| format!(":{p}")).unwrap_or_default();
        format!("{}://{host}{port}", self.hosts.scheme)
    }

    /// Local development: the panel is `localhost` or a `*.localhost` name,
    /// which browsers resolve to the loopback address without DNS. Each site
    /// is then also served at `<slug>.<parent of the panel>`, for example
    /// `blog.localhost` next to `admin.localhost`.
    pub fn local(&self) -> bool {
        self.hosts.panel == "localhost" || self.hosts.panel.ends_with(".localhost")
    }

    /// The preview's hostname of the site `slug`.
    pub fn preview_host(&self, slug: &str) -> String {
        format!("{slug}.preview.{}", self.hosts.panel)
    }

    /// In local development, the hostname that serves the published site
    /// `slug`; elsewhere a site is published at its own domains.
    pub fn local_host(&self, slug: &str) -> Option<String> {
        if !self.local() {
            return None;
        }
        let parent = self
            .hosts
            .panel
            .split_once('.')
            .map_or("localhost", |(_, parent)| parent);
        Some(format!("{slug}.{parent}"))
    }
}

/// Lowercases a hostname and removes a trailing dot. Rejects ports, paths,
/// schemes and empty values.
pub fn normalize_host(field: &str, value: &str) -> Result<String, ConfigError> {
    let host = value.trim().trim_end_matches('.').to_ascii_lowercase();
    let valid = !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
    if valid {
        Ok(host)
    } else {
        Err(ConfigError(format!(
            "{field}: {value:?} is not a hostname (expected something like example.com, without scheme, port or path)"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
        listen = "127.0.0.1:4100"
        data_dir = "/var/lib/alogeno"

        [hosts]
        panel = "Panel.Example.com."
    "#;

    #[test]
    fn parses_and_normalizes() {
        let config = Config::parse(VALID).unwrap();
        assert_eq!(config.listen, "127.0.0.1:4100".parse().unwrap());
        assert_eq!(config.hosts.panel, "panel.example.com");
        assert_eq!(config.domains.ask_listen, None);
        assert_eq!(
            config.preview_host("blog"),
            "blog.preview.panel.example.com"
        );
        assert!(!config.local());
        assert_eq!(config.local_host("blog"), None);
    }

    #[test]
    fn local_development_serves_each_site_next_to_the_panel() {
        let config =
            Config::parse(&VALID.replace("Panel.Example.com.", "admin.localhost")).unwrap();
        assert!(config.local());
        assert_eq!(config.preview_host("blog"), "blog.preview.admin.localhost");
        assert_eq!(config.local_host("blog").as_deref(), Some("blog.localhost"));
        let bare = Config::parse(&VALID.replace("Panel.Example.com.", "localhost")).unwrap();
        assert_eq!(bare.local_host("blog").as_deref(), Some("blog.localhost"));
    }

    #[test]
    fn the_configuration_of_alogeno_1_is_refused() {
        for old in [
            "preview = \"preview.example.com\"",
            "site = [\"example.com\"]",
        ] {
            let text = VALID.replace("[hosts]", &format!("[hosts]\n{old}"));
            let err = Config::parse(&text).unwrap_err().to_string();
            assert!(err.contains("alogeno 1.x"), "{err}");
        }
    }

    #[test]
    fn the_ask_address_is_on_loopback() {
        let text = format!("{VALID}\n[domains]\nask_listen = \"127.0.0.1:4099\"\n");
        let config = Config::parse(&text).unwrap();
        assert_eq!(
            config.domains.ask_listen,
            Some("127.0.0.1:4099".parse().unwrap())
        );
        let open = text.replace("127.0.0.1:4099", "0.0.0.0:4099");
        let err = Config::parse(&open).unwrap_err().to_string();
        assert!(err.contains("loopback"), "{err}");
    }

    #[test]
    fn rejects_url_instead_of_host() {
        let text = VALID.replace("Panel.Example.com.", "https://panel.example.com");
        let err = Config::parse(&text).unwrap_err().to_string();
        assert!(err.contains("hosts.panel"), "{err}");
    }

    #[test]
    fn rejects_unknown_fields() {
        let text = format!("{VALID}\n[extra]\nkey = 1\n");
        assert!(Config::parse(&text).is_err());
    }

    #[test]
    fn load_reports_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.toml");
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(err.contains("missing.toml"), "{err}");
    }

    #[test]
    fn relative_data_dir_follows_the_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("alogeno.toml");
        std::fs::write(&path, VALID.replace("/var/lib/alogeno", "data")).unwrap();
        assert_eq!(
            Config::load(&path).unwrap().data_dir,
            dir.path().join("data")
        );
        std::fs::write(&path, VALID).unwrap();
        assert_eq!(
            Config::load(&path).unwrap().data_dir,
            Path::new("/var/lib/alogeno")
        );
    }

    #[test]
    fn sandbox_allowlists_are_checked() {
        let text = format!(
            "{VALID}\n[sandbox]\nbuild_allow = [\"GitHub.com\", \"*.githubusercontent.com\"]\n"
        );
        let config = Config::parse(&text).unwrap();
        assert_eq!(
            config.sandbox.build_allow,
            ["github.com", "*.githubusercontent.com"]
        );
        for bad in [
            "\"*\"",
            "\"*.com\"",
            "\"https://example.com\"",
            "\"localhost\"",
        ] {
            let text = format!("{VALID}\n[sandbox]\nagent_allow = [{bad}]\n");
            assert!(Config::parse(&text).is_err(), "{bad}");
        }
    }
}
