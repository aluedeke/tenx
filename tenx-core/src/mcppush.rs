//! `/mcp`'s push half: MCP sessions (`Mcp-Session-Id`) that subscribe to
//! tenx's events and hear them on a `GET /mcp` event stream, as
//! `notifications/tenx/event`. The contract is `docs/web-protocol.md`
//! (`/mcp`). What is decided here: a session's subscription and its replay
//! buffer, when it expires, which tasks just finished a turn, whether a
//! screen's task changed, which session waits for which reply, and every
//! event's JSON. The binary watches the sources and runs the stream.
//!
//! A client that never sends the session id back stays stateless: none of
//! this applies to it.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::time::{Duration, SystemTime};

use serde_json::{json, Map, Value};

/// The session header (MCP streamable HTTP). Not `mcp::SESSION_HEADER`,
/// which names the `tenx web` page a caller talks from.
pub const SESSION_ID_HEADER: &str = "mcp-session-id";
/// A reconnecting stream's last seen event (SSE).
pub const LAST_EVENT_ID_HEADER: &str = "last-event-id";
/// The notification method every event goes out as.
pub const METHOD: &str = "notifications/tenx/event";
/// Events kept per session for a resume.
pub const BUFFER: usize = 200;
/// A session with no stream and no request for this long is gone.
pub const IDLE_EXPIRY: Duration = Duration::from_secs(600);
/// A comment line this often keeps a proxy (Tailscale Serve) from closing
/// a quiet stream.
pub const PING_EVERY: Duration = Duration::from_secs(15);
/// The most sessions held at once; a new one beyond it pushes out the one
/// idle longest. Every `initialize` makes one, also a stateless client's
/// that never uses it.
pub const MAX_SESSIONS: usize = 256;
/// A reply in an event is cut to this many characters.
pub const REPLY_MAX_CHARS: usize = 4000;

/// What a session can subscribe to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EventKind {
    /// A task's edge into "needs you", or into pending secrets.
    Attention,
    /// A task's agent finished a turn.
    Turn,
    /// The task on a screen changed.
    Focus,
    /// The first finished turn after this session's `tenx_send_message`.
    Reply,
}

impl EventKind {
    pub const ALL: [EventKind; 4] = [EventKind::Attention, EventKind::Turn, EventKind::Focus, EventKind::Reply];

    pub fn token(self) -> &'static str {
        match self {
            EventKind::Attention => "attention",
            EventKind::Turn => "turn",
            EventKind::Focus => "focus",
            EventKind::Reply => "reply",
        }
    }

    pub fn parse(s: &str) -> Option<EventKind> {
        EventKind::ALL.into_iter().find(|k| k.token() == s)
    }
}

/// `tenx_subscribe`'s `events` argument.
pub fn parse_events(value: Option<&Value>) -> Result<BTreeSet<EventKind>, String> {
    let names = || EventKind::ALL.map(EventKind::token).join(", ");
    let Some(Value::Array(items)) = value else {
        return Err(format!("events must be a list of event kinds ({})", names()));
    };
    items
        .iter()
        .map(|v| v.as_str().and_then(EventKind::parse).ok_or_else(|| format!("unknown event kind {v} (one of {})", names())))
        .collect()
}

/// What `tenx_subscribe` answers.
pub fn subscribed_text(events: &BTreeSet<EventKind>) -> String {
    if events.is_empty() {
        "subscribed to nothing".to_string()
    } else {
        format!("subscribed to {}", events.iter().map(|k| k.token()).collect::<Vec<_>>().join(", "))
    }
}

/// The error a session-only tool gives a stateless caller.
pub const NEEDS_SESSION: &str = "needs an MCP session (Mcp-Session-Id)";

/// A screen that shows a task.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Screen {
    /// A `tenx web` tab, by its session id.
    Web(String),
    /// The tenx tmux session, which every terminal client shows.
    Terminal,
}

impl Screen {
    pub fn token(&self) -> String {
        match self {
            Screen::Web(id) => format!("web:{id}"),
            Screen::Terminal => "terminal".to_string(),
        }
    }

