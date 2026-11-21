//! Egress of the sandbox: an HTTP CONNECT proxy on a Unix socket.
//!
//! The sandbox has no network of its own. Inside it, `bridge` forwards
//! 127.0.0.1:3128 to this proxy, which opens a TCP connection only when:
//! - the request is `CONNECT host:443` (HTTPS, end to end encrypted);
//! - the host is in the profile's allowlist, or a person approves it
//!   (agent only: the connection waits for the answer);
//! - every address the host resolves to is public: loopback, private,
//!   link-local and similar ranges are refused, so a hostname cannot point
//!   the sandbox at the machine itself or at the local network.

use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpStream, UnixListener, UnixStream};

/// Hosts every assistant may reach: the npm registry for the site's
/// dependencies. The hosts of the assistant in use (its model and its
/// login) pass through the approval, without a person.
pub const AGENT_ALLOW: &[&str] = &["registry.npmjs.org"];

/// Hosts a build may reach: only the npm registry.
pub const BUILD_ALLOW: &[&str] = &["registry.npmjs.org"];

/// Decides on a host outside the allowlist; true lets the connection go on.
/// It may wait, for example for a person to answer in the chat.
pub type Approve =
    Arc<dyn Fn(String) -> futures_util::future::BoxFuture<'static, bool> + Send + Sync>;

const HEAD_LIMIT: usize = 8 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Hostnames allowed through the proxy. `*.example.com` matches every
/// subdomain of example.com, not example.com itself.
#[derive(Debug, Clone)]
pub struct Allowlist(Vec<String>);

impl Allowlist {
    pub fn new<S: AsRef<str>>(entries: &[S]) -> Self {
        Self(
            entries
                .iter()
                .map(|e| e.as_ref().to_ascii_lowercase())
                .collect(),
        )
    }

    pub fn allows(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.0.iter().any(|entry| match entry.strip_prefix("*.") {
            Some(domain) => host
                .strip_suffix(domain)
                .is_some_and(|rest| rest.ends_with('.') && rest.len() > 1),
            None => *entry == host,
        })
    }
}

/// Addresses on the public internet. Everything else stays out of reach.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || a == 0
                || (a == 100 && (64..128).contains(&b)) // shared address space
                || a >= 224) // multicast and reserved
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00 // unique local
                || (first & 0xffc0) == 0xfe80) // link-local
        }
    }
}

/// A DNS name: letters, digits, dashes and dots, at most 253 characters.
pub fn is_hostname(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

/// Serves the proxy on `socket` until the task is dropped. `name` labels
/// the log lines (agent or build).
pub async fn serve(
    socket: &Path,
    allow: Allowlist,
    name: &'static str,
    approve: Option<Approve>,
) -> std::io::Result<()> {
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket)?;
    let allow = Arc::new(allow);
    loop {
        let (stream, _) = listener.accept().await?;
        let allow = allow.clone();
        let approve = approve.clone();
        tokio::spawn(async move {
            if let Err(err) = handle(stream, &allow, name, approve.as_ref()).await {
                eprintln!("damasco: {name} sandbox proxy: {err}");
            }
        });
    }
}

async fn handle(
    stream: UnixStream,
    allow: &Allowlist,
    name: &str,
    approve: Option<&Approve>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    let mut head = String::new();
    loop {
        let read = reader.read_line(&mut head).await?;
        if read == 0 || head.len() > HEAD_LIMIT {
            return Ok(());
        }
        if head.ends_with("\r\n\r\n") || head == "\r\n" {
            break;
        }
    }
    let first = head.lines().next().unwrap_or_default();
    let mut parts = first.split_whitespace();
    let (method, target) = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    );
    if method != "CONNECT" {
        return reply(
            reader,
            "405 Method Not Allowed",
            "only HTTPS through CONNECT",
        )
        .await;
    }
    let Some((host, port)) = target.rsplit_once(':') else {
        return reply(reader, "400 Bad Request", "expected host:port").await;
    };
    if port != "443" {
        eprintln!("damasco: {name} sandbox: refused {target}: only port 443");
        return reply(reader, "403 Forbidden", "only port 443").await;
    }
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let host = host.as_str();
    if !is_hostname(host) {
        return reply(reader, "400 Bad Request", "expected a hostname").await;
    }
    if !allow.allows(host) {
        let approved = match approve {
            Some(approve) => approve(host.to_string()).await,
            None => false,
        };
        if !approved {
            eprintln!("damasco: {name} sandbox: refused {host}: not in the allowlist");
            return reply(reader, "403 Forbidden", "host not allowed").await;
        }
    }
    let addresses: Vec<SocketAddr> = match tokio::net::lookup_host((host, 443)).await {
        Ok(found) => found.collect(),
        Err(_) => return reply(reader, "502 Bad Gateway", "host not found").await,
    };
    if addresses.is_empty() || !addresses.iter().all(|a| is_public(a.ip())) {
        eprintln!("damasco: {name} sandbox: refused {host}: resolves to a non-public address");
        return reply(reader, "403 Forbidden", "address not public").await;
    }
    let upstream =
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&addresses[..])).await {
            Ok(Ok(stream)) => stream,
            _ => return reply(reader, "502 Bad Gateway", "connection failed").await,
        };
    // Bytes the client sent after the request head stay in the buffer and
    // must reach the upstream first.
    let buffered = reader.buffer().to_vec();
    let mut client = reader.into_inner();
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    let mut upstream = upstream;
    upstream.write_all(&buffered).await?;
    tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
}

