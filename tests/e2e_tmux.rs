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
        // One spelling of every path: on macOS the temp dir is behind a
        // symlink (/var → /private/var), and a command run from inside the
        // workspace registers its canonical path.
        let root = root.canonicalize().unwrap();
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

    // An OSC 8 link printed in a pane reaches the real terminal as a link:
    // through tmux (the `hyperlinks` feature) and the client's emulator.
    // Written to the pane's tty, as its program would print it.
    let tty = h.tmux_out(&["display", "-p", "-t", "tenx", "#{pane_tty}"]);
    fs::write(&tty, "\x1b]8;;https://example.com/e2e\x1b\\LINKED\x1b]8;;\x1b\\\r\n").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let s = h.outer_out(&["capture-pane", "-p", "-e", "-t", "o"]);
        // Opened before the text and closed after it; tmux may put SGR in
        // between.
        if let Some((_, rest)) = s.split_once("\x1b]8;;https://example.com/e2e\x1b\\LINKED")
            && rest.split("\x1b]8;;\x1b\\").next().is_some_and(|between| !between.contains(' '))
            && rest.contains("\x1b]8;;\x1b\\")
        {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "the link never reached the outer terminal:\n{s}");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

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
    // Three tabs now (Tasks │ Repos │ Work), so Repos → Work → Tasks.
    h.keys(&["g", "t"]);
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

