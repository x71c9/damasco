//! Client addresses behind the reverse proxy, and limits on repeated
//! attempts such as wrong logins.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::HeaderMap;

use crate::config::Proxy;

/// The address of the client. A request from a trusted proxy carries the
/// client in `proxy.header`: the right-most address that is not itself a
/// trusted proxy is the client, because earlier entries can be forged by the
/// client.
pub fn client_ip(peer: IpAddr, headers: &HeaderMap, proxy: &Proxy) -> IpAddr {
    if !proxy.trusted.contains(&peer) {
        return peer;
    }
    let chain: Vec<IpAddr> = headers
        .get_all(proxy.header.as_str())
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|part| part.trim().parse().ok())
        .collect();
    chain
        .into_iter()
        .rev()
        .find(|ip| !proxy.trusted.contains(ip))
        .unwrap_or(peer)
}

/// Key for an address: IPv6 clients usually own a whole /64, so they are
/// counted per /64.
pub fn address_key(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => format!("ip:{v4}"),
        IpAddr::V6(v6) => {
            let s = v6.segments();
            format!("ip:{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
        }
    }
}

/// Counts failed attempts per key within a sliding window.
pub struct Attempts {
    max: usize,
    window: Duration,
    seen: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl Attempts {
    pub fn new(max: usize, window: Duration) -> Self {
        Self {
            max,
            window,
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// True while `key` has fewer than `max` failures in the window.
    pub fn allowed(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        match seen.get_mut(key) {
            Some(times) => {
                self.prune(times, now);
                times.len() < self.max
            }
            None => true,
        }
    }

    pub fn failed(&self, key: &str) {
        let now = Instant::now();
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        if seen.len() > 10_000 {
            seen.retain(|_, times| {
                times.retain(|t| now.duration_since(*t) < self.window);
                !times.is_empty()
            });
        }
        let times = seen.entry(key.to_string()).or_default();
        self.prune(times, now);
        times.push_back(now);
    }

    pub fn clear(&self, key: &str) {
        self.seen
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(key);
    }

    fn prune(&self, times: &mut VecDeque<Instant>, now: Instant) {
        while times
            .front()
            .is_some_and(|t| now.duration_since(*t) >= self.window)
        {
            times.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(value: &str) -> HeaderMap {
        let mut map = HeaderMap::new();
        map.insert("x-forwarded-for", value.parse().unwrap());
        map
    }

    #[test]
    fn untrusted_peers_are_the_client() {
        let peer: IpAddr = "203.0.113.9".parse().unwrap();
        let ip = client_ip(peer, &headers("1.2.3.4"), &Proxy::default());
        assert_eq!(ip, peer);
    }

    #[test]
    fn behind_the_proxy_the_right_most_untrusted_address_wins() {
        let peer: IpAddr = "127.0.0.1".parse().unwrap();
        let forged_then_real = headers("6.6.6.6, 198.51.100.7");
        assert_eq!(
            client_ip(peer, &forged_then_real, &Proxy::default()),
            "198.51.100.7".parse::<IpAddr>().unwrap()
        );
        assert_eq!(client_ip(peer, &HeaderMap::new(), &Proxy::default()), peer);
    }

    #[test]
    fn ipv6_is_counted_per_64() {
        let a = address_key("2001:db8:1:2:aaaa::1".parse().unwrap());
        let b = address_key("2001:db8:1:2:bbbb::9".parse().unwrap());
        assert_eq!(a, b);
    }

    #[test]
    fn attempts_block_after_the_limit_and_clear_on_success() {
        let attempts = Attempts::new(3, Duration::from_secs(60));
        for _ in 0..3 {
            assert!(attempts.allowed("k"));
            attempts.failed("k");
        }
        assert!(!attempts.allowed("k"));
        attempts.clear("k");
        assert!(attempts.allowed("k"));
    }

    #[test]
    fn attempts_expire_with_the_window() {
        let attempts = Attempts::new(1, Duration::from_millis(20));
        attempts.failed("k");
        assert!(!attempts.allowed("k"));
        std::thread::sleep(Duration::from_millis(30));
        assert!(attempts.allowed("k"));
    }
}