    pub fn parse(s: &str) -> Option<Screen> {
        match s.trim() {
            "terminal" => Some(Screen::Terminal),
            s => s.strip_prefix("web:").filter(|id| crate::web::valid_session_id(id)).map(|id| Screen::Web(id.to_string())),
        }
    }
}

/// A task as events name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRef {
    pub workspace: String,
    /// The slug.
    pub task: String,
    /// The display name.
    pub title: String,
}

/// A `tenx_send_message`'s id: `m<n>`, `n` counting up per server.
pub fn message_id(n: u64) -> String {
    format!("m{n}")
}

/// A `Last-Event-ID` header: the number we sent.
pub fn last_event_id(header: Option<&str>) -> Option<u64> {
    header?.trim().parse().ok()
}

/// RFC 3339, UTC, to the second.
pub fn rfc3339(t: SystemTime) -> String {
    let secs = t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Howard Hinnant's civil_from_days.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// An agent's replies as one text, cut to [`REPLY_MAX_CHARS`] (an ellipsis
/// marks a cut).
pub fn reply_text(replies: &[String]) -> String {
    let text = replies.join("\n\n");
    if text.chars().count() <= REPLY_MAX_CHARS {
        return text;
    }
    let mut cut: String = text.chars().take(REPLY_MAX_CHARS - 1).collect();
    cut.push('…');
    cut
}

fn task_params(kind: EventKind, at: &str, task: &TaskRef) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("kind".into(), kind.token().into());
    m.insert("at".into(), at.into());
    m.insert("workspace".into(), task.workspace.clone().into());
    m.insert("task".into(), task.task.clone().into());
    m.insert("title".into(), task.title.clone().into());
    m
}

/// `attention`: `status` is `blocked`, `signaled` or `secrets`.
pub fn attention(at: &str, task: &TaskRef, status: &str, reason: Option<&str>) -> Value {
    let mut m = task_params(EventKind::Attention, at, task);
    m.insert("status".into(), status.into());
    m.insert("reason".into(), reason.unwrap_or("").into());
    Value::Object(m)
}

pub fn turn(at: &str, task: &TaskRef, reply: &str) -> Value {
    let mut m = task_params(EventKind::Turn, at, task);
    m.insert("reply".into(), reply.into());
    Value::Object(m)
}

pub fn reply(at: &str, task: &TaskRef, message_id: &str, reply: &str) -> Value {
    let mut m = task_params(EventKind::Reply, at, task);
    m.insert("message_id".into(), message_id.into());
    m.insert("reply".into(), reply.into());
    Value::Object(m)
}

/// `focus`: the screen and its task, or `task: null`.
pub fn focus(at: &str, screen: &Screen, task: Option<&TaskRef>) -> Value {
    let mut m = match task {
        Some(t) => task_params(EventKind::Focus, at, t),
        None => {
            let mut m = Map::new();
            m.insert("kind".into(), EventKind::Focus.token().into());
            m.insert("at".into(), at.into());
            m.insert("task".into(), Value::Null);
            m
        }
    };
    m.insert("screen".into(), screen.token().into());
    Value::Object(m)
}

/// `tenx_current_task`'s answer.
pub fn current_task(screen: &Screen, task: Option<&TaskRef>) -> Value {
    match task {
        Some(t) => json!({ "screen": screen.token(), "workspace": t.workspace, "task": t.task, "title": t.title }),
        None => json!({ "screen": screen.token(), "task": null }),
    }
}

/// An event's params as the JSON-RPC notification that carries them.
pub fn notification(params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": METHOD, "params": params })
}

/// One session's state.
#[derive(Debug)]
struct Session {
    events: BTreeSet<EventKind>,
    /// `(event id, notification JSON)`, oldest first.
    buffer: VecDeque<(u64, String)>,
    last_id: u64,
    /// Seconds (the caller's clock) of the last request, or the last moment
    /// a stream was seen open.
    active_at: u64,
}

/// Every session. Times are seconds on the caller's clock.
#[derive(Debug, Default)]
pub struct Sessions {
    map: HashMap<String, Session>,
}

impl Sessions {
    /// A new session `id`, subscribed to nothing. At [`MAX_SESSIONS`], the one
    /// idle longest makes room (returned, so its stream can be ended).
    pub fn open(&mut self, id: &str, now: u64) -> Option<String> {
        let evicted = if self.map.len() >= MAX_SESSIONS {
            let oldest = self.map.iter().min_by_key(|(_, s)| s.active_at).map(|(k, _)| k.clone());
            if let Some(k) = &oldest {
                self.map.remove(k);
            }
            oldest
        } else {
            None
        };
        self.map.insert(id.to_string(), Session { events: BTreeSet::new(), buffer: VecDeque::new(), last_id: 0, active_at: now });
        evicted
    }

    /// Whether `id` is a live session; marks it active.
    pub fn touch(&mut self, id: &str, now: u64) -> bool {
        match self.map.get_mut(id) {
            Some(s) => {
                s.active_at = s.active_at.max(now);
                true
            }
            None => false,
        }
    }

    pub fn close(&mut self, id: &str) -> bool {
        self.map.remove(id).is_some()
    }

    /// Replace `id`'s subscription. `false`: no such session.
    pub fn subscribe(&mut self, id: &str, events: BTreeSet<EventKind>) -> bool {
        match self.map.get_mut(id) {
            Some(s) => {
                s.events = events;
                true
            }
            None => false,
        }
    }

    /// Number and buffer an event for every session subscribed to `kind` —
    /// or, with `to`, for that one only (if it is subscribed). Returns
    /// `(session, event id, notification JSON)` for each, to send to the
    /// sessions' open streams.
    pub fn publish(&mut self, kind: EventKind, params: &Value, to: Option<&str>) -> Vec<(String, u64, String)> {
        let data = notification(params.clone()).to_string();
        let mut out = Vec::new();
        for (id, s) in &mut self.map {
            if to.is_some_and(|t| t != id) || !s.events.contains(&kind) {
                continue;
            }
            s.last_id += 1;
            s.buffer.push_back((s.last_id, data.clone()));
            while s.buffer.len() > BUFFER {
                s.buffer.pop_front();
            }
            out.push((id.clone(), s.last_id, data.clone()));
        }
        out
    }

    /// What a stream resuming after event `after` gets first: the buffered
    /// events after it. Nothing without a `Last-Event-ID`.
    pub fn replay(&self, id: &str, after: Option<u64>) -> Vec<(u64, String)> {
        match (self.map.get(id), after) {
            (Some(s), Some(n)) => s.buffer.iter().filter(|(i, _)| *i > n).cloned().collect(),
            _ => Vec::new(),
        }
    }

    /// End the sessions idle past [`IDLE_EXPIRY`]; `streaming` says which
    /// have a stream open (those are active now). Returns the ended ones.
    pub fn expire(&mut self, now: u64, streaming: impl Fn(&str) -> bool) -> Vec<String> {
        let mut gone = Vec::new();
        for (id, s) in &mut self.map {
            if streaming(id) {
                s.active_at = s.active_at.max(now);
            } else if now.saturating_sub(s.active_at) >= IDLE_EXPIRY.as_secs() {
                gone.push(id.clone());
            }
        }
        for id in &gone {
            self.map.remove(id);
        }
        gone
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Which tasks just finished a turn: a task seen `working` that is now
/// `done` — or seen `done` and `done` again with a later status time, a
/// turn that started and ended between two polls (a quick answer, which a
/// `reply` must not miss). A task first seen done is a backlog, not news;
/// one that went working → blocked → done was already announced as needing
/// you. Keys are the caller's; a task missing from a poll is idle.
#[derive(Debug, Default)]
pub struct Turns {
    last: HashMap<String, (String, Option<SystemTime>)>,
}

impl Turns {
    /// One poll: `(key, status token, when the status changed)`. The keys
    /// that finished a turn since the last poll.
    pub fn step<'a>(&mut self, now: &[(&'a str, &str, Option<SystemTime>)]) -> Vec<&'a str> {
        let mut done = Vec::new();
        let mut next = HashMap::with_capacity(now.len());
        for &(key, status, changed) in now {
            if status == "done" {
                let finished = match self.last.get(key) {
                    Some((s, _)) if s == "working" => true,
                    Some((s, before)) if s == "done" => matches!((before, changed), (Some(b), Some(c)) if c > *b),
                    _ => false,
                };
                if finished {
                    done.push(key);
                }
            }
            next.insert(key.to_string(), (status.to_string(), changed));
        }
        self.last = next;
        done
    }
}

