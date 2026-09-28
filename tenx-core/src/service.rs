//! A long-running tenx process as a per-user service: the text of a macOS
//! LaunchAgent plist and a systemd user unit. The binary writes and loads
//! them (`tenx web service install`); what they say is decided here.

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
    fn the_systemd_unit_quotes_what_needs_it() {
        let s = systemd_unit(&unit());
        assert!(s.contains("ExecStart=\"/Users/a b/bin/tenx\" web --port 7070\n"), "{s}");
        assert!(s.contains("Environment=PATH=/opt/homebrew/bin:/usr/bin\n"));
        assert!(s.contains("Restart=always") && s.contains("WantedBy=default.target"));
        assert_eq!(systemd_word("100%"), "\"100%%\"");
    }
}
