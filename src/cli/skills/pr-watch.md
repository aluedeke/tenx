---
description: Watch a pull request until it is merged — wake on every review, comment (from people and bots) and failed check, decide whether it needs a code change, act, and wait again. Use after opening a PR when the user wants you to see it through, or when they ask you to watch, babysit, follow up on or handle feedback on a PR.
allowed-tools: Bash Read Edit Write
---

## The loop

`tenx pr wait` polls GitHub once a minute and exits when the PR has news. While it runs you spend nothing; its exit wakes you.

1. **Find the PR**: the `PR:` line in `TASK.md`, else `gh pr view --json url` in the worktree. Without an argument `tenx pr wait` finds it the same way.
2. **First wait without `--since`**: `tenx pr wait <url>`. It returns at once with any feedback you haven't handled yet — all of it on a new PR.
3. **Run every wait in the background** so the session stays free: in Claude Code, the Bash tool with `run_in_background: true`; you are called back when it exits. A harness without background commands runs it in the foreground. Tell the user the wait is running, and on which PR. They see it as 👀 on the PR's chip in the tenx column and in `tenx pr list`, and sweep leaves the task's window open while it runs.
4. **On exit, act on the exit code**:
   - `0`: news. Handle each event (below), then run the `next:` command from the output's last line. It carries `--since`, so nothing is reported twice.
   - `3`: timeout, nothing new. Run the `next:` command again.
   - `10`: merged. Stop the loop, check off the PR in `TASK.md`, tell the user.
   - `11`: closed without merging. Stop the loop and tell the user.
   - `1`: an error (no login, wrong PR). Tell the user; don't retry in a tight loop.
5. Keep going until `10` or `11`, or until the user says stop.
6. **Lost the `next:` line** (a restart, a resumed or compacted session)? Run `tenx pr wait <url>` without `--since`. tenx saved the last `--since` you started a wait with, so it resumes from there: nothing is lost, nothing already handled comes back.

## Deciding on each event

For every event, first understand it, then decide: **code change, reply only, or nothing**.

- **Failed check**: read the log (`gh run view <run-id> --log-failed`; the run id is in the URL). Before blaming the PR, see whether the same check fails elsewhere right now (`gh run list --workflow <workflow> -L 10`): shared test environments break for everyone at once. A real failure caused by the PR → fix it. A flaky or infrastructure failure (timeout, network, runner, failing on other branches too) → `gh run rerun <run-id> --failed`, once; if it fails again, tell the user. Cancelled runs are not reported: a newer push replaced them.
- **Review or comment asking for a change** (person or review bot): if it is correct and in scope, make the change. If it is wrong, out of scope or already handled, don't change code — reply with the reason.
- **Question**: answer it in a reply. Change code only if the answer shows a real problem.
- **Status bots** (deploy previews, plans, coverage, changelog bots): read them for problems (a failed deploy, a destructive plan), otherwise nothing. Don't reply to bots that only report status.
- **Approval** with a comment: read the comment; act only if it asks for something. Approvals without text are not reported.
- **Not sure, or a product decision, or you disagree with a person**: don't guess. Ask the user, and keep the wait running.

After a code change: run the tests, commit, `git push`. Never force-push, rewrite history, merge the PR or dismiss a review unless the user asked for it.

## Replying

End **every** comment you post with `<!-- tenx:agent -->`. It is invisible on GitHub; it tells `tenx pr wait` that the comment is yours, so your own reply doesn't wake you.

- To the PR conversation: `gh pr comment <url> --body "…<!-- tenx:agent -->"`.
- To a line comment: reply in its thread. The id is the number after `#discussion_r` in its URL: `gh api repos/<owner>/<repo>/pulls/<n>/comments/<id>/replies -f body="…<!-- tenx:agent -->"`.

Keep replies short: what you changed (with the commit), or why you didn't.

## Keep TASK.md current

Note under `## Notes` what feedback came in and what you decided, one line each, so the user can follow the PR's history without reading GitHub.
