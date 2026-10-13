//! `POST /clipboard/get` and `POST /clipboard/set`: the clipboard of the
//! machine `tenx web` runs on, for the key bar's two clipboard keys — text
//! copied on a phone goes to the Mac's clipboard, and text copied on the Mac
//! comes to the phone's. Text only.
//!
//! macOS: `pbcopy`/`pbpaste`, with a UTF-8 locale set — under launchd
//! (`tenx web service`) there may be none, and their text encoding follows
//! it. Elsewhere: `wl-copy`/`wl-paste`, else `xclip`. `TENX_WEB_CLIPBOARD`
//! names a file to use instead, for the end-to-end test.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

/// A clipboard program and its arguments, for reading or writing.
type Program = (&'static str, &'static [&'static str]);

fn programs(write: bool) -> &'static [Program] {
    if cfg!(target_os = "macos") {
        if write { &[("pbcopy", &[])] } else { &[("pbpaste", &[])] }
    } else if write {
        &[("wl-copy", &[]), ("xclip", &["-selection", "clipboard", "-in"])]
    } else {
        &[("wl-paste", &["--no-newline"]), ("xclip", &["-selection", "clipboard", "-out"])]
    }
}

fn command((program, args): Program) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args);
    if cfg!(target_os = "macos") {
        cmd.env("LC_ALL", "en_US.UTF-8");
    }
    cmd
}

/// The test's stand-in for the clipboard, when set.
fn file() -> Option<std::path::PathBuf> {
    std::env::var_os("TENX_WEB_CLIPBOARD").filter(|v| !v.is_empty()).map(Into::into)
}

/// The text on this machine's clipboard (empty when it holds none).
pub(super) fn get() -> Result<String> {
    if let Some(path) = file() {
        return Ok(std::fs::read_to_string(&path).unwrap_or_default());
    }
    let mut last = None;
    for &program in programs(false) {
        match command(program).stdin(Stdio::null()).stderr(Stdio::null()).output() {
            Ok(out) if out.status.success() => return Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
            // wl-paste fails on an empty clipboard; that is no reason to try xclip.
            Ok(out) => last = Some(anyhow::anyhow!("{} exited with {}", program.0, out.status)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("run {}", program.0)),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("no clipboard program found ({})", names(false))))
}

/// Put `text` on this machine's clipboard.
pub(super) fn set(text: &str) -> Result<()> {
    if let Some(path) = file() {
        return std::fs::write(&path, text).with_context(|| format!("write {}", path.display()));
    }
    for &program in programs(true) {
        let mut child = match command(program).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("run {}", program.0)),
        };
        // Dropped after the write, so the program sees the end of its input.
        child.stdin.take().context("clipboard program's stdin")?.write_all(text.as_bytes())?;
        let status = child.wait()?;
        if !status.success() {
            bail!("{} exited with {status}", program.0);
        }
        return Ok(());
    }
    bail!("no clipboard program found ({})", names(true))
}

fn names(write: bool) -> String {
    programs(write).iter().map(|p| p.0).collect::<Vec<_>>().join(", ")
}
