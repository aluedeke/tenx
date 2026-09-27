//! Web Push, the cryptographic half: encrypting a message for one browser
//! subscription (RFC 8291, `aes128gcm`) and signing the VAPID token that
//! identifies the sender to the push service (RFC 8292, an ES256 JWT).
//!
//! Pure: every random input — the sender's ephemeral key, the salt, the VAPID
//! key — is handed in by the caller (`tenx web` reads `/dev/urandom`), so the
//! RFC's own test vector reproduces byte for byte and nothing here touches
//! the network or the disk. Sending is the binary's job (`curl`).

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};
use base64::alphabet::URL_SAFE;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use base64::engine::DecodePaddingMode;
use base64::Engine;
use hkdf::Hkdf;
use p256::ecdsa::signature::{Signer, Verifier};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{PublicKey, SecretKey};
use sha2::Sha256;

/// base64url as Web Push speaks it: no padding out, padding or not in.
const B64: GeneralPurpose = GeneralPurpose::new(
    &URL_SAFE,
    GeneralPurposeConfig::new().with_encode_padding(false).with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// The record size written into the header. One record holds any message a
/// notification needs (a push service caps payloads at 4 KB anyway).
const RECORD_SIZE: u32 = 4096;

pub fn b64(bytes: &[u8]) -> String {
    B64.encode(bytes)
}

/// Decode base64url, ignoring whitespace (the RFC's vectors are wrapped).
pub fn unb64(s: &str) -> Result<Vec<u8>, String> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    B64.decode(s).map_err(|e| format!("bad base64url: {e}"))
}

fn secret(bytes: &[u8]) -> Result<SecretKey, String> {
    SecretKey::from_slice(bytes).map_err(|_| "not a P-256 private key".to_string())
}

fn public(bytes: &[u8]) -> Result<PublicKey, String> {
    PublicKey::from_sec1_bytes(bytes).map_err(|_| "not a P-256 public key".to_string())
}

/// The uncompressed public key (65 bytes, `0x04 || x || y`) of a private one.
pub fn public_key(private: &[u8]) -> Result<Vec<u8>, String> {
    Ok(secret(private)?.public_key().to_encoded_point(false).as_bytes().to_vec())
}

/// Whether 32 random bytes make a usable private key (a scalar in range;
/// all but astronomically unlikely draws do). The caller draws again if not.
pub fn valid_private_key(bytes: &[u8]) -> bool {
    bytes.len() == 32 && SecretKey::from_slice(bytes).is_ok()
}

/// The content-encryption key and nonce both sides derive (RFC 8291 §3.3–3.4).
fn derive(ecdh: &[u8], auth: &[u8], ua_public: &[u8], as_public: &[u8], salt: &[u8]) -> Result<([u8; 16], [u8; 12]), String> {
    let mut key_info = b"WebPush: info\0".to_vec();
    key_info.extend_from_slice(ua_public);
    key_info.extend_from_slice(as_public);
    let mut ikm = [0u8; 32];
    Hkdf::<Sha256>::new(Some(auth), ecdh).expand(&key_info, &mut ikm).map_err(|e| e.to_string())?;
    let prk = Hkdf::<Sha256>::new(Some(salt), &ikm);
    let mut cek = [0u8; 16];
    let mut nonce = [0u8; 12];
    prk.expand(b"Content-Encoding: aes128gcm\0", &mut cek).map_err(|e| e.to_string())?;
    prk.expand(b"Content-Encoding: nonce\0", &mut nonce).map_err(|e| e.to_string())?;
    Ok((cek, nonce))
}

