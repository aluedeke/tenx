//! The page tunnel (`tenx_core::pagetunnel`): a task's dev server for the
//! iOS app's pages, at its own `localhost` address. `GET
//! /pagetunnel?host=&port=` with the page token as a bearer: checked, the
//! local port connected to, and only then the WebSocket upgrade, so a
//! refusal is a plain status the app can tell apart. After that, binary
//! frames carry the connection's bytes both ways.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use tenx_core::pagetunnel::{self, Decision, Refusal};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

/// The ports tasks listen on, scanned at most this often; a port not in a
/// scan older than `RESCAN_MISS` is looked for again (a dev server started
/// a moment ago).
const RESCAN: Duration = Duration::from_secs(5);
const RESCAN_MISS: Duration = Duration::from_secs(2);

/// Tunnels open at once; one more is refused (`503`).
const MAX_TUNNELS: usize = 256;

/// A tunnel with no byte either way for this long is closed: a phone that
/// slept or left the network may never say goodbye.
/// Long, so a quiet dev server's live-reload socket isn't cut.
const IDLE: Duration = Duration::from_secs(2 * 60 * 60);

/// A port scan taking longer (`lsof` stuck on a dead mount) is given up on:
/// the last ports stand, and the lock is free for the next request.
const SCAN_TIMEOUT: Duration = Duration::from_secs(10);

/// What every tunnel shares: the last port scan and the open slots.
pub(super) struct Tunnels {
    ports: Mutex<Ports>,
    slots: Arc<Semaphore>,
}

impl Default for Tunnels {
    fn default() -> Tunnels {
        Tunnels { ports: Mutex::new(Ports::default()), slots: Arc::new(Semaphore::new(MAX_TUNNELS)) }
    }
}

#[derive(Default)]
struct Ports {
    seen: HashSet<u16>,
    at: Option<Instant>,
}

/// The request, decided: refused with a status, or connected and upgraded.
pub(super) async fn open(tunnels: &Tunnels, host: Option<&str>, port: Option<&str>, upgrade: WebSocketUpgrade) -> Response {
    let Ok(slot) = tunnels.slots.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    // Host and port first: only a request let through costs a scan.
    let target = match pagetunnel::target(host, port) {
        Ok(t) => t,
        Err(why) => return refused(why),
    };
    let mut decision = pagetunnel::port_decision(target, &task_ports(&tunnels.ports, false).await);
    if decision == Decision::Refuse(Refusal::NotATaskPort) {
        decision = pagetunnel::port_decision(target, &task_ports(&tunnels.ports, true).await);
    }
    if let Decision::Refuse(why) = decision {
        return refused(why);
    }
    // The address asked for; for `localhost`, IPv6's when IPv4's has nobody.
    let mut server = TcpStream::connect((target.addr, target.port)).await;
    if server.is_err() && target.either {
        server = TcpStream::connect((std::net::Ipv6Addr::LOCALHOST, target.port)).await;
    }
    let Ok(server) = server else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    let _ = server.set_nodelay(true);
    upgrade.on_upgrade(move |socket| join(socket, server, slot))
}

fn refused(why: Refusal) -> Response {
    StatusCode::from_u16(why.status()).unwrap_or(StatusCode::FORBIDDEN).into_response()
}

/// Copy both ways at once until either side ends, or neither has said
/// anything for `IDLE`. Each direction runs on its own, so a full buffer
/// one way never stops the other.
async fn join(socket: WebSocket, server: TcpStream, _slot: OwnedSemaphorePermit) {
    use futures_util::{SinkExt, StreamExt};
    let started = Instant::now();
    let last = AtomicU64::new(0);
    let (mut from_server, mut to_server) = server.into_split();
    let (mut to_page, mut from_page) = socket.split();
    let up = async {
        while let Some(Ok(msg)) = from_page.next().await {
            match msg {
                Message::Binary(bytes) => {
                    last.store(started.elapsed().as_secs(), Ordering::Relaxed);
                    if to_server.write_all(&bytes).await.is_err() {
                        break;
                    }
                }
                Message::Close(_) => break,
                // Pings are answered by axum; text is no part of this.
                _ => {}
            }
        }
    };
    let down = async {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match from_server.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    last.store(started.elapsed().as_secs(), Ordering::Relaxed);
                    if to_page.send(Message::Binary(buf[..n].to_vec().into())).await.is_err() {
                        return;
                    }
                }
            }
        }
        let _ = to_page.send(Message::Close(None)).await;
    };
    let idle = async {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            if started.elapsed().as_secs().saturating_sub(last.load(Ordering::Relaxed)) >= IDLE.as_secs() {
                return;
            }
        }
    };
    tokio::select! {
        _ = up => {}
        _ = down => {}
        _ = idle => {}
    }
    let _ = to_server.shutdown().await;
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
