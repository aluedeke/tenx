#!/bin/sh
# Upload an image for a PR description and print the HTML that shows it.
# GitHub has no API for the attachments the browser uploads, so the file goes
# to the `pr-assets` branch of this repository, in a folder named after the
# current branch; the branch is created on first use and holds nothing else.
# Its commits say [skip ci], so an upload starts no CI run. Images only
# (png, jpg, gif, webp): GitHub plays a video only when it was dragged into
# the description in the browser.
#
#   scripts/pr-image.sh <file> [name]     name defaults to the file's name
#
# e.g. scripts/pr-image.sh /tmp/before.png column-before.png
set -eu

[ $# -ge 1 ] && [ -f "$1" ] || { echo "usage: $0 <file> [name]" >&2; exit 2; }
file=$1
# Names go into a URL, a JSON body and an HTML attribute: keep them plain.
safe() { printf '%s' "$1" | tr -c 'A-Za-z0-9._/-' '-'; }
name=$(safe "${2:-$(basename "$file")}" | tr '/' '-')
case $name in
    *.png|*.jpg|*.jpeg|*.gif|*.webp) ;;
    *) echo "not an image (png, jpg, gif, webp): $name" >&2; exit 2 ;;
esac
branch=$(git rev-parse --abbrev-ref HEAD)
[ "$branch" != HEAD ] || { echo "check out the PR's branch first" >&2; exit 2; }
branch=$(safe "$branch")
repo=$(gh repo view --json nameWithOwner --jq .nameWithOwner)
assets=pr-assets
path="$branch/$name"
message="chore: add PR image [skip ci]"

# The assets branch: one root commit holding a README, made through the API
# so the local checkout is never touched.
if ! gh api "repos/$repo/branches/$assets" --silent 2>/dev/null; then
    tree=$(printf '%s' '{"tree":[{"path":"README.md","mode":"100644","type":"blob","content":"Images for PR descriptions (scripts/pr-image.sh).\n"}]}' |
        gh api "repos/$repo/git/trees" --input - --jq .sha)
    commit=$(gh api "repos/$repo/git/commits" -f message="chore: images for PR descriptions [skip ci]" -f tree="$tree" --jq .sha)
    gh api "repos/$repo/git/refs" -f ref="refs/heads/$assets" -f sha="$commit" --silent
fi

# Replace the file if this branch uploaded one of the same name before.
# (`gh api` prints a failed request's error to stdout, so keep only a SHA.)
sha=$(gh api "repos/$repo/contents/$path?ref=$assets" --jq .sha 2>/dev/null | grep -E '^[0-9a-f]{40}$' || true)
body=$(mktemp)
trap 'rm -f "$body"' EXIT
{
    printf '{"message":"%s","branch":"%s","content":"' "$message" "$assets"
    base64 < "$file" | tr -d '\n'
    printf '"'
    [ -n "$sha" ] && printf ',"sha":"%s"' "$sha"
    printf '}'
} > "$body"
gh api -X PUT "repos/$repo/contents/$path" --input "$body" --silent

echo "<img src=\"https://github.com/$repo/blob/$assets/$path?raw=true\" width=\"400\" alt=\"${name%.*}\">"
