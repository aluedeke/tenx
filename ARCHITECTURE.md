# tenx architecture

A map of the code for someone getting oriented. `CLAUDE.md` covers the same ground in the form an agent editing the repo needs; this document is the human version and stays at a higher altitude.

## Shape

Two crates in one Cargo workspace:

- **`tenx-core/`** holds pure logic: no `std::process::Command`, no filesystem access beyond data a caller hands in. Everything tenx *decides* lives here, as functions over plain values, with unit tests. Modules: `status` (what a task's state is), `dialog` (recognising and answering a permission prompt), `slug`, `time`, `sweep`, `taskmd` (`TASK.md` rendering and parsing), `live` (parsing the per-task cache of ports and PRs).
- **`tenx`** (the root package) is the binary. It does I/O: shells out to `git`, `tmux`, `gh`, `age` and `sops`, reads Claude Code's session registry, and renders the overlay.

Inside the binary the layers are independent and wired together by the CLI dispatch in `main.rs`. Each layer only knows about the layers below it.

```
cli/        argument parsing and one file per subcommand group
tui/        the ratatui overlay
tmux/       the session layer: server, config, windows, bell flags
workspace/  the on-disk model: workspaces, tasks, registry, Claude's session registry
git/        worktree and bare-repo operations
live.rs     the per-task cache of external facts (ports, PRs)
```

## On disk

A **workspace** is a directory with a `config.toml` and a `tasks/` subdirectory. `workspace::find()` walks up from the current directory looking for the config. Bare clones live under `<workspace>/.bare/<name>.git` by default.

A **task** is a subdirectory under `tasks/`. Its repo set is never recorded: `discover_task` scans for subdirectories containing a `.git` *file*, which is what a worktree has. The task's display name is the first heading of `TASK.md`; its slug is the directory, branch and tmux window name.

The only per-task files tenx owns:

| File | Role |
|---|---|
| `TASK.md` | The task's notes. Rendered on creation, heading rewritten on rename. |
| `.tenx-window-id` | Cache of the tmux window id. A fast path only; the window is always looked up by slug. |
| `.tenx-pinned` | Marker that exempts the task from sweep. |
| `.tenx-live.json` | Cache of ports and PR facts, written only by the watcher. |
| `.secrets-pending`, `.secrets-pending-set` | Queues of secret requests waiting for a human. |

User-level state lives under `~/.config/tenx/`: a global `config.toml`, the generated `tmux.conf`, the watcher's pid file, and `workspaces.d/`, a registry with one file per workspace so the overlay can find them all. Registration is one atomic file write.

## The session layer

tenx runs its own tmux server on a dedicated socket (`tmux -L tenx`) against a config it generates. Your `~/.tmux.conf` is never read. The config carries the theme, hides the window list in favour of per-window `@tenx_status` options, turns on `monitor-bell`, and binds `Ctrl+w` to a `display-popup` running `tenx overlay`.

Windows are tasks. `open_task_window` builds the default layout with `new-window` and `split-window`, or runs the workspace's layout script with the task's paths in the environment. `attach_or_create` execs `new-session -A` with window 0 running `tenx overlay --home` in a restart loop.

tmux reads its config once at server start. After changing the generated config, kill the server and run `tenx` again.

## Task state

Status resolution is in `tenx_core::status`. Its inputs are Claude Code's session registry (`~/.claude/sessions/<pid>.json`, checked against live pids and scoped to sessions whose pid descends from a pane of the tenx server) and the bell flag of the task's window. The rules, in order:

1. Any session `waiting` on a prompt or permission: **Blocked**, with Claude's reason.
2. The window's bell flag is set: **Signaled**. Any process can raise it with `printf '\a'`; tmux clears it when the window is visited.
3. Any session `busy`: **Working**.
4. A live but quiet session: **Done**.
5. Nothing: **Idle**.

Blocked and Signaled are the "needs you" states. The watcher notifies on the edge into them, and sweep never closes them. There is deliberately no failed state, because the registry cannot distinguish a failed turn from a finished one.

## The watcher

`tenx watch` is a detached, single-instance process started by every route into the session. It polls every two seconds and does one resolve pass per tick for three consumers:

- A desktop notification on the edge into Blocked, debounced. Tasks already waiting at start are treated as already notified. Done never notifies, since it fires after every turn.
- The status bar: `@tenx_status` per task window (glyph, lock when secrets are pending, PR and port chips) and the global right corner, which counts tasks needing you other than the current one. Done is gated on sixty seconds unanswered so the corner does not flicker. Nothing is sent when nothing changed.
- A log pane for each background agent, once per working directory and pid, running `tenx internal agent-log`, which follows the agent's transcript and exits with it.

The watcher also refreshes `.tenx-live.json`: ports every tick, PRs staggered on a helper thread with a time-to-live that lengthens for parked tasks. It exits when the tmux server is gone.

## The overlay

`tui/overlay.rs` is the single overlay implementation. It lists every task from every registered workspace, sectioned by attention group, fuzzy-filtered, with a search field in insert mode and a list in normal mode. Actions call straight into the `cli::task` and `cli::repo` functions rather than duplicating their logic.

It runs in two modes. *Home* is window 0 of the session: a jump selects the task's window and the overlay stays; quit keys are swallowed. *Popup* is the `Ctrl+w` instance: a jump exits, and tmux closes the popup. Run from a plain terminal outside tmux, a jump records the target and the process attaches after teardown.

Next to the list, a preview panel shows the selected task's Claude pane: `tmux capture-pane -e` on the pane the session registry names, refreshed on every tick. It is read-only and per client, which is why the overlay previews instead of switching the real window under itself: the current window is per session, so switching it would move every attached client, and merely visiting a window clears its bell flag.

A blocked task's permission prompt can be answered from the list with `y` or `N`. Claude Code has no API for this, so the answer is a keystroke sent into the pane, guarded twice right before sending: the registry must still say the session waits on a permission prompt (not a question, which `Enter` would answer wrongly), and the captured pane must still show the dialog. The check is `tenx_core::dialog`.

## Task lifecycle

**Create** slugifies the title, writes `TASK.md`, symlinks `.claude` to the workspace's shared one, then for each repo fetches the bare clone and adds a worktree on a fresh branch off the default branch. Then, if the server is running, it opens the window.

**Open** looks the window up by slug and selects it, or creates it. `--continue` is passed to Claude only when a transcript for that exact directory exists, because Claude exits when asked to continue a conversation that does not exist.

**Repo changes** share one function with creation. Detaching removes the worktree and its branch and does not force unless asked, so git's refusal to drop a dirty worktree is the safety net.

**Sweep** closes windows nobody is waiting on. The rule is a pure function in `tenx_core::sweep`: never the current window, a pinned task, or a Blocked or Working task; Idle immediately; Done after the configured age.

**Remove** asks for confirmation, then deletes each worktree and its branch and the task directory.

## Secrets

`cli::secrets` gives a task credentials through the system `age` and `sops`. The module's doc comment is the authoritative description of its invariants; this is the overview.

**Identity.** The workspace config's `age_identity` wins if set. Otherwise the standard locations `sops` and `age` already use are checked: `$SOPS_AGE_KEY_FILE`, `~/.config/sops/age`, `~/.config/age`. Failing that, `init` generates a passphrase-protected identity with `age-keygen` and `age -p`. The passphrase is the whole confirmation gate: `age` has no daemon and no cache, so every decrypt asks for it.

**A task's own bundle** is sealed at `tasks/<slug>/.secrets.enc.env` and released to `.secrets.env` beside it, mode 0600. The task directory is not a git repository, so nothing there needs ignoring.

**Adopted secrets** are a repo's own pre-existing sops setup: a `.sops.yaml` plus `*.enc.*` files sealed by that project's tooling. They are decrypted to `.secrets-adopted/` under the task directory, outside every worktree, and symlinked into place so the plaintext never sits inside a git checkout.

**Agent safety.** No function in the module writes a decrypted value to stdout, because an agent's captured stdout becomes a transcript that outlives the task. `decrypt` and `set` check whether `/dev/tty` is reachable, the same file `age`'s own prompt reads. When it is, they act. When it is not, which is the case inside an agent's shell tool, each appends its request to a queue (a name only: a queued *value* would mean plaintext on disk before any human confirmed anything) and then blocks until a human acts on it, polling the queue once a second for up to `--timeout`. Whether the name left the queue because it was fulfilled or withdrawn (`cancel`) is decided from the disk alone, `tenx_core::secrets::wait_outcome`: an output file modified at or after the request means fulfilled. Removal from the queue is the commit point, since every fulfilment writes its output first. A timed-out request stays queued so re-running resumes the wait; `--no-wait` restores fire-and-forget.

## Testing

Unit tests live in `tenx-core` next to the logic they cover, plus a few in the binary for parsing and platform detection. `tests/e2e_tmux.rs` drives a throwaway tmux server on its own socket to exercise task creation, the three-pane window layout, open and list end to end. CI runs all of it on macOS and Ubuntu with clippy at `-D warnings`.
