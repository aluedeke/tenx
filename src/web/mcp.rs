//! `/mcp`: tenx's tasks as MCP tools (`tenx_core::mcp`), for Claude on
//! the web and the voice assistant. `POST` takes one JSON-RPC message;
//! `initialize` also starts an MCP session (`Mcp-Session-Id`), whose
//! `GET` is its event stream and whose `DELETE` ends it (`events`,
//! `tenx_core::mcppush`). A client that never sends the id back is
//! stateless, as before. Not the page's cookie: a bearer token of its own,
//! `~/.config/tenx/mcp-token` (`tenx web mcp-token`), which the gateway in
//! front adds after it has logged the caller in — so a browser holding the
//! page's cookie can't call these, and the gateway's token can't open the
//! page. Every tool runs what the CLI runs (`cli::drive`, `cli::task`,
//! `tui::listing`), on a blocking thread.

use std::convert::Infallible;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use serde_json::{json, Value};
use tenx_core::mcp::{self, Call, FocusTarget, Incoming};
use tenx_core::mcppush;

use super::events::Hub;
use super::server::App;
use super::tab::{Input, Tabs};
use crate::workspace;

/// The `Origin` and bearer checks every `/mcp` request passes first.
fn refused(app: &App, headers: &HeaderMap) -> Option<Response> {
    if !mcp::origin_ok(headers.get(header::ORIGIN).and_then(|v| v.to_str().ok())) {
        return Some(StatusCode::FORBIDDEN.into_response());
    }
    if !mcp::bearer_ok(headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()), &app.mcp_token) {
        return Some((StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, "Bearer")]).into_response());
    }
    None
}

fn header_str<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim).filter(|v| !v.is_empty())
}

fn unknown_session() -> Response {
    (StatusCode::NOT_FOUND, json_body(mcp::error(Value::Null, mcp::INVALID_REQUEST, "unknown or expired MCP session"))).into_response()
}

pub(super) async fn post(State(app): State<Arc<App>>, headers: HeaderMap, body: axum::body::Bytes) -> Response {
    if let Some(r) = refused(&app, &headers) {
        return r;
    }
    // A session client names its session on every request; one we don't
    // know (expired, or from before a restart) initializes again.
    let session = header_str(&headers, mcppush::SESSION_ID_HEADER).map(str::to_string);
    if let Some(s) = &session
        && !app.hub.touch(s)
    {
        return unknown_session();
    }
    let (id, method, params) = match mcp::parse(&body) {
        Ok(Incoming::Request { id, method, params }) => (id, method, params),
        Ok(Incoming::Accepted) => return StatusCode::ACCEPTED.into_response(),
        Err(e) => return (StatusCode::BAD_REQUEST, json_body(e)).into_response(),
    };
    let focus = mcp::focus_target(headers.get(mcp::SESSION_HEADER).and_then(|v| v.to_str().ok()));
    if method == "initialize" {
        let reply = json_body(mcp::result(id, mcp::initialize(&params, env!("CARGO_PKG_VERSION"))));
        return match app.hub.open() {
            Ok(sid) => ([(mcppush::SESSION_ID_HEADER, sid)], reply).into_response(),
            Err(_) => reply.into_response(),
        };
    }
    let reply = match method.as_str() {
        "ping" => mcp::result(id, json!({})),
        "tools/list" => mcp::result(id, mcp::tools()),
        "tools/call" => {
            let outcome = match mcp::parse_call(&params) {
                Ok(call) => {
                    let (tabs, hub) = (app.tabs.clone(), app.hub.clone());
                    tokio::task::spawn_blocking(move || run(call, &tabs, &hub, session.as_deref(), focus))
                        .await
                        .unwrap_or_else(|e| Err(e.into()))
                }
                Err(why) => Err(anyhow::anyhow!(why)),
            };
            let result = match outcome {
                Ok(Answer { value, message_id: Some(mid) }) => {
                    mcp::tool_result_structured(&mcp::with_message_id(&pretty(&value), &mid), json!({ "message_id": mid }))
                }
                Ok(Answer { value, message_id: None }) => mcp::tool_result(&pretty(&value), false),
                Err(e) => mcp::tool_result(&format!("{e:#}"), true),
            };
            mcp::result(id, result)
        }
        other => mcp::error(id, mcp::METHOD_NOT_FOUND, &format!("unknown method: {other}")),
    };
    json_body(reply).into_response()
}

