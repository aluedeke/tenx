//! The HTTP side of `tenx web` (`docs/web-protocol.md`): the page behind
//! the token cookie, and `/ws`, one WebSocket per browser tab carrying its
//! terminal (binary frames) and its column (JSON text frames) to and from
//! the tab's driver thread (`tab`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;
use tenx_core::web;

use super::tab::{Input, Output, PageMsg, Tabs};

pub(super) struct App {
    pub(super) token: String,
    pub(super) dev_origins: Vec<String>,
    pub(super) tabs: Arc<Tabs>,
}

pub(super) async fn serve(listener: std::net::TcpListener, app: Arc<App>) -> Result<()> {
    let tabs = app.tabs.clone();
    let router = Router::new().route("/ws", get(ws)).fallback(get(page)).with_state(app);
    let listener = tokio::net::TcpListener::from_std(listener)?;
    // Not axum's graceful shutdown: it would wait for every open WebSocket,
    // and a browser tab never closes its own.
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        served = axum::serve(listener, router) => served?,
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
    // Every tab's grouped session goes with the server; give the drivers a
    // moment to kill them, so none is left behind for the next run to sweep.
    let ended = tabs.close_all();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && !ended.iter().all(|t| t.dead.load(std::sync::atomic::Ordering::SeqCst)) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

/// Anything but `/ws`: the page, once the browser has the cookie. `?token=`
/// is swapped for it and dropped from the address bar.
async fn page(State(app): State<Arc<App>>, uri: Uri, headers: HeaderMap) -> Response {
    if let Some(given) = uri.query().and_then(|q| query_param(q, "token")) {
        if !web::token_matches(given, &app.token) {
            return unauthorized("That token isn't this server's (rotated?).");
        }
        return Response::builder()
            .status(StatusCode::SEE_OTHER)
            .header(header::LOCATION, uri.path())
            .header(header::SET_COOKIE, web::set_cookie(&app.token))
            .header(header::REFERRER_POLICY, "no-referrer")
            .body(axum::body::Body::empty())
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    if !web::public_path(uri.path()) && !web::cookie_ok(header_str(&headers, header::COOKIE), &app.token) {
        return unauthorized("Open the address <code>tenx web</code> printed (the one with <code>?token=</code>).");
    }
    let mut response = super::assets::get(uri.path()).unwrap_or_else(|| StatusCode::NOT_FOUND.into_response());
    let h = response.headers_mut();
    h.insert(header::X_CONTENT_TYPE_OPTIONS, header::HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, header::HeaderValue::from_static("no-referrer"));
    response
}

fn unauthorized(why: &str) -> Response {
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>tenx</title>\
         <body style=\"background:#171820;color:#d6dbe3;font:14px/1.6 ui-monospace,Menlo,monospace;padding:2em\">\
         <p>{why}</p></body>"
    );
    (StatusCode::UNAUTHORIZED, [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], body)
        .into_response()
}

#[derive(Deserialize)]
struct WsQuery {
    session: Option<String>,
    token: Option<String>,
}

async fn ws(State(app): State<Arc<App>>, Query(q): Query<WsQuery>, headers: HeaderMap, upgrade: WebSocketUpgrade) -> Response {
    let origin = header_str(&headers, header::ORIGIN);
    let allowed = web::origin_allowed(origin, header_str(&headers, header::HOST), &app.dev_origins);
    let dev = web::is_dev_origin(origin, &app.dev_origins);
    let cookie = web::cookie_ok(header_str(&headers, header::COOKIE), &app.token);
    let query_token = q.token.as_deref().is_some_and(|t| web::token_matches(t, &app.token));
    if !web::ws_authorized(cookie, query_token, allowed, dev) {
        return if allowed { StatusCode::UNAUTHORIZED } else { StatusCode::FORBIDDEN }.into_response();
    }
    upgrade.on_upgrade(move |socket| connection(app, socket, q.session))
}

/// One socket, for as long as it lasts: attach to the tab it asks for (or a
/// new one), then shuttle messages both ways.
async fn connection(app: Arc<App>, mut socket: WebSocket, want: Option<String>) {
    let tabs = app.tabs.clone();
    // Making a tab runs tmux and reads every workspace: off the runtime.
    let attached = tokio::task::spawn_blocking(move || tabs.attach(want.as_deref())).await;
    let (tab, mut rx) = match attached {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => return close_with_error(socket, format!("{e:#}")).await,
        Err(e) => return close_with_error(socket, e.to_string()).await,
    };
    // What the tab printed while no socket was attached is stale: the page
    // is new and gets a full redraw (`Input::Attached`).
    while rx.try_recv().is_ok() {}
    let hello = serde_json::json!({
        "type": "hello",
        "session": tab.id,
        "host": host_name(),
        "version": env!("CARGO_PKG_VERSION"),
    });
    if socket.send(Message::Text(hello.to_string().into())).await.is_ok() {
        let _ = tab.input.send(Input::Attached);
        loop {
            tokio::select! {
                msg = socket.recv() => match msg {
                    Some(Ok(Message::Text(text))) => match serde_json::from_str::<PageMsg>(&text) {
                        Ok(msg) => { let _ = tab.input.send(Input::Page(msg)); }
                        Err(e) => {
                            let err = serde_json::json!({ "type": "error", "message": format!("bad message: {e}") });
                            if socket.send(Message::Text(err.to_string().into())).await.is_err() { break; }
                        }
                    },
                    Some(Ok(Message::Binary(bytes))) => { let _ = tab.input.send(Input::Term(bytes.to_vec())); }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => {}
                },
                out = rx.recv() => {
                    let sent = match out {
                        Some(Output::Text(text)) => socket.send(Message::Text(text.into())).await,
                        Some(Output::Binary(bytes)) => socket.send(Message::Binary(bytes.into())).await,
                        None => break,
                    };
                    if sent.is_err() {
                        break;
                    }
                }
            }
        }
    }
    drop(rx);
    app.tabs.detach(tab);
}

async fn close_with_error(mut socket: WebSocket, message: String) {
    let err = serde_json::json!({ "type": "error", "message": message });
    let _ = socket.send(Message::Text(err.to_string().into())).await;
    let _ = socket.send(Message::Close(None)).await;
}

fn header_str(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// `name`'s value in a query string. Tokens and session ids are hex, so no
/// percent-decoding is needed — a value that would need it is wrong anyway.
fn query_param<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == name).then_some(v)
    })
}

/// This machine's short name, for the page's header.
fn host_name() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer outlives the call and its length is passed.
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
    if !ok {
        return String::new();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let name = String::from_utf8_lossy(&buf[..end]).into_owned();
    name.split('.').next().unwrap_or_default().to_string()
}
