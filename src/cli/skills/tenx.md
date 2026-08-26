---
description: Show tenx workspace status and task list. Use when the user asks about tasks, workspace structure, active work, or how to migrate existing work into tenx.
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
├── .claude/             # shared Claude config (hooks, settings, skills)
│   ├── settings.json
│   ├── hooks/
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

`tenx task new <name>` creates the task directory, git worktrees, and a blank TASK.md.

Before running the command, ask the user for:

1. **Linear tickets** — one or more ticket IDs or URLs (e.g. `ENG-123`, `ENG-124`). If given only IDs, look them up with `linear issue view <ID>` to get the full URL and title.
2. **Linear project** — the project name or URL these tickets belong to (optional).
3. **Linear milestone** — the milestone or cycle (optional).

After `tenx task new` completes, fill in the generated `tasks/<name>/TASK.md`:
- Set the description
- Populate `Linear Project:`, `Linear Milestone:`, and `Linear:` lines with whatever the user provided; leave a line blank (not remove it) if the user has nothing for it yet

## TASK.md conventions

Keep TASK.md current at all times:
- Check off `## Todo` items as you complete them; add new ones as you discover sub-tasks
- After `gh pr create`, add the PR URL under `## Links` → `PR:`
- After linking a Linear issue, add the URL under `## Links` → `Linear:`
- Keep `Linear Project:` and `Linear Milestone:` up to date if they change
- Add decisions and gotchas to `## Notes`

## Secrets

If a task needs a credential (API key, token, DB password) and it isn't already sitting in `tasks/<name>/.secrets.env`, ask for it — don't try to find, guess, or work around it another way:

    tenx secrets request <NAME>

This only enqueues a request (a durable marker, visible in the tenx status bar and overlay) — it never touches the credential itself, and it's always safe to run even if this workspace hasn't set up secrets at all. Releasing it requires a human to type the decryption passphrase themselves, from a normal shell or the tenx overlay — that's the whole point, so **never** run `tenx secrets unlock`, `init`, or `seal` yourself, and don't wait around for it. Move on to other work, and check back later for `tasks/<name>/.secrets.env` (a plain `KEY=VALUE` file) or re-run `request` if it's been a while. `tenx secrets status` is safe to run any time — it only shows sealed/unlocked/pending state, never values.

## Common commands

    tenx task new <name>       create task with worktrees + TASK.md
    tenx task open <name>      switch to task's zellij tab
    tenx task list             list all tasks and open tabs
    tenx task rm <name>        remove task and worktrees
    tenx repo add <url>        add a repo to the workspace
    tenx secrets request <n>   ask for a credential (see Secrets above)
    tenx secrets status        check sealed/unlocked/pending state

## Migrating existing work

Push your current branch to remote, then:

    tenx task new <name>

and copy or move existing work into `tasks/<name>/<repo>/`.
