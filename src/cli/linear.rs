//! Linear: the API keys the watcher looks up a task's Linear issues with,
//! for the sidebar's titles and status (`tenx_core::sidebar::linear_request`).
//!
//! A personal API key belongs to one Linear organization, and workspaces
//! link issues of different ones, so there is one key per organization:
//! `~/.config/tenx/linear-tokens/<org>` (mode 600), `<org>` being the
//! organization's key in its links (`linear.app/<org>/issue/…`), which
//! `login` asks Linear for. A link's issue is looked up with its
//! organization's key (`tenx_core::sidebar::linear_groups`); the single key
//! older releases saved, `~/.config/tenx/linear-token`, still serves every
//! organization without one of its own.
//!
//! The keys are not a task's sealed secrets: those are released per task
//! and per sitting, with a passphrase, and the watcher runs unattended for
//! every task. `tenx linear login` reads a key from the terminal without
//! echo and never prints it.

use anyhow::{Context, Result, bail};
use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use tenx_core::sidebar::{LinearIssue, Viewer};

/// The key older releases saved, for no organization in particular.
const TOKEN_FILE: &str = "linear-token";
/// One key per organization, named by it.
const TOKENS_DIR: &str = "linear-tokens";
const API: &str = "https://api.linear.app/graphql";

fn config_dir() -> Result<PathBuf> {
    Ok(crate::workspace::home_dir()?.join(".config").join("tenx"))
}

fn read_key(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let t = text.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// The key saved for `org`; `None` for the fallback key.
fn key_for(org: Option<&str>) -> Option<String> {
    let dir = config_dir().ok()?;
    match org {
        Some(org) => read_key(&dir.join(TOKENS_DIR).join(org)),
        None => read_key(&dir.join(TOKEN_FILE)),
    }
}

/// The organizations with a key of their own, sorted.
fn keyed_orgs() -> Vec<String> {
    let Ok(dir) = config_dir() else { return Vec::new() };
    let mut orgs: Vec<String> = std::fs::read_dir(dir.join(TOKENS_DIR))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.ends_with(".tmp") && read_key(&dir.join(TOKENS_DIR).join(n)).is_some())
        .collect();
    orgs.sort();
    orgs
}

/// Whether any key is saved: the watcher looks nothing up without one.
pub fn any_key() -> bool {
    key_for(None).is_some() || !keyed_orgs().is_empty()
}

/// `tenx linear login`: read a personal API key (Linear → Settings →
/// Security & access → Personal API keys), ask Linear whose organization it
/// is, save it for that one. Once per organization.
pub fn login() -> Result<()> {
    let key = if std::io::stdin().is_terminal() {
        println!("Paste a Linear personal API key (Settings → Security & access → Personal API keys).");
        println!("A key sees one Linear organization; run this once for each.");
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
    save(&me.org, &key)?;
    println!("saved for {} (linear.app/{}/…); signed in to Linear as {}", me.org_name, me.org, me.name);
    println!("the sidebar shows its issues' titles and status within a few minutes");
    Ok(())
}

/// `tenx linear logout [ORG]`: remove one organization's key, or every key.
pub fn logout(org: Option<&str>) -> Result<()> {
    let dir = config_dir()?;
    let paths: Vec<PathBuf> = match org {
        Some(org) => vec![dir.join(TOKENS_DIR).join(org.to_ascii_lowercase())],
        None => keyed_orgs().iter().map(|o| dir.join(TOKENS_DIR).join(o)).chain([dir.join(TOKEN_FILE)]).collect(),
    };
    let mut removed = 0;
    for p in paths {
        match std::fs::remove_file(&p) {
            Ok(()) => {
                println!("removed {}", p.display());
                removed += 1;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("remove {}", p.display())),
        }
    }
    if removed == 0 {
        match org {
            Some(org) => println!("no Linear key saved for {org}"),
            None => println!("no Linear key saved"),
        }
    }
    Ok(())
}

/// `tenx linear status`: each saved key, and whom it signs in as.
pub fn status() -> Result<()> {
    let mut any = false;
    for org in keyed_orgs() {
        any = true;
        match key_for(Some(&org)).map(|k| viewer(&k)) {
            Some(Ok(me)) => println!("{org}: signed in to Linear as {} ({})", me.name, me.org_name),
            Some(Err(e)) => println!("{org}: a key is saved, but Linear refused it: {e}"),
            None => {}
        }
    }
    if let Some(key) = key_for(None) {
        any = true;
        match viewer(&key) {
            Ok(me) => println!("any other organization: the older single key, signed in as {} ({}); `tenx linear login` again saves it for {}", me.name, me.org_name, me.org),
            Err(e) => println!("any other organization: the older single key, but Linear refused it: {e}"),
        }
    }
    if !any {
        println!("no Linear key saved: `tenx linear login`, once per Linear organization");
    }
    Ok(())
}

fn save(org: &str, key: &str) -> Result<()> {
    if org.is_empty() || !org.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        bail!("Linear named an organization tenx can't file a key under: {org:?}");
    }
    let dir = config_dir()?.join(TOKENS_DIR);
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let p = dir.join(org);
    let tmp = dir.join(format!("{org}.tmp"));
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

fn viewer(key: &str) -> Result<Viewer> {
    let json = post(key, tenx_core::sidebar::VIEWER_QUERY)?;
    tenx_core::sidebar::parse_viewer(&json).map_err(|e| anyhow::anyhow!(e))
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

/// The issues a task links to, as (organization, issue key) pairs, each
/// looked up with its organization's key (else the older single key), one
/// request per key. `None` when no request answered — no key, or the
/// network — so the sidebar keeps what it had.
pub fn issues(issues: &[(Option<String>, String)]) -> Option<Vec<LinearIssue>> {
    let groups = tenx_core::sidebar::linear_groups(issues, &keyed_orgs(), key_for(None).is_some());
    let mut found = Vec::new();
    let mut answered = false;
    for (org, keys) in groups {
        let Some(key) = key_for(org.as_deref()) else { continue };
        if let Some(got) = issues_with(&key, &keys) {
            answered = true;
            found.extend(got);
        }
    }
    answered.then_some(found)
}

/// `keys` looked up with one API key, in one request. One issue Linear
/// doesn't know fails the whole request, so then each is asked for alone
/// and the unknown ones are left out; `None` when none answered.
fn issues_with(key: &str, keys: &[String]) -> Option<Vec<LinearIssue>> {
    let ask = |keys: &[String]| post(key, &tenx_core::sidebar::linear_request(keys)).ok().and_then(|json| tenx_core::sidebar::parse_linear(&json, keys).ok());
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
