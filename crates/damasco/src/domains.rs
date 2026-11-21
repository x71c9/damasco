//! The domains of the sites: which names publish a site, the DNS records
//! they need, and whether a name reaches this installation yet. A domain is
//! served as soon as it is added; its records and its check only tell what
//! is missing.

use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

/// A domain of a site. With `redirect_to`, it sends its visitors to that
/// domain of the same site: the www added together with a domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Domain {
    pub host: String,
    pub site_id: String,
    pub redirect_to: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DomainError {
    /// Not a name a site can have; the message says why.
    Invalid(String),
    /// Served already, by this site or another, or by the installation.
    Taken(String),
    NotFound,
    Db(String),
}

impl std::fmt::Display for DomainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) | Self::Taken(message) | Self::Db(message) => {
                f.write_str(message)
            }
            Self::NotFound => f.write_str("no such domain on this site"),
        }
    }
}

impl From<rusqlite::Error> for DomainError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Db(err.to_string())
    }
}

/// The name as typed, made plain: lowercase, without scheme, path or
/// trailing dot. Refuses what a site cannot have: IP addresses, names of a
/// single label, non-ASCII names, the panel and the names under it.
pub fn normalize(typed: &str, panel: &str) -> Result<String, DomainError> {
    let invalid = |message: String| Err(DomainError::Invalid(message));
    let mut host = typed.trim();
    for scheme in ["https://", "http://"] {
        if host.len() >= scheme.len() && host[..scheme.len()].eq_ignore_ascii_case(scheme) {
            host = &host[scheme.len()..];
        }
    }
    let host = host
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host.is_empty() {
        return invalid("enter a domain, such as example.com".into());
    }
    if !host.is_ascii() {
        return invalid(
            "the name has non-ASCII characters: enter it in punycode, for example xn--caff-dsa.it for caffè.it"
                .into(),
        );
    }
    if host.parse::<IpAddr>().is_ok() || host.starts_with('[') || host.contains(':') {
        return invalid("a site is published at a domain name, not at an IP address".into());
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return invalid(format!("{host} is not a domain such as example.com"));
    }
    let label_ok = |label: &&str| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    if host.len() > 253 || !labels.iter().all(label_ok) {
        return invalid(format!(
            "{host} is not a valid domain: letters, digits and hyphens, separated by dots"
        ));
    }
    if labels
        .last()
        .is_some_and(|tld| tld.bytes().all(|b| b.is_ascii_digit()))
    {
        return invalid(format!("{host} is not a valid domain"));
    }
    if host == panel || host.ends_with(&format!(".{panel}")) {
        return invalid(format!(
            "{host} belongs to the panel and its previews: a site needs a domain of its own"
        ));
    }
    Ok(host)
}

/// Every domain of the installation.
pub fn list(conn: &Connection) -> rusqlite::Result<Vec<Domain>> {
    let mut statement =
        conn.prepare("SELECT host, site_id, redirect_to FROM domain ORDER BY created_at, host")?;
    statement
        .query_map([], |row| {
            Ok(Domain {
                host: row.get(0)?,
                site_id: row.get(1)?,
                redirect_to: row.get(2)?,
            })
        })?
        .collect()
}

/// The domains of the site `site_id`.
pub fn of_site(conn: &Connection, site_id: &str) -> rusqlite::Result<Vec<Domain>> {
    Ok(list(conn)?
        .into_iter()
        .filter(|domain| domain.site_id == site_id)
        .collect())
}

