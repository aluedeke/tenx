//! Linear: the API key the watcher looks up a task's Linear issues with, for
//! the sidebar's status (`tenx_core::sidebar::linear_request`).
//!
//! The key lives in `~/.config/tenx/linear-token` (mode 600), not in a
//! task's sealed secrets: those are released per task and per sitting, with a
//! passphrase, and the watcher runs unattended for every task. `tenx linear
//! login` reads it from the terminal without echo and never prints it.

use anyhow::{Context, Result, bail};
use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

const TOKEN_FILE: &str = "linear-token";
const API: &str = "https://api.linear.app/graphql";

fn path() -> Result<PathBuf> {
    Ok(crate::workspace::home_dir()?.join(".config").join("tenx").join(TOKEN_FILE))
}

/// The saved key, if any.
pub fn token() -> Option<String> {
    let text = std::fs::read_to_string(path().ok()?).ok()?;
    let t = text.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// `tenx linear login`: read a personal API key (Linear → Settings →
/// Security & access → Personal API keys), check it, save it.
pub fn login() -> Result<()> {
    let key = if std::io::stdin().is_terminal() {
        println!("Paste a Linear personal API key (Settings → Security & access → Personal API keys).");
        crate::cli::secrets::read_masked_line("Linear API key")?
    } else {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).context("read the key from stdin")?;
        s
    };
    let key = key.trim().to_string();
    if key.is_empty() {
        bail!("no key given");
    }
    let me = viewer(&key)?;
    save(&key)?;
    println!("saved; signed in to Linear as {me}");
    println!("the sidebar shows each Linear issue's status within a few minutes");
    Ok(())
}

/// `tenx linear logout`.
pub fn logout() -> Result<()> {
    let p = path()?;
    match std::fs::remove_file(&p) {
        Ok(()) => println!("removed {}", p.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!("no Linear key saved"),
        Err(e) => return Err(e).with_context(|| format!("remove {}", p.display())),
    }
    Ok(())
}

/// `tenx linear status`.
pub fn status() -> Result<()> {
    match token() {
        None => println!("no Linear key saved: `tenx linear login`"),
        Some(key) => match viewer(&key) {
            Ok(me) => println!("signed in to Linear as {me}"),
            Err(e) => println!("a Linear key is saved, but Linear refused it: {e}"),
        },
    }
    Ok(())
}

fn save(key: &str) -> Result<()> {
    let p = path()?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let tmp = p.with_extension("tmp");
    let mut f = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)
    }
    .with_context(|| format!("create {}", tmp.display()))?;
    writeln!(f, "{key}")?;
    drop(f);
    std::fs::rename(&tmp, &p).with_context(|| format!("rename to {}", p.display()))?;
    Ok(())
}

fn viewer(key: &str) -> Result<String> {
    let json = post(key, r#"{"query":"{ viewer { name email } }"}"#)?;
    match json["data"]["viewer"]["name"].as_str() {
        Some(name) => Ok(name.to_string()),
        None => bail!("{}", json["errors"][0]["message"].as_str().unwrap_or("no answer from Linear")),
    }
}

/// POST `body` to Linear's API with `curl`. The key goes in through curl's
/// config on stdin, never on its command line, where `ps` would show it.
pub(crate) fn post(key: &str, body: &str) -> Result<serde_json::Value> {
    if key.contains(['"', '\n', '\\']) {
        bail!("that doesn't look like a Linear API key");
    }
    let mut child = Command::new("curl")
        .args(["-sS", "--max-time", "15", "-X", "POST", "-H", "Content-Type: application/json", "-K", "-"])
        .args(["--data-binary", body, API])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("run curl")?;
    child.stdin.take().context("curl's stdin")?.write_all(format!("header = \"Authorization: {key}\"\n").as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("curl: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    serde_json::from_slice(&out.stdout).context("Linear's answer is not JSON")
}

/// The issues `keys` name, looked up in one request. `None` without a key
/// or when the request failed — the sidebar then shows no status. One issue
/// Linear doesn't know fails the whole request, so then each key is asked
/// for alone and the unknown ones are left out.
pub fn issues(keys: &[String]) -> Option<Vec<tenx_core::sidebar::LinearIssue>> {
    let key = token()?;
    let ask = |keys: &[String]| post(&key, &tenx_core::sidebar::linear_request(keys)).ok().and_then(|json| tenx_core::sidebar::parse_linear(&json, keys).ok());
    if let Some(found) = ask(keys) {
        return Some(found);
    }
    if keys.len() < 2 {
        return None;
    }
    let found: Vec<Option<Vec<_>>> = keys.iter().map(|k| ask(std::slice::from_ref(k))).collect();
    // None of them answered: the key or the network, not an issue.
    if found.iter().all(Option::is_none) {
        return None;
    }
    Some(found.into_iter().flatten().flatten().collect())
}
