# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`tenx` is a CLI + TUI tool (Rust) that manages multi-repo "workspaces" and "tasks". A workspace holds one or more bare git repos; a task is a named unit of work that creates a git worktree (on a branch matching the task name) in each of the workspace's repos, then opens a zellij session/tab with an editor+shell layout for that task. It's essentially a wrapper around bare-repo + worktree git workflows, orchestrated through zellij tabs/sessions.

## Commands

```sh
cargo build --release   # build (Makefile `build` target does this)
cargo build              # debug build, faster iteration
cargo run -- <args>       # run without installing, e.g. `cargo run -- task list`
cargo check               # fast type-check without producing a binary
make install              # builds release and installs to $PREFIX/bin (default ~/.local/bin)
```

There is no test suite in this repo currently (no `#[test]` items). There is no separate lint config beyond default `cargo` — use `cargo clippy` if checking lint issues.

## Architecture

The codebase is organized as independent layers that `main.rs` wires together via CLI dispatch. Each layer only knows about the layer(s) below it:

- **`cli/`** — clap-based argument parsing (`mod.rs`) plus one file per subcommand group (`init.rs`, `repo.rs`, `task.rs`). These functions are the actual command implementations — `main.rs` just matches on the parsed `Commands` enum and calls into them. Note that the TUI (`tui/app.rs`) calls directly into `cli::task::{new,open,rm}` to reuse this logic instead of duplicating it.
- **`workspace/`** — the on-disk model. A *workspace* is a directory with a `config.toml` (name, layout path, list of repos) and a `tasks/` subdirectory; each subdirectory under `tasks/` is a *task*, discovered by scanning for child directories containing a `.git` file (i.e. worktrees). `workspace::find()` walks up from cwd looking for `config.toml`, so commands work from any subdirectory of a workspace. Global user config lives at `~/.config/tenx/config.toml` (currently just `bare_dir` override). Config writes are atomic (write to `.toml.tmp`, then rename).
- **`git/`** — thin wrapper around `git2` (bare clone, fetch) and shelling out to the `git` CLI for worktree add/remove (git2 doesn't support worktree ops well). Bare repos live under `<workspace>/../.bare/<name>.git` by default, or under the global `bare_dir` override if set.
- **`zellij/`** — shells out to the `zellij` CLI to manage sessions and tabs. Session names are derived from workspace names (`tenx-<slugified-name>`). Layouts are KDL strings — either a built-in default (editor+shell split) or a user-supplied layout file (with `{name}`/`{cwd}` placeholders substituted). All zellij action commands (`list-tabs`, `go-to-tab-name`, `new-tab`, etc.) require being run from inside a zellij session (`is_inside_session()` checks `$ZELLIJ`); creating/attaching a *session* (`zellij --session ...`, `zellij attach ...`) uses `exec()` to replace the current process since zellij takes over the terminal.
- **`tui/`** — ratatui/crossterm terminal UI, invoked via `tenx tui` (this is what the zellij session layout launches as its first tab). `app.rs` holds all state and mutations (`App` struct, `View::List`/`View::Create`), `ui.rs` is pure rendering, `mod.rs` wires the event loop and key handling together. The TUI lists tasks, shows which have open zellij tabs, and supports opening/closing tabs, creating new tasks (with per-repo checkboxes), and deleting tasks (with a confirm prompt) — all via the same `cli::task` functions the CLI uses.

## Key flows to understand together

- **No-argument invocation** (`tenx` with no subcommand, in `main.rs`): finds the enclosing workspace from cwd, determines the expected zellij session name, and either switches to the TUI tab (if already in that session), errors out (if inside a *different* session — to avoid clobbering it), or creates/attaches the session (if outside any session). This is the main entry point for daily use.
- **Task creation** (`cli::task::new`): fetches each selected repo's bare remote, then runs `git worktree add` per repo into `tasks/<name>/<repo>/`, branching off `origin/main` if the branch doesn't exist yet (reusing the existing branch otherwise). Then opens a zellij tab via the workspace's configured layout (or the built-in default) unless `--no-open` is passed.
- Task discovery (`workspace::discover_task`) infers a task's branch name by reading the `.git` worktree-link file of its *first* discovered repo subdirectory and following it to the bare repo's `HEAD` — it does not read this from any tenx-owned metadata file, so task branch names are always derived from git state directly.
