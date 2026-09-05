#!/bin/sh
# Commit-message check: Conventional Commits, because the subject line becomes
# the changelog and the release notes (cliff.toml), and the type decides the
# version bump (feat → minor, fix → patch, `!` → breaking).
#
#   scripts/check-commit-msg.sh <file>        one message (the commit-msg hook)
#   scripts/check-commit-msg.sh --range A..B  every commit in a range (CI)
#
# Accepted:  <type>(<scope>)!: <subject>   scope optional, `!` optional
#   types: feat fix docs refactor perf test chore ci build style revert
#   scope: lowercase area such as tmux, overlay, secrets, release
#   subject: imperative, no trailing period, at most 72 characters in total
# Also accepted as-is: merge commits, `Revert "…"`, fixup!/squash!, `release vX.Y.Z`.
set -eu

types='feat|fix|docs|refactor|perf|test|chore|ci|build|style|revert'
pattern="^($types)(\([a-z][a-z0-9/_-]*\))?!?: [^ ].*[^.]$"
exempt='^(Merge |Revert "|fixup! |squash! |release v[0-9])'

check() {
    subject=$1
    if printf '%s' "$subject" | grep -Eq "$exempt"; then return 0; fi
    if [ "${#subject}" -gt 72 ]; then
        echo "commit message too long (${#subject} > 72): $subject" >&2; return 1
    fi
    if ! printf '%s' "$subject" | grep -Eq "$pattern"; then
        cat >&2 <<MSG
commit message is not a Conventional Commit: $subject

  expected:  <type>(<scope>): <subject>     e.g. fix(tmux): keep the popup border on resize
  types:     ${types}
  The subject is printed verbatim in CHANGELOG.md and the release notes; feat bumps
  minor, fix bumps patch, a '!' after the type marks a breaking change.
MSG
        return 1
    fi
}

case "${1:-}" in
    --range)
        range=${2:?range required, e.g. origin/main..HEAD}
        bad=0
        for sha in $(git rev-list --no-merges "$range"); do
            subject=$(git log -1 --format=%s "$sha")
            check "$subject" || { echo "  in commit $sha" >&2; bad=1; }
        done
        exit $bad
        ;;
    "" ) echo "usage: $0 <message-file> | --range A..B" >&2; exit 2 ;;
    *)
        # First non-comment, non-empty line of the message file.
        subject=$(grep -v '^#' "$1" | grep -m1 -v '^[[:space:]]*$' || true)
        [ -n "$subject" ] || exit 0   # git rejects empty messages itself
        check "$subject"
        ;;
esac