/// Encrypt `plaintext` for the subscription whose keys are `ua_public`
/// (`p256dh`) and `auth`, from the one-off sender key `as_private` with
/// `salt` (16 random bytes): the request body, header included.
pub fn encrypt(plaintext: &[u8], ua_public: &[u8], auth: &[u8], as_private: &[u8], salt: &[u8]) -> Result<Vec<u8>, String> {
    if salt.len() != 16 {
        return Err("the salt is 16 bytes".into());
    }
    let as_key = secret(as_private)?;
    let as_public = as_key.public_key().to_encoded_point(false);
    let ua = public(ua_public)?;
    let ecdh = p256::ecdh::diffie_hellman(as_key.to_nonzero_scalar(), ua.as_affine());
    let (cek, nonce) = derive(ecdh.raw_secret_bytes(), auth, ua_public, as_public.as_bytes(), salt)?;
    // One record, the last: the content, then the 0x02 delimiter.
    let mut record = plaintext.to_vec();
    record.push(2);
    let sealed = Aes128Gcm::new_from_slice(&cek)
        .map_err(|e| e.to_string())?
        .encrypt(Nonce::from_slice(&nonce), record.as_slice())
        .map_err(|_| "encryption failed".to_string())?;
    let mut body = salt.to_vec();
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(as_public.as_bytes().len() as u8);
    body.extend_from_slice(as_public.as_bytes());
    body.extend_from_slice(&sealed);
    Ok(body)
}

/// The receiving side of [`encrypt`], as a browser does it — for tests, and
/// for anything that wants to check what it sent.
pub fn decrypt(body: &[u8], ua_private: &[u8], auth: &[u8]) -> Result<Vec<u8>, String> {
    let short = || "truncated message".to_string();
    let salt = body.get(..16).ok_or_else(short)?;
    let id_len = *body.get(20).ok_or_else(short)? as usize;
    let as_public = body.get(21..21 + id_len).ok_or_else(short)?;
    let sealed = body.get(21 + id_len..).ok_or_else(short)?;
    let ua_key = secret(ua_private)?;
    let ua_public = ua_key.public_key().to_encoded_point(false);
    let ecdh = p256::ecdh::diffie_hellman(ua_key.to_nonzero_scalar(), public(as_public)?.as_affine());
    let (cek, nonce) = derive(ecdh.raw_secret_bytes(), auth, ua_public.as_bytes(), as_public, salt)?;
    let mut record = Aes128Gcm::new_from_slice(&cek)
        .map_err(|e| e.to_string())?
        .decrypt(Nonce::from_slice(&nonce), sealed)
        .map_err(|_| "decryption failed".to_string())?;
    // Strip the padding back to the delimiter.
    while record.last() == Some(&0) {
        record.pop();
    }
    match record.pop() {
        Some(2) => Ok(record),
        _ => Err("no final-record delimiter".into()),
    }
}

/// `scheme://host[:port]` of a push endpoint — the VAPID token's audience.
pub fn endpoint_origin(endpoint: &str) -> Option<String> {
    let (scheme, rest) = endpoint.split_once("://")?;
    let host = rest.split(['/', '?', '#']).next()?;
    (!host.is_empty() && (scheme == "https" || scheme == "http")).then(|| format!("{scheme}://{host}"))
}

