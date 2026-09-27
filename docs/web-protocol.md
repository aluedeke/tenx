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

The token lives in `~/.config/tenx/web-token` (mode 600), created on first
start, replaced by `tenx web --rotate-token`. `--dev-origin
http://localhost:3000` lets `next dev` on another port connect; in dev the
page passes the token as `?token=` on the WebSocket URL instead of the cookie
(accepted only when the Origin is a `--dev-origin`).

## WebSocket `/ws?session=<id>`

`session` is optional: the id of a grouped session this tab had before (kept
in `sessionStorage`). If `tenx-web-<id>` still exists (within the 30 s grace
period after a disconnect) the connection re-attaches to it; otherwise the
server makes a new one, grouped with `tenx`, on `tenx`'s current window.

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

// What the TUI client would do with a ClientRequest.
{ "type": "request", "request": "focus_terminal" | "hide" | "quit" }

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
  column when narrow), `hide`, and `quit` (= hide; the tab stays open).

Bell (`\a`) and OSC 52 are handled in the browser by xterm.js (title/favicon
flash, clipboard addon).
