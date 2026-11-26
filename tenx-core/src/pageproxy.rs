//! `tenx web`'s page proxy: how a phone reaches a task's dev server. A task's
//! page at `http://localhost:3000` means the Mac's own port 3000, which the
//! phone can't reach; the iOS app sends a page's `localhost` connections to
//! this proxy instead (`WKWebsiteDataStore.proxyConfigurations`), so the
//! address, its cookies and the redirects set up for it stay `localhost`.
//!
//! The proxy speaks HTTP `CONNECT` only, over plain TCP on loopback: TLS and
//! who may connect at all are Tailscale's (`tailscale serve
//! --tls-terminated-tcp`, since its HTTP mode rewrites a proxy request).
//! Here: the page token as `Proxy-Authorization`, and only to a port a task's
//! own process listens on — never any other service on the machine.

use base64::Engine;
use std::collections::HashSet;

/// The proxy's port when `tenx web` isn't told another (`--proxy-port`).
pub const DEFAULT_PORT: u16 = 7075;

/// A request head longer than this is refused unread.
pub const MAX_HEAD: usize = 8 * 1024;

/// What to do with a connection, from its request head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Answer `200 Connection Established` and join it to this local port.
    Tunnel(u16),
    /// Answer this and close.
    Refuse(Refusal),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Not a `CONNECT`, or no target in it.
    BadRequest,
    /// No token, or the wrong one: asks for one.
    NeedsToken,
    /// Somewhere other than this machine's loopback.
    NotLocal,
    /// A local port no task listens on.
    NotATaskPort,
}

impl Refusal {
    /// The whole response.
    pub fn response(self) -> &'static str {
        match self {
            Refusal::BadRequest => "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            Refusal::NeedsToken => {
                "HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"tenx\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            }
            Refusal::NotLocal | Refusal::NotATaskPort => "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        }
    }
}

pub const ESTABLISHED: &str = "HTTP/1.1 200 Connection Established\r\n\r\n";

/// Whether `head` (the bytes so far) holds the whole request head.
pub fn head_complete(head: &[u8]) -> bool {
    head.windows(4).any(|w| w == b"\r\n\r\n")
}

/// A request the proxy may serve: the loopback address it names and the port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub addr: std::net::IpAddr,
    pub port: u16,
    /// Named `localhost`: IPv6's `::1` is tried when nothing answers on
    /// IPv4's, as a browser does (Vite on a Mac listens on `::1` alone).
    pub either: bool,
}

/// Decide on a request head: `CONNECT localhost:<port>`, with the page token
/// as the password of `Proxy-Authorization: Basic` (any user name), to a
/// port in `task_ports`.
pub fn decide(head: &str, token: &str, task_ports: &HashSet<u16>) -> Decision {
    match request(head, token) {
        Err(why) => Decision::Refuse(why),
        Ok(t) => port_decision(t, task_ports),
    }
}

/// The port step of `decide`, for a request `request` let through: the
/// caller looks the tasks' ports up only for those.
pub fn port_decision(target: Target, task_ports: &HashSet<u16>) -> Decision {
    if task_ports.contains(&target.port) { Decision::Tunnel(target.port) } else { Decision::Refuse(Refusal::NotATaskPort) }
}

/// Everything `decide` checks before the port: the method, the token, a
/// loopback name. `localhost` is IPv4's first, then IPv6's (`either`): the
/// one name the iOS app sends here.
pub fn request(head: &str, token: &str) -> Result<Target, Refusal> {
    let mut lines = head.split("\r\n");
    let Some((method, target)) = lines.next().and_then(|l| {
        let mut p = l.split(' ');
        Some((p.next()?, p.next()?))
    }) else {
        return Err(Refusal::BadRequest);
    };
    if !method.eq_ignore_ascii_case("CONNECT") {
        return Err(Refusal::BadRequest);
    }
    let auth = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("proxy-authorization"))
        .map(|(_, value)| value.trim());
    if !auth.is_some_and(|a| token_in(a, token)) {
        return Err(Refusal::NeedsToken);
    }
    let Some((host, port)) = target.rsplit_once(':') else { return Err(Refusal::BadRequest) };
    let Ok(port) = port.parse::<u16>() else { return Err(Refusal::BadRequest) };
    let addr = loopback_addr(host).ok_or(Refusal::NotLocal)?;
    Ok(Target { addr, port, either: host.eq_ignore_ascii_case("localhost") })
}

/// `Basic base64(user:token)`, compared in constant time.
fn token_in(header: &str, token: &str) -> bool {
    let Some(encoded) = header.strip_prefix("Basic ").or_else(|| header.strip_prefix("basic ")) else { return false };
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded.trim()) else { return false };
    let Ok(pair) = String::from_utf8(decoded) else { return false };
    let Some((_, given)) = pair.split_once(':') else { return false };
    crate::web::token_matches(given, token)
}

