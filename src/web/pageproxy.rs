//! The page proxy (`tenx_core::pageproxy`): a task's dev server for the iOS
//! app's pages, at its own `localhost` address. A listener of its own on
//! loopback, `CONNECT` only, behind `tailscale serve --tls-terminated-tcp`.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tenx_core::pageproxy::{self, Decision};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The ports tasks listen on, scanned at most this often; a port not in a
/// scan older than `RESCAN_MISS` is looked for again (a dev server started
/// a moment ago).
const RESCAN: Duration = Duration::from_secs(5);
const RESCAN_MISS: Duration = Duration::from_millis(500);

/// A request head must arrive within this.
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Default)]
struct Ports {
    seen: HashSet<u16>,
    at: Option<Instant>,
}

pub(super) async fn serve(listener: std::net::TcpListener, token: String) {
    let Ok(listener) = TcpListener::from_std(listener) else { return };
    let token = Arc::new(token);
    let ports = Arc::new(Mutex::new(Ports::default()));
    loop {
        let Ok((conn, _)) = listener.accept().await else { continue };
        let (token, ports) = (token.clone(), ports.clone());
        tokio::spawn(async move {
            let _ = handle(conn, &token, &ports).await;
        });
    }
}

async fn handle(mut conn: TcpStream, token: &str, ports: &Mutex<Ports>) -> std::io::Result<()> {
    let mut head = Vec::with_capacity(512);
    let mut buf = [0u8; 1024];
    while !pageproxy::head_complete(&head) {
        let n = tokio::time::timeout(HEAD_TIMEOUT, conn.read(&mut buf)).await.map_err(|_| std::io::ErrorKind::TimedOut)??;
        if n == 0 {
            return Ok(());
        }
        head.extend_from_slice(&buf[..n]);
        if head.len() > pageproxy::MAX_HEAD {
            return conn.write_all(pageproxy::Refusal::BadRequest.response().as_bytes()).await;
        }
    }
    // A client may send its first bytes right after the head: they belong to the tunnel.
    let end = head.windows(4).position(|w| w == b"\r\n\r\n").map_or(head.len(), |i| i + 4);
    let early = head.split_off(end);
    let text = String::from_utf8_lossy(&head);
    let mut decision = pageproxy::decide(&text, token, &task_ports(ports, false).await);
    if decision == Decision::Refuse(pageproxy::Refusal::NotATaskPort) {
        decision = pageproxy::decide(&text, token, &task_ports(ports, true).await);
    }
    let port = match decision {
        Decision::Tunnel(port) => port,
        Decision::Refuse(why) => return conn.write_all(why.response().as_bytes()).await,
    };
    // `localhost`: a dev server may listen on IPv4 or IPv6 only.
    let Ok(mut server) = TcpStream::connect(("localhost", port)).await else {
        return conn.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
    };
    conn.write_all(pageproxy::ESTABLISHED.as_bytes()).await?;
    if !early.is_empty() {
        server.write_all(&early).await?;
    }
    tokio::io::copy_bidirectional(&mut conn, &mut server).await.map(|_| ())
}

/// The ports tasks listen on: scanned again when the last scan is old, or,
/// with `missed`, when it is older than a moment.
async fn task_ports(ports: &Mutex<Ports>, missed: bool) -> HashSet<u16> {
    let fresh = {
        let p = ports.lock().unwrap_or_else(|e| e.into_inner());
        let limit = if missed { RESCAN_MISS } else { RESCAN };
        p.at.is_some_and(|at| at.elapsed() < limit).then(|| p.seen.clone())
    };
    if let Some(seen) = fresh {
        return seen;
    }
    let seen = tokio::task::spawn_blocking(crate::live::task_ports).await.unwrap_or_default();
    let mut p = ports.lock().unwrap_or_else(|e| e.into_inner());
    p.seen = seen.clone();
    p.at = Some(Instant::now());
    seen
}
