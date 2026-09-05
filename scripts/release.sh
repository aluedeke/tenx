#!/bin/sh
# Cut a release: `scripts/release.sh auto|patch|minor|major [--dry-run] [--push]`.
#
# The next version is never typed. `auto` lets git-cliff derive it from the
# commits since the last tag (feat → minor, fix → patch, see cliff.toml);
# patch|minor|major bump that part of Cargo.toml's version instead. Then both
# crates are bumped, the CHANGELOG section for the version is generated from
# those same commits and inserted above the previous one, Cargo.lock is
# refreshed, the dist config checked, and `release vX.Y.Z` committed. With `--push` the commit is pushed and the release
# workflow is dispatched for that tag (`gh workflow run release.yml`); the
# workflow builds the binaries and creates the tag itself, so a version can
# never be tagged without its release. Nothing here runs `git tag`.
#
# Refuses to run with uncommitted changes, off `main` (override with
# TENX_RELEASE_BRANCH), when the tag exists, or with no commits since the last
# release. Needs git-cliff, dist and agg on PATH.
# .github/workflows/bump.yml runs exactly this from the Actions tab.
set -eu

usage() { echo "usage: scripts/release.sh auto|patch|minor|major [--dry-run] [--push]" >&2; exit 2; }

bump=""; dry=0; push=0
for arg in "$@"; do
    case "$arg" in
        auto|patch|minor|major) bump=$arg ;;
        --dry-run|-n) dry=1 ;;
        --push) push=1 ;;
        *) usage ;;
    esac
done
[ -n "$bump" ] || usage

cd "$(dirname "$0")/.."

for tool in git-cliff dist agg; do
    command -v "$tool" >/dev/null || { echo "release: $tool not found (cargo binstall $tool)" >&2; exit 1; }
done
git fetch -q --tags origin 2>/dev/null || true

current=$(sed -nE 's/^version = "([^"]+)"/\1/p' Cargo.toml | head -1)
case "$current" in
    *.*.*) ;;
    *) echo "release: cannot read version from Cargo.toml (got '$current')" >&2; exit 1 ;;
esac
last_tag=$(git describe --tags --abbrev=0 --match 'v[0-9]*' 2>/dev/null || true)
if [ -n "$last_tag" ]; then
    since=$(git rev-list --count "$last_tag..HEAD")
    [ "$since" -gt 0 ] || { echo "release: no commits since $last_tag, nothing to release" >&2; exit 1; }
fi

if [ "$bump" = auto ]; then
    version=$(git cliff --bumped-version 2>/dev/null | tail -1)
    version=${version#v}
    case "$version" in
        *.*.*) ;;
        *) echo "release: git-cliff could not derive a version (got '$version')" >&2; exit 1 ;;
    esac
    [ "$version" != "$current" ] || { echo "release: derived version $version is the current one; nothing conventional to release?" >&2; exit 1; }
else
    major=${current%%.*}; rest=${current#*.}; minor=${rest%%.*}; patch=${rest#*.}
    patch=${patch%%[-+]*}   # drop any pre-release/build suffix
    case "$bump" in
        major) major=$((major + 1)); minor=0; patch=0 ;;
        minor) minor=$((minor + 1)); patch=0 ;;
        patch) patch=$((patch + 1)) ;;
    esac
    version="$major.$minor.$patch"
fi
tag="v$version"

branch=$(git rev-parse --abbrev-ref HEAD)
want=${TENX_RELEASE_BRANCH:-main}
[ "$branch" = "$want" ] || { echo "release: on '$branch', releases are cut from '$want' (TENX_RELEASE_BRANCH overrides)" >&2; exit 1; }
git diff --quiet && git diff --cached --quiet || { echo "release: commit or stash changes first" >&2; exit 1; }
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null || git ls-remote --exit-code --tags origin "refs/tags/$tag" >/dev/null 2>&1; then
    echo "release: tag $tag already exists" >&2; exit 1
fi

echo "release: $current -> $version ($bump${last_tag:+, $since commits since $last_tag}) on $branch"
[ "$dry" -eq 0 ] || { echo "release: dry run, nothing changed"; exit 0; }

# The README demo (animated SVG, asciinema cast, GIF) is rendered from this
# very code, so it rides along in the release commit and never lags the
# binary. `make demo` plays the scripted scene; `make demo-gif` needs agg.
make demo demo-gif >/dev/null 2>&1 || { echo "release: make demo demo-gif failed (run it by hand to see why)" >&2; exit 1; }
echo "  ✓ demo re-rendered (docs/overlay-demo.svg, .cast, .gif)"

sed -i.bak -E "s/^version = \"[^\"]+\"/version = \"$version\"/" Cargo.toml tenx-core/Cargo.toml
sed -i.bak -E "s/^tenx-core = \{ version = \"[^\"]+\"/tenx-core = { version = \"$version\"/" Cargo.toml
rm -f Cargo.toml.bak tenx-core/Cargo.toml.bak

# The new section, from the commits since the last tag, inserted above the
# previous version's heading (or appended when there is none).
# A section written ahead of time (the first release, whose history predates
# Conventional Commits) is kept as it is rather than duplicated.
if grep -q "^## \[$version\]" CHANGELOG.md; then
    echo "  ✓ CHANGELOG.md already has a $version section, keeping it"
else
    section=$(git cliff --unreleased --tag "$tag" --strip all 2>/dev/null)
    [ -n "$section" ] || { echo "release: git-cliff produced no changelog section" >&2; exit 1; }
    first=$(grep -n -m1 '^## \[' CHANGELOG.md | cut -d: -f1)
    if [ -n "$first" ]; then
        { head -n "$((first - 1))" CHANGELOG.md; printf '%s\n\n' "$section"; tail -n "+$first" CHANGELOG.md; } > CHANGELOG.md.new
    else
        { cat CHANGELOG.md; printf '\n%s\n' "$section"; } > CHANGELOG.md.new
    fi
    mv CHANGELOG.md.new CHANGELOG.md
fi

cargo update -w --offline 2>/dev/null || cargo update -w
dist plan >/dev/null

git commit -qam "release $tag"
echo "  ✓ committed release $tag"

if [ "$push" -eq 1 ]; then
    git push origin "HEAD:$branch"
    gh workflow run release.yml --ref "$branch" -f "tag=$tag"
    echo "  ✓ release workflow dispatched for $tag — it builds, publishes and creates the tag"
    echo "    watch: gh run watch \$(gh run list --workflow=release.yml -L1 --json databaseId -q '.[0].databaseId')"
else
    echo "    to release: git push && gh workflow run release.yml --ref $branch -f tag=$tag"
fi
