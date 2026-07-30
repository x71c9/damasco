//! Installation configuration, read from a TOML file at startup.

use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Hostnames served by one installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hosts {
    /// Control panel.
    pub panel: String,
    /// Preview of the unpublished state.
    pub preview: String,
    /// Published site, with any additional domains.
    pub site: Vec<String>,
}

/// Validated configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Local address the HTTP server listens on, behind the reverse proxy.
    pub listen: SocketAddr,
    /// Directory holding `axioma.db` and every other file of the installation.
    /// A relative path is resolved against the directory of the configuration
    /// file.
    pub data_dir: PathBuf,
    pub hosts: Hosts,
}

/// Which part of axioma a request is addressed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Panel,
    Preview,
    Site,
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
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHosts {
    panel: String,
    preview: String,
    site: Vec<String>,
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
        let hosts = Hosts {
            panel: normalize_host("hosts.panel", &raw.hosts.panel)?,
            preview: normalize_host("hosts.preview", &raw.hosts.preview)?,
            site: raw
                .hosts
                .site
                .iter()
                .map(|host| normalize_host("hosts.site", host))
                .collect::<Result<_, _>>()?,
        };
        if hosts.site.is_empty() {
            return Err(ConfigError("hosts.site: at least one hostname".into()));
        }
        let mut all: Vec<&str> = vec![&hosts.panel, &hosts.preview];
        all.extend(hosts.site.iter().map(String::as_str));
        for (index, host) in all.iter().enumerate() {
            if all[..index].contains(host) {
                return Err(ConfigError(format!(
                    "hosts: {host} is listed more than once; panel, preview and site need distinct hostnames"
                )));
            }
        }
        if raw.data_dir.as_os_str().is_empty() {
            return Err(ConfigError("data_dir: must not be empty".into()));
        }
        Ok(Self {
            listen: raw.listen,
            data_dir: raw.data_dir,
            hosts,
        })
    }

    /// Returns the role of a request hostname, already normalized.
    pub fn role_of(&self, host: &str) -> Option<Role> {
        if host == self.hosts.panel {
            Some(Role::Panel)
        } else if host == self.hosts.preview {
            Some(Role::Preview)
        } else if self.hosts.site.iter().any(|site| site == host) {
            Some(Role::Site)
        } else {
            None
        }
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
        data_dir = "/var/lib/axioma"

        [hosts]
        panel = "Panel.Example.com"
        preview = "preview.example.com."
        site = ["example.com", "www.example.com"]
    "#;

    #[test]
    fn parses_and_normalizes() {
        let config = Config::parse(VALID).unwrap();
        assert_eq!(config.listen, "127.0.0.1:4100".parse().unwrap());
        assert_eq!(config.hosts.panel, "panel.example.com");
        assert_eq!(config.hosts.preview, "preview.example.com");
        assert_eq!(config.hosts.site, ["example.com", "www.example.com"]);
    }

    #[test]
    fn maps_hosts_to_roles() {
        let config = Config::parse(VALID).unwrap();
        assert_eq!(config.role_of("panel.example.com"), Some(Role::Panel));
        assert_eq!(config.role_of("preview.example.com"), Some(Role::Preview));
        assert_eq!(config.role_of("www.example.com"), Some(Role::Site));
        assert_eq!(config.role_of("other.example.com"), None);
    }

    #[test]
    fn rejects_duplicate_hosts() {
        let text = VALID.replace("preview.example.com.", "example.com");
        let err = Config::parse(&text).unwrap_err().to_string();
        assert!(err.contains("more than once"), "{err}");
    }

    #[test]
    fn rejects_url_instead_of_host() {
        let text = VALID.replace("Panel.Example.com", "https://panel.example.com");
        let err = Config::parse(&text).unwrap_err().to_string();
        assert!(err.contains("hosts.panel"), "{err}");
    }

    #[test]
    fn rejects_empty_site_list() {
        let text = VALID.replace(r#"["example.com", "www.example.com"]"#, "[]");
        assert!(Config::parse(&text).is_err());
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
        let path = dir.path().join("axioma.toml");
        std::fs::write(&path, VALID.replace("/var/lib/axioma", "data")).unwrap();
        assert_eq!(
            Config::load(&path).unwrap().data_dir,
            dir.path().join("data")
        );
        std::fs::write(&path, VALID).unwrap();
        assert_eq!(
            Config::load(&path).unwrap().data_dir,
            Path::new("/var/lib/axioma")
        );
    }
}
