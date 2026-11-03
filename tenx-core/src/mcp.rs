//! `tenx web`'s `/mcp`: tenx's tasks as MCP tools, for an agent elsewhere —
//! Claude on the web, through a gateway that logs it in. The transport is
//! MCP's streamable HTTP in its plainest form: one JSON-RPC message per
//! `POST`, answered with one JSON body (never an event stream), no session
//! id, so every request stands alone. What a request *is* — the bearer
//! token, the message, the protocol version, a tool call's arguments — is
//! decided here; the binary runs the call.

use serde_json::{json, Map, Value};

/// Protocol revisions this server speaks, newest first. A client asking for
/// one gets it; any other gets the newest, and decides itself whether it can
/// go on (the spec's negotiation).
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];

/// The revision to answer `initialize` with.
pub fn negotiate(requested: Option<&str>) -> &'static str {
    PROTOCOL_VERSIONS.iter().copied().find(|v| Some(*v) == requested).unwrap_or(PROTOCOL_VERSIONS[0])
}

/// Whether an `Authorization` header carries `token` as a bearer token.
/// Compared in constant time, like the page's token (`web::token_matches`).
pub fn bearer_ok(header: Option<&str>, token: &str) -> bool {
    let Some(given) = header.and_then(|h| h.strip_prefix("Bearer ").or_else(|| h.strip_prefix("bearer "))) else {
        return false;
    };
    crate::web::token_matches(given.trim(), token)
}

/// Whether a request's `Origin` is acceptable. `/mcp` is called by a
/// gateway, server to server, which sends none; a browser always does, and
/// no page has any business here (the spec's DNS-rebinding rule).
pub fn origin_ok(origin: Option<&str>) -> bool {
    origin.is_none()
}

/// The header naming the `tenx web` tab a caller talks from: that tab's
/// session id (the page's `hello`). The voice assistant in the page sends
/// it, so `tenx_focus_task` switches the page you are talking to; the model
/// never sees it. A caller without it is on no page of ours (Claude on the
/// web), and the task goes to the tenx session — every terminal on it.
pub const SESSION_HEADER: &str = "tenx-session";

/// Where `tenx_focus_task` shows a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusTarget {
    /// The `tenx web` tab with this session id.
    Tab(String),
    /// The tenx session, which every terminal client shows.
    Terminals,
}

/// The target a request's [`SESSION_HEADER`] names. A malformed id is an
/// error rather than the terminals: the caller meant a page.
pub fn focus_target(header: Option<&str>) -> Result<FocusTarget, String> {
    match header.map(str::trim).filter(|h| !h.is_empty()) {
        None => Ok(FocusTarget::Terminals),
        Some(id) if crate::web::valid_session_id(id) => Ok(FocusTarget::Tab(id.to_string())),
        Some(_) => Err(format!("{SESSION_HEADER} must be a tenx web session id")),
    }
}

/// The largest request `/mcp` takes: a message to an agent is text.
pub const MAX_BYTES: usize = 1024 * 1024;

/// JSON-RPC error codes.
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;

/// One incoming message.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// A request: answer it.
    Request { id: Value, method: String, params: Value },
    /// A notification or a response: nothing to answer (HTTP 202).
    Accepted,
}

/// Parse a `POST` body. `Err` is the error response to send back.
pub fn parse(body: &[u8]) -> Result<Incoming, Value> {
    let value: Value = serde_json::from_slice(body).map_err(|e| error(Value::Null, PARSE_ERROR, &format!("not JSON: {e}")))?;
    let Value::Object(msg) = value else {
        return Err(error(Value::Null, INVALID_REQUEST, "expected one JSON-RPC message (batches are not supported)"));
    };
    let id = msg.get("id").cloned();
    match (msg.get("method").and_then(Value::as_str), id) {
        (Some(method), Some(id)) if !id.is_null() => {
            Ok(Incoming::Request { id, method: method.to_string(), params: msg.get("params").cloned().unwrap_or(Value::Null) })
        }
        // A notification (no id), or a response to something we never asked.
        (Some(_), None) | (None, Some(_)) => Ok(Incoming::Accepted),
        _ => Err(error(Value::Null, INVALID_REQUEST, "not a JSON-RPC request")),
    }
}

