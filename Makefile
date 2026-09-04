.PHONY: build install test clean try try-stop screenshot

build:
	cargo build --release

# Try this build without installing it: its own tmux server (`-L try`), its
# own generated config and watcher, the same workspaces and tasks. Ctrl+w and
# the home overlay run this build too. An installed tenx is unaffected.
try: build
	TENX_TMUX_SOCKET=try ./target/release/tenx

try-stop:
	-tmux -L try kill-server

# Installs the binary to ~/.cargo/bin (cargo's default, same as `cargo install`).
# Nothing else to place: the tmux config is generated at `~/.config/tenx/tmux.conf`
# whenever the tenx session is created, and it embeds this binary's path.
#
# A running tenx server keeps the config it started with (tmux reads it once,
# at server start). After an install that changes the generated config, restart
# the session to pick it up: `tmux -L tenx kill-server`, then `tenx`.
install:
	cargo install --path .
	@echo "  ✓ installed — run 'tenx' to (re)create the session"

test:
	cargo test -p tenx -p tenx-core
	cargo clippy -p tenx -p tenx-core --all-targets -- -D warnings

# Regenerate docs/overlay.svg from the overlay's own widgets and fixture
# data (src/tui/overlay/screenshot.rs) — no real workspace involved.
screenshot:
	TENX_SCREENSHOT=1 cargo test -p tenx --bin tenx screenshot -- --nocapture

clean:
	cargo clean