/// The bug this guards: windows are named by task slug, and a slug is only
/// unique *within* a workspace. `sweep` used to find a task's window by name
/// alone, across every registered workspace — so a dormant task in one
/// workspace resolved to `Idle`, matched a namesake's window in another, and
/// closed a live session mid-turn.
#[test]
fn sweep_never_closes_a_namesake_window_from_another_workspace() {
    let Some(h) = Harness::named("-sweep") else {
        eprintln!("tmux not installed — skipping e2e");
        return;
    };

    // The live one: a real window whose panes sit in its own task dir.
    let out = h.tenx().args(["task", "new", "Dup", "--ws-dir", &h.ws()]).output().unwrap();
    assert!(out.status.success(), "task new: {}", String::from_utf8_lossy(&out.stderr));
    let windows = h.tmux_out(&["list-windows", "-t", "tenx", "-F", "#{window_id} #{window_name}"]);
    let live_id = windows.lines().find(|l| l.ends_with(" dup")).expect("dup window").split(' ').next().unwrap().to_string();

    // The decoy: same slug, another workspace, never opened — so it has no
    // window of its own and resolves to `Idle` forever.
    let ws2 = h.root.join("ws2");
    fs::create_dir_all(ws2.join("tasks")).unwrap();
    fs::write(
        ws2.join("config.toml"),
        format!("name = \"other\"\nlayout = \"\"\n\n[[repos]]\nname = \"origin\"\nurl = \"{}\"\n", h.root.join("origin.git").display()),
    )
    .unwrap();
    let out = h.tenx().args(["task", "new", "Dup", "--ws-dir", s(&ws2), "--no-open"]).output().unwrap();
    assert!(out.status.success(), "decoy task new: {}", String::from_utf8_lossy(&out.stderr));
    assert!(ws2.join("tasks/dup").is_dir(), "decoy task dir exists");

    // Visiting the window clears tmux's bell/activity flags (so the task
    // reads as plain `Idle`, not "signaled"), and leaving it means it's no
    // longer the current window — both are spared for reasons of their own,
    // which would hide everything this test is about.
    h.tmux_out(&["select-window", "-t", "tenx:dup"]);
    h.tmux_out(&["select-window", "-t", "tenx:home"]);

    // Both registered, so sweep walks both.
    let reg = h.root.join("home/.config/tenx/workspaces.d");
    fs::create_dir_all(&reg).unwrap();
    fs::write(reg.join("e2e.toml"), format!("path = \"{}\"\n", h.ws())).unwrap();
    fs::write(reg.join("other.toml"), format!("path = \"{}\"\n", ws2.display())).unwrap();

    // `--idle-after 0s` waives the grace period, so what's left is the window
    // *identity* on its own — with the grace in place the seconds-old window
    // would survive either way and the real bug would go unnoticed.
    let out = h.tenx().args(["task", "sweep", "--idle-after", "0s", "--dry-run"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains("other/"), "the decoy owns no window and must name none: {text}");
    // The positive control: the window really is idle, and its *own* task
    // still finds it — the fix rejects the wrong window, not every window.
    assert!(text.contains("e2e/Dup"), "the task that owns the window still sweeps it: {text}");

    // With the grace period back, a real sweep leaves the seconds-old window
    // standing — `Idle` alone is no longer enough to close anything.
    let out = h.tenx().args(["task", "sweep"]).output().unwrap();
    assert!(out.status.success(), "sweep: {}", String::from_utf8_lossy(&out.stderr));
    let windows = h.tmux_out(&["list-windows", "-t", "tenx", "-F", "#{window_id} #{window_name}"]);
    assert!(windows.contains(&format!("{live_id} dup")), "the live window survives the sweep: {windows}");
}


/// The other half of the slug-collision bug: `task open` correlated to a
/// window by name too, so opening a task in one workspace raised — and
/// rewrote the cached window id of — a namesake's live window in another.
/// That's also why such a task could never acquire a session of its own.
#[test]
fn opening_a_task_never_raises_a_namesake_window_from_another_workspace() {
    let Some(h) = Harness::named("-open") else {
        eprintln!("tmux not installed — skipping e2e");
        return;
    };

    let out = h.tenx().args(["task", "new", "Dup", "--ws-dir", &h.ws()]).output().unwrap();
    assert!(out.status.success(), "task new: {}", String::from_utf8_lossy(&out.stderr));
    let first_id = fs::read_to_string(h.root.join("ws/tasks/dup/.tenx-window-id")).unwrap().trim().to_string();

    // A second workspace with the same slug, not opened.
    let ws2 = h.root.join("ws2");
    fs::create_dir_all(ws2.join("tasks")).unwrap();
    fs::write(
        ws2.join("config.toml"),
        format!("name = \"other\"\nlayout = \"\"\n\n[[repos]]\nname = \"origin\"\nurl = \"{}\"\n", h.root.join("origin.git").display()),
    )
    .unwrap();
    let out = h.tenx().args(["task", "new", "Dup", "--ws-dir", s(&ws2), "--no-open"]).output().unwrap();
    assert!(out.status.success(), "decoy task new: {}", String::from_utf8_lossy(&out.stderr));

    // Opening the second one must give it a window of its own.
    h.tmux_out(&["select-window", "-t", "tenx:home"]);
    let out = h.tenx().args(["task", "open", "dup", "--ws-dir", s(&ws2)]).output().unwrap();
    assert!(out.status.success(), "task open: {}", String::from_utf8_lossy(&out.stderr));

    let second_id = fs::read_to_string(ws2.join("tasks/dup/.tenx-window-id")).unwrap().trim().to_string();
    assert_ne!(second_id, first_id, "the second task must not adopt the first's window");
    assert_eq!(h.tmux_out(&["display", "-p", "-t", "tenx", "#{window_id}"]), second_id, "and it's the one we landed on");

    // Both windows exist, each tagged with the task it was opened for.
    let tagged = h.tmux_out(&["list-windows", "-t", "tenx", "-F", "#{window_id}\t#{@tenx_task_dir}"]);
    let dir_of = |id: &str| {
        tagged
            .lines()
            .find(|l| l.starts_with(&format!("{id}\t")))
            .and_then(|l| l.split_once('\t'))
            .map(|(_, d)| d.to_string())
            .unwrap_or_default()
    };
    assert!(dir_of(&first_id).ends_with("ws/tasks/dup"), "first window tagged with its task: {tagged}");
    assert!(dir_of(&second_id).ends_with("ws2/tasks/dup"), "second window tagged with its task: {tagged}");

    // And reopening the first still finds the first, not the newer namesake.
    let out = h.tenx().args(["task", "open", "dup", "--ws-dir", &h.ws()]).output().unwrap();
    assert!(out.status.success(), "reopen: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(h.tmux_out(&["display", "-p", "-t", "tenx", "#{window_id}"]), first_id, "reopening the original raises the original");
}

/// Sessions outside a repo: a task with no worktrees in an ordinary
/// workspace, `tenx ask` in the adhoc workspace tenx creates for itself,
/// and driving a session from outside — `task send` / `wait` / `output`.
#[test]
fn repo_less_and_adhoc_sessions_are_tasks_that_can_be_driven() {
    let Some(h) = Harness::named("-adhoc") else {
        eprintln!("tmux not installed — skipping e2e");
        return;
    };
    // A fake claude that records how it was launched and keeps whatever is
    // typed into it, so a test can see both the first prompt and a `send`.
    let claude = h.root.join("bin/claude");
    fs::write(&claude, "#!/bin/sh\nprintf '%s\\n' \"$@\" > .agent-args\nexec cat > .agent-input\n").unwrap();
    // Registered, so the adhoc session gets it as a readable directory.
    fs::create_dir_all(h.root.join("home/.config/tenx/workspaces.d")).unwrap();
    fs::write(h.root.join("home/.config/tenx/workspaces.d/e2e.toml"), format!("path = \"{}\"\n", h.ws())).unwrap();
    let wait_file = |p: &Path, what: &str, pred: &dyn Fn(&str) -> bool| -> String {
        for _ in 0..50 {
            if let Ok(t) = fs::read_to_string(p)
                && pred(&t)
            {
                return t;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("{what}: {}", fs::read_to_string(p).unwrap_or_default());
    };
    let panes = |slug: &str| h.tmux_out(&["list-panes", "-t", &format!("tenx:{slug}"), "-F", "#{pane_id}"]).lines().count();

    // A repo-less task in an ordinary workspace: no worktree, the agent alone
    // in its window, and the workspace readable from it.
    let out = h.tenx().args(["task", "new", "Plain Q", "--no-repos", "--no-focus", "--ws-dir", &h.ws()]).output().unwrap();
    assert!(out.status.success(), "task new --no-repos: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "plain-q", "prints the slug");
    let plain = h.root.join("ws/tasks/plain-q");
    assert!(!plain.join("origin").exists(), "no worktree");
    assert_eq!(panes("plain-q"), 1, "agent-only window");
    let args = wait_file(&plain.join(".agent-args"), "agent args", &|t| t.contains("--add-dir="));
    assert!(args.contains(&format!("--add-dir={}", h.ws())), "the workspace is readable: {args}");
    let current = h.tmux_out(&["display-message", "-p", "-t", "tenx", "#{window_name}"]);
    assert_ne!(current, "plain-q", "--no-focus leaves the current window alone");

    // `tenx ask`: the adhoc workspace appears, registered, with its skills.
    let out = h.tenx().args(["ask", "--no-focus", "how", "does", "sweep", "work?"]).output().unwrap();
    assert!(out.status.success(), "ask: {}", String::from_utf8_lossy(&out.stderr));
    let slug = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(slug, "how-does-sweep-work");
    let adhoc = h.root.join("home/.local/share/tenx/adhoc");
    let cfg = fs::read_to_string(adhoc.join("config.toml")).unwrap();
    assert!(cfg.contains("kind = \"adhoc\""), "config: {cfg}");
    assert!(adhoc.join(".claude/skills/orchestrate/SKILL.md").is_file(), "orchestrate skill");
    assert!(fs::read_to_string(adhoc.join(".claude/settings.json")).unwrap().contains("tenx task send"));
    // Canonical, as the registry (and a real agent's reported cwd) has it.
    let task = adhoc.join("tasks").join(&slug).canonicalize().unwrap();
    assert_eq!(fs::read_to_string(task.join("TASK.md")).unwrap().lines().next(), Some("# how does sweep work?"));
    let args = wait_file(&task.join(".agent-args"), "ask args", &|t| t.contains("how does sweep work?"));
    assert!(args.contains(&h.ws()), "every registered workspace is readable: {args}");
    assert!(!task.join(".tenx-prompt").exists(), "the prompt is consumed by the launch");
    assert_eq!(panes(&slug), 1, "agent-only window");
    let json: serde_json::Value = serde_json::from_slice(&h.tenx().args(["task", "list", "--json"]).output().unwrap().stdout).unwrap();
    assert!(json["workspaces"].as_array().unwrap().iter().any(|w| w["adhoc"] == true), "listed as adhoc");
    assert!(json["tasks"].as_array().unwrap().iter().any(|t| t["slug"] == slug.as_str() && t["ws"] == "adhoc"));

    // The same question again counts up instead of failing.
    let out = h.tenx().args(["ask", "--no-focus", "how does sweep work?"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), format!("{slug}-2"));

    // No repos in the adhoc workspace.
    let out = h.tenx().args(["repo", "add", "file:///nowhere.git", "--ws-dir", "adhoc"]).output().unwrap();
    assert!(!out.status.success(), "repo add into the adhoc workspace is refused");

    // send: typed into the agent's pane, addressed by workspace name.
    let out = h.tenx().args(["task", "send", &slug, "--ws-dir", "adhoc", "and", "idle", "windows?"]).output().unwrap();
    assert!(out.status.success(), "send: {}", String::from_utf8_lossy(&out.stderr));
    wait_file(&task.join(".agent-input"), "sent text", &|t| t.contains("and idle windows?"));

    // wait: Working keeps it waiting until the timeout (exit 3); once the
    // turn is over it returns 0 at once.
    let pane_pid = h.tmux_out(&["list-panes", "-t", &format!("tenx:{slug}"), "-F", "#{pane_pid}"]);
    let event = |name: &str| {
        let payload = format!(r#"{{"hook_event_name":"{name}","cwd":"{}","session_id":"s1"}}"#, task.display());
        let mut child = h
            .tenx()
            .args(["internal", "session-event", "--agent", "claude", "--pid", pane_pid.trim()])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
        assert!(child.wait().unwrap().success());
    };
    event("UserPromptSubmit");
    let out = h.tenx().args(["task", "wait", &slug, "--timeout", "2s"]).current_dir(&task).output().unwrap();
    assert_eq!(out.status.code(), Some(3), "times out while working: {}", String::from_utf8_lossy(&out.stderr));
    event("Stop");
    let out = h.tenx().args(["task", "wait", &slug, "--timeout", "5s"]).current_dir(&task).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "settles once the turn is over: {}", String::from_utf8_lossy(&out.stderr));

    // output: the replies since the last prompt, from the transcript.
    let project = h.root.join("home/.claude/projects").join(
        task.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect::<String>(),
    );
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("s1.jsonl"),
        [
            r#"{"type":"user","message":{"content":"how does sweep work?"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"old"}]}}"#,
            r#"{"type":"user","message":{"content":"and idle windows?"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"After 15 minutes."}]}}"#,
        ]
        .join("\n"),
    )
    .unwrap();
    let out = h.tenx().args(["task", "output", &slug, "--ws-dir", "adhoc"]).output().unwrap();
    assert!(out.status.success(), "output: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "After 15 minutes.");
}

/// What `tenx web` builds on (`tmux::new_grouped_session`,
/// `select_window_in`, `kill_session`): a session grouped with `tenx` has the
/// same windows but a current window of its own, so a browser tab switching
/// tasks leaves the terminal client where it is — and killing the tab's
/// session closes no task window.
#[test]
fn a_grouped_session_switches_windows_on_its_own() {
    let Some(h) = Harness::named("-grouped") else {
        eprintln!("tmux not installed — skipping e2e");
        return;
    };
    let out = h.tenx().args(["task", "new", "Grouped", "--ws-dir", &h.ws()]).output().unwrap();
    assert!(out.status.success(), "task new: {}", String::from_utf8_lossy(&out.stderr));
    let home = h.tmux_out(&["display-message", "-p", "-t", "tenx:home", "#{window_id}"]);
    let task = h.tmux_out(&["display-message", "-p", "-t", "tenx:grouped", "#{window_id}"]);
    let current = |session: &str| h.tmux_out(&["display-message", "-p", "-t", session, "#{window_id}"]);
    h.tmux_out(&["select-window", "-t", &format!("tenx:{task}")]);
    assert_eq!(current("tenx"), task);

    // The calls `new_grouped_session` makes. tmux starts a grouped session
    // on the group's first window, so it has to be moved to tenx's current.
    h.tmux_out(&["new-session", "-d", "-t", "tenx", "-s", "tenx-web-1"]);
    assert_eq!(current("tenx-web-1"), home, "tmux starts it on the first window");
    h.tmux_out(&["select-window", "-t", &format!("tenx-web-1:{}", current("tenx"))]);
    assert_eq!(current("tenx-web-1"), task);

    // `select_window_in`: only the grouped session moves.
    h.tmux_out(&["select-window", "-t", &format!("tenx-web-1:{home}")]);
    assert_eq!(current("tenx-web-1"), home);
    assert_eq!(current("tenx"), task, "the tenx session keeps its window");

    // A window opened in one is in both.
    let names = |session: &str| h.tmux_out(&["list-windows", "-t", session, "-F", "#{window_name}"]);
    h.tmux_out(&["new-window", "-d", "-t", "tenx", "-n", "later"]);
    assert_eq!(names("tenx"), names("tenx-web-1"));

    // `kill_session`: the windows stay.
    h.tmux_out(&["kill-session", "-t", "=tenx-web-1"]);
    assert!(names("tenx").contains("grouped"));
    assert!(names("tenx").contains("later"));
}

/// A raw HTTP/1.1 GET against `tenx web`: the status line and headers, and
/// the body.
fn http_get(port: u16, path: &str, cookie: Option<&str>) -> (String, String) {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    let cookie = cookie.map(|c| format!("Cookie: tenx_web={c}\r\n")).unwrap_or_default();
    write!(s, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{cookie}Connection: close\r\n\r\n").unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let (head, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
    (head.to_string(), body.to_string())
}

/// `POST path` with a body, an `Origin` and maybe the cookie; the head and body.
fn http_post(port: u16, path: &str, origin: &str, cookie: Option<&str>, content_type: &str, body: &[u8]) -> (String, String) {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    let cookie = cookie.map(|c| format!("Cookie: tenx_web={c}\r\n")).unwrap_or_default();
    write!(
        s,
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: {origin}\r\n{cookie}Content-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    s.write_all(body).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let (head, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
    (head.to_string(), body.to_string())
}

/// `tenx web` end to end: the token becomes a cookie, the page and the
/// socket refuse whoever lacks it or comes from another origin, and a
/// socket gets a grouped session of its own, a column that answers its keys,
/// the same session back when it reconnects in time — and the session is
/// gone once the grace period passes without one.
#[test]
fn web_serves_the_column_over_a_socket_with_a_session_of_its_own() {
    use futures_util::{SinkExt, StreamExt};
    use std::io::BufRead;
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

    let Some(h) = Harness::named("-web") else {
        eprintln!("tmux not installed — skipping e2e");
        return;
    };
    let out = h.tenx().args(["task", "new", "Web task", "--ws-dir", &h.ws()]).output().unwrap();
    assert!(out.status.success(), "task new: {}", String::from_utf8_lossy(&out.stderr));
    fs::create_dir_all(h.root.join("home/.config/tenx/workspaces.d")).unwrap();
    fs::write(h.root.join("home/.config/tenx/workspaces.d/e2e.toml"), format!("path = \"{}\"\n", h.ws())).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut server = h
        .tenx()
        .args(["web", "--port", &port.to_string()])
        .env("TENX_WEB_GRACE_MS", "800")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // The address it prints carries the token.
    let mut lines = std::io::BufReader::new(server.stdout.take().unwrap()).lines();
    let token = lines
        .by_ref()
        .map_while(Result::ok)
        .find_map(|l| l.split_once("?token=").map(|(_, t)| t.trim().to_string()))
        .expect("tenx web printed its address");
    let token_file = fs::read_to_string(h.root.join("home/.config/tenx/web-token")).unwrap();
    assert_eq!(token_file.trim(), token);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(h.root.join("home/.config/tenx/web-token")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the token is readable by you alone");
    }

    // The page: nothing without the cookie; the token swapped for it.
    assert!(http_get(port, "/", None).0.starts_with("HTTP/1.1 401"));
    assert!(http_get(port, "/?token=nope", None).0.starts_with("HTTP/1.1 401"));
    let (head, _) = http_get(port, &format!("/?token={token}"), None);
    assert!(head.starts_with("HTTP/1.1 303"), "{head}");
    assert!(head.to_lowercase().contains(&format!("set-cookie: tenx_web={token};")), "{head}");
    let (head, body) = http_get(port, "/", Some(&token));
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    // A signed-in visit renews the year-long cookie, so a page in use never signs out.
    assert!(head.to_lowercase().contains(&format!("set-cookie: tenx_web={token};")), "{head}");
    assert!(body.contains("<html") || body.contains("<!DOCTYPE") || body.contains("<!doctype"));

    // A pasted image: saved for the agent, 600, under the config dir — and
    // refused without the cookie, from another origin, or when not an image.
    let page_origin = format!("http://127.0.0.1:{port}");
    let png = b"\x89PNG\r\n\x1a\nfake";
    assert!(http_post(port, "/paste", &page_origin, None, "image/png", png).0.starts_with("HTTP/1.1 401"));
    assert!(http_post(port, "/paste", "http://evil.example", Some(&token), "image/png", png).0.starts_with("HTTP/1.1 403"));
    assert!(http_post(port, "/paste", &page_origin, Some(&token), "text/html", b"<script>").0.starts_with("HTTP/1.1 415"));
    let (head, body) = http_post(port, "/paste", &page_origin, Some(&token), "image/png", png);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    let saved: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
    let saved = std::path::PathBuf::from(saved["path"].as_str().unwrap());
    assert!(saved.starts_with(h.root.join("home/.config/tenx/web-paste")), "{}", saved.display());
    assert_eq!(fs::read(&saved).unwrap(), png);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&saved).unwrap().permissions().mode() & 0o777, 0o600);
    }

    let rt = tokio::runtime::Runtime::new().unwrap();
    let request = |origin: &str, cookie: Option<&str>, session: Option<&str>| {
        let url = match session {
            Some(id) => format!("ws://127.0.0.1:{port}/ws?session={id}"),
            None => format!("ws://127.0.0.1:{port}/ws"),
        };
        let mut req = url.into_client_request().unwrap();
        req.headers_mut().insert("Origin", origin.parse().unwrap());
        if let Some(c) = cookie {
            req.headers_mut().insert("Cookie", format!("tenx_web={c}").parse().unwrap());
        }
        req
    };
    let page = format!("http://127.0.0.1:{port}");

    rt.block_on(async {
        // Refused: another origin, even with the cookie; no cookie.
        let err = tokio_tungstenite::connect_async(request("http://evil.example", Some(&token), None)).await.unwrap_err();
        assert!(err.to_string().contains("403"), "{err}");
        let err = tokio_tungstenite::connect_async(request(&page, None, None)).await.unwrap_err();
        assert!(err.to_string().contains("401"), "{err}");

        // Text frames until one of `kind`.
        async fn next_of<S>(ws: &mut S, kind: &str) -> serde_json::Value
        where
            S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
        {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
            loop {
                let msg = tokio::time::timeout_at(deadline, ws.next()).await.expect("timed out").unwrap().unwrap();
                if let Message::Text(t) = msg {
                    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                    if v["type"] == kind {
                        return v;
                    }
                }
            }
        }

        let (mut ws, _) = tokio_tungstenite::connect_async(request(&page, Some(&token), None)).await.unwrap();
        let hello = next_of(&mut ws, "hello").await;
        let id = hello["session"].as_str().unwrap().to_string();
        let first = next_of(&mut ws, "view").await;
        assert!(first["view"]["items"].to_string().contains("Web task"), "{first}");
        assert_eq!(first["view"]["filter"], "");

        // The terminal: attached once the page says its size.
        ws.send(Message::Text(r#"{"type":"resize","cols":120,"rows":40}"#.into())).await.unwrap();
        let mut got_bytes = false;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while !got_bytes {
            if let Message::Binary(_) = tokio::time::timeout_at(deadline, ws.next()).await.expect("no terminal output").unwrap().unwrap() {
                got_bytes = true;
            }
        }

        // A key goes through the column's own handler: typed into the
        // search field, where the column starts.
        ws.send(Message::Text(r#"{"type":"key","key":"w","ctrl":false,"alt":false,"shift":false}"#.into())).await.unwrap();
        let typed = loop {
            let v = next_of(&mut ws, "view").await;
            if v["view"]["filter"] == "w" {
                break v;
            }
        };
        assert!(typed["view"]["items"].to_string().contains("Web task"));

        // A numbered input is acknowledged in a view — even one that changes
        // nothing in the column (Shift alone), so the page can drop its
        // prediction for it.
        ws.send(Message::Text(r#"{"type":"key","key":"Shift","ctrl":false,"alt":false,"shift":true,"seq":7}"#.into())).await.unwrap();
        loop {
            let v = next_of(&mut ws, "view").await;
            if v["ack"] == 7 {
                break;
            }
        }

        // The layout rule, for a phone and a desktop.
        ws.send(Message::Text(r#"{"type":"viewport","cols":60}"#.into())).await.unwrap();
        assert_eq!(next_of(&mut ws, "layout").await["narrow"], true);
        ws.send(Message::Text(r#"{"type":"viewport","cols":220}"#.into())).await.unwrap();
        assert_eq!(next_of(&mut ws, "layout").await["narrow"], false);
        ws.close(None).await.unwrap();
        drop(ws);

        // Back within the grace period: the same session, the column as it
        // was left.
        let (mut ws, _) = tokio_tungstenite::connect_async(request(&page, Some(&token), Some(&id))).await.unwrap();
        assert_eq!(next_of(&mut ws, "hello").await["session"], id.as_str());
        assert_eq!(next_of(&mut ws, "view").await["view"]["filter"], "w");
        ws.close(None).await.unwrap();
    });

    let session = format!("tenx-web-{id}", id = {
        // The id is in the session list: exactly one tab session exists.
        let list = h.tmux_out(&["list-sessions", "-F", "#{session_name}"]);
        let names: Vec<&str> = list.lines().filter(|l| l.starts_with("tenx-web-")).collect();
        assert_eq!(names.len(), 1, "one tab, one session: {list}");
        names[0].trim_start_matches("tenx-web-").to_string()
    });
    assert!(h.tmux_out(&["list-windows", "-t", &session, "-F", "#{window_name}"]).contains("web-task"));

    // Past the grace period: the session is gone, the task window is not.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while h.tmux_out(&["list-sessions", "-F", "#{session_name}"]).contains(&session) {
        assert!(std::time::Instant::now() < deadline, "{session} outlived its grace period");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(h.tmux_out(&["list-windows", "-t", "tenx", "-F", "#{window_name}"]).contains("web-task"));

    let _ = server.kill();
    let _ = server.wait();
    let _ = h.tmux().args(["kill-server"]).status();
}

/// A fake push service on a loopback port: every request it gets (head,
/// body) goes down the channel, and it answers `201 Created`.
fn fake_push_service() -> (u16, std::sync::mpsc::Receiver<(String, Vec<u8>)>) {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut head = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                head.push_str(&line);
            }
            let len = head
                .lines()
                .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                .unwrap_or(0);
            let mut body = vec![0u8; len];
            reader.read_exact(&mut body).unwrap();
            let mut s = stream;
            let _ = s.write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            if tx.send((head, body)).is_err() {
                break;
            }
        }
    });
    (port, rx)
}

/// Web Push end to end: a browser's subscription is stored (and refused
/// without the cookie, from another origin, or to a non-push endpoint), the
/// test route pushes, and a task going Blocked pushes on its own — each an
/// RFC 8291 message the subscription's key decrypts, signed with the
/// server's VAPID key for the push service's origin.
#[test]
fn web_pushes_a_blocked_task_to_subscribed_browsers() {
    use std::io::{BufRead, Write};
    use tenx_core::webpush;

    let Some(h) = Harness::named("-push") else {
        eprintln!("tmux not installed — skipping e2e");
        return;
    };
    let out = h.tenx().args(["task", "new", "Push task", "--ws-dir", &h.ws()]).output().unwrap();
    assert!(out.status.success(), "task new: {}", String::from_utf8_lossy(&out.stderr));
    fs::create_dir_all(h.root.join("home/.config/tenx/workspaces.d")).unwrap();
    fs::write(h.root.join("home/.config/tenx/workspaces.d/e2e.toml"), format!("path = \"{}\"\n", h.ws())).unwrap();

    let (push_port, pushes) = fake_push_service();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut server = h.tenx().args(["web", "--port", &port.to_string()]).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut lines = std::io::BufReader::new(server.stdout.take().unwrap()).lines();
    let token = lines
        .by_ref()
        .map_while(Result::ok)
        .find_map(|l| l.split_once("?token=").map(|(_, t)| t.trim().to_string()))
        .expect("tenx web printed its address");
    let page = format!("http://127.0.0.1:{port}");

    // The server's key, for a page that has the cookie.
    assert!(http_get(port, "/push/key", None).0.starts_with("HTTP/1.1 401"));
    let (head, body) = http_get(port, "/push/key", Some(&token));
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    let key: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
    let server_key = webpush::unb64(key["key"].as_str().unwrap()).unwrap();
    assert_eq!(server_key.len(), 65);

    // The browser's side of a subscription.
    let ua_private = webpush::unb64("q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94").unwrap();
    let auth = b"sixteen byte key";
    let endpoint = format!("http://127.0.0.1:{push_port}/push/e2e");
    let sub = serde_json::json!({
        "endpoint": endpoint,
        "expirationTime": null,
        "keys": { "p256dh": webpush::b64(&webpush::public_key(&ua_private).unwrap()), "auth": webpush::b64(auth) },
    })
    .to_string();
    let json = "application/json";
    assert!(http_post(port, "/push/subscribe", &page, None, json, sub.as_bytes()).0.starts_with("HTTP/1.1 401"));
    assert!(http_post(port, "/push/subscribe", "http://evil.example", Some(&token), json, sub.as_bytes()).0.starts_with("HTTP/1.1 403"));
    let elsewhere = sub.replace(&endpoint, "http://192.0.2.1/push");
    assert!(http_post(port, "/push/subscribe", &page, Some(&token), json, elsewhere.as_bytes()).0.starts_with("HTTP/1.1 400"));
    let (head, _) = http_post(port, "/push/subscribe", &page, Some(&token), json, sub.as_bytes());
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    let subs_file = h.root.join("home/.config/tenx/web-push-subs.json");
    assert!(fs::read_to_string(&subs_file).unwrap().contains(&endpoint));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&subs_file).unwrap().permissions().mode() & 0o777, 0o600);
        let vapid = h.root.join("home/.config/tenx/web-push-vapid");
        assert_eq!(fs::metadata(&vapid).unwrap().permissions().mode() & 0o777, 0o600);
    }

    // What a push looks like to the push service, and to the browser.
    let receive = |why: &str| -> serde_json::Value {
        let (head, body) = pushes.recv_timeout(std::time::Duration::from_secs(20)).unwrap_or_else(|_| panic!("no push for {why}"));
        let lower = head.to_lowercase();
        assert!(lower.starts_with("post /push/e2e "), "{head}");
        assert!(lower.contains("content-encoding: aes128gcm"), "{head}");
        assert!(lower.contains("ttl: "), "{head}");
        let auth_header = head.lines().find(|l| l.to_lowercase().starts_with("authorization:")).expect("Authorization");
        let (_, value) = auth_header.split_once(':').unwrap();
        let value = value.trim();
        let jwt = value.strip_prefix("vapid t=").unwrap().split(',').next().unwrap();
        assert!(value.ends_with(&format!("k={}", webpush::b64(&server_key))), "{value}");
        assert!(webpush::verify_jwt(jwt, &server_key), "signed by the server's key");
        let claims: serde_json::Value = serde_json::from_slice(&webpush::unb64(jwt.split('.').nth(1).unwrap()).unwrap()).unwrap();
        assert_eq!(claims["aud"], format!("http://127.0.0.1:{push_port}"));
        let plain = webpush::decrypt(&body, &ua_private, auth).expect("the subscription's key decrypts it");
        serde_json::from_slice(&plain).unwrap()
    };

    let (head, body) = http_post(port, "/push/test", &page, Some(&token), json, b"");
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(body.contains(r#""sent":1"#), "{body}");
    assert_eq!(receive("the test")["tag"], "tenx-test");

    // A task that starts waiting on you: pushed once, on the edge.
    let pane_pid = h.tmux_out(&["list-panes", "-t", "tenx:push-task", "-F", "#{pane_pid}"]).lines().next().unwrap().to_string();
    let task_dir = h.root.join("ws/tasks/push-task");
    let payload = format!(r#"{{"hook_event_name":"Notification","notification_type":"agent_needs_input","cwd":"{}"}}"#, task_dir.display());
    let mut child = h.tenx().args(["internal", "session-event", "--agent", "claude", "--pid", &pane_pid]).stdin(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
    assert!(child.wait().unwrap().success());
    let msg = receive("the blocked task");
    assert_eq!(msg["title"], "Push task");
    assert_eq!(msg["body"], "input needed · e2e");
    assert_eq!(msg["tag"], "e2e/push-task");
    assert_eq!(msg["url"], "/?task=e2e/push-task");
    assert!(pushes.recv_timeout(std::time::Duration::from_secs(5)).is_err(), "once per edge");

    // Unsubscribed: forgotten.
    let unsub = serde_json::json!({ "endpoint": endpoint }).to_string();
    assert!(http_post(port, "/push/unsubscribe", &page, Some(&token), json, unsub.as_bytes()).0.starts_with("HTTP/1.1 200"));
    assert!(!fs::read_to_string(&subs_file).unwrap().contains(&endpoint));

    let _ = server.kill();
    let _ = server.wait();
}
