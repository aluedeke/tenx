# Changelog

All notable changes to this project are documented in this file. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

First public release candidate.

### Added

- Workspaces: a directory of bare git clones plus a `tasks/` folder, registered globally so the overlay sees every workspace.
- Tasks: one branch and worktree per repo, a `TASK.md`, and a tmux window with Claude Code, an editor and a shell. Repos can be added to or detached from a task after creation.
- A single tmux session on a dedicated socket with a generated config; the user's own tmux config is untouched.
- The overlay: every task across every workspace, grouped by attention, fuzzy-filtered, with keys for jump, new, rename, edit repos, close, unlock secrets and delete. Bound to `Ctrl+w` and running permanently in window 0.
- Live task state from Claude Code's session registry and tmux's bell flag: Blocked, Signaled, Working, Done, Idle. No hooks installed.
- The attention watcher: desktop notifications when a task starts waiting, per-window status in the tmux status bar, PR and listening-port chips, and a log pane for background agents.
- Sweep and pin: close windows nobody is waiting on, keep conversations resumable.
- `tenx secrets`: per-task credentials via `age` and `sops`, safe to call from an agent, values never written to stdout. Adopts repos that already use sops.
- `tenx standup`: a summary of recent activity across tasks, with a `/standup` skill to format it.
- `/tenx` skill for Claude Code sessions, installed by `tenx init`.
- `--ws-dir` on every mutating command and `tenx overlay --json` for scripts and other front ends.
- CI on macOS and Ubuntu with an end-to-end test against a throwaway tmux server.
- Logo: the Lanes mark (four task bars, one stopped with an amber dot), lockup and favicons under `docs/logo/`.
- Desktop notifications carry the mark as their icon (`terminal-notifier` and `notify-send`), and the overlay's first-run screen draws it in text next to the wordmark.

[Unreleased]: https://github.com/aluedeke/tenx/commits/main