/// Adds `host`, already normalized, to the site `site_id`, and with `www`
/// also `www.<host>`, which redirects to it. `served` tells whether the
/// installation answers on a name already (the panel, a preview, a local
/// address): such a name is refused, as is a domain of any site.
pub fn add(
    conn: &Connection,
    site_id: &str,
    host: &str,
    www: bool,
    served: impl Fn(&str) -> bool,
) -> Result<Vec<Domain>, DomainError> {
    let mut added = vec![Domain {
        host: host.to_string(),
        site_id: site_id.to_string(),
        redirect_to: None,
    }];
    if www && !host.starts_with("www.") {
        added.push(Domain {
            host: format!("www.{host}"),
            site_id: site_id.to_string(),
            redirect_to: Some(host.to_string()),
        });
    }
    let tx = conn.unchecked_transaction()?;
    for domain in &added {
        let owner: Option<String> = tx
            .query_row(
                "SELECT site_id FROM domain WHERE host = ?1",
                [&domain.host],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(owner) = owner {
            let whose = if owner == site_id {
                "this site"
            } else {
                "another site"
            };
            return Err(DomainError::Taken(format!(
                "{} is already a domain of {whose}",
                domain.host
            )));
        }
        if served(&domain.host) {
            return Err(DomainError::Taken(format!(
                "{} is already an address of this installation",
                domain.host
            )));
        }
        tx.execute(
            "INSERT INTO domain (host, site_id, redirect_to) VALUES (?1, ?2, ?3)",
            params![domain.host, domain.site_id, domain.redirect_to],
        )?;
    }
    tx.commit()?;
    Ok(added)
}

/// Removes `host` from the site `site_id`, with the www that redirects to
/// it. Returns the names removed.
pub fn remove(conn: &Connection, site_id: &str, host: &str) -> Result<Vec<String>, DomainError> {
    let removed: Vec<String> = of_site(conn, site_id)?
        .into_iter()
        .filter(|domain| domain.host == host || domain.redirect_to.as_deref() == Some(host))
        .map(|domain| domain.host)
        .collect();
    if !removed.iter().any(|name| name == host) {
        return Err(DomainError::NotFound);
    }
    for name in &removed {
        conn.execute(
            "DELETE FROM domain WHERE host = ?1 AND site_id = ?2",
            params![name, site_id],
        )?;
    }
    Ok(removed)
}

/// A DNS record to create at the registrar of a domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Record {
    /// `A`, `AAAA` or `CNAME`.
    pub kind: &'static str,
    pub name: String,
    pub value: String,
}

/// The addresses `host` resolves to with the system's resolver, IPv4 first;
/// none when it does not resolve. Blocking.
pub fn addresses(host: &str) -> Vec<IpAddr> {
    let mut found: Vec<IpAddr> = (host, 443)
        .to_socket_addrs()
        .map(|addresses| addresses.map(|address| address.ip()).collect())
        .unwrap_or_default();
    found.sort_by_key(|ip| (ip.is_ipv6(), *ip));
    found.dedup();
    found
}

/// How long the panel waits for the system's resolver: a name that does not
/// exist can keep it busy for many seconds.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(3);

/// `addresses`, without blocking, and none after `LOOKUP_TIMEOUT`.
pub async fn lookup(host: String) -> Vec<IpAddr> {
    let task = tokio::task::spawn_blocking(move || addresses(&host));
    tokio::time::timeout(LOOKUP_TIMEOUT, task)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default()
}

/// The addresses of the panel, and whether the previews' names,
/// `*.preview.<panel>`, resolve to them: a made-up name stands for any
/// preview.
pub async fn panel_and_wildcard(panel: &str) -> (Vec<IpAddr>, bool) {
    let mut random = [0u8; 6];
    getrandom::fill(&mut random).expect("the operating system random source");
    let label: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let (addresses, previews) = tokio::join!(
        lookup(panel.to_string()),
        lookup(format!("check-{label}.preview.{panel}"))
    );
    let wildcard = !previews.is_empty() && previews == addresses;
    (addresses, wildcard)
}

/// The records that make `domain` reach this installation: the addresses of
/// the panel for a domain, a CNAME for a www that redirects.
pub fn records(domain: &Domain, panel_addresses: &[IpAddr]) -> Vec<Record> {
    if let Some(target) = &domain.redirect_to {
        return vec![Record {
            kind: "CNAME",
            name: domain.host.clone(),
            value: target.clone(),
        }];
    }
    address_records(&domain.host, panel_addresses)
}

/// A and AAAA records of `name` for `addresses`.
pub fn address_records(name: &str, addresses: &[IpAddr]) -> Vec<Record> {
    addresses
        .iter()
        .map(|ip| Record {
            kind: if ip.is_ipv4() { "A" } else { "AAAA" },
            name: name.to_string(),
            value: ip.to_string(),
        })
        .collect()
}

