.PHONY: build install clean

build:
	cargo build --release

# Installs to ~/.cargo/bin (cargo's default), the same location `cargo install`
# uses — so `make install` and `cargo install` never diverge. Override the
# location with CARGO_INSTALL_ROOT if needed.
install:
	cargo install --path .

clean:
	cargo clean
