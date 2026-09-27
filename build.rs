//! Where `tenx web` gets its page: `web/out`, the static export `make web`
//! builds, or — when it hasn't been built — a one-file stub saying so, so a
//! plain `cargo build`/`cargo install` needs no Node. The folder is handed to
//! `rust-embed` as `TENX_WEB_DIR` (src/web/assets.rs).

use std::path::Path;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let built = Path::new(&manifest).join("web/out");
    let dir = if built.join("index.html").is_file() {
        built.clone()
    } else {
        let stub = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("web-stub");
        std::fs::create_dir_all(&stub).expect("create the stub page's folder");
        std::fs::write(stub.join("index.html"), STUB).expect("write the stub page");
        stub
    };
    println!("cargo:rustc-env=TENX_WEB_DIR={}", dir.display());
    println!("cargo:rerun-if-changed=build.rs");
    // A directory is scanned whole for changes, so watch the export once it
    // exists — never `web/` itself, whose node_modules would be walked on
    // every build and whose every source edit would rebuild tenx. Until the
    // first export there is nothing to watch: `make web` touches this script
    // so the switch from the stub is picked up.
    if built.exists() {
        println!("cargo:rerun-if-changed={}", built.display());
    }
}

const STUB: &str = r#"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>tenx</title>
<style>body{margin:0;min-height:100vh;display:grid;place-items:center;background:#171820;color:#d6dbe3;font:14px/1.6 ui-monospace,Menlo,monospace}code{color:#a78bfa}</style></head>
<body><div><p>tenx web is running, but this build has no frontend.</p><p>Build it with <code>make web</code>, then rebuild tenx.</p></div></body>
</html>
"#;
