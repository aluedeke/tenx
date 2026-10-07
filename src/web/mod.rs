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
mod paste;
mod push;
mod server;
pub mod service;
mod tab;
mod token;

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
    if o.open {
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        let _ = std::process::Command::new(opener).arg(&url).spawn();
    }

    let push = Arc::new(push::Push::load().context("load the Web Push key")?);
    {
        let push = push.clone();
        std::thread::Builder::new().name("push".into()).spawn(move || push::notifier(push)).context("start the push notifier")?;
    }
    let app = Arc::new(server::App {
        token,
        dev_origins: o.dev_origins,
        tabs: Arc::new(tab::Tabs::new(grace())),
        push,
    });
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().context("start the async runtime")?;
    runtime.block_on(server::serve(listener, app))
}

/// `--listen` as it goes in a URL: an IPv6 address in brackets.
fn url_host(listen: &str) -> String {
    let bare = listen.trim_start_matches('[').trim_end_matches(']');
    if bare.contains(':') { format!("[{bare}]") } else { bare.to_string() }
}