/// What a check of a domain found, from the best to the worst.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Check {
    /// HTTPS reaches this installation.
    Active,
    /// HTTP reaches this installation, HTTPS not yet: the certificate is
    /// still missing.
    NoCertificate,
    /// Another server answers: the DNS points elsewhere, or a proxy or a
    /// CDN stands in front.
    OtherServer,
    /// Nothing answers, or the name does not exist.
    Unreachable,
}

/// The random id of the installation, which `/_damasco/health` names.
pub fn instance_id(conn: &Connection) -> rusqlite::Result<String> {
    conn.query_row("SELECT instance_id FROM installation", [], |row| row.get(0))
}

/// The body of `/_damasco/health` for the installation `instance_id`.
pub fn health_body(instance_id: &str) -> String {
    format!("ok {instance_id}")
}

/// Checks `host` over HTTPS, then over HTTP. Blocking.
pub fn check(host: &str, instance_id: &str) -> Check {
    check_at(
        &format!("https://{host}/_damasco/health"),
        &format!("http://{host}/_damasco/health"),
        instance_id,
    )
}

/// What one request found.
enum Answer {
    /// The body of `/_damasco/health` of this installation.
    Ours,
    /// A redirect to HTTPS: a proxy that does not serve HTTP itself.
    ToHttps,
    /// Any other answer of a server.
    Other,
    /// The name does not resolve.
    NoName,
    /// No answer: refused, timed out, or TLS failed.
    Nothing,
}

fn ask(url: &str, expected: &str) -> Answer {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into();
    match agent.get(url).call() {
        Ok(mut response) => {
            let status = response.status();
            if status.is_redirection() {
                let to_https = response
                    .headers()
                    .get("location")
                    .and_then(|location| location.to_str().ok())
                    .is_some_and(|location| location.starts_with("https://"));
                return if to_https {
                    Answer::ToHttps
                } else {
                    Answer::Other
                };
            }
            let body = response
                .body_mut()
                .with_config()
                .limit(4096)
                .read_to_string()
                .unwrap_or_default();
            if status.is_success() && body.trim() == expected {
                Answer::Ours
            } else {
                Answer::Other
            }
        }
        Err(ureq::Error::HostNotFound) => Answer::NoName,
        Err(_) => Answer::Nothing,
    }
}

