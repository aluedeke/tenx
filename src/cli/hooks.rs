use anyhow::Result;
use std::env;

/// `tenx hooks install` — retained under its original name because that's what
/// an upgrading user knows to run, but it now *removes* tenx's Claude Code
/// hooks. tenx installs none: every task state it shows is read live from
/// Claude Code's own session registry (`workspace::claude`), which reports what
/// the hooks could only approximate. See `cli::task::remove_tenx_hooks`.
pub fn install() -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = crate::workspace::find(&cwd)?;

    crate::cli::task::install_hooks(&ws.dir, true)?;
    eprintln!(
        "tenx hooks removed from {} — task state now comes from Claude Code's session registry",
        ws.dir.join(".claude").display()
    );
    Ok(())
}
