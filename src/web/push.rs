//! Web Push for `tenx web`: the phone (or desktop) that installed the page
//! gets a notification when a task starts needing you, even with the page
//! closed — on exactly the edges the attention watcher raises a desktop
//! notification for (`cli::watch::Attention`).
//!
//! - The VAPID key that identifies this server to push services is made on
//!   first use: `~/.config/tenx/web-push-vapid` (600, the private key as
//!   base64url).
//! - Subscriptions (one per browser that enabled notifications) live in
//!   `~/.config/tenx/web-push-subs.json` (600), one per endpoint.
//! - Messages are encrypted and signed in `tenx_core::webpush` and sent with
//!   the system `curl`, off the async runtime.
//! - A push service answering 404/410 means the browser dropped the
//!   subscription; it is forgotten.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};
use tenx_core::webpush::{self, Delivery};

/// Who the push services should contact about this sender (RFC 8292 `sub`).
const CONTACT: &str = "https://github.com/aluedeke/tenx";

/// How long a push service keeps a message for a device that is offline.
const TTL_SECS: u64 = 4 * 3600;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(super) struct Subscription {
    pub(super) endpoint: String,
    pub(super) keys: Keys,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(super) struct Keys {
    pub(super) p256dh: String,
    pub(super) auth: String,
}

/// What the service worker shows (`web/public/sw.js`).
#[derive(Debug, Serialize)]
pub(super) struct Message {
    pub(super) title: String,
    pub(super) body: String,
    /// The task's id: a newer notification for the same task replaces the
    /// older one instead of stacking.
    pub(super) tag: String,
    /// Where a tap on it goes: `/?task=<id>`.
    pub(super) url: String,
}

/// The server's push state: its VAPID key and the subscriptions file,
/// written by one thread at a time.
pub(super) struct Push {
    private: Vec<u8>,
    public: Vec<u8>,
    subs_path: PathBuf,
    lock: Mutex<()>,
}

fn config_dir() -> Result<PathBuf> {
    Ok(crate::workspace::home_dir()?.join(".config").join("tenx"))
}

/// Write `text` to `path` through a 600 temp file and a rename, so a reader
/// never sees half of it and nobody else ever can.
fn write_private(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("write {}", tmp.display()))?;
    f.write_all(text.as_bytes())?;
    drop(f);
    std::fs::rename(&tmp, path).with_context(|| format!("write {}", path.display()))
}

impl Push {
    pub(super) fn load() -> Result<Push> {
        let dir = config_dir()?;
        let key_path = dir.join("web-push-vapid");
        let private = match std::fs::read_to_string(&key_path).ok().and_then(|t| webpush::unb64(t.trim()).ok()) {
            Some(k) if webpush::valid_private_key(&k) => k,
            _ => {
                let key = loop {
                    let k = super::token::random_bytes(32)?;
                    if webpush::valid_private_key(&k) {
                        break k;
                    }
                };
                write_private(&key_path, &format!("{}\n", webpush::b64(&key)))?;
                key
            }
        };
        let public = webpush::public_key(&private).map_err(anyhow::Error::msg)?;
        Ok(Push { private, public, subs_path: dir.join("web-push-subs.json"), lock: Mutex::new(()) })
    }

    /// The application server key a browser subscribes with (base64url).
    pub(super) fn public_key(&self) -> String {
        webpush::b64(&self.public)
    }

