//! A long-running tenx process as a per-user service: the text of a macOS
//! LaunchAgent plist and a systemd user unit. The binary writes and loads
//! them (`tenx web service install`); what they say is decided here.

use std::path::{Path, PathBuf};

/// A process to run at login and keep running.
pub struct Unit {
    /// launchd label / systemd description.
    pub label: String,
    /// Absolute path of the program.
    pub program: String,
    pub args: Vec<String>,
    /// Environment for the process — launchd and systemd start it with
    /// almost none, and tmux panes it starts inherit it.
    pub env: Vec<(String, String)>,
    /// Where stdout and stderr go.
    pub log: String,
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// `~/Library/LaunchAgents/<label>.plist`: started at login, restarted
/// whenever it exits (at most every 10 s, so a port clash doesn't spin).
pub fn launchd_plist(u: &Unit) -> String {
    let mut args = format!("    <string>{}</string>\n", xml(&u.program));
    for a in &u.args {
        args.push_str(&format!("    <string>{}</string>\n", xml(a)));
    }
    let mut env = String::new();
    for (k, v) in &u.env {
        env.push_str(&format!("    <key>{}</key>\n    <string>{}</string>\n", xml(k), xml(v)));
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
{args}  </array>
  <key>EnvironmentVariables</key>
  <dict>
{env}  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ThrottleInterval</key>
  <integer>10</integer>
  <key>ProcessType</key>
  <string>Interactive</string>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
        label = xml(&u.label),
        log = xml(&u.log),
    )
}

/// The program in the macOS app, inside a Homebrew keg.
pub const APP_PROGRAM: &str = "tenx.app/Contents/MacOS/tenx";

/// `<prefix>/opt/tenx` for Homebrew's `<prefix>/bin/tenx`: the link Homebrew
/// keeps pointing at the formula's current keg.
pub fn homebrew_opt(bin: &Path) -> Option<PathBuf> {
    let prefix = bin.parent()?.parent()?;
    Some(prefix.join("opt").join("tenx"))
}

/// The program the macOS service runs, for the `tenx` it was installed with.
///
/// macOS keeps a privacy permission (iCloud Drive and the like) for an app
/// under its bundle id, but for a bare binary under its path as well, and a
/// Homebrew keg's path changes with every version: `tenx web` as a plain
/// LaunchAgent asks again after each upgrade. The release's keg also holds a
/// signed `tenx.app`, so when `bin` resolves into the keg `opt` points at (its
/// real path is `opt_real`) and the app is there, the service runs the app
/// through `opt`, a path that outlives upgrades. Anything else — `cargo
/// install`, `make install`'s test version behind an unlinked formula — runs
/// `bin` as before.
pub fn service_program(bin: &Path, bin_real: &Path, opt_real: Option<&Path>, app_exists: bool) -> PathBuf {
    match (homebrew_opt(bin), opt_real) {
        (Some(opt), Some(keg)) if app_exists && bin_real.starts_with(keg) => opt.join(APP_PROGRAM),
        _ => bin.to_path_buf(),
    }
}

/// `<prefix>/bin/tenx` for the app [`service_program`] picked
/// (`<prefix>/opt/tenx/tenx.app/Contents/MacOS/tenx`): the path a `tenx web`
/// running as the app hands on to what it starts — the tmux config, the
/// watcher, panes —, so they run what `$(brew --prefix)/bin/tenx` is (the
/// release, or `make install`'s test version), as from anywhere else. Under
/// the service they stay the app's, for macOS's permissions, all the same:
/// those go by the process launchd started.
pub fn app_bin(program: &Path) -> Option<PathBuf> {
    let mut up = program.ancestors();
    let opt_tenx = up.nth(APP_PROGRAM.split('/').count())?;
    let prefix = opt_tenx.parent().filter(|o| o.ends_with("opt"))?.parent()?;
    (program.ends_with(APP_PROGRAM) && opt_tenx.ends_with("tenx")).then(|| prefix.join("bin").join("tenx"))
}

fn unxml(s: &str) -> String {
    s.replace("&quot;", "\"").replace("&apos;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// The range of the program's path (the first `ProgramArguments` string) in
/// a plist [`launchd_plist`] wrote.
fn program_span(plist: &str) -> Option<std::ops::Range<usize>> {
    let args = plist.find("<key>ProgramArguments</key>")?;
    let start = args + plist[args..].find("<string>")? + "<string>".len();
    let end = start + plist[start..].find("</string>")?;
    Some(start..end)
}

/// The program an installed plist runs.
pub fn launchd_program(plist: &str) -> Option<String> {
    program_span(plist).map(|r| unxml(&plist[r]))
}

/// The plist with its program replaced, everything else kept.
pub fn with_launchd_program(plist: &str, program: &str) -> Option<String> {
    let r = program_span(plist)?;
    Some(format!("{}{}{}", &plist[..r.start], xml(program), &plist[r.end..]))
}

/// systemd's quoting for one word of `ExecStart=` / `Environment=`.
fn systemd_word(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-:=,+@".contains(c)) {
        return s.to_string();
    }
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%"))
}

/// `~/.config/systemd/user/<name>.service`: enabled for the user's session,
/// restarted when it exits.
pub fn systemd_unit(u: &Unit) -> String {
    let exec: Vec<String> = std::iter::once(&u.program).chain(&u.args).map(|a| systemd_word(a)).collect();
    let env: String = u.env.iter().map(|(k, v)| format!("Environment={}\n", systemd_word(&format!("{k}={v}")))).collect();
    format!(
        "[Unit]\nDescription={label}\n\n[Service]\nExecStart={exec}\n{env}Restart=always\nRestartSec=10\nStandardOutput=append:{log}\nStandardError=append:{log}\n\n[Install]\nWantedBy=default.target\n",
        label = u.label,
        exec = exec.join(" "),
        log = u.log,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit() -> Unit {
        Unit {
            label: "io.github.aluedeke.tenx.web".into(),
            program: "/Users/a b/bin/tenx".into(),
            args: vec!["web".into(), "--port".into(), "7070".into()],
            env: vec![("PATH".into(), "/opt/homebrew/bin:/usr/bin".into())],
            log: "/Users/a/.config/tenx/web.log".into(),
        }
    }

    #[test]
    fn the_plist_runs_at_login_and_keeps_running() {
        let p = launchd_plist(&unit());
        assert!(p.contains("<string>io.github.aluedeke.tenx.web</string>"));
        assert!(p.contains("<string>/Users/a b/bin/tenx</string>\n    <string>web</string>\n    <string>--port</string>\n    <string>7070</string>"));
        assert!(p.contains("<key>PATH</key>\n    <string>/opt/homebrew/bin:/usr/bin</string>"));
        assert!(p.contains("<key>RunAtLoad</key>\n  <true/>") && p.contains("<key>KeepAlive</key>\n  <true/>"));
        assert!(p.contains("<key>StandardErrorPath</key>\n  <string>/Users/a/.config/tenx/web.log</string>"));
    }

    #[test]
    fn plist_values_are_escaped() {
        let mut u = unit();
        u.args.push("--dev-origin=http://x?a=1&b=<2>".into());
        assert!(launchd_plist(&u).contains("<string>--dev-origin=http://x?a=1&amp;b=&lt;2&gt;</string>"));
    }

    #[test]
    fn homebrew_runs_the_app_through_opt_anything_else_the_binary() {
        let bin = Path::new("/opt/homebrew/bin/tenx");
        let keg = Path::new("/opt/homebrew/Cellar/tenx/0.32.0");
        let real = keg.join("bin/tenx");
        let app = PathBuf::from("/opt/homebrew/opt/tenx/tenx.app/Contents/MacOS/tenx");
        assert_eq!(service_program(bin, &real, Some(keg), true), app);
        // A release without the app (Intel, or one from before it).
        assert_eq!(service_program(bin, &real, Some(keg), false), bin);
        // `make install`: bin/tenx links to the test version, opt still to the keg.
        let test = Path::new("/Users/a/.local/share/tenx/test-version/tenx");
        assert_eq!(service_program(bin, test, Some(keg), true), bin);
        // `cargo install`: no Homebrew at all.
        let cargo = Path::new("/Users/a/.cargo/bin/tenx");
        assert_eq!(service_program(cargo, cargo, None, false), cargo);
    }

    #[test]
    fn the_app_hands_on_homebrews_bin_tenx() {
        let app = Path::new("/opt/homebrew/opt/tenx/tenx.app/Contents/MacOS/tenx");
        assert_eq!(app_bin(app), Some(PathBuf::from("/opt/homebrew/bin/tenx")));
        assert_eq!(app_bin(Path::new("/usr/local/opt/tenx/tenx.app/Contents/MacOS/tenx")), Some(PathBuf::from("/usr/local/bin/tenx")));
        for other in ["/opt/homebrew/bin/tenx", "/Users/a/tenx.app/Contents/MacOS/tenx", "/x/opt/other/tenx.app/Contents/MacOS/tenx", "/tenx.app/Contents/MacOS/tenx"] {
            assert_eq!(app_bin(Path::new(other)), None, "{other}");
        }
    }

    #[test]
    fn the_program_of_an_installed_plist_is_read_and_replaced() {
        let p = launchd_plist(&unit());
        assert_eq!(launchd_program(&p).as_deref(), Some("/Users/a b/bin/tenx"));
        let q = with_launchd_program(&p, "/opt/homebrew/opt/tenx/tenx.app/Contents/MacOS/tenx").unwrap();
        assert_eq!(launchd_program(&q).as_deref(), Some("/opt/homebrew/opt/tenx/tenx.app/Contents/MacOS/tenx"));
        assert_eq!(q.replace("/opt/homebrew/opt/tenx/tenx.app/Contents/MacOS/tenx", "/Users/a b/bin/tenx"), p);
        let mut u = unit();
        u.program = "/a&b/tenx".into();
        assert_eq!(launchd_program(&launchd_plist(&u)).as_deref(), Some("/a&b/tenx"));
    }

    #[test]
    fn the_systemd_unit_quotes_what_needs_it() {
        let s = systemd_unit(&unit());
        assert!(s.contains("ExecStart=\"/Users/a b/bin/tenx\" web --port 7070\n"), "{s}");
        assert!(s.contains("Environment=PATH=/opt/homebrew/bin:/usr/bin\n"));
        assert!(s.contains("Restart=always") && s.contains("WantedBy=default.target"));
        assert_eq!(systemd_word("100%"), "\"100%%\"");
    }
}
