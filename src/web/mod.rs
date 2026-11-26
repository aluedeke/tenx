//! `tenx web`: the column as a web page beside an xterm.js terminal, for a
//! browser — a phone on the couch, or a desktop tab. The page (`web/`, a
//! static Next.js export embedded by `assets`) only draws: every browser tab
//! gets a `tab::Tab` here, a `Column` of its own following a tmux session
//! grouped with `tenx`, and `tmux attach` to that session in a pty, both
//! carried over one WebSocket. The contract is `docs/web-protocol.md`; what
//! is decided about a request (token, cookie, `Origin`, layout) is
//! `tenx_core::web`.
//!
//! The only async code in tenx lives here (tokio + axum), and only to move
//! bytes: the column and the pty stay synchronous on a thread per tab.

mod assets;
mod clipboard;
mod events;
mod mcp;
mod pageproxy;
mod paste;
mod secrets;
mod server;
pub mod service;
mod speak;
mod tts;
mod tab;
pub(crate) mod token;
mod transcribe;
mod dictate;
mod whisper;

use anyhow::{bail, Context, Result};
use std::net::ToSocketAddrs;
use std::sync::Arc;
use std::time::Duration;

/// `tenx web`'s flags.
pub struct Options {
    pub listen: String,
    pub port: u16,
    pub open: bool,
    pub rotate_token: bool,
    pub dev_origins: Vec<String>,
    /// The page proxy's port on loopback (`tenx_core::pageproxy`); 0: none.
    pub proxy_port: u16,
}

/// How long a tab outlives its socket: long enough for a reload or a phone
/// waking up. `TENX_WEB_GRACE_MS` shortens it for the end-to-end test.
fn grace() -> Duration {
    std::env::var("TENX_WEB_GRACE_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(30))
}

pub fn run(o: Options) -> Result<()> {
    crate::tmux::check_version()?;
    crate::tmux::ensure_session()?;
    if let Ok(bin) = crate::tmux::self_bin() {
        crate::cli::watch::ensure_running(&bin);
    }
    // The adhoc workspace (`tenx ask`'s sessions) is listed in the column
    // here too, as the client's launch does.
    crate::cli::adhoc::ensure_quiet();
    let token = token::load_or_create(o.rotate_token)?;
    let mcp_token = token::load_or_create_named(token::MCP_TOKEN, false)?;

    let addr = (o.listen.trim_start_matches('[').trim_end_matches(']'), o.port)
        .to_socket_addrs()
        .with_context(|| format!("resolve --listen {}", o.listen))?
        .next()
        .with_context(|| format!("--listen {} resolves to nothing", o.listen))?;
    let listener = match std::net::TcpListener::bind(addr) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            bail!("port {} is already in use on {} — another tenx web? Pick another with --port", o.port, o.listen)
        }
        Err(e) => return Err(e).with_context(|| format!("listen on {addr}")),
    };
    listener.set_nonblocking(true)?;
    // The page proxy: a phone's way to a task's dev server. Not starting it
    // (its port taken) leaves the page as it was, a note for localhost pages.
    // It listens on loopback only, for `tailscale serve` to publish: a
    // `--listen` beyond this machine would advertise a port nobody reaches.
    let proxy = match o.proxy_port {
        0 => None,
        _ if !tenx_core::web::is_loopback(&o.listen) => {
            eprintln!("  page proxy: off, since --listen {} isn't loopback; tasks' localhost pages stay on the Mac", o.listen);
            None
        }
        port => match std::net::TcpListener::bind(("127.0.0.1", port)) {
            Ok(l) if l.set_nonblocking(true).is_ok() => Some((l, port)),
            Ok(_) => None,
            Err(e) => {
                eprintln!("  page proxy: port {port} is not free ({e}); tasks' localhost pages stay on the Mac");
                None
            }
        },
    };

    // Sessions an earlier run left behind (it was killed): nobody can come
    // back to them, since the tabs they belonged to were this process's.
    for (name, attached) in crate::tmux::sessions_with_prefix(tenx_core::web::SESSION_PREFIX) {
        if !attached {
            let _ = crate::tmux::kill_session(&name);
        }
    }

    let browse_host = if addr.ip().is_unspecified() { "127.0.0.1".to_string() } else { url_host(&o.listen) };
    let url = format!("http://{browse_host}:{}/?token={token}", o.port);
    println!("tenx web on http://{}", addr);
    println!("  open {url}");
    if !tenx_core::web::is_loopback(&o.listen) {
        eprintln!(
            "  warning: listening beyond this machine over plain HTTP — the page is a shell on it.\n  \
             Prefer the default (127.0.0.1) behind `tailscale serve {}`, which adds TLS and your tailnet's identity.",
            o.port
        );
    }
    if let Some((_, port)) = &proxy {
        println!("  page proxy on 127.0.0.1:{port} — for the iOS app on your tailnet: tailscale serve --bg --tls-terminated-tcp={port} tcp://127.0.0.1:{port}");
    }
    if o.open {
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        let _ = std::process::Command::new(opener).arg(&url).spawn();
    }

    let push = Arc::new(crate::push::Push::load().context("load the Web Push key")?);
    let hub = Arc::new(events::Hub::default());
    {
        let (push, hub) = (push.clone(), hub.clone());
        std::thread::Builder::new()
            .name("notifier".into())
            .spawn(move || events::notifier(push, hub))
            .context("start the notifier")?;
    }
    let app = Arc::new(server::App {
        token,
        mcp_token,
        dev_origins: o.dev_origins,
        tabs: Arc::new(tab::Tabs::new(grace(), hub.clone(), proxy.as_ref().map(|(_, port)| *port))),
        push,
        hub,
    });
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().context("start the async runtime")?;
    if let Some((l, _)) = proxy {
        runtime.spawn(pageproxy::serve(l, app.token.clone()));
    }
    runtime.block_on(server::serve(listener, app))
}

/// `tenx web mcp-token [--rotate]`: print `/mcp`'s bearer token — what the
/// gateway in front of it sends — making it on first use. A running
/// `tenx web` reads it at start, so a rotated one needs a restart.
pub fn mcp_token(rotate: bool) -> Result<()> {
    println!("{}", token::load_or_create_named(token::MCP_TOKEN, rotate)?);
    if rotate {
        eprintln!("rotated — restart tenx web (tenx web service restart) to use it");
    }
    Ok(())
}

/// `--listen` as it goes in a URL: an IPv6 address in brackets.
fn url_host(listen: &str) -> String {
    let bare = listen.trim_start_matches('[').trim_end_matches(']');
    if bare.contains(':') { format!("[{bare}]") } else { bare.to_string() }
}
