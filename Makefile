PREFIX ?= $(HOME)/.local

.PHONY: build install clean

build:
	cargo build --release

install: build
	install -d $(PREFIX)/bin
	install -m 755 target/release/tenx $(PREFIX)/bin/tenx

clean:
	cargo clean
