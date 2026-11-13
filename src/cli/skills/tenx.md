---
description: Show tenx workspace status and task list. Use when the user asks about tasks, workspace structure, active work, scheduled tasks (something that should run on a timetable, or the runs of one), or how to migrate existing work into tenx — and also when you (the agent) need a credential, API key, token, or other secret to do your own work, even if the user never asked about tenx at all.
allowed-tools: Bash Read
---

## Active tasks

!`tenx task list 2>/dev/null || echo "(no tasks yet — run: tenx task new <name>)"`

## Workspace layout

```
<workspace>/
├── config.toml          # workspace config
├── .bare/               # shared bare git clones (one per repo)
│   └── <repo>.git/
├── .claude/             # shared Claude config (settings, skills)
│   ├── settings.json
│   └── skills/
├── schedules/           # scheduled tasks, one <name>.toml each (see below)
└── tasks/
    └── <name>/          # one directory per task  ← your working area
        ├── TASK.md      # task notes, todos, links
        ├── .claude      # symlink → ../../.claude
        └── <repo>/      # git worktree (one per repo)
```

Claude Code's project root is `tasks/<name>/` — that's where `.claude` is found.

## Boundaries — read this carefully

**You may only modify files inside `tasks/<current-task>/`** without explicit user approval. This means:

- Edit code inside `tasks/<name>/<repo>/` worktrees — that's your sandbox.
- Keep `tasks/<name>/TASK.md` current (see conventions below).
- Do **not** touch `config.toml`, `.bare/`, `.claude/` (settings, hooks, skills), or any other task's directory.
- Do **not** run `tenx repo add`, `tenx task rm`, or any command that mutates workspace-level state without asking first.

If a task requires something outside these boundaries — adding a repo, changing a shared hook, touching another task — **stop and ask the user** before proceeding.

## Creating a new task

`tenx task new "<title>"` creates the task directory, git worktrees, and a TASK.md, and opens a window with a new agent in it. It can pre-fill the file so you don't have to edit it afterward:

    tenx task new "<title>" --no-focus \
      --prompt "<what the new agent should do>" \
      --link "Linear: <ticket url>" \
      --link "Linear Project: <project>" \
      --link "Linear Milestone: <milestone>"

**Always pass `--prompt`.** It is the new agent's first message, and the only thing that starts it working; tenx also writes it into TASK.md's `## Description`. Without it the agent sits idle with an empty conversation. The new agent does not see your conversation, so make the prompt self-contained — the goal, the constraints, what "done" means, and where the details are (the ticket, the files). Pass `--no-focus` too, so the user's screen doesn't jump to the new task.

Every `--link` is `"Label: value"`; the default rows (Linear Project, Linear Milestone, Linear, PR) are filled in place, any other label (`Jira:`, `GitHub:`) is added as a new row.

**From a ticket.** If the user names a ticket (`ENG-123`, a Linear/Jira/GitHub issue URL), fetch it first, then create the task from what you fetched — title from the ticket title, `--prompt` from its body (trimmed to the essentials, plus what to do about it), `--link` with its URL:

- **Linear** — use a Linear MCP tool if one is connected (`get_issue` / search by identifier). Without one, ask the user for the URL and title; there is no `linear` CLI to shell out to.
- **GitHub issues** — `gh issue view <number-or-url> --json title,body,url`.
- **Jira** — a Jira MCP tool if connected, else ask.

Never store a ticketing credential yourself and never put one in `tenx secrets` — fetching is your job, through tools you already have; tenx only renders what you pass it.