fn check_at(https: &str, http: &str, instance_id: &str) -> Check {
    let expected = health_body(instance_id);
    match ask(https, &expected) {
        Answer::Ours => return Check::Active,
        Answer::Other | Answer::ToHttps => return Check::OtherServer,
        Answer::NoName => return Check::Unreachable,
        Answer::Nothing => {}
    }
    match ask(http, &expected) {
        // A proxy redirecting to HTTPS while HTTPS fails is, most likely,
        // Caddy still obtaining the certificate.
        Answer::Ours | Answer::ToHttps => Check::NoCertificate,
        Answer::Other => Check::OtherServer,
        Answer::NoName | Answer::Nothing => Check::Unreachable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    const PANEL: &str = "admin.example.com";

    fn site_db() -> Connection {
        let conn = crate::db::memory_installation();
        for (id, slug) in [("A", "blog"), ("B", "shop")] {
            conn.execute(
                "INSERT INTO site (id, slug, name) VALUES (?1, ?2, ?2)",
                [id, slug],
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn names_are_made_plain_and_odd_ones_refused() {
        for (typed, plain) in [
            ("Shop.IT", "shop.it"),
            ("  https://shop.it/about  ", "shop.it"),
            ("shop.it.", "shop.it"),
            ("xn--caff-dsa.it", "xn--caff-dsa.it"),
            ("www.shop.co.uk", "www.shop.co.uk"),
        ] {
            assert_eq!(normalize(typed, PANEL), Ok(plain.into()), "{typed}");
        }
        for typed in [
            "",
            "caffè.it",
            "192.168.1.1",
            "[::1]",
            "::1",
            "localhost",
            "shop..it",
            "-shop.it",
            "shop_x.it",
            "1.2.3.4.5",
            PANEL,
            "blog.preview.admin.example.com",
        ] {
            assert!(
                matches!(normalize(typed, PANEL), Err(DomainError::Invalid(_))),
                "{typed}"
            );
        }
    }

    #[test]
    fn a_domain_comes_with_its_www_and_goes_with_it() {
        let conn = site_db();
        let added = add(&conn, "A", "shop.it", true, |_| false).unwrap();
        assert_eq!(
            added
                .iter()
                .map(|d| (d.host.as_str(), d.redirect_to.as_deref()))
                .collect::<Vec<_>>(),
            [("shop.it", None), ("www.shop.it", Some("shop.it"))]
        );
        // A www needs no www of its own.
        add(&conn, "A", "www.other.it", true, |_| false).unwrap();
        assert_eq!(list(&conn).unwrap().len(), 3);

        assert_eq!(
            remove(&conn, "B", "shop.it"),
            Err(DomainError::NotFound),
            "another site's domain stays"
        );
        assert_eq!(
            remove(&conn, "A", "shop.it").unwrap(),
            ["shop.it", "www.shop.it"]
        );
        assert_eq!(of_site(&conn, "A").unwrap().len(), 1);
    }

    #[test]
    fn a_name_in_use_is_refused_whole() {
        let conn = site_db();
        add(&conn, "A", "www.shop.it", false, |_| false).unwrap();
        // shop.it is free, its www is not: neither is added.
        let refused = add(&conn, "B", "shop.it", true, |_| false);
        assert!(matches!(refused, Err(DomainError::Taken(_))), "{refused:?}");
        assert!(of_site(&conn, "B").unwrap().is_empty());
        let refused = add(&conn, "B", "blog.localhost", false, |h| {
            h == "blog.localhost"
        });
        assert!(matches!(refused, Err(DomainError::Taken(_))), "{refused:?}");
    }

    #[test]
    fn deleting_a_site_deletes_its_domains() {
        let conn = site_db();
        add(&conn, "A", "shop.it", true, |_| false).unwrap();
        crate::sites::delete(&conn, "A").unwrap();
        assert!(list(&conn).unwrap().is_empty());
    }

    #[test]
    fn records_follow_the_panel() {
        let ips: Vec<IpAddr> = vec![
            "203.0.113.7".parse().unwrap(),
            "2001:db8::7".parse().unwrap(),
        ];
        let domain = Domain {
            host: "shop.it".into(),
            site_id: "A".into(),
            redirect_to: None,
        };
        let kinds: Vec<_> = records(&domain, &ips)
            .into_iter()
            .map(|r| (r.kind, r.value))
            .collect();
        assert_eq!(
            kinds,
            [("A", "203.0.113.7".into()), ("AAAA", "2001:db8::7".into())]
        );
        let www = Domain {
            host: "www.shop.it".into(),
            redirect_to: Some("shop.it".into()),
            ..domain
        };
        assert_eq!(
            records(&www, &ips),
            [Record {
                kind: "CNAME",
                name: "www.shop.it".into(),
                value: "shop.it".into()
            }]
        );
    }

    /// A server on a free local port answering every request with
    /// `response`; returns its address.
    fn server(response: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut buffer = [0; 1024];
                let _ = stream.read(&mut buffer);
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{address}/_damasco/health")
    }

    /// An address where nothing listens.
    fn closed() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        format!("http://{address}/_damasco/health")
    }

    fn reply(body: &str) -> &'static str {
        Box::leak(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .into_boxed_str(),
        )
    }

    #[test]
    fn the_check_tells_the_four_situations_apart() {
        let ours = server(reply("ok ID"));
        let other = server(reply("ok ANOTHER"));
        let to_https = server(
            "HTTP/1.1 308 Permanent Redirect\r\nlocation: https://shop.it/_damasco/health\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        );
        let none = closed();
        // The first address stands for HTTPS, the second for HTTP.
        assert_eq!(check_at(&ours, &none, "ID"), Check::Active);
        assert_eq!(check_at(&none, &ours, "ID"), Check::NoCertificate);
        assert_eq!(check_at(&none, &to_https, "ID"), Check::NoCertificate);
        assert_eq!(check_at(&other, &ours, "ID"), Check::OtherServer);
        assert_eq!(check_at(&none, &other, "ID"), Check::OtherServer);
        assert_eq!(check_at(&none, &none, "ID"), Check::Unreachable);
        assert_eq!(
            check_at("http://nothing.invalid/_damasco/health", &none, "ID"),
            Check::Unreachable
        );
    }
}
