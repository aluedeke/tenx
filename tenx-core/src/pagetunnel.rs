//! `tenx web`'s page tunnel: how a phone reaches a task's dev server. A
//! task's page at `http://localhost:3000` means the Mac's own port 3000,
//! which the phone can't reach. The iOS app sends a page's `localhost`
//! connections to a small proxy of its own on the phone
//! (`WKWebsiteDataStore.proxyConfigurations`), so the address, its cookies
//! and the redirects set up for it stay `localhost`; that proxy carries each
//! connection to `tenx web` as a WebSocket, `GET /pagetunnel?host=&port=`,
//! on the address the app already uses — so whatever reaches `tenx web`
//! (`tailscale serve`, a reverse proxy) reaches the pages too.
//!
//! Here: who may open one (the page token as a bearer, from an app — no
//! `Origin`, so no web page's script), and where to: only a loopback address
//! and only a port a task's own process listens on — never any other service
//! on the machine.

use std::collections::HashSet;

/// What to do with a tunnel request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Connect to this local port and join it to the socket.
    Tunnel(u16),
    /// Answer this and close.
    Refuse(Refusal),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// No host or port, or a port that is no number.
    BadRequest,
    /// No token, the wrong one, or a request from a web page (`Origin`).
    NeedsToken,
    /// Somewhere other than this machine's loopback.
    NotLocal,
    /// A local port no task listens on.
    NotATaskPort,
}

impl Refusal {
    /// The HTTP status the upgrade request gets.
    pub fn status(self) -> u16 {
        match self {
            Refusal::BadRequest => 400,
            Refusal::NeedsToken => 401,
            Refusal::NotLocal | Refusal::NotATaskPort => 403,
        }
    }
}

/// A request the tunnel may serve: the loopback address it names and the port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub addr: std::net::IpAddr,
    pub port: u16,
    /// Named `localhost`: IPv6's `::1` is tried when nothing answers on
    /// IPv4's, as a browser does (Vite on a Mac listens on `::1` alone).
    pub either: bool,
}

/// Whether a tunnel request may be served at all: `Authorization: Bearer
/// <page token>` and no `Origin`. A browser sends `Origin` with every
/// WebSocket and lets no script set `Authorization`, so a web page — even
/// one served by `tenx web` itself — can't open a tunnel; the app's own
/// `URLSession` can.
pub fn authorized(origin: Option<&str>, authorization: Option<&str>, token: &str) -> bool {
    if origin.is_some() {
        return false;
    }
    let Some(given) = authorization.map(str::trim).and_then(|a| a.strip_prefix("Bearer ").or_else(|| a.strip_prefix("bearer "))) else {
        return false;
    };
    crate::web::token_matches(given.trim(), token)
}

/// Decide on a request let through by `authorized`: the host and port of
/// `?host=&port=`, to a port in `task_ports`.
pub fn decide(host: Option<&str>, port: Option<&str>, task_ports: &HashSet<u16>) -> Decision {
    match target(host, port) {
        Err(why) => Decision::Refuse(why),
        Ok(t) => port_decision(t, task_ports),
    }
}

/// The port step of `decide`, for a target `target` let through: the
/// caller looks the tasks' ports up only for those.
pub fn port_decision(target: Target, task_ports: &HashSet<u16>) -> Decision {
    if task_ports.contains(&target.port) { Decision::Tunnel(target.port) } else { Decision::Refuse(Refusal::NotATaskPort) }
}

/// Everything `decide` checks before the port: a loopback name and a port.
/// `localhost` is IPv4's first, then IPv6's (`either`): the one name the
/// iOS app sends here.
pub fn target(host: Option<&str>, port: Option<&str>) -> Result<Target, Refusal> {
    let (Some(host), Some(port)) = (host, port) else { return Err(Refusal::BadRequest) };
    let Ok(port) = port.parse::<u16>() else { return Err(Refusal::BadRequest) };
    if port == 0 {
        return Err(Refusal::BadRequest);
    }
    let addr = loopback_addr(host).ok_or(Refusal::NotLocal)?;
    Ok(Target { addr, port, either: host.eq_ignore_ascii_case("localhost") })
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

    fn ports() -> HashSet<u16> {
        [3000, 5173].into()
    }

    #[test]
    fn the_app_with_the_token_may_open_one() {
        assert!(authorized(None, Some("Bearer s3cret"), "s3cret"));
        assert!(authorized(None, Some(" bearer s3cret "), "s3cret"));
    }

    #[test]
    fn no_token_a_wrong_one_or_a_web_page_may_not() {
        assert!(!authorized(None, None, "s3cret"));
        assert!(!authorized(None, Some("Bearer guess"), "s3cret"));
        assert!(!authorized(None, Some("Basic s3cret"), "s3cret"));
        assert!(!authorized(None, Some("Bearer "), ""), "no token set: nobody");
        // A page's script: its browser always says where it comes from.
        assert!(!authorized(Some("https://msal.example.ts.net"), Some("Bearer s3cret"), "s3cret"));
        assert_eq!(Refusal::NeedsToken.status(), 401);
    }

    #[test]
    fn a_task_port_on_this_machine_is_tunnelled() {
        assert_eq!(decide(Some("localhost"), Some("3000"), &ports()), Decision::Tunnel(3000));
        assert_eq!(decide(Some("[::1]"), Some("5173"), &ports()), Decision::Tunnel(5173));
        assert_eq!(decide(Some("::1"), Some("5173"), &ports()), Decision::Tunnel(5173));
        assert_eq!(decide(Some("127.0.0.1"), Some("3000"), &ports()), Decision::Tunnel(3000));
    }

    #[test]
    fn only_a_port_a_task_listens_on_and_only_on_this_machine() {
        assert_eq!(decide(Some("localhost"), Some("22"), &ports()), Decision::Refuse(Refusal::NotATaskPort));
        assert_eq!(decide(Some("localhost"), Some("7070"), &ports()), Decision::Refuse(Refusal::NotATaskPort));
        assert_eq!(decide(Some("github.com"), Some("443"), &ports()), Decision::Refuse(Refusal::NotLocal));
        assert_eq!(decide(Some("192.168.1.5"), Some("3000"), &ports()), Decision::Refuse(Refusal::NotLocal));
        for sly in ["127.evil.com", "app.localhost", "127.0.0.2", ""] {
            assert_eq!(decide(Some(sly), Some("3000"), &ports()), Decision::Refuse(Refusal::NotLocal), "{sly}");
        }
        assert_eq!(Refusal::NotATaskPort.status(), 403);
        let t = target(Some("LOCALHOST"), Some("3000")).unwrap();
        assert_eq!(t.addr, std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        assert!(t.either, "localhost: IPv6 too");
        assert!(!target(Some("127.0.0.1"), Some("3000")).unwrap().either, "an address is that address");
        assert_eq!(target(Some("::1"), Some("3000")).unwrap().addr, std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST));
    }

    #[test]
    fn a_missing_or_mangled_port_is_a_bad_request() {
        for (host, port) in [(None, Some("3000")), (Some("localhost"), None), (Some("localhost"), Some("http")), (Some("localhost"), Some("0")), (Some("localhost"), Some("70000"))] {
            assert_eq!(decide(host, port, &ports()), Decision::Refuse(Refusal::BadRequest), "{host:?} {port:?}");
        }
    }
}
