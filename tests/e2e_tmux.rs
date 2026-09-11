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
        Self::named("")
    }

    /// Tests in this file run in parallel: each gets its own root, socket and
    /// home, told apart by `tag`.
    fn named(tag: &str) -> Option<Harness> {
        let tmux = find_tmux()?;
        let name = format!("tenx-e2e-{}{tag}", std::process::id());
        let root = std::env::temp_dir().join(&name);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        fs::create_dir_all(root.join("ws/tasks")).unwrap();
        let h = Harness { root, socket: name, tmux };

        // Fake agents/editor: something that stays alive so the pane persists.
        for name in ["claude", "codex", "pi", "nvim"] {
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
            // A desktop-sized window: a detached server defaults to 80×24,
            // where three task panes leave a shell too narrow to print a
            // port number on one line.
            .args(["-f", s(&conf), "new-session", "-d", "-x", "200", "-y", "50", "-s", "tenx", "-n", "home", "-c", s(&h.root), "sleep 600"])
            .status()
            .unwrap();
        assert!(st.success(), "tmux new-session failed (config rejected?)");
        Some(h)
    }

    fn tmux(&self) -> Command {
        let mut c = Command::new(&self.tmux);
        c.args(["-L", &self.socket]);
        // The server's environment is what its panes inherit; anything that
        // runs this build's `tenx` in a pane must see the isolated home.
        c.env("HOME", self.root.join("home"));
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

    /// A second, throwaway tmux server standing in for the user's terminal:
    /// its one pane runs the client, so keys can be sent and the screen read.
    fn outer(&self) -> String {
        format!("{}-outer", self.socket)
    }

    fn outer_tmux(&self) -> Command {
        let mut c = Command::new(&self.tmux);
        c.args(["-L", &self.outer()]);
        c
    }

    fn outer_out(&self, args: &[&str]) -> String {
        let out = self.outer_tmux().args(args).output().unwrap();
        assert!(out.status.success(), "outer tmux {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn screen(&self) -> String {
        self.outer_out(&["capture-pane", "-p", "-t", "o"])
    }

    fn keys(&self, keys: &[&str]) {
        let mut args = vec!["send-keys", "-t", "o"];
        args.extend_from_slice(keys);
        self.outer_out(&args);
    }

    /// Poll the outer screen until `pred` holds, or fail after `secs`.
    fn wait_screen(&self, what: &str, secs: u64, pred: impl Fn(&str) -> bool) -> String {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        loop {
            let s = self.screen();
            if pred(&s) {
                return s;
            }
            if std::time::Instant::now() >= deadline {
                let err = fs::read_to_string(self.root.join("client.err")).unwrap_or_default();
                panic!("waiting for {what}, screen:\n{s}\nclient stderr: {err}");
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.tmux().arg("kill-server").stdout(Stdio::null()).stderr(Stdio::null()).status();
        let _ = Command::new(&self.tmux).args(["-L", &self.outer(), "kill-server"]).stdout(Stdio::null()).stderr(Stdio::null()).status();
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
        let out = h.tenx().args(["task", "list", "--json"]).output().unwrap();
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
        let panes = h.tmux_out(&["list-panes", "-s", "-t", "tenx", "-F", "#{window_name} #{pane_index} w=#{pane_width} h=#{pane_height} #{pane_current_command}"]);
        let out = h.tenx().args(["internal", "ports"]).output().unwrap();
        assert!(found, "port {port} should be attributed to smoke-test\npanes:\n{panes}\nports: {}", String::from_utf8_lossy(&out.stdout));
    }

    // A ticket import: description and links land in TASK.md's own rows.
    let out = h
        .tenx()
        .args([
            "task", "new", "ENG-7: Add login", "--ws-dir", &h.ws(),
            "--description", "Users can log in.",
            "--link", "Linear: https://linear.app/x/ENG-7",
            "--link", "Jira: https://j/8",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "ticket task new: {}", String::from_utf8_lossy(&out.stderr));
    let md = fs::read_to_string(h.root.join("ws/tasks/eng-7-add-login/TASK.md")).unwrap();
    assert!(md.starts_with("# ENG-7: Add login\n\n## Description\n\nUsers can log in.\n\n## Todo\n"), "{md}");
    assert!(md.contains("- Linear: https://linear.app/x/ENG-7\n- PR:\n- Jira: https://j/8\n\n## Notes\n"), "{md}");
    let cfg = fs::read_to_string(h.root.join("ws/config.toml")).unwrap();
    assert!(cfg.contains("schema_version = 1"), "config migrated: {cfg}");

    // Closing the window and re-opening recreates it (a swept task comes back).
    h.tmux_out(&["kill-window", "-t", id]);
    let out = h.tenx().args(["task", "open", "smoke-test", "--ws-dir", &h.ws()]).output().unwrap();
    assert!(out.status.success(), "task open after kill: {}", String::from_utf8_lossy(&out.stderr));
    let windows = h.tmux_out(&["list-windows", "-t", "tenx", "-F", "#{window_name}"]);
    assert!(windows.lines().any(|l| l == "smoke-test"), "recreated: {windows}");
}

/// The client: `tenx` in a terminal draws the task column beside the session
/// it embeds. Driven through a stand-in terminal (a second tmux server whose
/// pane runs the client), since the client owns a real tty.
#[test]
fn client_column_beside_the_embedded_session() {
    let Some(h) = Harness::named("-client") else {
        eprintln!("tmux not installed — skipping e2e");
        return;
    };
    fs::create_dir_all(h.root.join("home/.config/tenx/workspaces.d")).unwrap();
    fs::write(h.root.join("home/.config/tenx/workspaces.d/e2e.toml"), format!("path = \"{}\"\n", h.ws())).unwrap();

    // The client in a 180×40 stand-in terminal, with the isolated home and
    // the fake agent/editor on its PATH, so the windows it opens are inert.
    let path = format!("{}:{}", h.root.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    let q = |v: &str| format!("'{}'", v.replace('\'', "'\\''"));
    let client = format!(
        "env HOME={} TENX_TMUX_SOCKET={} TERM=xterm-256color PATH={} {} 2>{}; sleep 60",
        q(&h.root.join("home").to_string_lossy()),
        q(&h.socket),
        q(&path),
        q(env!("CARGO_BIN_EXE_tenx")),
        q(&h.root.join("client.err").to_string_lossy())
    );
    // Started in the harness root: the client registers the workspace its
    // cwd is in, and `cargo test` runs inside a real one.
    let st = h
        .outer_tmux()
        .args(["-f", "/dev/null", "new-session", "-d", "-x", "180", "-y", "40", "-s", "o", "-c", s(&h.root), &client])
        .status()
        .unwrap();
    assert!(st.success(), "outer tmux new-session failed");
    h.wait_screen("the column", 10, |s| s.contains("Tasks") && s.contains("Repos"));

    for name in ["One", "Two", "Three"] {
        let out = h.tenx().args(["task", "new", name, "--ws-dir", &h.ws()]).output().unwrap();
        assert!(out.status.success(), "task new {name}: {}", String::from_utf8_lossy(&out.stderr));
        std::thread::sleep(std::time::Duration::from_millis(1100)); // distinct creation times keep the order stable
    }
    let current = || h.tmux_out(&["display", "-p", "-t", "tenx", "#{window_name}"]);
    assert_eq!(current(), "three", "the newest task's window is current");
    h.wait_screen("all three rows", 5, |s| s.contains("One") && s.contains("Two") && s.contains("Three"));

    // Ctrl+w: the column takes the keyboard with the cursor on the current
    // task; ↓/↑ switch the window under the terminal, one task per press.
    h.keys(&["C-w"]);
    h.wait_screen("normal mode", 3, |s| s.contains(" NORMAL "));
    h.keys(&["Down"]);
    std::thread::sleep(std::time::Duration::from_millis(700));
    assert_eq!(current(), "two", "Down switches to the task below");
    h.keys(&["Down"]);
    std::thread::sleep(std::time::Duration::from_millis(700));
    assert_eq!(current(), "one");
    h.keys(&["Up"]);
    std::thread::sleep(std::time::Duration::from_millis(700));
    assert_eq!(current(), "two", "Up switches back");
    // The embedded session shows it: tmux's status line names the window.
    h.wait_screen("the embedded status line", 3, |s| s.lines().last().is_some_and(|l| l.contains("two")));

    // ⏎ hands the keyboard to the task; the column's cursor goes away.
    h.keys(&["Enter"]);
    h.wait_screen("insert mode after the jump", 3, |s| s.contains(" INSERT "));

    // A workspace registered while the client runs (what `tenx init` does)
    // is listed without a restart, its tasks included. The column is the
    // only place the *title* can appear above the embedded status line:
    // the panes are fake, and the status line (the last line) shows the
    // window's title on its own, so it must not count.
    let ws2 = h.root.join("ws2");
    fs::create_dir_all(ws2.join("tasks")).unwrap();
    fs::write(
        ws2.join("config.toml"),
        format!("name = \"late\"\nlayout = \"\"\n\n[[repos]]\nname = \"origin\"\nurl = \"{}\"\n", h.root.join("origin.git").display()),
    )
    .unwrap();
    fs::write(h.root.join("home/.config/tenx/workspaces.d/late.toml"), format!("path = \"{}\"\n", ws2.display())).unwrap();
    let out = h.tenx().args(["task", "new", "Four", "--ws-dir", &ws2.to_string_lossy()]).output().unwrap();
    assert!(out.status.success(), "task new Four: {}", String::from_utf8_lossy(&out.stderr));
    h.wait_screen("the late workspace's task in the column", 5, |s| {
        let mut lines: Vec<&str> = s.lines().collect();
        lines.pop();
        lines.iter().any(|l| l.contains("Four"))
    });

    // `:init <path>` creates a workspace from the column: the form takes a
    // first repo, and the column reports the new workspace. On disk: the
    // config, the registry entry, the bare clone, the skills.
    let ws3 = h.root.join("fresh");
    h.keys(&["C-w"]);
    h.wait_screen("normal mode", 3, |s| s.contains(" NORMAL "));
    h.keys(&["-l", &format!(":init {}", ws3.display())]);
    h.keys(&["Enter"]);
    h.wait_screen("the new-workspace form", 3, |s| s.contains(" new workspace "));
    h.keys(&["Tab", "Tab"]); // path (prefilled), name (defaults), git URL
    h.keys(&["-l", &h.root.join("origin.git").to_string_lossy()]);
    h.keys(&["Enter"]);
    h.wait_screen("the fresh workspace created", 15, |s| s.contains("workspace 'fresh' created"));
    assert!(ws3.join("config.toml").exists(), "config written");
    assert!(ws3.join(".bare").join("origin.git").exists(), "repo cloned");
    assert!(ws3.join(".claude/skills/tenx/SKILL.md").exists(), "skills installed");
    assert!(ws3.join("AGENTS.md").exists(), "AGENTS.md written");
    let registered = fs::read_dir(h.root.join("home/.config/tenx/workspaces.d"))
        .unwrap()
        .flatten()
        .filter_map(|e| fs::read_to_string(e.path()).ok())
        .any(|t| t.contains("fresh"));
    assert!(registered, "the new workspace is registered");
    let out = h.tenx().args(["task", "new", "Five", "--ws-dir", &ws3.to_string_lossy()]).output().unwrap();
    assert!(out.status.success(), "task new in the fresh workspace: {}", String::from_utf8_lossy(&out.stderr));
    // Back to the Tasks tab, where the new task shows up on the next
    // refresh, and to the task: Ctrl+w lands on the current task's row,
    // which only the Tasks tab has, and only once the row exists.
    h.keys(&["g", "t"]);
    h.wait_screen("the fresh workspace's task in the column", 5, |s| {
        let mut lines: Vec<&str> = s.lines().collect();
        lines.pop();
        lines.iter().any(|l| l.contains("Five"))
    });
    h.keys(&["q"]);

    // `:q` from the column quits the client; the session lives on.
    h.keys(&["C-w"]);
    h.wait_screen("normal mode", 3, |s| s.contains(" NORMAL "));
    h.keys(&[":q", "Enter"]);
    h.wait_screen("the client to exit", 5, |s| !s.contains("Tasks"));
    let err = fs::read_to_string(h.root.join("client.err")).unwrap_or_default();
    assert!(err.trim().is_empty(), "client stderr: {err}");
    let windows = h.tmux_out(&["list-windows", "-t", "tenx", "-F", "#{window_name}"]);
    assert!(windows.lines().any(|l| l == "two"), "session survives the client: {windows}");
}

#[test]
fn codex_task_launches_and_reports_state_through_the_registry() {
    let Some(h) = Harness::named("-codex") else {
        eprintln!("tmux not installed — skipping e2e");
        return;
    };
    // Register the workspace so `task list --json` enumerates it.
    fs::create_dir_all(h.root.join("home/.config/tenx/workspaces.d")).unwrap();
    fs::write(h.root.join("home/.config/tenx/workspaces.d/e2e.toml"), format!("path = \"{}\"\n", h.ws())).unwrap();

    // A task pinned to Codex launches the fake `codex` in its first pane.
    let out = h.tenx().args(["task", "new", "Cx Task", "--agent", "codex", "--ws-dir", &h.ws()]).output().unwrap();
    assert!(out.status.success(), "task new --agent codex: {}", String::from_utf8_lossy(&out.stderr));
    let task_dir = h.root.join("ws/tasks/cx-task");
    assert_eq!(fs::read_to_string(task_dir.join(".tenx-agent")).unwrap().trim(), "codex");
    let cmds = h.tmux_out(&["list-panes", "-t", "tenx:cx-task", "-F", "#{pane_start_command}"]);
    assert!(cmds.lines().any(|l| l.contains("codex")), "first pane runs codex: {cmds}");

    // `task agent` reports the override.
    let out = h.tenx().args(["task", "agent", "cx-task", "--ws-dir", &h.ws()]).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("codex"), "task agent shows codex");

    // A Codex hook event (keyed by the pane's own pid) drives the task to
    // Working through tenx's registry — the same path a real hook takes.
    let pane_pid = h
        .tmux_out(&["list-panes", "-t", "tenx:cx-task", "-F", "#{pane_pid}"])
        .lines()
        .next()
        .unwrap()
        .to_string();
    let payload = format!(r#"{{"hook_event_name":"UserPromptSubmit","cwd":"{}"}}"#, task_dir.display());
    let mut child = h
        .tenx()
        .args(["internal", "session-event", "--agent", "codex", "--pid", &pane_pid])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
    assert!(child.wait().unwrap().success());

    let mut status = String::new();
    for _ in 0..20 {
        let out = h.tenx().args(["task", "list", "--json"]).output().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        if let Some(task) = json["tasks"].as_array().unwrap().iter().find(|t| t["slug"] == "cx-task") {
            status = task["status"].as_str().unwrap_or_default().to_string();
            assert_eq!(task["agent"].as_str(), Some("codex"), "agent field in json");
            if status == "working" {
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(status, "working", "codex UserPromptSubmit should surface as working");

    // SessionEnd removes the record → the task falls back to idle.
    let payload = format!(r#"{{"hook_event_name":"SessionEnd","cwd":"{}"}}"#, task_dir.display());
    let mut child = h
        .tenx()
        .args(["internal", "session-event", "--agent", "codex", "--pid", &pane_pid])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
    assert!(child.wait().unwrap().success());
    assert!(!h.root.join(format!("home/.config/tenx/sessions/{pane_pid}.json")).exists(), "record deleted on SessionEnd");
}
