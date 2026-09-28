//! `tenx web service install|uninstall|status`: `tenx web` as a per-user
//! service — a LaunchAgent on macOS, a systemd user unit on Linux — started
//! at login and restarted if it exits, instead of a terminal or `nohup`
//! keeping it alive. What the files say is `tenx_core::service`.
//!
//! The service starts with launchd's/systemd's bare environment, and if the
//! tmux server isn't up yet `tenx web` starts it — so every pane would
//! inherit that environment. The installing shell's `PATH` (and locale) is
//! written into the unit, so agents, `git` and `gh` resolve as they do for
//! you.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};
use tenx_core::service::{self, Unit};

const LABEL: &str = "io.github.aluedeke.tenx.web";
const SYSTEMD_NAME: &str = "tenx-web.service";

fn home() -> Result<PathBuf> {
    crate::workspace::home_dir()
}

fn log_path() -> Result<PathBuf> {
    Ok(home()?.join(".config").join("tenx").join("web.log"))
}

fn plist_path() -> Result<PathBuf> {
    Ok(home()?.join("Library").join("LaunchAgents").join(format!("{LABEL}.plist")))
}

fn systemd_path() -> Result<PathBuf> {
    Ok(home()?.join(".config").join("systemd").join("user").join(SYSTEMD_NAME))
}

fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

fn run(cmd: &mut Command) -> Result<std::process::Output> {
    let out = cmd.output().with_context(|| format!("run {cmd:?}"))?;
    Ok(out)
}

fn check(cmd: &mut Command) -> Result<()> {
    let out = run(cmd)?;
    if !out.status.success() {
        bail!("{cmd:?} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Write `contents` to `path` atomically (a temp file renamed over it).
fn write(path: &PathBuf, contents: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("write {}", path.display()))
}

pub fn install(listen: &str, port: u16, dev_origins: &[String]) -> Result<()> {
    let program = std::env::current_exe().context("locate the tenx binary")?.canonicalize()?;
    let mut args = vec!["web".to_string(), "--listen".into(), listen.to_string(), "--port".into(), port.to_string()];
    for o in dev_origins {
        args.push("--dev-origin".into());
        args.push(o.clone());
    }
    let mut env = Vec::new();
    for key in ["PATH", "LANG", "LC_ALL", "LC_CTYPE"] {
        if let Ok(v) = std::env::var(key) {
            env.push((key.to_string(), v));
        }
    }
    let log = log_path()?;
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let unit = Unit {
        label: LABEL.into(),
        program: program.to_string_lossy().into_owned(),
        args,
        env,
        log: log.to_string_lossy().into_owned(),
    };

    if cfg!(target_os = "macos") {
        let path = plist_path()?;
        write(&path, &service::launchd_plist(&unit))?;
        let domain = format!("gui/{}", uid());
        // Replacing an installed one: unload it first (fails harmlessly when
        // it isn't loaded).
        let _ = run(Command::new("launchctl").args(["bootout", &format!("{domain}/{LABEL}")]));
        check(Command::new("launchctl").args(["bootstrap", &domain]).arg(&path))?;
        println!("installed {}", path.display());
    } else {
        let path = systemd_path()?;
        write(&path, &service::systemd_unit(&unit))?;
        check(Command::new("systemctl").args(["--user", "daemon-reload"]))?;
        check(Command::new("systemctl").args(["--user", "enable", "--now", SYSTEMD_NAME]))?;
        check(Command::new("systemctl").args(["--user", "restart", SYSTEMD_NAME]))?;
        println!("installed {}", path.display());
        println!("  (to keep it running while you're logged out: loginctl enable-linger)");
    }
    println!("  runs {} {}", unit.program, unit.args.join(" "));
    println!("  log  {}", log.display());
    if let Ok(token) = super::token::load_or_create(false) {
        println!("  open http://{listen}:{port}/?token={token}");
    }
    println!("A `tenx web` you started yourself on the same port must be stopped, or the service can't bind it.");
    Ok(())
}

pub fn uninstall() -> Result<()> {
    if cfg!(target_os = "macos") {
        let path = plist_path()?;
        let _ = run(Command::new("launchctl").args(["bootout", &format!("gui/{}/{LABEL}", uid())]));
        if path.exists() {
            std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
            println!("removed {}", path.display());
        } else {
            println!("not installed");
        }
    } else {
        let path = systemd_path()?;
        let _ = run(Command::new("systemctl").args(["--user", "disable", "--now", SYSTEMD_NAME]));
        if path.exists() {
            std::fs::remove_file(&path)?;
            let _ = run(Command::new("systemctl").args(["--user", "daemon-reload"]));
            println!("removed {}", path.display());
        } else {
            println!("not installed");
        }
    }
    Ok(())
}

pub fn status() -> Result<()> {
    if cfg!(target_os = "macos") {
        let path = plist_path()?;
        if !path.exists() {
            println!("not installed (tenx web service install)");
            return Ok(());
        }
        let out = run(Command::new("launchctl").args(["print", &format!("gui/{}/{LABEL}", uid())]))?;
        if !out.status.success() {
            println!("installed ({}) but not loaded", path.display());
            return Ok(());
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let field = |name: &str| {
            text.lines().map(str::trim).find_map(|l| l.strip_prefix(name).map(|v| v.trim_start_matches([' ', '=']).trim().to_string()))
        };
        println!("installed {}", path.display());
        println!("  state {}", field("state").unwrap_or_else(|| "?".into()));
        if let Some(pid) = field("pid") {
            println!("  pid   {pid}");
        }
        if let Some(code) = field("last exit code") {
            println!("  last exit {code}");
        }
    } else {
        let out = run(Command::new("systemctl").args(["--user", "status", "--no-pager", SYSTEMD_NAME]))?;
        print!("{}", String::from_utf8_lossy(&out.stdout));
    }
    println!("  log   {}", log_path()?.display());
    Ok(())
}