    fn read_subs(&self) -> Vec<Subscription> {
        std::fs::read_to_string(&self.subs_path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    fn write_subs(&self, subs: &[Subscription]) -> Result<()> {
        write_private(&self.subs_path, &serde_json::to_string_pretty(subs)?)
    }

    pub(super) fn subscribe(&self, sub: Subscription) -> Result<()> {
        anyhow::ensure!(webpush::valid_endpoint(&sub.endpoint), "not a push service endpoint: {}", sub.endpoint);
        let p256dh = webpush::unb64(&sub.keys.p256dh).map_err(anyhow::Error::msg)?;
        anyhow::ensure!(p256dh.len() == 65, "p256dh is not an uncompressed P-256 key");
        anyhow::ensure!(!webpush::unb64(&sub.keys.auth).map_err(anyhow::Error::msg)?.is_empty(), "empty auth secret");
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut subs = self.read_subs();
        subs.retain(|s| s.endpoint != sub.endpoint);
        subs.push(sub);
        self.write_subs(&subs)
    }

    pub(super) fn unsubscribe(&self, endpoint: &str) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut subs = self.read_subs();
        subs.retain(|s| s.endpoint != endpoint);
        self.write_subs(&subs)
    }

    /// Send `msg` to every subscription; forget the ones the push service
    /// says are gone. Blocks on the network — never call it on the runtime.
    pub(super) fn send_all(&self, msg: &Message) -> (usize, usize) {
        let subs = { self.read_subs() };
        let payload = serde_json::to_vec(msg).unwrap_or_default();
        let mut sent = 0;
        let mut gone = Vec::new();
        for sub in &subs {
            match self.send(sub, &payload) {
                Ok(Delivery::Sent) => sent += 1,
                Ok(Delivery::Gone) => gone.push(sub.endpoint.clone()),
                Ok(Delivery::Failed) => {}
                Err(e) => eprintln!("tenx web: push to {}: {e:#}", webpush::endpoint_origin(&sub.endpoint).unwrap_or_default()),
            }
        }
        if !gone.is_empty() {
            let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
            let mut subs = self.read_subs();
            subs.retain(|s| !gone.contains(&s.endpoint));
            let _ = self.write_subs(&subs);
        }
        (sent, subs.len())
    }

    fn send(&self, sub: &Subscription, payload: &[u8]) -> Result<Delivery> {
        let ua_public = webpush::unb64(&sub.keys.p256dh).map_err(anyhow::Error::msg)?;
        let auth = webpush::unb64(&sub.keys.auth).map_err(anyhow::Error::msg)?;
        let ephemeral = loop {
            let k = super::token::random_bytes(32)?;
            if webpush::valid_private_key(&k) {
                break k;
            }
        };
        let salt = super::token::random_bytes(16)?;
        let body = webpush::encrypt(payload, &ua_public, &auth, &ephemeral, &salt).map_err(anyhow::Error::msg)?;
        let aud = webpush::endpoint_origin(&sub.endpoint).context("endpoint has no origin")?;
        let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs();
        let jwt = webpush::vapid_jwt(&self.private, &aud, now + 12 * 3600, CONTACT).map_err(anyhow::Error::msg)?;
        let mut child = Command::new("curl")
            .args(["-sS", "-o", "/dev/null", "-w", "%{http_code}", "--max-time", "15", "-X", "POST"])
            .args(["-H", &format!("TTL: {TTL_SECS}")])
            .args(["-H", "Content-Encoding: aes128gcm"])
            .args(["-H", "Content-Type: application/octet-stream"])
            .args(["-H", "Urgency: high"])
            .args(["-H", &format!("Authorization: {}", webpush::authorization(&jwt, &self.public))])
            .args(["--data-binary", "@-", &sub.endpoint])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("run curl")?;
        child.stdin.take().context("curl's stdin")?.write_all(&body)?;
        let out = child.wait_with_output()?;
        let status: u16 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0);
        Ok(webpush::delivery(status))
    }
}

/// The notifier: every `POLL`, the watcher's resolve pass and edge rules;
/// each new edge pushed to every subscription. Runs for the server's life.
pub(super) fn notifier(push: std::sync::Arc<Push>) {
    const POLL: Duration = Duration::from_secs(2);
    let snapshot = crate::cli::watch::resolve_all();
    let mut attention = crate::cli::watch::Attention::primed(&snapshot);
    loop {
        std::thread::sleep(POLL);
        let snapshot = crate::cli::watch::resolve_all();
        for (note, _) in attention.step(&snapshot) {
            let reason = note.reason.clone().unwrap_or_else(|| "waiting for you".into());
            let msg = Message {
                title: note.task.clone(),
                body: format!("{reason} · {}", note.workspace),
                tag: note.id.clone(),
                url: format!("/?task={}", note.id),
            };
            push.send_all(&msg);
        }
    }
}
