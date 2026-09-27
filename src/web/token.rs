//! The token that opens the page: 32 random bytes as hex in
//! `~/.config/tenx/web-token` (mode 600), made on first start and replaced by
//! `tenx web --rotate-token` — which also logs out every browser holding the
//! old one, since the cookie *is* the token.

use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::path::PathBuf;

fn path() -> Result<PathBuf> {
    Ok(crate::workspace::home_dir()?.join(".config").join("tenx").join("web-token"))
}

/// The token, made (or remade, with `rotate`) if needed.
pub fn load_or_create(rotate: bool) -> Result<String> {
    let path = path()?;
    if !rotate
        && let Ok(text) = std::fs::read_to_string(&path)
        && !text.trim().is_empty()
    {
        return Ok(text.trim().to_string());
    }
    let token = random_hex(32)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    // Written beside and renamed over, created 600 from the start: the token
    // is a shell on this machine, never world-readable even for a moment.
    let tmp = path.with_extension("tmp");
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("write {}", tmp.display()))?;
        writeln!(f, "{token}")?;
    }
    std::fs::rename(&tmp, &path).with_context(|| format!("write {}", path.display()))?;
    Ok(token)
}

/// `n` bytes from the system's random source, as hex.
pub fn random_hex(n: usize) -> Result<String> {
    let mut buf = vec![0u8; n];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)).context("read /dev/urandom")?;
    Ok(tenx_core::web::hex(&buf))
}
