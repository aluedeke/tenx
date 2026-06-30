use anyhow::{Context, Result};
use std::env;

pub fn install() -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = crate::workspace::find(&cwd)?;

    let tenx = std::env::current_exe().context("cannot determine tenx binary path")?;

    let zellij = crate::zellij::find_bin().ok_or_else(|| {
        anyhow::anyhow!(
            "zellij not found — install it and ensure it is accessible in one of: \
             ~/.cargo/bin, ~/.local/bin, /opt/homebrew/bin, /usr/local/bin"
        )
    })?;

    eprintln!("  tenx:   {}", tenx.display());
    eprintln!("  zellij: {}", zellij.display());

    crate::cli::task::install_hooks(&ws.dir, true)?;
    eprintln!("hooks installed in {}", ws.dir.join(".claude/hooks").display());
    Ok(())
}
