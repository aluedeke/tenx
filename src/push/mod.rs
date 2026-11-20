//! Notifications on your devices, even with the page closed: Web Push to
//! every browser that turned them on in `tenx web`'s page, and APNs to the
//! tenx iOS app (`apns`). Sent by `tenx web` on exactly the edges the
//! attention watcher raises a desktop notification for
//! (`cli::watch::Attention`), and by `tenx notify` (`cli::notify`) when an
//! agent tells you something because you asked it to.
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
use std::time::SystemTime;
use tenx_core::webpush::{self, Delivery};

pub(crate) mod apns;

/// Who the push services should contact about this sender (RFC 8292 `sub`).
const CONTACT: &str = "https://github.com/aluedeke/tenx";

/// How long a push service keeps a message for a device that is offline.
const TTL_SECS: u64 = 4 * 3600;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct Subscription {
    pub(crate) endpoint: String,
    pub(crate) keys: Keys,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct Keys {
    pub(crate) p256dh: String,
    pub(crate) auth: String,
}

/// What the service worker shows (`web/public/sw.js`).
#[derive(Debug, Serialize)]
pub(crate) struct Message {
    pub(crate) title: String,
    pub(crate) body: String,
    /// The task's id: a newer notification for the same task replaces the
    /// older one instead of stacking.
    pub(crate) tag: String,
    /// Where a tap on it goes: `/?task=<id>`.
    pub(crate) url: String,
}

/// The server's push state: its VAPID key and the subscriptions file,
/// written by one thread at a time, and the iOS app's devices.
pub(crate) struct Push {
    private: Vec<u8>,
    public: Vec<u8>,
    subs_path: PathBuf,
    lock: Mutex<()>,
    pub(crate) apns: apns::Apns,
}

pub(crate) fn config_dir() -> Result<PathBuf> {
    Ok(crate::workspace::home_dir()?.join(".config").join("tenx"))
}

/// Write `text` to `path` through a 600 temp file and a rename, so a reader
/// never sees half of it and nobody else ever can.
pub(crate) fn write_private(path: &Path, text: &str) -> Result<()> {
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
    pub(crate) fn load() -> Result<Push> {
        let dir = config_dir()?;
        let key_path = dir.join("web-push-vapid");
        let private = match std::fs::read_to_string(&key_path).ok().and_then(|t| webpush::unb64(t.trim()).ok()) {
            Some(k) if webpush::valid_private_key(&k) => k,
            _ => {
                let key = loop {
                    let k = crate::web::token::random_bytes(32)?;
                    if webpush::valid_private_key(&k) {
                        break k;
                    }
                };
                write_private(&key_path, &format!("{}\n", webpush::b64(&key)))?;
                key
            }
        };
        let public = webpush::public_key(&private).map_err(anyhow::Error::msg)?;
        Ok(Push { private, public, subs_path: dir.join("web-push-subs.json"), lock: Mutex::new(()), apns: apns::Apns::new(dir) })
    }

    /// The application server key a browser subscribes with (base64url).
    pub(crate) fn public_key(&self) -> String {
        webpush::b64(&self.public)
    }

    fn read_subs(&self) -> Vec<Subscription> {
        std::fs::read_to_string(&self.subs_path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    fn write_subs(&self, subs: &[Subscription]) -> Result<()> {
        write_private(&self.subs_path, &serde_json::to_string_pretty(subs)?)
    }

    pub(crate) fn subscribe(&self, sub: Subscription) -> Result<()> {
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

    pub(crate) fn unsubscribe(&self, endpoint: &str) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut subs = self.read_subs();
        subs.retain(|s| s.endpoint != endpoint);
        self.write_subs(&subs)
    }

    /// Send `msg` to every browser and every iOS device: how many took it,
    /// of how many. Blocks on the network — never call it on the runtime.
    pub(crate) fn send_all(&self, msg: &Message) -> (usize, usize) {
        let (web_sent, web_total) = self.send_web(msg);
        let (app_sent, app_total) = self.apns.send_all(msg);
        (web_sent + app_sent, web_total + app_total)
    }

    /// Send `msg` to every browser subscription; forget the ones the push
    /// service says are gone.
    fn send_web(&self, msg: &Message) -> (usize, usize) {
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
            let k = crate::web::token::random_bytes(32)?;
            if webpush::valid_private_key(&k) {
                break k;
            }
        };
        let salt = crate::web::token::random_bytes(16)?;
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

/// What a phone shows for an attention edge (`events::notifier` sends it).
pub(crate) fn message(note: &crate::cli::watch::Note) -> Message {
    let reason = note.reason.clone().unwrap_or_else(|| "waiting for you".into());
    Message {
        title: note.task.clone(),
        body: format!("{reason} · {}", note.workspace),
        tag: note.id.clone(),
        // A secrets request opens its unlock prompt, not just the task.
        url: if note.secrets { format!("/?task={}&unlock=1", note.id) } else { format!("/?task={}", note.id) },
    }
}
