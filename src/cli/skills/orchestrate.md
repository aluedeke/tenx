---
description: Drive tenx tasks in other workspaces from this detached session — list them, start new ones with a prompt, message their agents, wait for their turns and read what they said. Use when the user asks you to coordinate, delegate to, check on, or collect results from tasks or agents in their tenx workspaces, or to fan a piece of work out over several repos.
allowed-tools: Bash Read
---

## Where you are

This session runs in tenx's **detached workspace**: it has no repos of its own. The code lives in the user's other workspaces, each a set of tasks with their own git worktrees and their own agent session in a tmux window. You can **read** every workspace (they were added with `--add-dir`); you **change** code only by asking the task's own agent to — never edit files in another task's directory yourself.

## What is going on

!`tenx task list --json 2>/dev/null || echo '(tenx session not running)'`

`workspaces[]` has each workspace's `name`, `dir` and repos; `tasks[]` each task's `ws` (workspace name), `ws_dir`, `slug`, `title`, `repos` and `status` (`working`, `blocked` = waiting on a dialog — `waiting_for` says which, `signaled`, `done` = turn over, `idle` = no agent running). Tasks of the `detached` workspace are sessions like this one. Run it again whenever you need fresh state.

## Driving a task

Every command takes the task's slug and `--ws-dir <workspace name or dir>`.

    tenx task new "<title>" --ws-dir <ws> --no-focus --prompt "<what to do>"    # new task, agent starts on the prompt; prints the slug
    tenx task new "<title>" --ws-dir <ws> --no-focus --repos a,b --prompt "…"  # only some of the workspace's repos
    tenx task send <slug> --ws-dir <ws> "<message>"                 # message an existing task's agent (opens its window if closed)
    tenx task wait <slug> --ws-dir <ws> --timeout 20m               # until its turn is over
    tenx task output <slug> --ws-dir <ws>                           # what it said since the last prompt (--json for prompt + status)

Always pass `--no-focus` to `task new`: without it the user's screen jumps to the new task.

`task wait` exits **0** when the turn is over, **2** when the agent stopped on something that needs an answer (a permission prompt, a question — the reason is printed), **3** on timeout (re-run to keep waiting). Wait with your Bash tool in the background when you can, so several tasks run at once; otherwise in the foreground with the tool's timeout at its maximum and `--timeout 9m`.

On exit 2: read `task output` to see the question. If it's a question you can answer from what the user told you, answer with `task send`. A **permission prompt** is the user's to answer — `task send` refuses to type into one (don't pass `--force` to get around that); tell the user which task is waiting and on what.

## How to work

- Before creating a task, check `tenx task list --json` for one that already covers the work; message it instead.
- Give each task a self-contained prompt: the goal, the constraints, what "done" means, and what to report back. The task's agent does not see this conversation.
- Fan out first, then wait: start every independent task, then wait on each.
- Report back to the user with what each task did (from `task output`), what is still running, and what needs them.
- Never delete tasks (`tenx task rm`) or add repos to a workspace without asking.

For anything else about tenx — secrets, TASK.md conventions — see the `/tenx` skill.
