//! APNs for the tenx iOS app (`mobile/ios`): a WKWebView has no Web Push,
//! so the app registers with Apple itself and hands its device token to
//! the page, which posts it to `POST /push/apns/subscribe`.
//!
//! - The key: Apple's `AuthKey_<id>.p8`, put in place by `tenx web apns-key`
//!   as `~/.config/tenx/apns-key.p8` (600), with its key id, team and topic
//!   (the app's bundle id) in `apns.json`. Without it nothing is sent.
//! - Devices: `apns-devices.json` (600), one per device token, each with
//!   the APNs environment it belongs to (an Xcode build: sandbox;
//!   TestFlight: production).
//! - The provider token is kept in `apns-token.json` and reused for
//!   `tenx_core::apns::TOKEN_REUSE_SECS`: APNs refuses a sender that signs a
//!   new one every few minutes, and every `tenx notify` is a new process.
//! - Sent with the system `curl` over HTTP/2; a token APNs calls unknown or
//!   unregistered is forgotten (`tenx_core::apns::delivery`).

use super::{write_private, Message};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::SystemTime;
use tenx_core::apns::{self, Env};
use tenx_core::webpush::Delivery;

/// How long APNs keeps a notification for a device that is offline.
const EXPIRY_SECS: u64 = 4 * 3600;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct Device {
    pub(crate) token: String,
    pub(crate) env: Env,
}

/// `apns.json`: which key, whose team, which app.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct Config {
    pub(crate) key_id: String,
    pub(crate) team_id: String,
    pub(crate) topic: String,
}

/// `apns-token.json`: the provider token last signed, and for what.
#[derive(Serialize, Deserialize)]
struct SavedToken {
    key_id: String,
    team_id: String,
    iat: u64,
    jwt: String,
}

pub(crate) struct Apns {
    dir: PathBuf,
    lock: Mutex<()>,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs()
}

impl Apns {
    pub(crate) fn new(dir: PathBuf) -> Apns {
        Apns { dir, lock: Mutex::new(()) }
    }

    fn devices_path(&self) -> PathBuf {
        self.dir.join("apns-devices.json")
    }

    fn token_path(&self) -> PathBuf {
        self.dir.join("apns-token.json")
    }

    /// The key and its config, when `tenx web apns-key` put them in place.
    pub(crate) fn configured(&self) -> Option<(Config, Vec<u8>)> {
        let config: Config = serde_json::from_str(&std::fs::read_to_string(self.dir.join("apns.json")).ok()?).ok()?;
        let key = apns::parse_p8(&std::fs::read_to_string(self.dir.join("apns-key.p8")).ok()?).ok()?;
        Some((config, key))
    }

