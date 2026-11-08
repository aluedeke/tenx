#!/bin/bash
# Swaps the Homebrew release of tenx for a test build of this checkout, and
# back. `make install` / `make uninstall` / `make installed` run it.
#
#   scripts/test-version.sh on       build this checkout (page included) and use it
#   scripts/test-version.sh off      back to the Homebrew release
#   scripts/test-version.sh status   which one runs
#
# Everything that runs tenx calls one path: `$(brew --prefix)/bin/tenx` — the
# `tenx web` service, the generated tmux config, the watcher, the agents'
# hooks. `on` puts the test build behind that path (`brew unlink tenx`, then
# links to a copy in ~/.local/share/tenx/test-version), `off` puts the release
# back (`brew link`); both then restart `tenx web` and reload the tmux config,
# which restarts the watcher. The copy, never `target/`, so a rebuild or a
# deleted task can't change what runs. A `brew upgrade tenx` while a test
# build runs upgrades the release and leaves the test build in place; `off`
# links the new release.

set -euo pipefail

BREW=$(brew --prefix 2>/dev/null) || { echo "Homebrew not found"; exit 1; }
BIN=$BREW/bin
DEST=$HOME/.local/share/tenx/test-version
BINARIES=(tenx tenx-whisper tenx-tts)
# Homebrew's rustup keeps cargo and rustc out of PATH.
command -v cargo >/dev/null || PATH=$BREW/opt/rustup/bin:$PATH

testing() {
    [[ "$(readlink "$BIN/tenx" 2>/dev/null)" == "$DEST/"* ]]
}

reload() {
    "$BIN/tenx" web service restart 2>/dev/null || echo "  (no tenx web service installed)"
    if tmux -L "${TENX_TMUX_SOCKET:-tenx}" has-session 2>/dev/null; then
        "$BIN/tenx" tmux reload
    fi
}

status() {
    if testing; then
        echo "test build: $(cat "$DEST/source" 2>/dev/null)"
    else
        echo "release: $("$BIN/tenx" --version 2>/dev/null || echo "not installed")"
    fi
}

case "${1:-status}" in
on)
    brew list --versions tenx >/dev/null || { echo "brew install aluedeke/tap/tenx first"; exit 1; }
    root=$(git rev-parse --show-toplevel)
    cd "$root"
    # Without the page, `tenx web` serves a stub that says to build it.
    make web
    cargo build --release --bins
    mkdir -p "$DEST"
    for b in "${BINARIES[@]}"; do
        [ -x "target/release/$b" ] || continue
        # A new file renamed into place: overwriting a binary that is running
        # gets that process killed on macOS.
        cp "target/release/$b" "$DEST/.$b.new"
        mv -f "$DEST/.$b.new" "$DEST/$b"
    done
    dirty=$(git status --porcelain --untracked-files=no | grep -q . && echo ", uncommitted changes" || true)
    echo "$(git rev-parse --abbrev-ref HEAD) $(git rev-parse --short HEAD)$dirty, from $root, built $(date '+%Y-%m-%d %H:%M')" >"$DEST/source"
    testing || brew unlink tenx >/dev/null
    for b in "${BINARIES[@]}"; do
        if [ -x "$DEST/$b" ]; then ln -sfn "$DEST/$b" "$BIN/$b"; fi
    done
    reload
    echo "  ✓ $(status) — 'make uninstall' goes back to the release"
    ;;
off)
    if ! testing; then
        status
        exit 0
    fi
    for b in "${BINARIES[@]}"; do
        if [[ "$(readlink "$BIN/$b" 2>/dev/null)" == "$DEST/"* ]]; then rm -f "$BIN/$b"; fi
    done
    brew link --overwrite tenx >/dev/null
    reload
    echo "  ✓ $(status)"
    ;;
status)
    status
    ;;
*)
    echo "usage: $0 on|off|status"
    exit 2
    ;;
esac