/// The task each screen shows (by the caller's task key), and which
/// screen changed last — what `tenx_current_task` answers by default.
#[derive(Debug, Default)]
pub struct Focus {
    screens: HashMap<Screen, Option<String>>,
    last: Option<Screen>,
}

impl Focus {
    /// What `screen` shows now. `true` when that is a change (a screen seen
    /// for the first time counts as one). `prime`: record a first sight
    /// without calling it a change — the state the server started in.
    pub fn update(&mut self, screen: &Screen, task: Option<&str>, prime: bool) -> bool {
        let task = task.map(str::to_string);
        match self.screens.get(screen) {
            Some(old) if *old == task => false,
            None if prime => {
                self.screens.insert(screen.clone(), task);
                false
            }
            _ => {
                self.screens.insert(screen.clone(), task);
                self.last = Some(screen.clone());
                true
            }
        }
    }

    /// A screen that is gone (a tab closed): no longer answered for.
    pub fn remove(&mut self, screen: &Screen) {
        self.screens.remove(screen);
        if self.last.as_ref() == Some(screen) {
            self.last = None;
        }
    }

    /// The task on `screen` — by default the screen that changed last, else
    /// the terminal. `None` for the task: the screen shows none, or is not
    /// known.
    pub fn current(&self, screen: Option<&Screen>) -> (Screen, Option<String>) {
        let screen = screen.cloned().or_else(|| self.last.clone()).unwrap_or(Screen::Terminal);
        let task = self.screens.get(&screen).cloned().flatten();
        (screen, task)
    }
}