    pub(crate) fn devices(&self) -> Vec<Device> {
        std::fs::read_to_string(self.devices_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    fn write_devices(&self, devices: &[Device]) -> Result<()> {
        write_private(&self.devices_path(), &serde_json::to_string_pretty(devices)?)
    }

    pub(crate) fn subscribe(&self, device: Device) -> Result<()> {
        anyhow::ensure!(apns::valid_device_token(&device.token), "not an APNs device token");
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut devices = self.devices();
        devices.retain(|d| d.token != device.token);
        devices.push(device);
        self.write_devices(&devices)
    }

    pub(crate) fn unsubscribe(&self, token: &str) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut devices = self.devices();
        devices.retain(|d| d.token != token);
        self.write_devices(&devices)
    }

    /// Send `msg` to every device; forget the ones APNs says are gone. No
    /// key in place: nothing is sent, and the devices are kept.
    pub(crate) fn send_all(&self, msg: &Message) -> (usize, usize) {
        let devices = self.devices();
        if devices.is_empty() {
            return (0, 0);
        }
        let Some((config, key)) = self.configured() else {
            eprintln!("tenx: {} iOS device(s) signed up, but no APNs key — run `tenx web apns-key <AuthKey_….p8>`", devices.len());
            return (0, devices.len());
        };
        let jwt = match self.provider_token(&config, &key) {
            Ok(jwt) => jwt,
            Err(e) => {
                eprintln!("tenx: APNs provider token: {e:#}");
                return (0, devices.len());
            }
        };
        let payload = apns::payload(&msg.title, &msg.body, &msg.tag, &msg.url);
        let mut sent = 0;
        let mut gone = Vec::new();
        for device in &devices {
            match self.send(&config, &jwt, device, &msg.tag, &payload) {
                Ok(Delivery::Sent) => sent += 1,
                Ok(Delivery::Gone) => gone.push(device.token.clone()),
                Ok(Delivery::Failed) => {}
                Err(e) => eprintln!("tenx: APNs: {e:#}"),
            }
        }
        if !gone.is_empty() {
            let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
            let mut devices = self.devices();
            devices.retain(|d| !gone.contains(&d.token));
            let _ = self.write_devices(&devices);
        }
        (sent, devices.len())
    }

    /// The saved provider token while it is young enough, else a new one.
    fn provider_token(&self, config: &Config, key: &[u8]) -> Result<String> {
        let now = now_secs();
        let saved: Option<SavedToken> = std::fs::read_to_string(self.token_path()).ok().and_then(|t| serde_json::from_str(&t).ok());
        if let Some(t) = saved {
            if t.key_id == config.key_id && t.team_id == config.team_id && apns::token_reusable(t.iat, now) {
                return Ok(t.jwt);
            }
        }
        let jwt = apns::provider_token(key, &config.key_id, &config.team_id, now).map_err(anyhow::Error::msg)?;
        let saved = SavedToken { key_id: config.key_id.clone(), team_id: config.team_id.clone(), iat: now, jwt: jwt.clone() };
        write_private(&self.token_path(), &serde_json::to_string(&saved)?)?;
        Ok(jwt)
    }

    fn send(&self, config: &Config, jwt: &str, device: &Device, tag: &str, payload: &str) -> Result<Delivery> {
        let url = format!("{}/3/device/{}", device.env.host(), device.token);
        let mut cmd = Command::new("curl");
        cmd.args(["-sS", "--http2", "--max-time", "15", "-w", "\n%{http_code}", "-X", "POST"])
            .args(["-H", &format!("authorization: bearer {jwt}")])
            .args(["-H", &format!("apns-topic: {}", config.topic)])
            .args(["-H", "apns-push-type: alert"])
            .args(["-H", "apns-priority: 10"])
            .args(["-H", &format!("apns-expiration: {}", now_secs() + EXPIRY_SECS)]);
        if let Some(id) = apns::collapse_id(tag) {
            cmd.args(["-H", &format!("apns-collapse-id: {id}")]);
        }
        let mut child = cmd
            .args(["--data-binary", "@-", &url])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("run curl")?;
        child.stdin.take().context("curl's stdin")?.write_all(payload.as_bytes())?;
        let out = child.wait_with_output()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let (body, status) = text.rsplit_once('\n').unwrap_or(("", &text));
        let status: u16 = status.trim().parse().unwrap_or(0);
        if status == 0 {
            bail!("no answer from {}: {}", device.env.host(), String::from_utf8_lossy(&out.stderr).trim());
        }
        if status == 403 {
            // An expired or refused provider token: sign a new one next time.
            let _ = std::fs::remove_file(self.token_path());
        }
        let delivery = apns::delivery(status, body);
        if delivery == Delivery::Failed {
            eprintln!("tenx: APNs answered {status} {}", body.trim());
        }
        Ok(delivery)
    }
}

/// `tenx web apns-key <file>`: check Apple's key and put it in place.
pub(crate) fn install_key(file: &Path, key_id: Option<&str>, team_id: &str, topic: &str) -> Result<()> {
    let pem = std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    apns::parse_p8(&pem).map_err(anyhow::Error::msg)?;
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let key_id = match key_id {
        Some(id) => id.to_string(),
        None => apns::key_id_from_name(&name).context("no key id in the file name (AuthKey_<id>.p8) — pass --key-id")?,
    };
    anyhow::ensure!(apns::valid_key_id(&key_id), "not a key id: {key_id} (ten letters and digits)");
    anyhow::ensure!(apns::valid_key_id(team_id), "not a team id: {team_id} (ten letters and digits)");
    let dir = super::config_dir()?;
    write_private(&dir.join("apns-key.p8"), &pem)?;
    let config = Config { key_id: key_id.clone(), team_id: team_id.to_string(), topic: topic.to_string() };
    write_private(&dir.join("apns.json"), &serde_json::to_string_pretty(&config)?)?;
    let _ = std::fs::remove_file(dir.join("apns-token.json"));
    let devices = Apns::new(dir).devices().len();
    println!("APNs key {key_id} (team {team_id}, app {topic}) is in place; {devices} iOS device(s) signed up.");
    println!("You can delete {} now: tenx keeps its own copy.", file.display());
    Ok(())
}
