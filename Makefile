.PHONY: build plugin install clean

PLUGIN_DIR := $(HOME)/.local/share/tenx
# zellij is not reliably on PATH (it lives under ~/.local/bin or ~/.cargo/bin,
# and make's shell doesn't source the user's profile), same reason
# `zellij::find_bin()` exists on the Rust side.
ZELLIJ := $(shell command -v zellij 2>/dev/null || echo $(HOME)/.local/bin/zellij)

build:
	cargo build --release

# The zellij launcher plugin (WASM). Opens the tenx overlay TUI in a floating
# pane sized responsively for the current screen.
plugin:
	cargo build -p tenx-zellij --release --target wasm32-wasip1

# Installs the binary to ~/.cargo/bin (cargo's default, same as `cargo install`)
# and the zellij plugin to ~/.local/share/tenx/ (referenced by the Ctrl+w
# keybind in ~/.config/zellij/config.kdl).
#
# The reload step is REQUIRED, and copying the wasm is not enough. Two separate
# caches sit between this target and what Ctrl+w shows:
#
#  1. Live plugin *panes*. Handled by the plugin itself — `dismiss` closes its
#     pane rather than hiding it, so no pane can outlive a build.
#  2. The zellij *server's* compiled-module cache, keyed by plugin path and
#     living as long as the session. This one closing a pane cannot touch: every
#     Ctrl+w re-instantiates the module compiled the first time the server read
#     this path, so a fresh wasm on disk is simply ignored — for days, across
#     any number of installs. `start-or-reload-plugin` is the only action that
#     evicts it.
#
# It spawns a plugin pane as a side effect (that's the "start" half, and why
# this step was once removed as useless — the extra pane masked the fact that it
# was doing the one necessary thing). The pane is the *new* build, so it doubles
# as confirmation; esc closes it. Errors are ignored so an install works with no
# session running — the module cache dies with the server anyway.
install: plugin
	cargo install --path .
	mkdir -p $(PLUGIN_DIR)
	cp target/wasm32-wasip1/release/tenx-zellij.wasm $(PLUGIN_DIR)/tenx-zellij.wasm
	-@$(ZELLIJ) --session tenx action start-or-reload-plugin \
		"file:$(PLUGIN_DIR)/tenx-zellij.wasm" \
		-c tenx_bin=$(HOME)/.cargo/bin/tenx >/dev/null 2>&1
	@echo "  ✓ installed — the plugin pane that just opened is the new build; esc closes it"

clean:
	cargo clean