/// Which session waits for which task's next finished turn.
#[derive(Debug, Default)]
pub struct Replies {
    pending: HashMap<String, Vec<(String, String)>>,
}

impl Replies {
    /// `session` sent `message_id` to `task`.
    pub fn expect(&mut self, task: &str, session: &str, message_id: &str) {
        self.pending.entry(task.to_string()).or_default().push((session.to_string(), message_id.to_string()));
    }

    /// `task` finished a turn: who gets a `reply`, `(session, message_id)`.
    /// One reply per session — the first turn after its messages answers
    /// them all, with the last message's id.
    pub fn take(&mut self, task: &str) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for (session, id) in self.pending.remove(task).unwrap_or_default() {
            match out.iter_mut().find(|(s, _)| *s == session) {
                Some(entry) => entry.1 = id,
                None => out.push((session, id)),
            }
        }
        out
    }

    /// A session that ended waits for nothing.
    pub fn forget(&mut self, session: &str) {
        for list in self.pending.values_mut() {
            list.retain(|(s, _)| s != session);
        }
        self.pending.retain(|_, l| !l.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn task() -> TaskRef {
        TaskRef { workspace: "tenx".into(), task: "pipecat".into(), title: "Pipecat".into() }
    }

    #[test]
    fn events_parse_by_name() {
        let all = parse_events(Some(&json!(["attention", "turn", "focus", "reply"]))).unwrap();
        assert_eq!(all.len(), 4);
        assert!(parse_events(Some(&json!([]))).unwrap().is_empty());
        assert!(parse_events(Some(&json!(["turn", "nope"]))).is_err());
        assert!(parse_events(Some(&json!("turn"))).is_err());
        assert!(parse_events(None).is_err());
        assert_eq!(subscribed_text(&all), "subscribed to attention, turn, focus, reply");
        assert_eq!(subscribed_text(&BTreeSet::new()), "subscribed to nothing");
    }

    #[test]
    fn screens_round_trip() {
        assert_eq!(Screen::parse("terminal"), Some(Screen::Terminal));
        assert_eq!(Screen::parse("web:3f9a0b1c2d3e4f50"), Some(Screen::Web("3f9a0b1c2d3e4f50".into())));
        assert_eq!(Screen::Web("ab12".into()).token(), "web:ab12");
        assert_eq!(Screen::parse("web:../x"), None);
        assert_eq!(Screen::parse("tv"), None);
    }

    #[test]
    fn times_are_rfc3339_utc() {
        assert_eq!(rfc3339(at(0)), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(at(951_782_400)), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(at(1_793_806_245)), "2026-11-04T15:30:45Z");
    }

    #[test]
    fn replies_are_joined_and_cut() {
        assert_eq!(reply_text(&["a".into(), "b".into()]), "a\n\nb");
        let long = reply_text(&["é".repeat(5000)]);
        assert_eq!(long.chars().count(), REPLY_MAX_CHARS);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn events_carry_the_contract_fields() {
        let a = attention("T", &task(), "blocked", None);
        assert_eq!(
            a,
            json!({"kind":"attention","at":"T","workspace":"tenx","task":"pipecat","title":"Pipecat","status":"blocked","reason":""})
        );
        assert_eq!(turn("T", &task(), "hi")["reply"], "hi");
        let r = reply("T", &task(), "m3", "hi");
        assert_eq!((r["kind"].as_str(), r["message_id"].as_str()), (Some("reply"), Some("m3")));
        let f = focus("T", &Screen::Terminal, None);
        assert_eq!(f, json!({"kind":"focus","at":"T","task":null,"screen":"terminal"}));
        assert_eq!(focus("T", &Screen::Web("ab".into()), Some(&task()))["screen"], "web:ab");
        assert_eq!(
            current_task(&Screen::Terminal, Some(&task())),
            json!({"screen":"terminal","workspace":"tenx","task":"pipecat","title":"Pipecat"})
        );
        assert_eq!(notification(json!({}))["method"], METHOD);
        assert_eq!(last_event_id(Some(" 12 ")), Some(12));
        assert_eq!(last_event_id(Some("x")), None);
        assert_eq!(message_id(7), "m7");
    }

    #[test]
    fn a_session_gets_what_it_subscribed_to_numbered() {
        let mut s = Sessions::default();
        s.open("a", 0);
        s.open("b", 0);
        assert!(s.publish(EventKind::Turn, &json!({}), None).is_empty(), "nothing subscribed yet");
        assert!(s.subscribe("a", [EventKind::Turn, EventKind::Reply].into()));
        assert!(s.subscribe("b", [EventKind::Turn].into()));
        assert!(!s.subscribe("c", BTreeSet::new()));
        let mut sent = s.publish(EventKind::Turn, &json!({"n": 1}), None);
        sent.sort();
        assert_eq!(sent.iter().map(|(id, n, _)| (id.as_str(), *n)).collect::<Vec<_>>(), vec![("a", 1), ("b", 1)]);
        assert!(sent[0].2.contains(METHOD));
        // A reply goes to its session only, and only if it subscribed.
        assert_eq!(s.publish(EventKind::Reply, &json!({}), Some("a")).len(), 1);
        assert!(s.publish(EventKind::Reply, &json!({}), Some("b")).is_empty());
        assert!(s.publish(EventKind::Focus, &json!({}), None).is_empty());
        // Resume after 1: event 2 only.
        assert_eq!(s.replay("a", Some(1)).iter().map(|(n, _)| *n).collect::<Vec<_>>(), vec![2]);
        assert!(s.replay("a", None).is_empty());
        // A new subscription replaces the old.
        s.subscribe("a", BTreeSet::new());
        assert_eq!(s.publish(EventKind::Turn, &json!({}), None).len(), 1);
    }

    #[test]
    fn the_buffer_keeps_the_last_events() {
        let mut s = Sessions::default();
        s.open("a", 0);
        s.subscribe("a", [EventKind::Turn].into());
        for _ in 0..BUFFER + 5 {
            s.publish(EventKind::Turn, &json!({}), None);
        }
        let all = s.replay("a", Some(0));
        assert_eq!(all.len(), BUFFER);
        assert_eq!(all[0].0, 6);
    }

    #[test]
    fn sessions_expire_without_a_stream_or_a_request() {
        let mut s = Sessions::default();
        s.open("quiet", 0);
        s.open("streaming", 0);
        s.open("asking", 0);
        assert!(s.expire(300, |_| false).is_empty());
        assert!(s.touch("asking", 500));
        assert!(!s.touch("nope", 500));
        let gone = s.expire(600, |id| id == "streaming");
        assert_eq!(gone, vec!["quiet".to_string()]);
        // Asked at 500: ten minutes from then.
        assert_eq!(s.expire(1100, |_| false), vec!["asking".to_string()]);
        // The stream was last seen open at 600.
        assert!(s.expire(1199, |_| false).is_empty());
        assert_eq!(s.expire(1200, |_| false), vec!["streaming".to_string()]);
        assert!(s.is_empty());
        s.open("x", 0);
        assert!(s.close("x"));
        assert!(!s.close("x"));
    }

    #[test]
    fn too_many_sessions_push_out_the_idlest() {
        let mut s = Sessions::default();
        for i in 0..MAX_SESSIONS {
            assert_eq!(s.open(&format!("s{i}"), 10 + i as u64), None);
        }
        s.touch("s0", 10_000);
        assert_eq!(s.open("new", 10_001), Some("s1".to_string()));
        assert_eq!(s.len(), MAX_SESSIONS);
    }

    #[test]
    fn a_turn_finishes_on_working_to_done_or_a_newer_done() {
        let mut t = Turns::default();
        // First sight: a backlog.
        assert!(t.step(&[("a", "done", Some(at(1))), ("b", "working", Some(at(1)))]).is_empty());
        assert_eq!(t.step(&[("a", "done", Some(at(1))), ("b", "done", Some(at(2)))]), vec!["b"]);
        // Still done, same time: said once.
        assert!(t.step(&[("a", "done", Some(at(1))), ("b", "done", Some(at(2)))]).is_empty());
        // A turn between two polls: done, then done again later.
        assert_eq!(t.step(&[("a", "done", Some(at(5))), ("b", "done", Some(at(2)))]), vec!["a"]);
        // Through blocked: announced as needing you.
        assert!(t.step(&[("b", "working", Some(at(6)))]).is_empty());
        assert!(t.step(&[("b", "blocked", Some(at(7)))]).is_empty());
        assert!(t.step(&[("b", "done", Some(at(8)))]).is_empty());
        // Gone (idle) and back done: a new session, not a turn.
        assert!(t.step(&[]).is_empty());
        assert!(t.step(&[("b", "done", Some(at(9)))]).is_empty());
    }

    #[test]
    fn focus_reports_changes_and_answers_the_last_changed_screen() {
        let mut f = Focus::default();
        let web = Screen::Web("ab".into());
        assert!(!f.update(&Screen::Terminal, Some("tenx/a"), true), "the start is no change");
        assert_eq!(f.current(None), (Screen::Terminal, Some("tenx/a".into())));
        assert!(f.update(&web, Some("tenx/b"), false), "a new tab is");
        assert!(!f.update(&web, Some("tenx/b"), false));
        assert_eq!(f.current(None), (web.clone(), Some("tenx/b".into())));
        assert!(f.update(&Screen::Terminal, None, false));
        assert_eq!(f.current(None), (Screen::Terminal, None));
        assert_eq!(f.current(Some(&web)), (web.clone(), Some("tenx/b".into())));
        f.remove(&web);
        assert_eq!(f.current(Some(&web)), (web, None));
    }

    #[test]
    fn a_reply_goes_once_to_each_waiting_session() {
        let mut r = Replies::default();
        r.expect("tenx/a", "s1", "m1");
        r.expect("tenx/a", "s1", "m2");
        r.expect("tenx/a", "s2", "m3");
        r.expect("tenx/b", "s1", "m4");
        assert_eq!(r.take("tenx/a"), vec![("s1".into(), "m2".into()), ("s2".into(), "m3".into())]);
        assert!(r.take("tenx/a").is_empty());
        r.forget("s1");
        assert!(r.take("tenx/b").is_empty());
    }
}
