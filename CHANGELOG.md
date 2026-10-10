# Changelog

All notable changes to this project are documented here. Sections are generated
from commit messages by [git-cliff](https://git-cliff.org) when a release is cut
(`scripts/release.sh`, see `cliff.toml`); nothing here is written by hand.
Versions follow [Semantic Versioning](https://semver.org).

## [0.5.0] - 2026-10-10

### Added

- **web:** Dictate into the terminal with tenx-whisper

[0.5.0]: https://github.com/aluedeke/tenx/compare/v0.4.0...v0.5.0

## [0.4.0] - 2026-10-09

### Added

- **pr:** Wake agents on PR feedback with `tenx pr wait`
- **pr:** Resume `tenx pr wait` from the last point the agent handled
- **pr:** Show running PR waits in the column and keep their windows

[0.4.0]: https://github.com/aluedeke/tenx/compare/v0.3.3...v0.4.0

## [0.3.3] - 2026-10-09

### Fixed

- **web:** Copy from a non-Claude pane reaches the browser clipboard
- **tmux:** Let pane programs copy to the attached terminal's clipboard

[0.3.3]: https://github.com/aluedeke/tenx/compare/v0.3.2...v0.3.3

## [0.3.2] - 2026-10-07

### Fixed

- **init:** Refuse to nest a workspace in a directory of its name
- **workspace:** Re-register a moved workspace from commands inside it

[0.3.2]: https://github.com/aluedeke/tenx/compare/v0.3.1...v0.3.2

## [0.3.1] - 2026-10-07

### Fixed

- **web:** Type a dead key's composed character once

[0.3.1]: https://github.com/aluedeke/tenx/compare/v0.3.0...v0.3.1

## [0.3.0] - 2026-10-07

### Added

- **task:** Rename detached sessions to adhoc sessions (breaking)

[0.3.0]: https://github.com/aluedeke/tenx/compare/v0.2.3...v0.3.0

## [0.2.3] - 2026-10-06

### Fixed

- **web:** Paste from the key bar on iOS, where a held tap fires no click

[0.2.3]: https://github.com/aluedeke/tenx/compare/v0.2.1...v0.2.3

## [0.2.1] - 2026-10-05

### Fixed

- **web:** Keep a page in use signed in by renewing its cookie

[0.2.1]: https://github.com/aluedeke/tenx/compare/v0.2.0...v0.2.1

## [0.2.0] - 2026-10-05

### Added

- **column:** Show a task's subagents with live status
- **column:** Open a Claude subagent in Claude's own agent view
- **column:** Open subagent transcripts in a tmux window
- **column:** Switch between a task's agents as you move through them
- **column:** Mark the current task with a gutter bar
- **column:** Reject pending secrets requests with D
- **task:** Add sessions outside a repo and task send/wait/output
- **web:** Serve the column and the session to a browser with tenx web
- **web:** The page tenx web serves — the column beside the session
- **web:** Share one Work tab across every browser tab
- **web:** Drive the whole page with a mouse or touch
- **web:** Tap in the text you're editing to move the cursor there
- **web:** Paste, drop or pick images for the agent
- **web:** Paste the phone's clipboard, images included
- **web:** Encrypt and sign Web Push messages
- **web:** Install the page as an app and push when a task needs you
- **web:** Put each action on the row it acts on
- **web:** Fold the key bar away when a hardware keyboard types
- **web:** Scroll the terminal's history with a finger
- **web:** Run tenx web as a login service
- **web:** Restart the tenx web service onto a new binary
- **web:** Pick the terminal's font size and weight per device
- **web:** Draw the terminal in 12 px JetBrains Mono Light on desktops
- **web:** Draw the column in the terminal's font size and weight
- **web:** Put a header across the page with the task you're in
- **web:** Show the special keys only while the on-screen keyboard is up
- **web:** Drop the floating esc and paste keys when no keyboard is up
- **web:** Mark the current task with a gutter bar, as the TUI does
- **web:** Make the column's forms real web forms
- **web:** Show a closed task's empty screen and mark what's shown
- **web:** Give the header more room on desktops
- **web:** Give the column's forms sections, chips and an action bar
- **web:** Reject a task's pending credential requests from the web
- **web:** Open terminal links in the system browser, not in the app
- **web:** Move the column's cursor before the server answers
- **web:** List detached sessions and create tasks without repos
- **web:** Log what the paste key finds, for debugging on a phone

### Fixed

- **column:** Stop listing Claude Code's internal helpers as subagents
- **column:** Keep a subagent running while it waits on its own work
- **column:** Keep switching to a subagent once Claude summarizes it
- **column:** Drop finished subagents when Claude does
- **column:** Show a paused background subagent as running again
- **column:** Tell apart same-named tasks in different workspaces
- **column:** Move the current-task marker onto a shown closed task
- **git:** Base new tasks on the default branch as last fetched
- **agent:** Resume Claude in task paths with dots or underscores
- **web:** Serve the manifest and icons without the cookie
- **web:** Keep the keyboard up when pasting on a phone
- **web:** Load the service reliably when replacing or restarting it
- **web:** Draw bold terminal text in the real bold face
- **web:** Keep the on-screen keyboard up when tapping the key bar
- **web:** Type dictation and text suggestions once, not per word
- **web:** Keep the whole screen after switching apps on an iPad
- **web:** Reopen the app on the task it was showing
- **web:** Show a web form's title inside its frame
- **web:** Scroll the task list to keep the selection in view
- **web:** Answer a secrets request from its notification, keyboard up
- **web:** Stop handing iOS links to Safari with an invalid address
- **web:** Paste into the terminal once, not once per handler
- **column:** Show a secrets request that arrives while the list is open
- **web:** Show the key bar's paste key as paste, not as a clipboard
- **web:** Paste copied text and page selections from the key bar

### Changed

- **column:** Read the task list through a shared snapshot module
- **column:** Describe the column as data for other front ends
- **tmux:** Let a column follow a session grouped with tenx
- **watch:** Decide notification edges in tenx-core

### Documentation

- **web:** The protocol between tenx web and its page

[0.2.0]: https://github.com/aluedeke/tenx/compare/v0.1.0...v0.2.0

## [0.1.0] - 2026-09-05

First public release.

### Added

- Workspaces: a directory of bare git clones plus a `tasks/` folder, registered globally so the column sees every workspace.
- Tasks: one branch and worktree per repo, a `TASK.md`, and a tmux window with Claude Code, an editor and a shell. Repos can be added to or detached from a task after creation.
- A single tmux session on a dedicated socket with a generated config; the user's own tmux config is untouched.
- The column: every task across every workspace, grouped by attention, fuzzy-filtered, with keys for open, new, rename, edit repos, close, unlock secrets and delete. `tenx` draws it beside the session it embeds, in one process that owns the terminal; `Ctrl+w` moves between the two.
- Live task state from Claude Code's session registry and tmux's bell flag: Blocked, Signaled, Working, Done, Idle. No hooks installed.
- The attention watcher: desktop notifications when a task starts waiting, per-window status in the tmux status bar, PR and listening-port chips, and a log pane for background agents.
- Sweep and pin: close windows nobody is waiting on, keep conversations resumable.
- `tenx secrets`: per-task credentials via `age` and `sops`, safe to call from an agent, values never written to stdout. Adopts repos that already use sops.
- `tenx secrets decrypt`/`set` from an agent block until a human fulfils the request (`--timeout`, default 100 s; `--no-wait` to enqueue and return), so the agent resumes the moment a secret lands. `tenx secrets cancel <name>|--all` and the overlay's `:cancel` withdraw a request; a waiting agent is told it was withdrawn.
- `tenx standup`: a summary of recent activity across tasks, with a `/standup` skill to format it.
- `/tenx` skill for Claude Code sessions, installed by `tenx init`.
- `--ws-dir` on every mutating command and `tenx task list --json` for scripts and other front ends.
- CI on macOS and Ubuntu with an end-to-end test against a throwaway tmux server.
- Logo: the Lanes mark (four task bars, one stopped with an amber dot), lockup and favicons under `docs/logo/`.
- An animated README demo and an asciinema cast, generated by playing a scripted scene against fixture data through the real widgets (`make demo`), never recorded.
- Desktop notifications carry the mark as their icon (`terminal-notifier` and `notify-send`), and the column's first-run screen draws it in text next to the wordmark.
- Releases via cargo-dist: static binaries for macOS and Linux (Intel and ARM), a shell installer, and a Homebrew formula in `aluedeke/homebrew-tap` that depends on tmux. `make release auto` cuts one, with the version and this changelog derived from the commits.
- `tenx` says when the running session was started by an older tenx and needs a restart to pick up the new binary.
- **task:** Pre-approve Claude Code's trust dialog for new tasks
- **core:** Agent-agnostic session events, transcripts, and codex parsing
- **agents:** Uniform session registry fed by each agent's hooks/extension
- **standup:** Include Codex and pi activity
- **init:** Portable skills and AGENTS.md for Codex and pi
- **agents:** Global default agent and column :agent, with full docs
- **tui:** Pick the agent in the new-task form
- **column:** N jumps to the next task that needs you, A/D answer (breaking)
- **column:** Create a workspace from the column with W or :init
- **column:** Pick the workspace in the new-task form
- **column:** Live clone progress, off the UI thread, on a Work tab
- **column:** A new task opens without taking over your terminal
- **column:** List every key with ? or :help
- **secrets:** Ask with `need --why`, answer all at once, one passphrase
- **init:** Keep installed skills current in every workspace
- **column:** Answer secrets in a popup that keeps the column live
- **column:** Draw the secrets popup in the column's colours
- **client:** Make OSC 8 hyperlinks in task panes clickable
- **doctor:** Report whether the terminal sends Shift+Enter
- **column:** Color each workspace's name to tell projects apart

### Changed

- The package is `tenx-cli` (the crates.io name `tenx` belongs to an unrelated project). The binary is still `tenx`.
- The path embedded in the generated tmux config is the one tenx was invoked by, not the resolved executable, so a package manager's `bin/tenx` symlink survives upgrades on Linux too.

### Fixed

- **status:** Detect permission prompts from a parked turn's worker
- **status:** Read a parked turn's status from its worker, not the pane
- **watch:** A killed watcher no longer blocks the next one as a zombie
- Adapt multi-agent support to the client-column refactor
- **status:** Answer and status follow the real permission dialog
- **column:** List workspaces registered while the client runs
- **sweep:** A task's window is the task's, not whatever shares its name (breaking)
- **git:** Serialise bare-repo writes and recover interrupted clones
- **secrets:** Store pasted secret values without terminal escape codes
- **client:** Make Shift+Enter insert a newline in agents

### Documentation

- **demo:** Play a whole client session in the README demo
- **demo:** Claude Code sessions on the right, a task created on camera
- Describe multi-agent support
- Document every column key, command and form

[0.1.0]: https://github.com/aluedeke/tenx/releases/tag/v0.1.0