/// `GET /mcp`: the session's event stream — the buffered events after
/// `Last-Event-ID` first, then live ones, a `: ping` comment every 15 s.
/// A second stream for the session replaces the first.
pub(super) async fn get(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if let Some(r) = refused(&app, &headers) {
        return r;
    }
    let Some(session) = header_str(&headers, mcppush::SESSION_ID_HEADER) else {
        return (StatusCode::BAD_REQUEST, json_body(mcp::error(Value::Null, mcp::INVALID_REQUEST, "GET /mcp needs an Mcp-Session-Id (initialize gives one)")))
            .into_response();
    };
    let last = mcppush::last_event_id(header_str(&headers, mcppush::LAST_EVENT_ID_HEADER));
    let Some((replay, live)) = app.hub.stream(session, last) else {
        return unknown_session();
    };
    let live = futures_util::stream::unfold(live, |mut rx| async move { rx.recv().await.map(|e| (e, rx)) });
    let events = futures_util::stream::iter(replay)
        .chain(live)
        .map(|(n, data)| Ok::<_, Infallible>(Event::default().id(n.to_string()).data(data)));
    Sse::new(events).keep_alive(KeepAlive::new().interval(mcppush::PING_EVERY).text("ping")).into_response()
}

/// `DELETE /mcp`: end the session.
pub(super) async fn delete(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if let Some(r) = refused(&app, &headers) {
        return r;
    }
    match header_str(&headers, mcppush::SESSION_ID_HEADER) {
        None => StatusCode::BAD_REQUEST.into_response(),
        Some(s) if app.hub.close(s) => StatusCode::OK.into_response(),
        Some(_) => unknown_session(),
    }
}

/// A tool's answer; `message_id` for `tenx_send_message`.
struct Answer {
    value: Value,
    message_id: Option<String>,
}

impl From<Value> for Answer {
    fn from(value: Value) -> Answer {
        Answer { value, message_id: None }
    }
}

