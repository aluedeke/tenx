//! Apple Push Notification service (APNs) for the tenx iOS app
//! (`mobile/ios`), the half without I/O: the key Apple hands out as a `.p8`
//! file, the provider token signed with it (an ES256 JWT), the request a
//! notification is, and what APNs' answer means for the device token.
//!
//! Sending is the binary's job (`src/push/apns.rs`, `curl --http2`).

use crate::webpush::{b64, Delivery};
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::pkcs8::DecodePrivateKey;
use serde::{Deserialize, Serialize};

/// The bundle id of the tenx iOS app (`mobile/ios/fastlane/Appfile`): the
/// `apns-topic` every notification is sent with, unless configured otherwise.
pub const DEFAULT_TOPIC: &str = "it.luedeke.mobile.tenx";

/// The team the app is signed by (`mobile/ios/fastlane/Fastfile`): the
/// provider token's issuer, unless configured otherwise.
pub const DEFAULT_TEAM: &str = "RUJR5DY35F";

/// How long a provider token is reused. APNs refuses one older than an hour
/// and answers `TooManyProviderTokenUpdates` to a sender that makes a new one
/// more often than every 20 minutes — and every `tenx notify` is a new
/// process, so the token is kept on disk and reused for 40 minutes.
pub const TOKEN_REUSE_SECS: u64 = 40 * 60;

/// Which APNs a device token belongs to: an app Xcode built and ran
/// (`development`, the sandbox) or one from TestFlight or the App Store.
/// A token is only valid at the server of its own environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Env {
    Sandbox,
    Production,
}

impl Env {
    pub fn host(self) -> &'static str {
        match self {
            Env::Sandbox => "https://api.sandbox.push.apple.com",
            Env::Production => "https://api.push.apple.com",
        }
    }
}

/// The 32-byte private key out of Apple's `AuthKey_<id>.p8` (a PKCS#8 PEM).
pub fn parse_p8(pem: &str) -> Result<Vec<u8>, String> {
    let key = p256::SecretKey::from_pkcs8_pem(pem.trim()).map_err(|e| format!("not an APNs key (.p8): {e}"))?;
    Ok(key.to_bytes().to_vec())
}

/// The key id out of the file name Apple gives the download, `AuthKey_<id>.p8`.
pub fn key_id_from_name(name: &str) -> Option<String> {
    let id = name.strip_prefix("AuthKey_")?.strip_suffix(".p8")?;
    valid_key_id(id).then(|| id.to_string())
}

