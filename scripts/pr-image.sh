#!/bin/sh
# Upload an image (or a short video) for a PR description and print the
# Markdown that shows it. GitHub has no API for the attachments the browser
# uploads, so the file goes to the `pr-assets` branch of this repository, in a
# folder named after the current branch; the branch is created on first use
# and holds nothing else.
#
#   scripts/pr-image.sh <file> [name]     name defaults to the file's name
#
# e.g. scripts/pr-image.sh /tmp/before.png column-before.png
set -eu

[ $# -ge 1 ] && [ -f "$1" ] || { echo "usage: $0 <file> [name]" >&2; exit 2; }
file=$1
name=${2:-$(basename "$file")}
branch=$(git rev-parse --abbrev-ref HEAD)
repo=$(gh repo view --json nameWithOwner --jq .nameWithOwner)
assets=pr-assets
path="$branch/$name"

# The assets branch: one root commit holding a README, made through the API
# so the local checkout is never touched.
if ! gh api "repos/$repo/branches/$assets" --silent 2>/dev/null; then
    tree=$(printf '%s' '{"tree":[{"path":"README.md","mode":"100644","type":"blob","content":"Images for PR descriptions (scripts/pr-image.sh).\n"}]}' |
        gh api "repos/$repo/git/trees" --input - --jq .sha)
    commit=$(gh api "repos/$repo/git/commits" -f message="chore: images for PR descriptions" -f tree="$tree" --jq .sha)
    gh api "repos/$repo/git/refs" -f ref="refs/heads/$assets" -f sha="$commit" --silent
fi

# Replace the file if this branch uploaded one of the same name before.
# (`gh api` prints a failed request's error to stdout, so keep only a SHA.)
sha=$(gh api "repos/$repo/contents/$path?ref=$assets" --jq .sha 2>/dev/null | grep -E '^[0-9a-f]{40}$' || true)
body=$(mktemp)
trap 'rm -f "$body"' EXIT
{
    printf '{"message":"chore: %s","branch":"%s","content":"' "$path" "$assets"
    base64 < "$file" | tr -d '\n'
    printf '"'
    [ -n "$sha" ] && printf ',"sha":"%s"' "$sha"
    printf '}'
} > "$body"
gh api -X PUT "repos/$repo/contents/$path" --input "$body" --silent

url="https://github.com/$repo/blob/$assets/$path?raw=true"
case $name in
    *.mp4|*.mov) echo "$url" ;;
    *) echo "<img src=\"$url\" width=\"400\" alt=\"${name%.*}\">" ;;
esac