pub fn result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// The `initialize` result.
pub fn initialize(params: &Value, version: &str) -> Value {
    json!({
        "protocolVersion": negotiate(params.get("protocolVersion").and_then(Value::as_str)),
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "tenx", "title": "tenx", "version": version },
        "instructions": INSTRUCTIONS,
    })
}

const INSTRUCTIONS: &str = "tenx runs coding-agent tasks on Andreas's Mac: each task is a \
directory with git worktrees and an agent (usually Claude Code) in a tmux window. Start \
with tenx_list_tasks. A task is named by its workspace and slug. To drive one: \
tenx_send_message, then tenx_wait until it settles, then tenx_get_output for the reply. \
tenx_create_task starts a new one (no workspace = the adhoc workspace, for a question \
without repos). A task whose status is 'blocked' waits on a dialog: tenx_read_screen \
shows it, tenx_answer_permission answers a permission prompt. tenx_focus_task shows a task \
on the user's screen: when they ask to see one, or to switch to the task you talk about.";

/// A tool call, its arguments checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    ListWorkspaces,
    /// `status`: only tasks with this status token.
    ListTasks { workspace: Option<String>, status: Option<String> },
    GetTask { workspace: String, task: String },
    GetOutput { workspace: String, task: String },
    ReadScreen { workspace: String, task: String, lines: usize },
    /// `workspace: None` = the adhoc workspace; `repos: None` = all of the
    /// workspace's.
    CreateTask {
        workspace: Option<String>,
        title: Option<String>,
        prompt: String,
        description: Option<String>,
        repos: Option<Vec<String>>,
    },
    SendMessage { workspace: String, task: String, text: String, force: bool },
    Wait { workspace: String, task: String, timeout_secs: u64 },
    AnswerPermission { workspace: String, task: String, approve: bool },
    FocusTask { workspace: String, task: String },
}

/// How long one `tenx_wait` call may block: under the 100 s a Cloudflare
/// tunnel gives a response, with room to spare.
pub const WAIT_MAX_SECS: u64 = 60;
pub const WAIT_DEFAULT_SECS: u64 = 30;
pub const SCREEN_DEFAULT_LINES: usize = 40;
pub const SCREEN_MAX_LINES: usize = 200;

/// The task statuses `tenx_list_tasks` filters on (`TaskStatus::token`).
const STATUSES: &[&str] = &["blocked", "signaled", "working", "done", "idle"];

/// A `tools/call` request's `name` and `arguments` as a [`Call`]; `Err` is
/// the message for the caller.
pub fn parse_call(params: &Value) -> Result<Call, String> {
    let name = params.get("name").and_then(Value::as_str).ok_or("no tool name")?;
    let empty = Map::new();
    let args = match params.get("arguments") {
        None | Some(Value::Null) => &empty,
        Some(Value::Object(m)) => m,
        Some(_) => return Err("arguments must be an object".into()),
    };
    let task = || -> Result<(String, String), String> { Ok((required(args, "workspace")?, required(args, "task")?)) };
    Ok(match name {
        "tenx_list_workspaces" => Call::ListWorkspaces,
        "tenx_list_tasks" => {
            let status = optional(args, "status")?;
            if let Some(s) = &status
                && !STATUSES.contains(&s.as_str())
            {
                return Err(format!("status must be one of {}", STATUSES.join(", ")));
            }
            Call::ListTasks { workspace: optional(args, "workspace")?, status }
        }
        "tenx_get_task" => {
            let (workspace, task) = task()?;
            Call::GetTask { workspace, task }
        }
        "tenx_get_output" => {
            let (workspace, task) = task()?;
            Call::GetOutput { workspace, task }
        }
        "tenx_read_screen" => {
            let (workspace, task) = task()?;
            let lines = number(args, "lines")?.map(|n| n as usize).unwrap_or(SCREEN_DEFAULT_LINES).clamp(1, SCREEN_MAX_LINES);
            Call::ReadScreen { workspace, task, lines }
        }
        "tenx_create_task" => {
            let prompt = required(args, "prompt")?;
            let repos = match args.get("repos") {
                None | Some(Value::Null) => None,
                Some(Value::Array(items)) => Some(
                    items
                        .iter()
                        .map(|v| v.as_str().map(str::to_string).ok_or("repos must be a list of repo names"))
                        .collect::<Result<Vec<_>, _>>()?,
                ),
                Some(_) => return Err("repos must be a list of repo names".into()),
            };
            Call::CreateTask {
                workspace: optional(args, "workspace")?,
                title: optional(args, "title")?,
                prompt,
                description: optional(args, "description")?,
                repos,
            }
        }
        "tenx_send_message" => {
            let (workspace, task) = task()?;
            Call::SendMessage { workspace, task, text: required(args, "text")?, force: flag(args, "force")?.unwrap_or(false) }
        }
        "tenx_wait" => {
            let (workspace, task) = task()?;
            let timeout_secs = number(args, "timeout_secs")?.unwrap_or(WAIT_DEFAULT_SECS).clamp(1, WAIT_MAX_SECS);
            Call::Wait { workspace, task, timeout_secs }
        }
        "tenx_answer_permission" => {
            let (workspace, task) = task()?;
            let approve = flag(args, "approve")?.ok_or("missing argument: approve")?;
            Call::AnswerPermission { workspace, task, approve }
        }
        "tenx_focus_task" => {
            let (workspace, task) = task()?;
            Call::FocusTask { workspace, task }
        }
        other => return Err(format!("unknown tool: {other}")),
    })
}

