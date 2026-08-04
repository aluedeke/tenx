.PHONY: build plugin install clean

PLUGIN_DIR := $(HOME)/.local/share/tenx

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
# No plugin-reload step: the overlay CLOSES its pane when dismissed (see
# `dismiss` in tenx-zellij), so the next Ctrl+w always loads the wasm this
# target just wrote. `zellij action start-or-reload-plugin` used to live here
# and was worse than useless — it spawns an *additional* plugin pane instead of
# swapping the running one, and LaunchOrFocusPlugin then keeps focusing the
# oldest (stale) pane, so installs appeared to do nothing.
install: plugin
	cargo install --path .
	mkdir -p $(PLUGIN_DIR)
	cp target/wasm32-wasip1/release/tenx-zellij.wasm $(PLUGIN_DIR)/tenx-zellij.wasm
	@echo "  ✓ installed — press esc in any open overlay; the next Ctrl+w loads the new build"

clean:
	cargo clean