/// A key or team id: ten upper-case letters and digits.
pub fn valid_key_id(id: &str) -> bool {
    id.len() == 10 && id.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// A device token as the app reports it: hex, 32 bytes or more.
pub fn valid_device_token(token: &str) -> bool {
    token.len() >= 64 && token.len() <= 200 && token.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The provider token: an ES256 JWT with the key id in its header and the
/// team as its issuer, issued at `iat` (Unix seconds).
pub fn provider_token(private: &[u8], key_id: &str, team_id: &str, iat: u64) -> Result<String, String> {
    let key = SigningKey::from_slice(private).map_err(|_| "not a P-256 private key".to_string())?;
    let header = b64(serde_json::json!({ "alg": "ES256", "kid": key_id }).to_string().as_bytes());
    let claims = b64(serde_json::json!({ "iss": team_id, "iat": iat }).to_string().as_bytes());
    let signing_input = format!("{header}.{claims}");
    let sig: Signature = key.sign(signing_input.as_bytes());
    Ok(format!("{signing_input}.{}", b64(&sig.to_bytes())))
}

/// A saved provider token may be sent again: made for this key and team,
/// and younger than `TOKEN_REUSE_SECS`.
pub fn token_reusable(saved_iat: u64, now: u64) -> bool {
    saved_iat <= now && now - saved_iat < TOKEN_REUSE_SECS
}

/// The notification's JSON: an alert with a title and a body, grouped and
/// replaced by `tag` (a newer one for the same task stands in for the
/// older), and the page address a tap opens (`url`, read by the app).
pub fn payload(title: &str, body: &str, tag: &str, url: &str) -> String {
    serde_json::json!({
        "aps": {
            "alert": { "title": title, "body": body },
            "sound": "default",
            "thread-id": tag,
        },
        "url": url,
    })
    .to_string()
}

/// `apns-collapse-id` may be at most 64 bytes; the task's id usually is.
pub fn collapse_id(tag: &str) -> Option<&str> {
    (!tag.is_empty() && tag.len() <= 64).then_some(tag)
}

/// What APNs' answer means for the device token: its HTTP status and the
/// `reason` of its JSON body.
pub fn delivery(status: u16, body: &str) -> Delivery {
    let reason = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(str::to_string))
        .unwrap_or_default();
    match status {
        200 => Delivery::Sent,
        // The app was deleted, or the user turned its notifications off for good.
        410 => Delivery::Gone,
        // A token from the other environment, or another app's.
        400 if matches!(reason.as_str(), "BadDeviceToken" | "DeviceTokenNotForTopic") => Delivery::Gone,
        _ => Delivery::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webpush::unb64;
    use p256::ecdsa::signature::Verifier;
    use p256::pkcs8::{EncodePrivateKey, LineEnding};

    fn key() -> p256::SecretKey {
        p256::SecretKey::from_slice(&[7u8; 32]).unwrap()
    }

    #[test]
    fn reads_the_private_key_of_a_p8_file() {
        let pem = key().to_pkcs8_pem(LineEnding::LF).unwrap();
        assert_eq!(parse_p8(&pem).unwrap(), vec![7u8; 32]);
        assert!(parse_p8("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----").is_err());
    }

    #[test]
    fn key_id_comes_from_apples_file_name() {
        assert_eq!(key_id_from_name("AuthKey_ZSRCM3A69Y.p8").as_deref(), Some("ZSRCM3A69Y"));
        assert_eq!(key_id_from_name("key.p8"), None);
        assert_eq!(key_id_from_name("AuthKey_short.p8"), None);
    }

    #[test]
    fn provider_token_is_es256_with_kid_and_issuer() {
        let jwt = provider_token(&[7u8; 32], "ZSRCM3A69Y", "RUJR5DY35F", 1_700_000_000).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: serde_json::Value = serde_json::from_slice(&unb64(parts[0]).unwrap()).unwrap();
        assert_eq!(header["alg"], "ES256");
        assert_eq!(header["kid"], "ZSRCM3A69Y");
        let claims: serde_json::Value = serde_json::from_slice(&unb64(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["iss"], "RUJR5DY35F");
        assert_eq!(claims["iat"], 1_700_000_000u64);
        let public = *SigningKey::from(key()).verifying_key();
        let sig = Signature::from_slice(&unb64(parts[2]).unwrap()).unwrap();
        assert!(public.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig).is_ok());
    }

    #[test]
    fn a_token_is_reused_for_forty_minutes() {
        assert!(token_reusable(1000, 1000));
        assert!(token_reusable(1000, 1000 + TOKEN_REUSE_SECS - 1));
        assert!(!token_reusable(1000, 1000 + TOKEN_REUSE_SECS));
        // A clock that went back: make a new one.
        assert!(!token_reusable(2000, 1000));
    }

    #[test]
    fn payload_carries_alert_thread_and_url() {
        let p: serde_json::Value = serde_json::from_str(&payload("Fix login", "Tests pass", "work/fix-login", "/?task=work/fix-login")).unwrap();
        assert_eq!(p["aps"]["alert"]["title"], "Fix login");
        assert_eq!(p["aps"]["alert"]["body"], "Tests pass");
        assert_eq!(p["aps"]["thread-id"], "work/fix-login");
        assert_eq!(p["url"], "/?task=work/fix-login");
    }

    #[test]
    fn device_tokens_and_ids_are_checked() {
        assert!(valid_device_token(&"ab".repeat(32)));
        assert!(!valid_device_token("abc"));
        assert!(!valid_device_token(&"zz".repeat(32)));
        assert!(valid_key_id("RUJR5DY35F"));
        assert!(!valid_key_id("rujr5dy35f"));
        assert_eq!(collapse_id("work/x"), Some("work/x"));
        assert_eq!(collapse_id(&"x".repeat(65)), None);
    }

    #[test]
    fn answers_decide_whether_the_device_is_kept() {
        assert_eq!(delivery(200, ""), Delivery::Sent);
        assert_eq!(delivery(410, r#"{"reason":"Unregistered"}"#), Delivery::Gone);
        assert_eq!(delivery(400, r#"{"reason":"BadDeviceToken"}"#), Delivery::Gone);
        assert_eq!(delivery(400, r#"{"reason":"DeviceTokenNotForTopic"}"#), Delivery::Gone);
        assert_eq!(delivery(400, r#"{"reason":"BadCollapseId"}"#), Delivery::Failed);
        assert_eq!(delivery(403, r#"{"reason":"ExpiredProviderToken"}"#), Delivery::Failed);
        assert_eq!(delivery(429, ""), Delivery::Failed);
    }
}
