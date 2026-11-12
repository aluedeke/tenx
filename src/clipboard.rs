//! The text on the clipboard of the machine tenx runs on, through the
//! system's programs: `pbpaste`/`pbcopy` on macOS, else `wl-paste`/`wl-copy`,
//! else `xclip`. Shared by `tenx web`'s clipboard routes (`web::clipboard`,
//! which adds images) and `tenx secrets` (a value from the clipboard).
//! `TENX_WEB_CLIPBOARD` names a file to use instead, for the tests.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

/// A clipboard program and its arguments.
pub(crate) type Program<'a> = (&'static str, &'a [&'a str]);

/// The programs that read or write text, in the order they are tried.
pub(crate) fn programs(write: bool) -> &'static [Program<'static>] {
    if cfg!(target_os = "macos") {
        if write { &[("pbcopy", &[])] } else { &[("pbpaste", &[])] }
    } else if write {
        &[("wl-copy", &[]), ("xclip", &["-selection", "clipboard", "-in"])]
    } else {
        &[("wl-paste", &["--no-newline"]), ("xclip", &["-selection", "clipboard", "-out"])]
    }
}

/// `programs` that can work here: not wl-clipboard in an X11 session —
/// installed beside xclip there, it would fail before xclip got its turn.
pub(crate) fn usable<'a, 'p>(programs: &'a [Program<'p>]) -> impl Iterator<Item = Program<'p>> + 'a {
    let set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    // With neither named (a service's environment), wl-clipboard still
    // finds the default Wayland socket: it keeps its turn.
    let x11_only = set("DISPLAY") && !set("WAYLAND_DISPLAY");
    programs.iter().copied().filter(move |p| !(x11_only && p.0.starts_with("wl-")))
}

pub(crate) fn command((program, args): Program) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args);
    if cfg!(target_os = "macos") {
        cmd.env("LC_ALL", "en_US.UTF-8");
    }
    cmd
}

/// What the first of `programs` that is installed prints.
pub(crate) fn read_with(programs: &[Program]) -> Result<Vec<u8>> {
    let mut last = None;
    for program in usable(programs) {
        match command(program).stdin(Stdio::null()).stderr(Stdio::null()).output() {
            Ok(out) if out.status.success() => return Ok(out.stdout),
            // wl-paste fails on an empty clipboard; that is no reason to try xclip.
            Ok(out) => last = Some(anyhow::anyhow!("{} exited with {}", program.0, out.status)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("run {}", program.0)),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("no clipboard program found ({})", names(programs))))
}

/// Hand `bytes` to the first of `programs` that is installed.
pub(crate) fn write_with(programs: &[Program], bytes: &[u8]) -> Result<()> {
    for program in usable(programs) {
        let mut child = match command(program).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("run {}", program.0)),
        };
        // Dropped after the write, so the program sees the end of its input.
        child.stdin.take().context("clipboard program's stdin")?.write_all(bytes)?;
        let status = child.wait()?;
        if !status.success() {
            bail!("{} exited with {status}", program.0);
        }
        return Ok(());
    }
    bail!("no clipboard program found ({})", names(programs))
}

pub(crate) fn names(programs: &[Program]) -> String {
    programs.iter().map(|p| p.0).collect::<Vec<_>>().join(", ")
}

/// The test's stand-in for the clipboard, when set.
pub(crate) fn file() -> Option<PathBuf> {
    std::env::var_os("TENX_WEB_CLIPBOARD").filter(|v| !v.is_empty()).map(Into::into)
}

/// The text on this machine's clipboard — `tenx secrets` takes a value
/// from it (`cli::secrets::read_masked_line`'s Ctrl-V, `set --clipboard`).
/// Only text: an image there reads as nothing on macOS, as its bytes
/// elsewhere, which `clean_typed_value` then refuses.
pub(crate) fn text() -> Result<String> {
    if let Some(path) = file() {
        return Ok(std::fs::read_to_string(&path).unwrap_or_default());
    }
    get_text()
}

pub(crate) fn get_text() -> Result<String> {
    read_with(programs(false)).map(|out| String::from_utf8_lossy(&out).into_owned())
}

