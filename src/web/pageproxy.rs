//! The page proxy (`tenx_core::pageproxy`): a task's dev server for the iOS
//! app's pages, at its own `localhost` address. A listener of its own on
//! loopback, `CONNECT` only, behind `tailscale serve --tls-terminated-tcp`.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tenx_core::pageproxy::{self, Decision};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Semaphore};

/// The ports tasks listen on, scanned at most this often; a port not in a
/// scan older than `RESCAN_MISS` is looked for again (a dev server started
/// a moment ago).
const RESCAN: Duration = Duration::from_secs(5);
const RESCAN_MISS: Duration = Duration::from_secs(2);

/// Tunnels open at once; one more waits for a slot.
const MAX_TUNNELS: usize = 256;

/// A tunnel with no byte either way for this long is closed: a phone that
/// slept or left the tailnet may never say goodbye.
/// Long, so a quiet dev server's live-reload socket isn't cut.
const IDLE: Duration = Duration::from_secs(2 * 60 * 60);

/// A request head must arrive within this.
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);

/// A port scan taking longer (`lsof` stuck on a dead mount) is given up on:
/// the last ports stand, and the lock is free for the next connection.
const SCAN_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Default)]
struct Ports {
    seen: HashSet<u16>,
    at: Option<Instant>,
}

pub(super) async fn serve(listener: std::net::TcpListener, token: String) {
    let Ok(listener) = TcpListener::from_std(listener) else { return };
    let token = Arc::new(token);
    let ports = Arc::new(Mutex::new(Ports::default()));
    let slots = Arc::new(Semaphore::new(MAX_TUNNELS));
    loop {
        let mut conn = match listener.accept().await {
            Ok((conn, _)) => conn,
            // Out of descriptors, say: not a busy loop.
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
        };
        // Full: said at once, not left waiting in the backlog.
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            tokio::spawn(async move {
                let _ = conn.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
            });
            continue;
        };
        let (token, ports) = (token.clone(), ports.clone());
        tokio::spawn(async move {
            let _ = handle(conn, &token, &ports).await;
            drop(slot);
        });
    }
}

async fn handle(mut conn: TcpStream, token: &str, ports: &Mutex<Ports>) -> std::io::Result<()> {
    let mut head = Vec::with_capacity(512);
    // The whole head within the limit, however slowly it trickles in.
    let read = async {
        let mut buf = [0u8; 1024];
        while !pageproxy::head_complete(&head) {
            let n = conn.read(&mut buf).await?;
            if n == 0 || head.len() + n > pageproxy::MAX_HEAD {
                return Ok::<bool, std::io::Error>(false);
            }
            head.extend_from_slice(&buf[..n]);
        }
        Ok(true)
    };
    let complete = tokio::time::timeout(HEAD_TIMEOUT, read).await.map_err(|_| std::io::ErrorKind::TimedOut)??;
    if !complete {
        return conn.write_all(pageproxy::Refusal::BadRequest.response().as_bytes()).await;
    }
    // A client may send its first bytes right after the head: they belong to the tunnel.
    let end = head.windows(4).position(|w| w == b"\r\n\r\n").map_or(head.len(), |i| i + 4);
    let early = head.split_off(end);
    let text = String::from_utf8_lossy(&head);
    // Method, token and host first: only a request let through costs a scan.
    let target = match pageproxy::request(&text, token) {
        Ok(target) => target,
        Err(why) => return conn.write_all(why.response().as_bytes()).await,
    };
    let mut decision = pageproxy::port_decision(target, &task_ports(ports, false).await);
    if decision == Decision::Refuse(pageproxy::Refusal::NotATaskPort) {
        decision = pageproxy::port_decision(target, &task_ports(ports, true).await);
    }
    if let Decision::Refuse(why) = decision {
        return conn.write_all(why.response().as_bytes()).await;
    }
    // The address asked for; for `localhost`, IPv6's when IPv4's has nobody.
    let mut server = TcpStream::connect((target.addr, target.port)).await;
    if server.is_err() && target.either {
        server = TcpStream::connect((std::net::Ipv6Addr::LOCALHOST, target.port)).await;
    }
    let Ok(mut server) = server else {
        return conn.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
    };
    conn.write_all(pageproxy::ESTABLISHED.as_bytes()).await?;
    if !early.is_empty() {
        server.write_all(&early).await?;
    }
    join(conn, server).await;
    Ok(())
}

/// Copy both ways until either side ends, or neither has said anything for
/// `IDLE`.
async fn join(client: TcpStream, server: TcpStream) {
    let started = Instant::now();
    let last = AtomicU64::new(0);
    let (mut cr, mut cw) = client.into_split();
    let (mut sr, mut sw) = server.into_split();
    let both = async {
        tokio::join!(pump(&mut cr, &mut sw, &last, started), pump(&mut sr, &mut cw, &last, started));
    };
    let idle = async {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let quiet = started.elapsed().as_secs().saturating_sub(last.load(Ordering::Relaxed));
            if quiet >= IDLE.as_secs() {
                return;
            }
        }
    };
    tokio::select! {
        _ = both => {}
        _ = idle => {}
    }
}

/// One direction: read, write on, note the time; at the end, close the
/// other side's writing half.
async fn pump(from: &mut tokio::net::tcp::OwnedReadHalf, to: &mut tokio::net::tcp::OwnedWriteHalf, last: &AtomicU64, started: Instant) {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match from.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                last.store(started.elapsed().as_secs(), Ordering::Relaxed);
                if to.write_all(&buf[..n]).await.is_err() {
                    break;
                }
            }
        }
    }
    let _ = to.shutdown().await;
}

/// The ports tasks listen on: scanned again when the last scan is old, or,
/// with `missed`, when it is older than a moment. One scan at a time: the
/// lock is held across it, so the connections of one page load share it.
async fn task_ports(ports: &Mutex<Ports>, missed: bool) -> HashSet<u16> {
    let mut p = ports.lock().await;
    let limit = if missed { RESCAN_MISS } else { RESCAN };
    if p.at.is_some_and(|at| at.elapsed() < limit) {
        return p.seen.clone();
    }
    if let Ok(Ok(seen)) = tokio::time::timeout(SCAN_TIMEOUT, tokio::task::spawn_blocking(crate::live::task_ports)).await {
        p.seen = seen;
    }
    p.at = Some(Instant::now());
    p.seen.clone()
}
