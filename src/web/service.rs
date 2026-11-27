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

use std::path::{Path, PathBuf};
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

/// The program the service runs: the path of this `tenx` as you reach it —
/// `~/.cargo/bin/tenx`, `/opt/homebrew/bin/tenx` — not with symlinks
/// resolved, since Homebrew's link target is a versioned Cellar directory that
/// an upgrade deletes; or, on macOS, the release's signed `tenx.app` through
/// Homebrew's `opt` link, so the privacy permissions macOS asks for once
/// outlive upgrades (`tenx_core::service::service_program`). Either way
/// replacing the binary and `service restart` is an upgrade.
fn program() -> Result<PathBuf> {
    let bin = crate::tmux::self_bin()?;
    if !cfg!(target_os = "macos") {
        return Ok(bin);
    }
    let real = bin.canonicalize().unwrap_or_else(|_| bin.clone());
    let opt = service::homebrew_opt(&bin);
    let keg = opt.as_ref().and_then(|o| o.canonicalize().ok());
    let app = opt.as_ref().is_some_and(|o| o.join(service::APP_PROGRAM).is_file());
    Ok(service::service_program(&bin, &real, keg.as_deref(), app))
}

/// [`program`], for a `tenx` that is Homebrew's `bin/tenx` (its `opt/tenx`
/// exists) only.
fn homebrew_program() -> Option<PathBuf> {
    let bin = crate::tmux::self_bin().ok()?;
    service::homebrew_opt(&bin).filter(|o| o.exists())?;
    program().ok()
}

/// (Re)load the LaunchAgent at `path`. Replacing an installed one: unload it
/// first (fails harmlessly when it isn't loaded). `bootout` returns before
/// the job is gone, and a `bootstrap` racing it fails with "Input/output
/// error" — so wait for it to disappear, and retry the load a few times.
fn reload_plist(path: &Path) -> Result<()> {
    let domain = format!("gui/{}", uid());
    let target = format!("{domain}/{LABEL}");
    if run(Command::new("launchctl").args(["bootout", &target]))?.status.success() {
        for _ in 0..50 {
            if !run(Command::new("launchctl").args(["print", &target]))?.status.success() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    let mut attempt = 0;
    loop {
        match check(Command::new("launchctl").args(["bootstrap", &domain]).arg(path)) {
            Ok(()) => return Ok(()),
            Err(_) if attempt < 5 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            Err(e) => return Err(e),
        }
    }
}

pub fn install(listen: &str, port: u16, dev_origins: &[String]) -> Result<()> {
    let program = program()?;
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
        reload_plist(&path)?;
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

/// Restart it — onto a replaced binary after an upgrade, on macOS onto the
/// program Homebrew's tenx wants when that changed. Nothing to do (and
/// no error) when it isn't installed, so `make install` can always call it.
pub fn restart() -> Result<()> {
    if cfg!(target_os = "macos") {
        if !plist_path()?.exists() {
            println!("tenx web service not installed — nothing to restart");
            return Ok(());
        }
        // Homebrew's tenx wants another program than the one it runs now —
        // the app arrived with an upgrade, `make install` put a test version
        // in front of it, `make uninstall` took it away: point the plist at
        // it and reload. Never for any other tenx (`cargo run` in a task's
        // checkout): its path goes away with the checkout. And when this
        // tenx can't be located, a plain restart is still the better answer.
        let path = plist_path()?;
        let text = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let want = homebrew_program().map(|p| p.to_string_lossy().into_owned());
        if let Some((have, want)) = want.and_then(|want| Some((service::launchd_program(&text).filter(|have| *have != want)?, want))) {
            let Some(text) = service::with_launchd_program(&text, &want) else { bail!("{}: no ProgramArguments", path.display()) };
            write(&path, &text)?;
            reload_plist(&path)?;
            println!("restarted tenx web, now running {want} (was {have})");
            return Ok(());
        }
        let domain = format!("gui/{}", uid());
        // Installed but not loaded (a failed load, a `bootout` by hand): load
        // it, which starts it.
        if !run(Command::new("launchctl").args(["kickstart", "-k", &format!("{domain}/{LABEL}")]))?.status.success() {
            check(Command::new("launchctl").args(["bootstrap", &domain]).arg(plist_path()?))?;
        }
    } else {
        if !systemd_path()?.exists() {
            println!("tenx web service not installed — nothing to restart");
            return Ok(());
        }
        check(Command::new("systemctl").args(["--user", "restart", SYSTEMD_NAME]))?;
    }
    println!("restarted tenx web");
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
        if let Some(program) = field("program") {
            println!("  runs  {program}");
        }
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