fn json_body(value: Value) -> ([(header::HeaderName, &'static str); 1], String) {
    ([(header::CONTENT_TYPE, "application/json")], value.to_string())
}

fn pretty(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        v => serde_json::to_string_pretty(v).unwrap_or_default(),
    }
}

fn run(call: Call, tabs: &Tabs, hub: &Hub, session: Option<&str>, focus: Result<FocusTarget, String>) -> Result<Answer> {
    if let Call::SendMessage { workspace, task, text, force } = call {
        let (ws, task) = find(&workspace, &task)?;
        let sent = crate::cli::drive::send_in(&ws, &task.name, &text, force)?;
        let how = match sent {
            crate::cli::drive::Sent::Opened => "opened the task's window with the message",
            crate::cli::drive::Sent::Typed => "sent",
        };
        let message_id = hub.message(session, &format!("{}/{}", ws.config.name, task.name));
        return Ok(Answer { value: json!({ "result": how, "status": state(&task.path).status.token() }), message_id: Some(message_id) });
    }
    run_tool(call, tabs, hub, session, focus).map(Answer::from)
}

fn run_tool(call: Call, tabs: &Tabs, hub: &Hub, session: Option<&str>, focus: Result<FocusTarget, String>) -> Result<Value> {
    match call {
        Call::ListWorkspaces => Ok(crate::tui::listing()["workspaces"].clone()),
        Call::ListTasks { workspace, status } => {
            let listing = crate::tui::listing();
            let tasks = listing["tasks"].as_array().cloned().unwrap_or_default();
            let tasks: Vec<Value> = tasks
                .iter()
                .filter(|t| workspace.as_deref().is_none_or(|w| t["ws"] == w))
                .filter(|t| status.as_deref().is_none_or(|s| t["status"] == s))
                .map(summary)
                .collect();
            Ok(Value::Array(tasks))
        }
        Call::GetTask { workspace, task } => {
            let (ws, task) = find(&workspace, &task)?;
            let st = state(&task.path);
            let mut v = workspace::task_json(&ws, &task, &st);
            v["task_md"] = std::fs::read_to_string(task.path.join("TASK.md")).unwrap_or_default().into();
            Ok(v)
        }
        Call::GetOutput { workspace, task } => {
            let (ws, task) = find(&workspace, &task)?;
            let (turn, st) = settled_turn(&ws, &task)?;
            Ok(json!({
                "status": st.status.token(),
                "waiting_for": st.waiting_for,
                "prompt": turn.prompt,
                "replies": turn.replies,
            }))
        }
        Call::ReadScreen { workspace, task, lines } => {
            let (_, task) = find(&workspace, &task)?;
            let st = state(&task.path);
            let pane = match st.pane {
                Some(p) => p,
                None => match crate::tmux::find_task_window(&task.name, &task.path)? {
                    Some(w) => format!("{}.0", w.id),
                    None => bail!("'{}' has no window open", task.display_name),
                },
            };
            let plain = tenx_core::dialog::strip_ansi(&crate::tmux::capture_pane(&pane)?);
            Ok(tenx_core::dialog::preview_tail(&plain, lines).join("\n").into())
        }
        Call::CreateTask { workspace, title, prompt, description, repos } => {
            let ws = match workspace {
                Some(name) => registered(&name)?,
                None => crate::cli::adhoc::ensure()?,
            };
            let title = match title {
                Some(t) => t,
                None => unique_title(&ws, crate::cli::task::title_for_prompt(&ws, &prompt)),
            };
            let md = crate::cli::task::TaskMd { description: description.as_deref().unwrap_or(""), links: &[], prompt: &prompt };
            let slug = crate::cli::task::new_with(
                &ws,
                &title,
                repos.as_deref(),
                crate::cli::task::OpenMode::Background,
                &md,
                Default::default(),
                &crate::progress::Silent,
            )?;
            let dir = ws.tasks_dir().join(&slug);
            if crate::tmux::server_running() {
                crate::cli::drive::await_turn(&dir, crate::cli::drive::TURN_LIMIT);
            }
            Ok(json!({ "workspace": ws.config.name, "task": slug, "title": title, "status": state(&dir).status.token() }))
        }
        Call::SendMessage { .. } => unreachable!("run answers it"),
        Call::Subscribe { events } => {
            let session = session.context(mcppush::NEEDS_SESSION)?;
            if !hub.subscribe(session, events.clone()) {
                bail!("unknown or expired MCP session");
            }
            Ok(mcppush::subscribed_text(&events).into())
        }
        Call::CurrentTask { screen } => {
            let (screen, row) = hub.current(screen.as_ref());
            let task = row.as_deref().and_then(super::events::task_ref);
            Ok(mcppush::current_task(&screen, task.as_ref()))
        }
        Call::Wait { workspace, task, timeout_secs } => {
            let (_, task) = find(&workspace, &task)?;
            let (outcome, st) = crate::cli::drive::wait_in(&task.path, Duration::from_secs(timeout_secs));
            let outcome = match outcome {
                tenx_core::orchestrate::WaitOutcome::Settled => "settled",
                tenx_core::orchestrate::WaitOutcome::NeedsYou => "needs_answer",
                tenx_core::orchestrate::WaitOutcome::TimedOut => "still_working",
            };
            Ok(json!({ "outcome": outcome, "status": st.status.token(), "waiting_for": st.waiting_for }))
        }
        Call::AnswerPermission { workspace, task, approve } => {
            let (_, task) = find(&workspace, &task)?;
            let answer = if approve { tenx_core::dialog::Answer::Yes } else { tenx_core::dialog::Answer::No };
            crate::cli::drive::answer(&task.path, &task.display_name, &crate::tmux::signals(), answer)?;
            Ok(format!("{} '{}'", answer.verb(), task.display_name).into())
        }
        Call::FocusTask { workspace, task } => {
            let (ws, task) = find(&workspace, &task)?;
            match focus.map_err(anyhow::Error::msg)? {
                FocusTarget::Tab(id) => {
                    let tab = tabs.get(&id).context("that tenx web page is gone (reload it)")?;
                    let row = format!("{}/{}", ws.config.name, task.name);
                    tab.input.send(Input::Show { task: row }).ok().context("that tenx web page is closing")?;
                    Ok(json!({ "result": "shown on the page", "status": state(&task.path).status.token() }))
                }
                FocusTarget::Terminals => {
                    let was_open = crate::tmux::find_task_window(&task.name, &task.path)?.is_some();
                    // What ⏎ in the column and `tenx task open` do:
                    // `select-window` on the tenx session, so every terminal
                    // on it shows the task.
                    crate::cli::task::open_in(&ws, &task.name)?;
                    let how = if was_open { "focused" } else { "opened and focused" };
                    Ok(json!({ "result": how, "status": state(&task.path).status.token() }))
                }
            }
        }
    }
}

/// How long `tenx_get_output` waits for a settled turn's reply to reach the
/// transcript.
const REPLY_GRACE: Duration = Duration::from_secs(3);

/// The task's last turn, as `tenx_get_output` reads it. A turn can be
/// reported over a moment before its reply is in the transcript (seen on a
/// new task's first turn): read again for a little while rather than
/// answer "no reply".
fn settled_turn(ws: &workspace::Workspace, task: &workspace::Task) -> Result<(tenx_core::orchestrate::Turn, workspace::TaskState)> {
    let mut turn = crate::cli::drive::last_turn(ws, task)?;
    let mut st = state(&task.path);
    let deadline = std::time::Instant::now() + REPLY_GRACE;
    while turn.replies.is_empty() && st.status != workspace::TaskStatus::Working && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        turn = crate::cli::drive::last_turn(ws, task)?;
        st = state(&task.path);
    }
    Ok((turn, st))
}