**Otherwise**, ask the user for the tickets (IDs or URLs), the Linear project and milestone (both optional), then run the command above with what they gave you; leave a row blank (don't remove it) when there's nothing for it yet.

## TASK.md conventions

Keep TASK.md current at all times:
- Check off `## Todo` items as you complete them; add new ones as you discover sub-tasks
- After `gh pr create`, add the PR URL under `## Links` → `PR:`
- After linking a Linear issue, add the URL under `## Links` → `Linear:`
- Keep `Linear Project:` and `Linear Milestone:` up to date if they change
- Add decisions and gotchas to `## Notes`

## Secrets

If a task needs a credential (API key, token, DB password) and it isn't already sitting somewhere readable, ask for it — don't try to find, guess, or work around it another way. One command, whatever the secret:

    tenx secrets need STRIPE_KEY DATABASE_URL --why "run the webhook integration tests" --timeout 30m

Always say **why** in a few words: the human sees it in the notification and when they answer, and it's what they decide on. `need` works out by itself what each name needs — already released (returns at once), sealed and waiting for a human to release it, or not stored anywhere yet so a human must type a value — so you never have to know which. It's always safe to run: from your Bash tool (no real terminal) it can't touch key material; it only enqueues a request (the tenx column and status bar show it, the user gets a desktop notification) and waits for a human, who unlocks with their own passphrase. **Never** run `tenx secrets init`, `encrypt`, or `fulfill` yourself.

How to wait: if your harness can run a command in the background and tell you when it exits (Claude Code's Bash tool with `run_in_background`), do that with a long `--timeout` like `30m` and carry on with other work. Otherwise run it in the foreground with the Bash tool's timeout at its maximum (600000 ms) and `--timeout 9m`. The exit code says what happened:

- **0** — granted. Continue.
- **3** — still pending (nobody answered in time). The request stays queued; re-run the same command to keep waiting — it won't notify again.
- **4** — denied. The output has the human's note if they left one. Don't ask again without explaining why you need it.
- **5** — withdrawn with `cancel`.

`--no-wait` enqueues and returns at once, for a secret that's nice to have.

When the user is about to hand you a new value — "I'll copy the key for you" — ask with `--clipboard`: the request tells them to copy it, and they take it from the clipboard when they answer instead of typing it. It always asks for a new value, even for a name already sealed in the task's bundle (not for one in a repo's own sops file). The value never passes through you either way:

    tenx secrets need SENTRY_TOKEN --clipboard --why "upload source maps"

Where granted secrets land — read them from the file, never print them:
- **Keys** (`STRIPE_KEY`) go to `tasks/<name>/.secrets.env`, a plain `KEY=VALUE` file holding exactly the names you've been granted.
- **Repos with their own sops setup** (a `.sops.yaml` in a worktree): name a key inside one of its `*.enc.*` files, or a fragment of the file's name (`staging`). The whole file is released as its plaintext sibling in the worktree (`secrets.staging.enc.env` → `secrets.staging.env`, a symlink to a copy outside git). If there's more than one such file, name the one you need.

Granted secrets are deleted when the task's window closes (closed by hand, or swept after being idle). In a new session, ask again with `need`.

If you no longer need something you asked for, withdraw it so nobody is chased for it:

    tenx secrets cancel <NAME>      # or --all

`cancel` and `tenx secrets status` only read or edit the request queues, never values, so both are safe from your Bash tool.

## Scheduled tasks

A schedule runs a prompt in the workspace on a timetable. Each run is a fresh task of its own (worktrees, branch, window), or — with `--task` — a message to an existing task. The tenx watcher starts runs while the tenx tmux server is up; a slot missed while it was down runs once afterwards if it is less than 12 hours old.

    tenx schedule add nightly-deps --cron "0 6 * * 1-5" --title "Nightly dependency check" \
      --prompt "Run cargo outdated; open a PR if a bump is safe. Last run: {last_run}, its PR: {last_run_pr}" \
      --repos tenx --agent-args "--permission-mode acceptEdits --allowedTools 'Bash(cargo *)'"

- **When**: `--cron` (5 fields, local time, or `@daily` etc.) or `--every 4h`. `{date}`, `{last_run}` and `{last_run_pr}` in the prompt are filled in per run.
- **Unattended**: nobody may be there to answer a permission prompt. Give the agent what it needs with `--agent-args`, or every run stops at its first prompt until its timeout.
- **Timeout**: a run is stopped after `--timeout` (default `1h`), time waiting for a person included: the agent is interrupted, and a run's own window is closed 30 s later. Nothing on disk is deleted.
- **Secrets**: a new task never has secrets released (that takes a person typing a passphrase). A schedule that needs some must use `--task <slug>` and `--secret NAME`; the run is skipped while the name isn't released in that task.
- **Created by you, it starts paused.** `tenx schedule add` from your Bash tool writes the schedule paused; a person turns it on with `tenx schedule resume <name>` in a terminal. A schedule is workspace-level state: ask the user before adding one.

**Inside a run** (the prompt says "This is a scheduled run of …"): do the job, and if you find nothing that needs a person's attention, run `tenx schedule quiet` before you finish — the run is then filed as "nothing to report" instead of shown as unread.

Runs stay off the task list (unless one waits on a person); the column's **Runs** tab lists them per schedule. Old runs are deleted automatically only when they have nothing to lose (no uncommitted change, no unpushed commit, no open PR); `--keep` / `--keep-failed` set how many stay.

## Background work and services

tenx shows a task as working while its agent waits for something, and as done when the agent's turn is over. Start background work so that tenx can tell which is which:

- **Work you wait for** (tests, a build, a script that processes data, `gh run watch --exit-status`): run it in the background as usual. The task stays working until it ends.
- **A stream of events** (CI checks one by one, new lines in a log): a monitor, where your agent has one. The task stays working.
- **A process that must stay up** (a dev server, `tilt up`, `npm run dev`, a watch build): wrap it as `tenx service [--name NAME] -- <command>` and run that in the background. Your turn then ends normally, the sidebar lists the service with its ports, and closing the task's window or deleting the task stops it.

    tenx service --name web -- npm run dev     # in the background
    tenx service list                          # what runs, and in which task
    tenx service stop [NAME]                   # this task's service (--all: every one)

## Common commands

    tenx task new "<title>" --no-focus --prompt "<what to do>" [--link "Label: value"]…
                               create task with worktrees + TASK.md; the agent starts on the prompt
    tenx task new "<title>" --no-repos
                               a task without worktrees: its agent can read the whole workspace
    tenx ask "<question>"      a session of its own for a question, outside any workspace
    tenx task open <name>      switch to the task's window
    tenx task open <name> --no-focus
                               open (or reopen) it without switching anyone's screen to it
    tenx task list             list all tasks and open tabs
    tenx task rm <name>        remove task and worktrees
    tenx task agent <name> [--args "…"] [--confirm-dev-channels]
                               show or set extra start options for the task's agent
    tenx repo add <url>        add a repo to the workspace
    tenx schedule add <name> --cron "…" --prompt "…"   a scheduled task (see above)
    tenx schedule list         every schedule: next run, last outcome
    tenx schedule runs [name]  runs, newest first (--json)
    tenx schedule run <name>   start a run now, to try a schedule out
    tenx schedule pause <name> stop starting runs (--all); resume needs a person
    tenx schedule quiet        inside a run: nothing to report
    tenx schedule promote      inside a run: make it a normal task
    tenx secrets need <n>… --why "…"   ask for credentials and wait (see Secrets above)
    tenx secrets cancel <n>    withdraw a request you no longer need
    tenx secrets status        check sealed/unlocked/pending state

## Migrating existing work

Push your current branch to remote, then:

    tenx task new <name>

and copy or move existing work into `tasks/<name>/<repo>/`.