/// The VAPID token for `aud` (a push service's origin), valid until `exp`
/// (seconds since the epoch; RFC 8292 caps it at 24 h out), naming `sub`.
pub fn vapid_jwt(private: &[u8], aud: &str, exp: u64, sub: &str) -> Result<String, String> {
    let key = SigningKey::from_slice(private).map_err(|_| "not a P-256 private key".to_string())?;
    let header = b64(br#"{"typ":"JWT","alg":"ES256"}"#);
    let claims = b64(serde_json::json!({ "aud": aud, "exp": exp, "sub": sub }).to_string().as_bytes());
    let signing_input = format!("{header}.{claims}");
    let sig: Signature = key.sign(signing_input.as_bytes());
    Ok(format!("{signing_input}.{}", b64(&sig.to_bytes())))
}

/// Whether `jwt` is an ES256 token signed by `public` (uncompressed).
pub fn verify_jwt(jwt: &str, public: &[u8]) -> bool {
    let Some((input, sig)) = jwt.rsplit_once('.') else { return false };
    let (Ok(key), Ok(sig)) = (VerifyingKey::from_sec1_bytes(public), unb64(sig)) else { return false };
    let Ok(sig) = Signature::from_slice(&sig) else { return false };
    key.verify(input.as_bytes(), &sig).is_ok()
}

/// The `Authorization` header for a push request (RFC 8292 §3).
pub fn authorization(jwt: &str, public: &[u8]) -> String {
    format!("vapid t={jwt}, k={}", b64(public))
}

/// What a push service's HTTP status means for the subscription.
#[derive(Debug, PartialEq, Eq)]
pub enum Delivery {
    Sent,
    /// 404 / 410: the browser unsubscribed or the subscription expired —
    /// forget it.
    Gone,
    /// Anything else: keep the subscription, the next push may get through.
    Failed,
}

pub fn delivery(status: u16) -> Delivery {
    match status {
        200..=299 => Delivery::Sent,
        404 | 410 => Delivery::Gone,
        _ => Delivery::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 8291 §5 and Appendix A.
    const PLAINTEXT: &str = "When I grow up, I want to be a watermelon";
    const AS_PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
    const AS_PUBLIC: &str = "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";
    const UA_PRIVATE: &str = "q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94";
    const UA_PUBLIC: &str = "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
    const SALT: &str = "DGv6ra1nlYgDCS1FRnbzlw";
    const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";
    const BODY: &str = "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27ml
        mlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPT
        pK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN";

    fn b(s: &str) -> Vec<u8> {
        unb64(s).unwrap()
    }

    #[test]
    fn encrypts_the_rfc_8291_example_byte_for_byte() {
        assert_eq!(public_key(&b(AS_PRIVATE)).unwrap(), b(AS_PUBLIC));
        let body = encrypt(PLAINTEXT.as_bytes(), &b(UA_PUBLIC), &b(AUTH), &b(AS_PRIVATE), &b(SALT)).unwrap();
        assert_eq!(b64(&body), b64(&b(BODY)));
    }

    #[test]
    fn the_receiver_decrypts_it() {
        assert_eq!(decrypt(&b(BODY), &b(UA_PRIVATE), &b(AUTH)).unwrap(), PLAINTEXT.as_bytes());
        // The wrong auth secret: the tag doesn't verify.
        assert!(decrypt(&b(BODY), &b(UA_PRIVATE), &[0u8; 16]).is_err());
    }

    #[test]
    fn a_vapid_token_verifies_against_its_public_key_only() {
        let private = b(AS_PRIVATE);
        let jwt = vapid_jwt(&private, "https://push.example.net", 1_700_000_000, "https://github.com/aluedeke/tenx").unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let claims: serde_json::Value = serde_json::from_slice(&b(parts[1])).unwrap();
        assert_eq!(claims["aud"], "https://push.example.net");
        assert_eq!(claims["exp"], 1_700_000_000u64);
        assert!(verify_jwt(&jwt, &public_key(&private).unwrap()));
        assert!(!verify_jwt(&jwt, &b(UA_PUBLIC)), "another key");
        assert!(!verify_jwt(&jwt.replace(".ey", ".eX"), &public_key(&private).unwrap()), "tampered");
        assert!(authorization(&jwt, &b(AS_PUBLIC)).starts_with(&format!("vapid t={jwt}, k=BP4z")));
    }

    #[test]
    fn the_audience_is_the_endpoints_origin() {
        assert_eq!(
            endpoint_origin("https://web.push.apple.com/QGuQyavXutnMM?x=1").as_deref(),
            Some("https://web.push.apple.com")
        );
        assert_eq!(endpoint_origin("http://127.0.0.1:5000/push/abc").as_deref(), Some("http://127.0.0.1:5000"));
        assert_eq!(endpoint_origin("ftp://x/y"), None);
        assert_eq!(endpoint_origin("nonsense"), None);
    }

    #[test]
    fn gone_subscriptions_are_dropped_others_kept() {
        assert_eq!(delivery(201), Delivery::Sent);
        assert_eq!(delivery(410), Delivery::Gone);
        assert_eq!(delivery(404), Delivery::Gone);
        assert_eq!(delivery(429), Delivery::Failed);
        assert_eq!(delivery(0), Delivery::Failed);
    }

    #[test]
    fn keys_are_checked() {
        assert!(valid_private_key(&b(AS_PRIVATE)));
        assert!(!valid_private_key(&[0u8; 32]), "zero is not a scalar");
        assert!(!valid_private_key(&[1u8; 31]));
        assert!(encrypt(b"x", &[4u8; 65], &b(AUTH), &b(AS_PRIVATE), &b(SALT)).is_err(), "not a curve point");
    }
}