/// The loopback address a target names: exactly `localhost` (IPv4),
/// `127.0.0.1` or `::1` — stricter than `web::is_loopback` (a listen
/// address), since whatever passes here is connected to.
fn loopback_addr(host: &str) -> Option<std::net::IpAddr> {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    match host.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase().as_str() {
        "localhost" | "127.0.0.1" => Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        "::1" => Some(IpAddr::V6(Ipv6Addr::LOCALHOST)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth(token: &str) -> String {
        format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("tenx:{token}")))
    }

    fn ports() -> HashSet<u16> {
        [3000, 5173].into()
    }

    #[test]
    fn a_task_port_on_this_machine_is_tunnelled_with_the_token() {
        let head = format!("CONNECT localhost:3000 HTTP/1.1\r\nHost: localhost:3000\r\nProxy-Authorization: {}\r\n\r\n", auth("s3cret"));
        assert_eq!(decide(&head, "s3cret", &ports()), Decision::Tunnel(3000));
        let head = format!("CONNECT [::1]:5173 HTTP/1.1\r\nproxy-authorization: {}\r\n\r\n", auth("s3cret"));
        assert_eq!(decide(&head, "s3cret", &ports()), Decision::Tunnel(5173));
        let head = format!("CONNECT 127.0.0.1:3000 HTTP/1.1\r\nProxy-Authorization: {}\r\n\r\n", auth("s3cret"));
        assert_eq!(decide(&head, "s3cret", &ports()), Decision::Tunnel(3000));
    }

    #[test]
    fn no_token_or_a_wrong_one_asks_for_it() {
        let bare = "CONNECT localhost:3000 HTTP/1.1\r\nHost: localhost:3000\r\n\r\n";
        assert_eq!(decide(bare, "s3cret", &ports()), Decision::Refuse(Refusal::NeedsToken));
        let wrong = format!("CONNECT localhost:3000 HTTP/1.1\r\nProxy-Authorization: {}\r\n\r\n", auth("guess"));
        assert_eq!(decide(&wrong, "s3cret", &ports()), Decision::Refuse(Refusal::NeedsToken));
        let mangled = "CONNECT localhost:3000 HTTP/1.1\r\nProxy-Authorization: Basic !!!\r\n\r\n";
        assert_eq!(decide(mangled, "s3cret", &ports()), Decision::Refuse(Refusal::NeedsToken));
        assert_eq!(decide(&wrong, "", &ports()), Decision::Refuse(Refusal::NeedsToken), "no token set: nobody");
        assert!(Refusal::NeedsToken.response().contains("Proxy-Authenticate: Basic"));
    }

    #[test]
    fn only_a_port_a_task_listens_on_and_only_on_this_machine() {
        let a = auth("s3cret");
        let ssh = format!("CONNECT localhost:22 HTTP/1.1\r\nProxy-Authorization: {a}\r\n\r\n");
        assert_eq!(decide(&ssh, "s3cret", &ports()), Decision::Refuse(Refusal::NotATaskPort));
        let tenx = format!("CONNECT localhost:7070 HTTP/1.1\r\nProxy-Authorization: {a}\r\n\r\n");
        assert_eq!(decide(&tenx, "s3cret", &ports()), Decision::Refuse(Refusal::NotATaskPort));
        let away = format!("CONNECT github.com:443 HTTP/1.1\r\nProxy-Authorization: {a}\r\n\r\n");
        assert_eq!(decide(&away, "s3cret", &ports()), Decision::Refuse(Refusal::NotLocal));
        let lan = format!("CONNECT 192.168.1.5:3000 HTTP/1.1\r\nProxy-Authorization: {a}\r\n\r\n");
        assert_eq!(decide(&lan, "s3cret", &ports()), Decision::Refuse(Refusal::NotLocal));
        for sly in ["127.evil.com", "app.localhost", "127.0.0.2"] {
            let head = format!("CONNECT {sly}:3000 HTTP/1.1\r\nProxy-Authorization: {a}\r\n\r\n");
            assert_eq!(decide(&head, "s3cret", &ports()), Decision::Refuse(Refusal::NotLocal), "{sly}");
        }
        let head = format!("CONNECT LOCALHOST:3000 HTTP/1.1\r\nProxy-Authorization: {a}\r\n\r\n");
        assert_eq!(request(&head, "s3cret").unwrap().addr, std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        assert!(request(&head, "s3cret").unwrap().either, "localhost: IPv6 too");
        let head = format!("CONNECT 127.0.0.1:3000 HTTP/1.1\r\nProxy-Authorization: {a}\r\n\r\n");
        assert!(!request(&head, "s3cret").unwrap().either, "an address is that address");
        let head = format!("CONNECT [::1]:3000 HTTP/1.1\r\nProxy-Authorization: {a}\r\n\r\n");
        assert_eq!(request(&head, "s3cret").unwrap().addr, std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST));
    }

    #[test]
    fn anything_but_connect_is_a_bad_request() {
        let a = auth("s3cret");
        for head in [
            format!("GET http://localhost:3000/ HTTP/1.1\r\nProxy-Authorization: {a}\r\n\r\n"),
            format!("CONNECT localhost HTTP/1.1\r\nProxy-Authorization: {a}\r\n\r\n"),
            format!("CONNECT localhost:http HTTP/1.1\r\nProxy-Authorization: {a}\r\n\r\n"),
            String::new(),
        ] {
            assert_eq!(decide(&head, "s3cret", &ports()), Decision::Refuse(Refusal::BadRequest), "{head:?}");
        }
    }

    #[test]
    fn the_head_ends_at_the_blank_line() {
        assert!(!head_complete(b"CONNECT localhost:3000 HTTP/1.1\r\n"));
        assert!(head_complete(b"CONNECT localhost:3000 HTTP/1.1\r\n\r\n"));
    }
}