/// A finished turn's reply as the `turn` and `reply` events carry it.
pub(super) fn settled_reply(workspace: &str, task: &str) -> Result<String> {
    let (ws, task) = find(workspace, task)?;
    let (turn, _) = settled_turn(&ws, &task)?;
    Ok(mcppush::reply_text(&turn.replies))
}

/// A task in `tenx_list_tasks`: what tells tasks apart, without the
/// subagents' and secrets' detail (`tenx_get_task` has it all).
fn summary(t: &Value) -> Value {
    let prs: Vec<Value> = t["prs"]
        .as_array()
        .map(|prs| prs.iter().map(|p| json!({ "url": p["url"], "state": p["state"], "chip": p["chip"] })).collect())
        .unwrap_or_default();
    json!({
        "ws": t["ws"],
        "slug": t["slug"],
        "title": t["title"],
        "status": t["status"],
        "waiting_for": t["waiting_for"],
        "age_secs": t["age_secs"],
        "agent": t["agent"],
        "repos": t["repos"],
        "prs": prs,
        "subagents": t["subagents"].as_array().map_or(0, Vec::len),
        "secrets_pending": t["secrets_pending"],
    })
}

/// A registered workspace by name — never a path: the caller is on another
/// machine and names only what `tenx_list_workspaces` showed it.
fn registered(name: &str) -> Result<workspace::Workspace> {
    workspace::registered_workspaces()
        .into_iter()
        .find(|w| w.config.name == name)
        .with_context(|| format!("no workspace named {name:?} (tenx_list_workspaces lists them)"))
}

/// A task by workspace name and slug (or the title it was made from).
fn find(ws_name: &str, task: &str) -> Result<(workspace::Workspace, workspace::Task)> {
    let ws = registered(ws_name)?;
    let slug = crate::cli::task::slug_for(&ws, task);
    let task = ws.find_task(&slug).with_context(|| format!("no task {task:?} in {ws_name}"))?;
    Ok((ws, task))
}

fn state(task_dir: &Path) -> workspace::TaskState {
    workspace::resolve_task_state(task_dir, &workspace::sessions::sessions(), &crate::tmux::signals())
}

/// A made-up title an ordinary workspace would refuse as a repeat gets a
/// number, as `tenx ask` does; the adhoc workspace counts its slugs up itself.
fn unique_title(ws: &workspace::Workspace, title: String) -> String {
    if ws.is_adhoc() {
        return title;
    }
    let mut candidate = title.clone();
    let mut n = 2;
    while crate::cli::task::plan_slug(ws, &candidate).is_err() && n < 100 {
        candidate = format!("{title} {n}");
        n += 1;
    }
    candidate
}
