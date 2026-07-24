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
# zellij does NOT reload plugins when the wasm file changes — a running session
# keeps the instance it loaded at startup. Reload it explicitly (best-effort:
# a dead/absent session is fine, it'll load the new wasm on next start).
install: plugin
	cargo install --path .
	mkdir -p $(PLUGIN_DIR)
	cp target/wasm32-wasip1/release/tenx-zellij.wasm $(PLUGIN_DIR)/tenx-zellij.wasm
	-zellij action start-or-reload-plugin "file:$(PLUGIN_DIR)/tenx-zellij.wasm" 2>/dev/null

clean:
	cargo clean
