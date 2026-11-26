<!--
Every section below is required. The `pr-description` check blocks the merge
until each one is filled in and every box in the checklist is ticked
(scripts/check-pr-body.py). Edit the description and the check runs again.

Write for a reader who did not follow the work: simple words, short
sentences, no jargon. Explain every name the reader may not know.
These comments are not shown on the PR page; you may leave them.
-->

## What and why

<!-- The feature or fix this PR was meant to deliver, and why it matters to
the user. 2 to 5 sentences. Link the task or issue if there is one. -->

## What changed

<!-- A short list of the changes in the code: what is new, what works
differently, what was removed. Name the area or file of each. -->

## Contract changes

<!-- Everything that other programs, apps or people rely on and that this PR
changes. In tenx that is: the web page protocol (docs/web-protocol.md), the
MCP tools (tenx-agent uses them), CLI commands, flags and `--json` output,
config files and keys (config.toml, ~/.config/tenx/*), files in a task
(.tenx-*, TASK.md), the agents' hooks and the skills, the iOS app's API.
For each one: what changed, does the old side still work with it, what must
be updated together. Write "None" if nothing changed. -->

## Screenshots

<!-- Before and after for every UI change: the terminal column, the web page,
the iOS app. Upload an image with `scripts/pr-image.sh <file>` and paste the
line it prints. For something new, write "did not exist" under Before.
Write "No UI change" if there is none, and delete the table. -->

| Before | After |
| --- | --- |
|  |  |

## How it was tested

<!-- The commands you ran and their result (e.g. `make test`: passed), and the
steps you tried by hand, on which device. -->

## Risks and follow-ups

<!-- What could break, and how to undo it. AI review findings you did not fix
after the third round, with the reason. Work left for later. Write "None" if
there is nothing. -->

## Checklist

- [ ] The sections above are complete and written in simple words.
- [ ] Tests cover the new or changed behaviour, and `make test` passes.
- [ ] Docs are updated where behaviour changed (README.md, ARCHITECTURE.md, CLAUDE.md, docs/), or nothing needed an update.
- [ ] I read the whole diff myself: no debug code, no commented-out code, no secrets or personal data.
- [ ] AI review: I ran `/code-review` on the changes and fixed all findings, in at most 3 rounds (review, fix, review again). Findings still open after round 3 are listed under "Risks and follow-ups".