/// A non-empty string argument.
fn required(args: &Map<String, Value>, key: &str) -> Result<String, String> {
    optional(args, key)?.ok_or_else(|| format!("missing argument: {key}"))
}

/// A string argument; empty counts as absent.
fn optional(args: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.trim().to_string())),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

fn number(args: &Map<String, Value>, key: &str) -> Result<Option<u64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().map(Some).ok_or_else(|| format!("{key} must be a whole number")),
    }
}

fn flag(args: &Map<String, Value>, key: &str) -> Result<Option<bool>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(format!("{key} must be true or false")),
    }
}

/// A `tools/call` result: `text` for the model, `is_error` when the tool
/// failed (a tool error, not a protocol one — the model reads it).
pub fn tool_result(text: &str, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

/// The `tools/list` result.
pub fn tools() -> Value {
    let ws = json!({ "type": "string", "description": "Workspace name, as tenx_list_tasks reports it (`ws`)." });
    let task = json!({ "type": "string", "description": "The task's slug (`slug` in tenx_list_tasks)." });
    let task_args = json!({ "workspace": ws, "task": task });
    let read_only = json!({ "readOnlyHint": true, "openWorldHint": false });
    let tools = vec![
        tool(
            "tenx_list_workspaces",
            "List workspaces",
            "Every registered tenx workspace with its repos.",
            json!({}),
            &[],
            read_only.clone(),
        ),
        tool(
            "tenx_list_tasks",
            "List tasks",
            "Every task with its status (blocked = waits on a dialog, signaled = rang a bell, working, done = \
             finished its turn, idle = no agent running), what it waits for, its PRs and its age, most recently \
             active first.",
            json!({
                "workspace": { "type": "string", "description": "Only this workspace's tasks." },
                "status": { "type": "string", "enum": STATUSES, "description": "Only tasks with this status." },
            }),
            &[],
            read_only.clone(),
        ),
        tool(
            "tenx_get_task",
            "Get task",
            "One task: its status and PRs, and its TASK.md (description, todo list, links, notes).",
            task_args.clone(),
            &["workspace", "task"],
            read_only.clone(),
        ),
        tool(
            "tenx_get_output",
            "Get agent reply",
            "The task's last prompt and everything its agent replied since, read from the transcript.",
            task_args.clone(),
            &["workspace", "task"],
            read_only.clone(),
        ),
        tool(
            "tenx_read_screen",
            "Read agent screen",
            "The last lines on the task's agent pane, as text — what a dialog asks, or what a turn is doing.",
            json!({
                "workspace": ws,
                "task": task,
                "lines": { "type": "integer", "minimum": 1, "maximum": SCREEN_MAX_LINES, "description": "How many lines (default 40)." },
            }),
            &["workspace", "task"],
            read_only.clone(),
        ),
        tool(
            "tenx_create_task",
            "Create task",
            "Create a task (git worktrees on a new branch per repo) and start its agent on `prompt`, in a window \
             that does not take the user's screen. Returns the workspace and slug.",
            json!({
                "workspace": { "type": "string", "description": "Workspace name. Leave out for the adhoc workspace (no repos)." },
                "prompt": { "type": "string", "description": "The agent's first message." },
                "title": { "type": "string", "description": "Task title. Default: one made from the prompt." },
                "description": { "type": "string", "description": "Goes under ## Description in TASK.md." },
                "repos": { "type": "array", "items": { "type": "string" }, "description": "Repos to check out. Default: all of the workspace's; [] for none." },
            }),
            &["prompt"],
            json!({ "readOnlyHint": false, "destructiveHint": false, "openWorldHint": false }),
        ),
        tool(
            "tenx_send_message",
            "Send message to agent",
            "Type a message into the task's agent and submit it (opens the task's window with the message when it \
             is closed). Refused while the agent waits on a dialog, unless `force`.",
            json!({
                "workspace": ws,
                "task": task,
                "text": { "type": "string", "description": "The message." },
                "force": { "type": "boolean", "description": "Type it even over an open dialog." },
            }),
            &["workspace", "task", "text"],
            json!({ "readOnlyHint": false, "destructiveHint": true, "openWorldHint": false }),
        ),
        tool(
            "tenx_wait",
            "Wait for agent",
            "Block until the task's agent ends its turn or needs an answer, up to `timeout_secs` (default 30, at \
             most 60). Returns the status; call again while it is still working.",
            json!({
                "workspace": ws,
                "task": task,
                "timeout_secs": { "type": "integer", "minimum": 1, "maximum": WAIT_MAX_SECS },
            }),
            &["workspace", "task"],
            read_only,
        ),
        tool(
            "tenx_answer_permission",
            "Answer permission prompt",
            "Approve or deny the permission prompt the task's agent shows (only when one is on screen).",
            json!({
                "workspace": ws,
                "task": task,
                "approve": { "type": "boolean", "description": "true approves, false denies." },
            }),
            &["workspace", "task", "approve"],
            json!({ "readOnlyHint": false, "destructiveHint": true, "openWorldHint": false }),
        ),
        tool(
            "tenx_focus_task",
            "Focus task",
            "Show the task on the user's screen, with its agent's history: in the tenx web page the call comes \
             from (the voice assistant's), else in every tenx terminal. Opens the task's window first when it is \
             closed. Use it when the user asks to see a task, or to switch to the task you are talking about.",
            task_args,
            &["workspace", "task"],
            json!({ "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }),
        ),
    ];
    json!({ "tools": tools })
}

fn tool(name: &str, title: &str, description: &str, properties: Value, required: &[&str], annotations: Value) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required, "additionalProperties": false },
        "annotations": annotations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiates_a_known_version_or_the_newest() {
        assert_eq!(negotiate(Some("2025-06-18")), "2025-06-18");
        assert_eq!(negotiate(Some("1999-01-01")), PROTOCOL_VERSIONS[0]);
        assert_eq!(negotiate(None), PROTOCOL_VERSIONS[0]);
    }

    #[test]
    fn bearer_token_must_match() {
        assert!(bearer_ok(Some("Bearer abc"), "abc"));
        assert!(!bearer_ok(Some("Bearer abd"), "abc"));
        assert!(!bearer_ok(Some("abc"), "abc"));
        assert!(!bearer_ok(None, "abc"));
        // No token configured opens nothing.
        assert!(!bearer_ok(Some("Bearer "), ""));
    }

    #[test]
    fn any_origin_is_refused() {
        assert!(origin_ok(None));
        assert!(!origin_ok(Some("https://evil.example")));
    }

    #[test]
    fn focus_goes_to_the_named_tab_or_the_terminals() {
        assert_eq!(focus_target(None), Ok(FocusTarget::Terminals));
        assert_eq!(focus_target(Some(" ")), Ok(FocusTarget::Terminals));
        assert_eq!(focus_target(Some("3f9a0b1c2d3e4f50")), Ok(FocusTarget::Tab("3f9a0b1c2d3e4f50".into())));
        assert!(focus_target(Some("../tenx")).is_err());
    }

    #[test]
    fn parses_requests_and_accepts_notifications() {
        let req = parse(br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        assert_eq!(req, Incoming::Request { id: json!(1), method: "tools/list".into(), params: Value::Null });
        assert_eq!(parse(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).unwrap(), Incoming::Accepted);
        assert_eq!(parse(br#"{"jsonrpc":"2.0","id":3,"result":{}}"#).unwrap(), Incoming::Accepted);
        assert_eq!(parse(b"nope").unwrap_err()["error"]["code"], PARSE_ERROR);
        assert_eq!(parse(b"[]").unwrap_err()["error"]["code"], INVALID_REQUEST);
        assert_eq!(parse(br#"{"jsonrpc":"2.0","id":null,"method":"x"}"#).unwrap_err()["error"]["code"], INVALID_REQUEST);
    }

    #[test]
    fn checks_call_arguments() {
        let call = |v: Value| parse_call(&v);
        assert_eq!(call(json!({"name": "tenx_list_workspaces"})), Ok(Call::ListWorkspaces));
        assert_eq!(
            call(json!({"name": "tenx_list_tasks", "arguments": {"status": "blocked", "workspace": ""}})),
            Ok(Call::ListTasks { workspace: None, status: Some("blocked".into()) })
        );
        assert!(call(json!({"name": "tenx_list_tasks", "arguments": {"status": "asleep"}})).is_err());
        assert_eq!(
            call(json!({"name": "tenx_get_task", "arguments": {"workspace": "w"}})),
            Err("missing argument: task".into())
        );
        assert_eq!(
            call(json!({"name": "tenx_wait", "arguments": {"workspace": "w", "task": "t", "timeout_secs": 900}})),
            Ok(Call::Wait { workspace: "w".into(), task: "t".into(), timeout_secs: WAIT_MAX_SECS })
        );
        assert_eq!(
            call(json!({"name": "tenx_read_screen", "arguments": {"workspace": "w", "task": "t"}})),
            Ok(Call::ReadScreen { workspace: "w".into(), task: "t".into(), lines: SCREEN_DEFAULT_LINES })
        );
        assert_eq!(
            call(json!({"name": "tenx_create_task", "arguments": {"prompt": " hi ", "repos": []}})),
            Ok(Call::CreateTask { workspace: None, title: None, prompt: "hi".into(), description: None, repos: Some(vec![]) })
        );
        assert!(call(json!({"name": "tenx_create_task", "arguments": {"prompt": "hi", "repos": [1]}})).is_err());
        assert_eq!(
            call(json!({"name": "tenx_answer_permission", "arguments": {"workspace": "w", "task": "t"}})),
            Err("missing argument: approve".into())
        );
        assert_eq!(
            call(json!({"name": "tenx_focus_task", "arguments": {"workspace": "w", "task": "t"}})),
            Ok(Call::FocusTask { workspace: "w".into(), task: "t".into() })
        );
        assert!(call(json!({"name": "tenx_rm"})).is_err());
    }

    /// Every listed tool parses with its required arguments, so the list and
    /// `parse_call` can't drift apart.
    #[test]
    fn every_listed_tool_is_callable() {
        let list = tools();
        let tools = list["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 10);
        for t in tools {
            let mut args = Map::new();
            for key in t["inputSchema"]["required"].as_array().unwrap() {
                let key = key.as_str().unwrap();
                let kind = t["inputSchema"]["properties"][key]["type"].as_str().unwrap_or("string");
                args.insert(key.into(), if kind == "boolean" { json!(true) } else { json!("x") });
            }
            let params = json!({ "name": t["name"], "arguments": args });
            assert!(parse_call(&params).is_ok(), "{}: {:?}", t["name"], parse_call(&params));
        }
    }
}
