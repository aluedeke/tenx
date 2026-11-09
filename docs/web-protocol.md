# tenx web protocol

`tenx web` serves the column as a web page next to an xterm.js terminal. The
Rust server owns the column (one `Column` per connection, the same state
machine as the TUI); the browser only renders the `ColumnView` it is sent and
forwards keys and clicks. This file is the contract between `src/web/` and
`web/`; change both sides together.

## HTTP

| Route | Auth | What |
|---|---|---|
| `GET /?token=<t>` | token | Sets the `tenx_web` cookie (HttpOnly, SameSite=Lax, Path=/, a year, renewed by every page load) and redirects `303` to `/` without the query. A wrong token gets `401` — unless the browser is signed in already (an old address), which is just redirected. |
| `GET /<asset>` | cookie | The embedded static export of `web/` (`web/out`), `index.html` for `/`. Without the cookie: `401` with a short page telling you to open the URL `tenx web` printed. |
| `GET /ws` | cookie + Origin | Upgrades to the WebSocket below. The `Origin` header must equal `http(s)://<Host>` or an origin passed with `--dev-origin`. |
| `GET /push/key` | cookie | `{"key": …}`: the server's VAPID public key (base64url), the `applicationServerKey` a browser subscribes with. A same-origin GET carries no `Origin`, so only the cookie is checked. |
| `POST /push/subscribe` | cookie + Origin | A `PushSubscription` as `toJSON()` gives it (`{endpoint, keys: {p256dh, auth}}`). Stored in `~/.config/tenx/web-push-subs.json` (600), one per endpoint. `400` for an endpoint that isn't HTTPS (plain HTTP only to loopback, for tests) or keys that aren't a P-256 point and a secret. |
| `POST /push/unsubscribe` | cookie + Origin | `{endpoint}`: forget that subscription. |
| `POST /push/test` | cookie + Origin | Push a test message to every subscription: `{sent, subscriptions}`. |
| `POST /mcp` | bearer token, no Origin | MCP over streamable HTTP, for an agent on another machine (not the page): one JSON-RPC message per request, answered with one `application/json` body; notifications get `202`. `Authorization: Bearer` with `~/.config/tenx/mcp-token` (`tenx web mcp-token`), never the cookie; any `Origin` header gets `403`. `initialize` also answers `Mcp-Session-Id: <id>`: a client that sends it back on every later request has a session (see [MCP sessions and events](#mcp-sessions-and-events)); an unknown or expired one gets `404`. A client that never sends it is stateless. A `tenx-session: <id>` header names the page the caller talks from (the voice assistant sends its page's session id): `tenx_focus_task` then shows the task on that page (`show_task` below) instead of in the tenx session. Tools and rules: `tenx_core::mcp`. |
| `GET /mcp` | bearer token, no Origin | A session's event stream (`text/event-stream`), with `Mcp-Session-Id`: `400` without one, `404` for an unknown one. |
| `DELETE /mcp` | bearer token, no Origin | Ends the session named by `Mcp-Session-Id` (`200`; `404` unknown, `400` none). |
| `GET /manifest.webmanifest` | — | Public (browsers fetch it without cookies). With the cookie — the page links it `crossorigin="use-credentials"` — its `start_url` is `/?token=…`, so an app installed to an iOS Home Screen, which has cookies of its own, signs itself in on first launch. |
| `POST /paste` | cookie + Origin | An image (`Content-Type` PNG, JPEG, GIF or WebP; at most 25 MB) saved to `~/.config/tenx/web-paste/` (600, swept after a day). Answers `{"path": …}`; the page pastes that path into the terminal, which Claude Code attaches as an image. `415` for anything else. |
| `POST /clipboard/get` | cookie + Origin | The text on the clipboard of the machine `tenx web` runs on: `{"text": …}` (empty when it holds none). A POST so that even a same-origin request carries its `Origin` — a clipboard holds passwords. macOS `pbpaste`; elsewhere `wl-paste`, else `xclip`. `502` when there is no such program. |
| `POST /clipboard/set` | cookie + Origin | The body (UTF-8 text, at most 4 MB) onto that machine's clipboard (`pbcopy`; `wl-copy`, else `xclip`). Answers `{"ok": true}`; `400` for a body that isn't UTF-8, `502` when there is no clipboard program. `TENX_WEB_CLIPBOARD=<file>` uses a file instead of the clipboard, for tests. |
| `GET /speech[?language=]` (WebSocket) | cookie + Origin | Live dictation, one socket per recording (`dictate.rs`). The page sends binary frames of 16-bit little-endian PCM, 16 kHz mono, as it records, then `{"type":"stop"}`. While audio comes in, about once a second: `{"type":"live","done":…,"committed":…,"tentative":…,"corrected":bool}` — `done` (finished sentences, each after its own final pass) and `committed` (words two runs agreed on) never change, `tentative` may; `corrected` when a sentence's final pass changed a word. After stop: `{"type":"text","text":…}` (empty when there was no speech) or, at any time, `{"type":"error","error":…}`; then it closes. With `TENX_WEB_STT_URL` there are no `live` messages: the whole recording goes to that server on stop. At most ~13 minutes. |
| `POST /secrets?task=<id>` | cookie + Origin | The task's pending secrets requests, for the page's secrets form (`src/web/secrets.rs`): `{title, requests: [{name, ask: "release" \| "value", why, sealed_in}], passphrase, problem}`. `passphrase`: the age identity is passphrase-protected, so the form asks for it; `problem`: why granting can't work from here (no identity yet — that is created at the terminal), `null` otherwise. Answering it in the terminal (`tenx secrets fulfill`) is the same sitting. `422` for an unknown task. |
| `POST /secrets/answer?task=<id>` | cookie + Origin | The form's answers, JSON (at most 256 KB): `{shown, granted, values: [[name, value]], note, passphrase}`. Of the `shown` names the `granted` ones are granted, the rest denied with `note`; a name answered elsewhere since, or one that arrived after the form opened, is left alone (`tenx_core::secrets::plan_answers`). The passphrase is typed into `age`'s own prompt through a pty (`age` takes it from nowhere else); it gets one try, and a wrong one changes nothing — the denials too wait for it. Answers `{denied, released, skipped}` (`skipped`: granted value requests left empty, still pending). `422` with the reason when nothing was changed. Values and passphrase are never logged. |
| `POST /speak?task=<id>` | cookie + Origin | The last reply of the task's agent (`<id>`: a column row's id, `<workspace>/<slug>`), to read aloud: the agent's last message since the last prompt, from its transcript (what `tenx task output` prints last), made speakable (`tenx_core::readaloud`: code blocks left out, links read as their text, table rows as lists) and cut into pieces of whole sentences (`tenx_core::tts::pieces`: a short first one, so the voice starts soon). Answers `{"pieces": [...]}` and starts `tenx-tts` loading. `422` with the reason when there is no such task, no reply since the last prompt, or no `[read_aloud] model` set. |
| `POST /speak/audio` | cookie + Origin | The body (UTF-8 text, one piece; at most 64 KB) read aloud on this machine by `tenx-tts` (Kokoro through sherpa-onnx) with `[read_aloud]` from the config: `audio/wav`, 16-bit mono at the model's rate (24 kHz). One piece at a time. The page asks for the next piece while one plays. `400` for a body that isn't UTF-8 or is empty, `502` with the reason when no model is set or the program fails. |
| `POST /transcribe[?language=]` | cookie + Origin | One recording at once (the page streams to `/speech` instead): a recording (`Content-Type` `audio/wav`, `audio/mp4`, `audio/webm` or `audio/ogg`; at most 25 MB), turned into text on this machine: by `tenx-whisper` with `[speech] model` from the config (16 kHz mono 16-bit WAV only — what the page sends), or by the whisper.cpp `whisper-server` at `TENX_WEB_STT_URL` when that is set (any of the formats). `language`: a Whisper code or `auto`, anything else dropped. Answers `{"text": …}`, trimmed; `502` with the reason when no model is set, or the program or server fails. |

The token lives in `~/.config/tenx/web-token` (mode 600), created on first
start and the same across restarts, reinstalls and upgrades; only
`tenx web --rotate-token` replaces it. The cookie lasts a year and every
signed-in load of the page (`/`, an `.html` page) renews it, so a browser or
an installed app in use stays signed in. `--dev-origin
http://localhost:3000` lets `next dev` on another port connect; in dev the
page passes the token as `?token=` on the WebSocket URL instead of the cookie
(accepted only when the Origin is a `--dev-origin`).

## WebSocket `/ws?session=<id>&patch=1`

`patch=1` says the page applies `patch` messages (below); without it every
view is sent whole, as to a page from before them.

`session` is optional: the id of a grouped session this tab had before (kept
in `localStorage`). If `tenx-web-<id>` still exists (within the 30 s grace
period after a disconnect) the connection re-attaches to it; otherwise the
server makes a new one, grouped with `tenx`, on `tenx`'s current window. The page then goes back to the task that device last had in front of it (remembered in `localStorage`), if that task's window is open — a closed one isn't reopened, since that would start its agent.

Two frame kinds:

- **Binary** frames are terminal bytes: server → browser is the PTY output of
  `tmux attach -t tenx-web-<id>`, browser → server is keyboard input for it.
- **Text** frames are JSON messages with a `type`.

### Server → browser

```jsonc
{ "type": "hello", "session": "3f9a…", "host": "mbal", "version": "0.2.0" }

// The whole column, whenever it changes (after every key/click, and when the
// refresh — 500 ms statuses, 2 s windows — changes what it would show).
// `ack` is the highest input `seq` the view reflects (0: none yet); a view
// goes out after every numbered input, even one that changed nothing.
{ "type": "view", "ack": 17, "view": ColumnView }   // src/tui/column/view.rs, serialized
// `view.sidebar` is the sidebar of the task the terminal shows (or null):
// `{id, title, ws, ws_color, sections: [{title, count?, progress?, entries:
// [{icon, icon_tone?, label, checks?, parts?: [{text, tone, pill?}], url?,
// kind?}]}]}` — tenx_core::sidebar. A tone is plain | good | warn | bad |
// merged | muted; `checks` is one tone per PR check (drawn as squares).

// With `patch=1`, every view after a socket's first is sent as what changed
// since the one before (tenx-core/src/viewpatch.rs; the page's side is
// web/src/lib/viewpatch.ts), unless that would be longer than the view:
// `set` — top-level fields that changed, whole; `unset` — fields that are
// gone; `items` — when the rows changed, the new list, each entry the index
// of an equal row in the previous list or a row. Each part is left out when
// empty; a patch with none of them only moves the ack. A view is ~40 KB, a
// cursor move's patch ~1 KB.
{ "type": "patch", "ack": 18, "set": { "focus": "list" }, "items": [0, {"kind": "task", …}, 2, 3] }

// The column's layout for the browser's width in cells (reply to `viewport`).
// While a page is narrow (a phone), the server zooms the agent's pane of each
// task window the page moves to, so the phone shows the agent alone; the
// other panes are tmux's own keys away (`Ctrl+b z`, `Ctrl+b o`). It unzooms
// what it zoomed when the page leaves the window, grows wide, or ends.
{ "type": "layout", "column_cols": 36, "narrow": false }

// What the TUI client would do with a ClientRequest — and `focus_column`,
// to hand the keyboard back to the column. `sidebar` (`s`, `:sidebar`):
// show or hide the sidebar.
{ "type": "request", "request": "focus_terminal" | "focus_column" | "hide" | "quit" | "sidebar" }

// The column asked to unlock a task's secrets (`u`, `:unlock`, a row's
// unlock): the page opens its secrets form (`/secrets` above) in the
// column's place. The terminal client opens `tenx secrets fulfill` in a tmux
// popup instead; a web page can paste into a form, not into a terminal.
{ "type": "unlock", "task": "tenx/cloud-tasks" }

// Show this task (a row id): `tenx_focus_task` from the voice assistant
// talking in this page (`/mcp` with this page's `tenx-session`). The page
// opens it as a notification's tap does — `click` + `open` — so the
// terminal shows it, and a phone hides the column.
{ "type": "show_task", "task": "tenx/cloud-tasks" }

// A link the column picked (`L`, `f`) that the page didn't open itself — a
// fallback: the page opens links inside the key press (see below).
{ "type": "link", "action": "open" | "copy", "url": "https://…", "label": "…" }

{ "type": "error", "message": "…" }
```

### Browser → server

Inputs that change the column — `key`, `click`, `action`, `form` — may carry
an increasing `seq` (per socket, from 1; a new socket starts over). The server
applies them in order and reports the highest applied in each view's `ack`.
The page uses that to draw the column ahead of the round trip: the last view
with the not-yet-acknowledged inputs replayed on it — only cursor moves, the
search field's focus and the filter text, and a tab switch as "loading"
(`web/src/lib/predict.ts`); everything else waits for the server. Inputs
without `seq` work as before.

```jsonc
// A key while the column has the keyboard. `key` is KeyboardEvent.key.
{ "type": "key", "key": "j", "ctrl": false, "alt": false, "shift": false }

// A click in the column, by id (view.rs `Click`).
{ "type": "click", "kind": "tab", "index": 1 }
{ "type": "click", "kind": "search" }
{ "type": "click", "kind": "task", "id": "acme/fix-login", "sub": "agent-id" }
{ "type": "click", "kind": "item", "pos": 3 }
// A form field, by its place in ⇥ order: it takes the focus.
{ "type": "click", "kind": "field", "index": 2 }

// A row's or the header's button: the list key it names, pressed with the list (not the
// search field) focused. Ignored outside list mode — forms and prompts get
// their buttons as plain `key` messages (Enter, Escape, y, space, ←/→).
// open | approve | deny | rename | edit_repos | close | pin | delete | unlock |
// transcript | next | new | quick | add_repo | new_workspace | help | reject
// (`quick` opens the quick task, ^⇧n: a prompt-only form on the adhoc workspace)
// (`reject` opens the reject form for the row's pending secrets requests —
// `:reject`, not `D`, which answers a permission prompt first)
{ "type": "action", "name": "rename" }

// An edit in one of the column's web forms (view.rs `FormOp`), applied to the
// open form (create, add repo, new workspace, edit repos, rename); ignored
// when no such form is open. `set` carries a text field's whole value (`name`,
// `prompt` — the only one that keeps line breaks —, `url`, `path`,
// `repo_url`, `title`); `check` sets (never toggles) a checkbox
// (`repo` by index, `skills`); `pick` chooses by index into the create form's
// `workspace_options` / `agent_options` (a new workspace reloads its repos, as
// ←/→ does). `submit` / `cancel` are the form's own ⏎ / esc, so they run the
// terminal's paths (jobs, errors, where it lands).
{ "type": "form", "op": "set", "field": "name", "value": "Rate limit login" }
{ "type": "form", "op": "check", "field": "repo", "index": 1, "on": false }
{ "type": "form", "op": "pick", "field": "workspace", "index": 0 }
{ "type": "form", "op": "submit" }

// The terminal's size in cells (xterm fit addon) → PTY resize. `width` /
// `height` (optional) are its size in CSS pixels, the pty's pixel size: tmux
// passes the cell size on to the panes, where an image program (image.nvim's
// Sixel output) sizes its pictures by it. xterm's image addon draws one
// Sixel pixel per CSS pixel.
{ "type": "resize", "cols": 120, "rows": 40, "width": 1010, "height": 748 }

// The page's whole width in cells, for tenx_core::column::width → `layout`.
{ "type": "viewport", "cols": 180 }

// Keyboard focus moved: into the column (select_current) or out (blur).
{ "type": "focus", "column": true }

// The tab became visible / the window regained focus → sweep, as the TUI does
// on FocusGained.
{ "type": "visible" }

// The next terminal bytes are a paste: the pane leaves copy-mode first (a
// touch scroll leaves it there, and tmux drops a paste that reaches it).
{ "type": "paste" }
```

## Keys the browser keeps

The browser, not the server, owns focus and visibility, like `src/tui/client.rs`:

- `Ctrl+w` (and `Alt+w` off macOS) cycles: terminal focused → column focused →
  column hidden → column shown and focused. Under the `narrow` layout the
  column is an overlay over the terminal. A view whose `mode` is a form or
  the help (anything but `list` and `links`) shows a hidden column while it
  lasts, so `+` over a phone's terminal opens a form you can see.
- Everything else goes to whichever side has focus: the column as `key`
  messages, the terminal as binary input.
- In the column's `links` mode (`L`) a digit, Enter, `o`, `l` or `y` name a
  link: the page opens it (or copies it, `y`) itself, inside the key press — a browser opens a window
  only then — and sends `Escape` instead of the key (`lib/links.ts`
  `linkKey`). Other keys (moving, Escape) go to the server as usual.
- `request` messages from the server apply `focus_terminal` (and hide the
  column when narrow), `focus_column` (show it and focus it), `hide`,
  `quit` (= hide; the tab stays open), and `sidebar` (show or hide the
  sidebar: a panel right of the terminal while the task keeps 80 columns
  beside it — the terminal client's rule, `web/src/lib/sidebarfit.ts` —,
  remembered per browser; else a drawer over it from the right. Below
  1000 px the task list is a drawer from the left too). The sidebar's
  width is the page's own: dragging its left edge sets it (not on a
  phone), kept in `localStorage` (`tenx-sidebar-width`) and never sent to
  the server; the 80-column rule uses it. The task list's right edge is
  dragged the same way: whole columns, at most half the page, kept as
  `tenx-column-cols`; while one is kept it wins over `layout`'s
  `column_cols`. The server doesn't move the column's
  focus itself: the page reports every change with a `focus` message.

The terminal attaches on the first `resize`, so send one as soon as xterm.js
is fitted — until then there is no terminal output.

Everything is also reachable by pointer. A header runs across the page: `☰`
/ `⟨` shows and hides the column (the same as `^w`, and how a phone opens its
overlay), then the current task (`view.current` — glyph, title, workspace,
its reason chip; tapping it opens the column on that task with a `task`
click), then `● next`, `+`, the notifications bell, the connection and `?`.
A task's rows carry no link chips (only `run`, a scheduled run's ⏰ chip);
`links` is everything the
task links to: the header's ↗ (the current task's; inside ⋯ on the narrowest
phones) and the column's `links` mode show it as a menu, or a bottom sheet on
touch, each link with a copy button.
In the column, a click selects a row, a double click opens it. A row's controls send a
`task` click (select it) then an `action`: allow / deny on a blocked row's
chip (`answerable`), and the row's menu — `⋯`, a right-click, or on touch a
long-press (a bottom sheet) — for open, unlock (`locked`), links (`links`), rename, edit
repos, close, pin / unpin (`pinned`, also a 📌 on the row) and delete; on touch a swipe uncovers allow / deny (left) or
delete (right). The header's `+`, `● next` (shown when a task other than the
current one needs you) and `?` send a menu of `quick` / `new` (on Repos of
`add_repo` / `new_workspace`), `next` and `help`. The column's forms
(new task, new workspace, add repo, edit repos, rename) are native web forms —
a form owns the column (title, ✕, captioned sections, an action bar pinned to
the bottom): text inputs, chips that are radios / checkboxes underneath
(workspace, repos, agent — a native select past six workspaces), Tab order,
Enter submits, Escape cancels — sending `form` messages. The create form shows
the server's `slug` for the name and what `default` resolves to
(`agent_default`), and a `prompt` box (Shift+Enter for a new line); with a
prompt and no name, Claude titles the task and `slug` is empty. With `quick`
set it shows only the prompt. A refused submit shows the view's `status` under the field
it is about (else above the bar). A submit that starts a job keeps the form on
screen, frozen, with that job's progress from `view.jobs` (the one whose `id`
wasn't there before the submit) until it lands; esc leaves it running on the
Work tab. A delete asks on its row with `key` messages
(y, n). Tapping
something that takes typing focuses an off-screen input so a phone raises its
keyboard; what it types goes over as `key` messages.

## Push messages

What the service worker (`web/public/sw.js`) receives, encrypted (RFC 8291 `aes128gcm`, VAPID RFC 8292), and shows as a notification:

```jsonc
{ "title": "Fix login timeout", "body": "permission: Bash · acme", "tag": "acme/fix-login-timeout", "url": "/?task=acme/fix-login-timeout" }
```

Sent on the edges the attention watcher notifies on (a task going Blocked or Signaled, a secrets request), once per edge. `tag` replaces an older notification for the same task. A tap focuses an open page and posts it `{type: "open-task", task}`, or opens `url`; either way the page selects the task (`click`) and opens it (`action: open`).

The marker: `view.current` (and each task's `current`) is what the terminal
area shows (`Column::is_shown`) — the session's current window, or, while the
cursor rests on a task with no open window, that task. For the latter the
view also carries `shown_closed` (`{id, title, ws, ws_color}`), and the page
draws that task's empty screen over the terminal — its title, "no window
open", "⏎ open it here" — as the TUI's `render_closed` does; a click there
sends `action: open`. Tasks and windows are matched by task directory, never
by name alone (`snapshot::Windows::window_of`), and a tab's current window is
its own grouped session's (`tmux::current_window_id_in`).

Bell (`\a`) and OSC 52 are handled in the browser by xterm.js (title/favicon
flash, clipboard addon).

## MCP sessions and events

The push half of `/mcp` (`src/web/events.rs`, rules in `tenx_core::mcppush`),
for a client that holds only the URL and the MCP token — the voice assistant —
and should hear about tenx the moment something happens.

- **Session.** `initialize` answers with `Mcp-Session-Id` (128 random bits,
  hex). Later requests carry it; unknown or expired → `404`, and the client
  initializes again. `DELETE /mcp` ends it. A session with no open stream and
  no request for 10 minutes expires; at most 256 are held, a new one beyond
  that pushing out the one idle longest.
- **Stream.** `GET /mcp` with the session id: one SSE event per notification,
  `id:` counting up per session, and a `: ping` comment every 15 s:
  ```
  id: 7
  data: {"jsonrpc":"2.0","method":"notifications/tenx/event","params":{…}}

  ```
  One stream per session; a second `GET` replaces the first, which ends. A
  `GET` with `Last-Event-ID: <n>` first replays the session's buffered events
  after `n` (the last 200 are kept), then goes live; without it, only live ones.
- **`tenx_subscribe`** `{"events": [...]}` — any of `attention`, `turn`,
  `focus`, `reply`; `[]` for none. Replaces the earlier subscription; a session
  starts with none. Without a session it is a tool error ("needs an MCP session
  (Mcp-Session-Id)").
- **`tenx_send_message`**'s result gains a last line `message_id: m<n>` (per
  server, counting up) and `structuredContent: {"message_id": "m<n>"}` — for
  every caller; only a session gets the `reply` for it.
- **`tenx_current_task`** `{}` or `{"screen": "terminal" | "web:<tab id>"}`:
  the task on that screen (default: the screen that changed last, else the
  terminal), `{screen, workspace, task, title}` or `{screen, task: null}`.

Every event's `params` has `kind`, `at` (RFC 3339, UTC) and, for a task,
`workspace`, `task` (slug) and `title`:

| kind | when | extra fields |
|---|---|---|
| `attention` | a task's edge into needing you, or into pending secrets: the notifier's `cli::watch::Attention` edges — the very ones the phone gets a push for | `status` (`blocked`, `signaled`, `secrets`), `reason` (may be empty) |
| `turn` | a task's agent finished a turn: seen `working` and now `done`, or `done` again with a later status time (a turn between two 2 s polls); not through `blocked`, not a first sight | `reply`: the turn's replies as `tenx_get_output` reads them, joined by a blank line, cut to 4000 characters |
| `reply` | the first finished turn of a task after this session's `tenx_send_message` (one per session; several messages before it are answered with the last one's id). The `turn` goes out too, to whoever subscribed | `message_id`, `reply` |
| `focus` | the task on a screen changed: a `tenx web` tab's `view.current` (its driver reports it), or the tenx session's current window (looked at every 500 ms) | `screen` (`web:<tab session id>` or `terminal`) and the task fields — or `task: null` with no workspace or title |

```json
{"kind":"reply","at":"2026-11-04T11:49:37Z","workspace":"adhoc","task":"mcp-push-probe","title":"mcp push probe","message_id":"m1","reply":"pong"}
{"kind":"focus","at":"2026-11-04T11:50:22Z","workspace":"adhoc","task":"mcp-push-probe","title":"mcp push probe","screen":"web:f6f0c99a9b9c61cd"}
```

`TENX_WEB_NO_PUSH=1` makes a `tenx web` compute every edge (and these events)
but deliver no Web Push — for a second instance run to test with, which would
otherwise send the phone every notification twice.

## Rejecting a credential request

A task row carries `wants: [{name, why}]` — its pending secrets requests and
the reason the agent gave (`need --why`, empty when none). `action: reject`
(the row menu's "reject request…", the touch sheet's) opens `mode.kind =
"reject"`: `{task, id, names: [{name, why}], note}`. The page edits the note
with `form` `set` (`field: "note"`) and ends it with `submit` (every pending
name is denied — the waiting agent is told no, with the note; not a
withdrawal) or `cancel`. The outcome is the column's status message
("rejected A, B for '<slug>'", "'<slug>' had nothing pending any more", or
the error). `D` and `:reject` reach the same mode from the keyboard.
