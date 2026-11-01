//! `POST /mcp`: tenx's tasks as MCP tools (`tenx_core::mcp`), for Claude on
//! the web. Not the page's cookie: a bearer token of its own,
//! `~/.config/tenx/mcp-token` (`tenx web mcp-token`), which the gateway in
//! front adds after it has logged the caller in — so a browser holding the
//! page's cookie can't call these, and the gateway's token can't open the
//! page. Every tool runs what the CLI runs (`cli::drive`, `cli::task`,
//! `tui::listing`), on a blocking thread.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use tenx_core::mcp::{self, Call, Incoming};

use super::server::App;
use crate::workspace;

pub(super) async fn post(State(app): State<Arc<App>>, headers: HeaderMap, body: axum::body::Bytes) -> Response {
    if !mcp::origin_ok(headers.get(header::ORIGIN).and_then(|v| v.to_str().ok())) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !mcp::bearer_ok(headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()), &app.mcp_token) {
        return (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, "Bearer")]).into_response();
    }
    let (id, method, params) = match mcp::parse(&body) {
        Ok(Incoming::Request { id, method, params }) => (id, method, params),
        Ok(Incoming::Accepted) => return StatusCode::ACCEPTED.into_response(),
        Err(e) => return (StatusCode::BAD_REQUEST, json_body(e)).into_response(),
    };
    let reply = match method.as_str() {
        "initialize" => mcp::result(id, mcp::initialize(&params, env!("CARGO_PKG_VERSION"))),
        "ping" => mcp::result(id, json!({})),
        "tools/list" => mcp::result(id, mcp::tools()),
        "tools/call" => {
            let outcome = match mcp::parse_call(&params) {
                Ok(call) => tokio::task::spawn_blocking(move || run(call)).await.unwrap_or_else(|e| Err(e.into())),
                Err(why) => Err(anyhow::anyhow!(why)),
            };
            let result = match outcome {
                Ok(value) => mcp::tool_result(&pretty(&value), false),
                Err(e) => mcp::tool_result(&format!("{e:#}"), true),
            };
            mcp::result(id, result)
        }
        other => mcp::error(id, mcp::METHOD_NOT_FOUND, &format!("unknown method: {other}")),
    };
    json_body(reply).into_response()
}

/// `GET`/`DELETE /mcp`: no event stream and no sessions here.
pub(super) async fn other() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST")]).into_response()
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

fn run(call: Call) -> Result<Value> {
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
            let mut turn = crate::cli::drive::last_turn(&ws, &task)?;
            let mut st = state(&task.path);
            // A turn can be reported over a moment before its reply is in
            // the transcript (seen on a new task's first turn): read again
            // for a little while rather than answer "no reply".
            let deadline = std::time::Instant::now() + REPLY_GRACE;
            while turn.replies.is_empty() && st.status != workspace::TaskStatus::Working && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(250));
                turn = crate::cli::drive::last_turn(&ws, &task)?;
                st = state(&task.path);
            }
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
        Call::SendMessage { workspace, task, text, force } => {
            let (ws, task) = find(&workspace, &task)?;
            let sent = crate::cli::drive::send_in(&ws, &task.name, &text, force)?;
            let how = match sent {
                crate::cli::drive::Sent::Opened => "opened the task's window with the message",
                crate::cli::drive::Sent::Typed => "sent",
            };
            Ok(json!({ "result": how, "status": state(&task.path).status.token() }))
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
    }
}

/// How long `tenx_get_output` waits for a settled turn's reply to reach the
/// transcript.
const REPLY_GRACE: Duration = Duration::from_secs(3);

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