async fn reply(reader: BufReader<UnixStream>, status: &str, reason: &str) -> std::io::Result<()> {
    let mut stream = reader.into_inner();
    let body = format!("{reason}\n");
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[test]
    fn allowlist_matches_hosts_and_subdomains() {
        let allow = Allowlist::new(&["*.anthropic.com", "registry.npmjs.org"]);
        assert!(allow.allows("api.anthropic.com"));
        assert!(allow.allows("API.Anthropic.com."));
        assert!(allow.allows("registry.npmjs.org"));
        assert!(!allow.allows("anthropic.com"));
        assert!(!allow.allows("evilanthropic.com"));
        assert!(!allow.allows("anthropic.com.evil.org"));
        assert!(!allow.allows("npmjs.org"));
    }

    #[test]
    fn only_public_addresses_pass() {
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!is_public(private.parse().unwrap()), "{private}");
        }
        for public in ["1.1.1.1", "160.79.104.10", "2606:4700::1111"] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
    }

    async fn ask(socket: &Path, request: &str) -> String {
        let mut stream = UnixStream::connect(socket).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).await.unwrap();
        answer.lines().next().unwrap_or_default().to_string()
    }

    #[tokio::test]
    async fn hosts_outside_the_list_go_to_the_approver() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("proxy.sock");
        let path = socket.clone();
        let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let approve: Approve = {
            let asked = asked.clone();
            Arc::new(move |host: String| {
                asked.lock().unwrap().push(host.clone());
                Box::pin(async move { host == "localhost" })
            })
        };
        let server = tokio::spawn(async move {
            serve(&path, Allowlist::new::<&str>(&[]), "test", Some(approve)).await
        });
        for _ in 0..50 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let body = |answer: String| answer.lines().last().unwrap_or_default().to_string();
        let refused = full(&socket, "CONNECT Evil.Example.:443 HTTP/1.1\r\n\r\n").await;
        assert_eq!(body(refused), "host not allowed");
        // Approved, then refused for resolving to loopback: the approval
        // never skips the address check.
        let approved = full(&socket, "CONNECT localhost:443 HTTP/1.1\r\n\r\n").await;
        assert_eq!(body(approved), "address not public");
        let odd = full(&socket, "CONNECT bad_host!:443 HTTP/1.1\r\n\r\n").await;
        assert_eq!(body(odd), "expected a hostname");
        assert_eq!(*asked.lock().unwrap(), ["evil.example", "localhost"]);
        server.abort();
    }

    async fn full(socket: &Path, request: &str) -> String {
        let mut stream = UnixStream::connect(socket).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).await.unwrap();
        answer
    }

    #[tokio::test]
    async fn refuses_plain_http_other_ports_unknown_and_local_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("proxy.sock");
        let path = socket.clone();
        let allow = Allowlist::new(&["localhost", "allowed.example"]);
        let server = tokio::spawn(async move { serve(&path, allow, "test", None).await });
        for _ in 0..50 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            ask(&socket, "GET http://localhost/ HTTP/1.1\r\n\r\n").await,
            "HTTP/1.1 405 Method Not Allowed"
        );
        assert_eq!(
            ask(&socket, "CONNECT localhost:80 HTTP/1.1\r\n\r\n").await,
            "HTTP/1.1 403 Forbidden"
        );
        assert_eq!(
            ask(&socket, "CONNECT evil.example:443 HTTP/1.1\r\n\r\n").await,
            "HTTP/1.1 403 Forbidden"
        );
        // Allowed by name, refused because it resolves to loopback.
        assert_eq!(
            ask(&socket, "CONNECT localhost:443 HTTP/1.1\r\n\r\n").await,
            "HTTP/1.1 403 Forbidden"
        );
        server.abort();
    }
}
