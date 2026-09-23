---
description: Show tenx workspace status and task list. Use when the user asks about tasks, workspace structure, active work, or how to migrate existing work into tenx — and also when you (the agent) need a credential, API key, token, or other secret to do your own work, even if the user never asked about tenx at all.
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

`tenx task new "<title>"` creates the task directory, git worktrees, and a TASK.md. It can pre-fill the file so you don't have to edit it afterward:

    tenx task new "<title>" \
      --description "<what the task is about>" \
      --link "Linear: <ticket url>" \
      --link "Linear Project: <project>" \
      --link "Linear Milestone: <milestone>"

Every `--link` is `"Label: value"`; the default rows (Linear Project, Linear Milestone, Linear, PR) are filled in place, any other label (`Jira:`, `GitHub:`) is added as a new row.

**From a ticket.** If the user names a ticket (`ENG-123`, a Linear/Jira/GitHub issue URL), fetch it first, then create the task from what you fetched — title from the ticket title, `--description` from its body (trimmed to the essentials), `--link` with its URL:

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

Where granted secrets land — read them from the file, never print them:
- **Keys** (`STRIPE_KEY`) go to `tasks/<name>/.secrets.env`, a plain `KEY=VALUE` file holding exactly the names you've been granted.
- **Repos with their own sops setup** (a `.sops.yaml` in a worktree): name a key inside one of its `*.enc.*` files, or a fragment of the file's name (`staging`). The whole file is released as its plaintext sibling in the worktree (`secrets.staging.enc.env` → `secrets.staging.env`, a symlink to a copy outside git). If there's more than one such file, name the one you need.

If you no longer need something you asked for, withdraw it so nobody is chased for it:

    tenx secrets cancel <NAME>      # or --all

`cancel` and `tenx secrets status` only read or edit the request queues, never values, so both are safe from your Bash tool.

## Common commands

    tenx task new "<title>" [--description …] [--link "Label: value"]…
                               create task with worktrees + TASK.md
    tenx task open <name>      switch to the task's window
    tenx task list             list all tasks and open tabs
    tenx task rm <name>        remove task and worktrees
    tenx repo add <url>        add a repo to the workspace
    tenx secrets need <n>… --why "…"   ask for credentials and wait (see Secrets above)
    tenx secrets cancel <n>    withdraw a request you no longer need
    tenx secrets status        check sealed/unlocked/pending state

## Migrating existing work

Push your current branch to remote, then:

    tenx task new <name>

and copy or move existing work into `tasks/<name>/<repo>/`.
