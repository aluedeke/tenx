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

If a task needs a credential (API key, token, DB password) and it isn't already sitting somewhere readable, ask for it — don't try to find, guess, or work around it another way:

    tenx secrets decrypt <NAME> --timeout 9m

It's always safe to run, even if this workspace hasn't set up secrets at all: from your Bash tool (no real terminal attached) it can't touch the credential itself — it enqueues a durable, visible request (the tenx status bar and overlay show it, and the user gets a desktop notification) and then **blocks until a human releases it**. Releasing it for real requires that human typing the decryption passphrase themselves, from a normal shell or the tenx overlay — that's the whole point, so **never** run `tenx secrets init` or `encrypt` yourself.

How to wait: run it with your Bash tool's timeout set to its maximum (600000 ms) and `--timeout 9m`, so the command outlives a slow human. Exit code 0 means it's released — carry on. Exit code 1 with "still pending" means nobody has answered yet: the request is still queued, so if you genuinely can't proceed without it, just run the same command again (an already-pending name is a no-op, not a repeat notification) — or do other useful work first and come back. Exit code 1 with "withdrawn" means a human cancelled the request; don't re-ask for the same thing without saying why you need it. If you'd rather not block at all (the secret is nice-to-have, or you have plenty of other work), pass `--no-wait` to enqueue and return immediately.

If you no longer need something you asked for — the task changed, you found another way, the user gave you the value some other way — withdraw it so nobody is chased for it:

    tenx secrets cancel <NAME>      # or --all

`cancel` only edits the request queue, never key material, so it's safe from your Bash tool too. `tenx secrets status` is also safe to run any time — it only shows sealed/unlocked/pending state, never values.

Two shapes of secret you might find, depending on the repo:
- **Sealed by tenx** — lands at `tasks/<name>/.secrets.env` (a plain `KEY=VALUE` file). `<NAME>` here is just a label for the human approving it; decrypting always releases the whole file.
- **Adopted from the repo's own setup** (`.sops.yaml` already in a worktree) — lands as a plaintext sibling of its ciphertext, inside that worktree (e.g. `secrets.staging.enc.env` → `secrets.staging.env`). Here `<NAME>` actually matters: it's matched against candidate filenames, so if a repo has more than one (e.g. `secrets.staging.enc.env` *and* `secrets.prod.enc.env`), name the **file** you need (or a distinctive fragment like `staging`) so only that one gets released — not a field inside it, and not the whole set. A name that doesn't match any file falls back to releasing everything found, so still err toward naming the file rather than nothing.

If instead **you need a secret that doesn't exist yet** — nothing to release, someone has to supply a value (an API key you don't have, a password to generate) — ask for it the same way:

    tenx secrets set <NAME> --timeout 9m

Same rules as `decrypt`: your Bash tool has no real terminal, so this can never actually set anything — it enqueues a durable "someone needs to supply a value for `<NAME>`" request and waits for a human to fulfil it (same timeout/re-run/`--no-wait`/`cancel` handling as above). You never type or pipe a value here at all; a human supplies it later, prompted for it directly when they run `set` themselves (from a real shell or the overlay) — you never see or relay the value in either direction, so nothing about it ever touches your own output or the conversation transcript. Once it reports the value was set, run `tenx secrets decrypt <NAME>` to have it released to you.

## Common commands

    tenx task new "<title>" [--description …] [--link "Label: value"]…
                               create task with worktrees + TASK.md
    tenx task open <name>      switch to the task's window
    tenx task list             list all tasks and open tabs
    tenx task rm <name>        remove task and worktrees
    tenx repo add <url>        add a repo to the workspace
    tenx secrets decrypt <n>   ask for a credential and wait for it (see Secrets above)
    tenx secrets set <n>       ask for a secret that doesn't exist yet (see Secrets above)
    tenx secrets cancel <n>    withdraw a request you no longer need
    tenx secrets status        check sealed/unlocked/pending state

## Migrating existing work

Push your current branch to remote, then:

    tenx task new <name>

and copy or move existing work into `tasks/<name>/<repo>/`.
