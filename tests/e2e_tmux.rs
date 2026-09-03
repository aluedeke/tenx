//! End-to-end: the real `tenx` binary against a real, throwaway tmux server.
//!
//! Everything is isolated — its own socket (`TENX_TMUX_SOCKET`), its own
//! `$HOME` (so the global config and registry are never touched), a local bare
//! repo as the "remote", and fake `claude`/`nvim` on `PATH` so no agent or
//! editor is actually launched. Skips (passes) when tmux isn't installed, so
//! `cargo test` still works on a box without it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Harness {
    root: PathBuf,
    socket: String,
    tmux: PathBuf,
}

impl Harness {
    fn new() -> Option<Harness> {
        let tmux = find_tmux()?;
        let root = std::env::temp_dir().join(format!("tenx-e2e-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        fs::create_dir_all(root.join("ws/tasks")).unwrap();
        let h = Harness { root, socket: format!("tenx-e2e-{}", std::process::id()), tmux };

        // Fake agent/editor: something that stays alive so the pane persists.
        for name in ["claude", "nvim"] {
            let p = h.root.join("bin").join(name);
            fs::write(&p, "#!/bin/sh\nexec sleep 600\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        // A local repo and a bare clone of it standing in for the remote.
        let src = h.root.join("src");
        fs::create_dir_all(&src).unwrap();
        sh(&["git", "-C", s(&src), "init", "-q", "-b", "main"]);
        sh(&["git", "-C", s(&src), "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "init"]);
        let origin = h.root.join("origin.git");
        sh(&["git", "clone", "-q", "--bare", s(&src), s(&origin)]);
        fs::write(
            h.root.join("ws/config.toml"),
            format!("name = \"e2e\"\nlayout = \"\"\n\n[[repos]]\nname = \"origin\"\nurl = \"{}\"\n", origin.display()),
        )
        .unwrap();

        // The server, from the generated config, with a placeholder home window.
        let conf = h.root.join("tmux.conf");
        let out = h.tenx().args(["internal", "tmux-conf"]).output().unwrap();
        assert!(out.status.success(), "tenx internal tmux-conf failed");
        fs::write(&conf, out.stdout).unwrap();
        let st = h
            .tmux()
            .args(["-f", s(&conf), "new-session", "-d", "-s", "tenx", "-n", "home", "-c", s(&h.root), "sleep 600"])
            .status()
            .unwrap();
        assert!(st.success(), "tmux new-session failed (config rejected?)");
        Some(h)
    }

    fn tmux(&self) -> Command {
        let mut c = Command::new(&self.tmux);
        c.args(["-L", &self.socket]);
        c
    }

    fn tenx(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_tenx"));
        let path = format!("{}:{}", self.root.join("bin").display(), std::env::var("PATH").unwrap_or_default());
        c.env("TENX_TMUX_SOCKET", &self.socket).env("HOME", self.root.join("home")).env("PATH", path);
        c.env_remove("TMUX");
        c
    }

    fn tmux_out(&self, args: &[&str]) -> String {
        let out = self.tmux().args(args).output().unwrap();
        assert!(out.status.success(), "tmux {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn ws(&self) -> String {
        self.root.join("ws").to_string_lossy().into_owned()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.tmux().arg("kill-server").stdout(Stdio::null()).stderr(Stdio::null()).status();
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn find_tmux() -> Option<PathBuf> {
    let out = Command::new("sh").args(["-c", "command -v tmux"]).output().ok()?;
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if p.is_empty() {
        return None;
    }
    let ok = Command::new(&p).arg("-V").stdout(Stdio::null()).status().ok()?.success();
    ok.then(|| PathBuf::from(p))
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn sh(args: &[&str]) {
    let st = Command::new(args[0]).args(&args[1..]).status().unwrap();
    assert!(st.success(), "{args:?} failed");
}

#[test]
fn task_new_open_and_list_against_real_tmux() {
    let Some(h) = Harness::new() else {
        eprintln!("tmux not installed — skipping e2e");
        return;
    };

    // task new: worktree + TASK.md + a window with the default three panes.
    let out = h.tenx().args(["task", "new", "Smoke Test!", "--ws-dir", &h.ws()]).output().unwrap();
    assert!(out.status.success(), "task new: {}", String::from_utf8_lossy(&out.stderr));

    let task_dir = h.root.join("ws/tasks/smoke-test");
    assert!(task_dir.join("origin/.git").is_file(), "worktree .git file");
    assert!(fs::read_to_string(task_dir.join("TASK.md")).unwrap().starts_with("# Smoke Test!\n"));

    let windows = h.tmux_out(&["list-windows", "-t", "tenx", "-F", "#{window_id} #{window_name} #{window_panes} #{window_active}"]);
    let smoke = windows.lines().find(|l| l.contains(" smoke-test ")).expect("smoke-test window exists");
    let mut f = smoke.split(' ');
    let id = f.next().unwrap();
    assert_eq!(f.nth(1), Some("3"), "three panes: claude, nvim, shell");
    assert_eq!(f.next(), Some("1"), "new window is the session's current one");
    assert_eq!(fs::read_to_string(task_dir.join(".tenx-window-id")).unwrap().trim(), id);

    let branch = h.tmux_out(&["list-panes", "-t", "tenx:smoke-test", "-F", "#{pane_current_path}"]);
    assert!(branch.lines().all(|l| l.ends_with("smoke-test")), "every pane starts in the task dir: {branch}");

    // task list sees the open window.
    let out = h.tenx().args(["task", "list"]).current_dir(h.root.join("ws")).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Smoke Test!") && text.contains('●'), "task list: {text}");

    // task open from the home window selects the task's window.
    h.tmux_out(&["select-window", "-t", "tenx:home"]);
    let out = h.tenx().args(["task", "open", "smoke-test", "--ws-dir", &h.ws()]).output().unwrap();
    assert!(out.status.success(), "task open: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(h.tmux_out(&["display", "-p", "-t", "tenx", "#{window_name}"]), "smoke-test");

    // The status line reads the pushed window option.
    h.tmux_out(&["set-option", "-w", "-t", "tenx:smoke-test", "@tenx_status", "▷ Smoke"]);
    let left = h.tmux_out(&["display", "-p", "-t", "tenx:smoke-test", "#{T:status-left}"]);
    assert!(left.contains("▷ Smoke"), "status-left: {left}");

    // A bell from any pane in the window, while you're elsewhere, reads as
    // "signaled" — the generic attention channel.
    fs::create_dir_all(h.root.join("home/.config/tenx/workspaces.d")).unwrap();
    fs::write(h.root.join("home/.config/tenx/workspaces.d/e2e.toml"), format!("path = \"{}\"\n", h.ws())).unwrap();
    h.tmux_out(&["select-window", "-t", "tenx:home"]);
    h.tmux_out(&["send-keys", "-t", "tenx:smoke-test.2", "printf '\\a'", "Enter"]);
    let mut status = String::new();
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let out = h.tenx().args(["overlay", "--json"]).output().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let task = json["tasks"].as_array().unwrap().iter().find(|t| t["slug"] == "smoke-test").unwrap().clone();
        status = task["status"].as_str().unwrap_or_default().to_string();
        if status == "signaled" {
            break;
        }
    }
    assert_eq!(status, "signaled", "bell in a pane should surface as signaled");
    assert_eq!(h.tmux_out(&["display", "-p", "-t", "tenx:smoke-test", "#{window_bell_flag}"]), "1");

    // A process listening in one of the task's panes shows up as the task's
    // port (pane pid → descendants → lsof), when python3 is around to listen.
    if Command::new("python3").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success()) {
        let listen = "python3 -c 'import socket,time;s=socket.socket();s.bind((\"127.0.0.1\",0));s.listen();print(\"PORT\",s.getsockname()[1],flush=True);time.sleep(300)'";
        h.tmux_out(&["send-keys", "-t", "tenx:smoke-test.2", listen, "Enter"]);
        let mut port = None;
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let screen = h.tmux_out(&["capture-pane", "-p", "-t", "tenx:smoke-test.2"]);
            port = screen.lines().find_map(|l| l.strip_prefix("PORT ")).and_then(|p| p.trim().parse::<u16>().ok());
            if port.is_some() {
                break;
            }
        }
        let port = port.expect("listener printed its port");
        let mut found = false;
        for _ in 0..30 {
            let out = h.tenx().args(["internal", "ports"]).output().unwrap();
            let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
            if json["smoke-test"].as_array().is_some_and(|ps| ps.iter().any(|p| p.as_u64() == Some(port as u64))) {
                found = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        assert!(found, "port {port} should be attributed to smoke-test");
    }

    // Closing the window and re-opening recreates it (a swept task comes back).
    h.tmux_out(&["kill-window", "-t", id]);
    let out = h.tenx().args(["task", "open", "smoke-test", "--ws-dir", &h.ws()]).output().unwrap();
    assert!(out.status.success(), "task open after kill: {}", String::from_utf8_lossy(&out.stderr));
    let windows = h.tmux_out(&["list-windows", "-t", "tenx", "-F", "#{window_name}"]);
    assert!(windows.lines().any(|l| l == "smoke-test"), "recreated: {windows}");
}
