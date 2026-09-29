# tenx web protocol

`tenx web` serves the column as a web page next to an xterm.js terminal. The
Rust server owns the column (one `Column` per connection, the same state
machine as the TUI); the browser only renders the `ColumnView` it is sent and
forwards keys and clicks. This file is the contract between `src/web/` and
`web/`; change both sides together.

## HTTP

| Route | Auth | What |
|---|---|---|
| `GET /?token=<t>` | token | Sets the `tenx_web` cookie (HttpOnly, SameSite=Strict, Path=/) and redirects `303` to `/` without the query. A wrong token gets `401`. |
| `GET /<asset>` | cookie | The embedded static export of `web/` (`web/out`), `index.html` for `/`. Without the cookie: `401` with a short page telling you to open the URL `tenx web` printed. |
| `GET /ws` | cookie + Origin | Upgrades to the WebSocket below. The `Origin` header must equal `http(s)://<Host>` or an origin passed with `--dev-origin`. |
| `GET /push/key` | cookie | `{"key": …}`: the server's VAPID public key (base64url), the `applicationServerKey` a browser subscribes with. A same-origin GET carries no `Origin`, so only the cookie is checked. |
| `POST /push/subscribe` | cookie + Origin | A `PushSubscription` as `toJSON()` gives it (`{endpoint, keys: {p256dh, auth}}`). Stored in `~/.config/tenx/web-push-subs.json` (600), one per endpoint. `400` for an endpoint that isn't HTTPS (plain HTTP only to loopback, for tests) or keys that aren't a P-256 point and a secret. |
| `POST /push/unsubscribe` | cookie + Origin | `{endpoint}`: forget that subscription. |
| `POST /push/test` | cookie + Origin | Push a test message to every subscription: `{sent, subscriptions}`. |
| `GET /manifest.webmanifest` | — | Public (browsers fetch it without cookies). With the cookie — the page links it `crossorigin="use-credentials"` — its `start_url` is `/?token=…`, so an app installed to an iOS Home Screen, which has cookies of its own, signs itself in on first launch. |
| `POST /paste` | cookie + Origin | An image (`Content-Type` PNG, JPEG, GIF or WebP; at most 25 MB) saved to `~/.config/tenx/web-paste/` (600, swept after a day). Answers `{"path": …}`; the page pastes that path into the terminal, which Claude Code attaches as an image. `415` for anything else. |

The token lives in `~/.config/tenx/web-token` (mode 600), created on first
start, replaced by `tenx web --rotate-token`. `--dev-origin
http://localhost:3000` lets `next dev` on another port connect; in dev the
page passes the token as `?token=` on the WebSocket URL instead of the cookie
(accepted only when the Origin is a `--dev-origin`).

## WebSocket `/ws?session=<id>`

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
{ "type": "view", "view": ColumnView }   // src/tui/column/view.rs, serialized

// The column's layout for the browser's width in cells (reply to `viewport`).
{ "type": "layout", "column_cols": 36, "narrow": false }

// What the TUI client would do with a ClientRequest — and `focus_column`,
// when an unlock popup has closed and the column should come back.
{ "type": "request", "request": "focus_terminal" | "focus_column" | "hide" | "quit" }

{ "type": "error", "message": "…" }
```

### Browser → server

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
// open | approve | deny | rename | edit_repos | close | delete | unlock |
// transcript | next | new | add_repo | new_workspace | help
{ "type": "action", "name": "rename" }

// An edit in one of the column's web forms (view.rs `FormOp`), applied to the
// open form (create, add repo, new workspace, edit repos, rename); ignored
// when no such form is open. `set` carries a text field's whole value (`name`,
// `url`, `path`, `repo_url`, `title`); `check` sets (never toggles) a checkbox
// (`repo` by index, `skills`); `pick` chooses by index into the create form's
// `workspace_options` / `agent_options` (a new workspace reloads its repos, as
// ←/→ does). `submit` / `cancel` are the form's own ⏎ / esc, so they run the
// terminal's paths (jobs, errors, where it lands).
{ "type": "form", "op": "set", "field": "name", "value": "Rate limit login" }
{ "type": "form", "op": "check", "field": "repo", "index": 1, "on": false }
{ "type": "form", "op": "pick", "field": "workspace", "index": 0 }
{ "type": "form", "op": "submit" }

// The terminal's size in cells (xterm fit addon) → PTY resize.
{ "type": "resize", "cols": 120, "rows": 40 }

// The page's whole width in cells, for tenx_core::column::width → `layout`.
{ "type": "viewport", "cols": 180 }

// Keyboard focus moved: into the column (select_current) or out (blur).
{ "type": "focus", "column": true }

// The tab became visible / the window regained focus → sweep, as the TUI does
// on FocusGained.
{ "type": "visible" }
```

## Keys the browser keeps

The browser, not the server, owns focus and visibility, like `src/tui/client.rs`:

- `Ctrl+w` (and `Alt+w` off macOS) cycles: terminal focused → column focused →
  column hidden → column shown and focused. Under the `narrow` layout the
  column is an overlay over the terminal.
- Everything else goes to whichever side has focus: the column as `key`
  messages, the terminal as binary input.
- `request` messages from the server apply `focus_terminal` (and hide the
  column when narrow), `focus_column` (show it and focus it), `hide`, and
  `quit` (= hide; the tab stays open). The server doesn't move the column's
  focus itself: the page reports every change with a `focus` message.

The terminal attaches on the first `resize`, so send one as soon as xterm.js
is fitted — until then there is no terminal output.

Everything is also reachable by pointer. A header runs across the page: `☰`
/ `⟨` shows and hides the column (the same as `^w`, and how a phone opens its
overlay), then the current task (`view.current` — glyph, title, workspace,
its reason chip; tapping it opens the column on that task with a `task`
click), then `● next`, `+`, the notifications bell, the connection and `?`.
In the column, a click selects a row, a double click opens it. A row's controls send a
`task` click (select it) then an `action`: allow / deny on a blocked row's
chip (`answerable`), and the row's menu — `⋯`, a right-click, or on touch a
long-press (a bottom sheet) — for open, unlock (`locked`), rename, edit
repos, close and delete; on touch a swipe uncovers allow / deny (left) or
delete (right). The header's `+`, `● next` (shown when a task other than the
current one needs you) and `?` send `new` (on Repos a menu of `add_repo` /
`new_workspace`), `next` and `help`. The column's forms
(new task, new workspace, add repo, edit repos, rename) are native web forms —
text inputs, a workspace select, agent radios, checkboxes, Tab order, Enter
submits, Escape cancels — sending `form` messages; a refused submit shows the
view's `status` inside the form. A delete asks on its row with `key` messages
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
