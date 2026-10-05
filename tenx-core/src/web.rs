//! Decisions `tenx web` makes about a request, as functions over the request's
//! plain parts: whether it is authorised (token, cookie, `Origin`), how the
//! column is laid out for a page's width, and what a browser tab's grouped
//! session is called. The server (`src/web/`) does the I/O around them; the
//! wire contract is `docs/web-protocol.md`.

/// The cookie a page carries once it has shown the token.
pub const COOKIE: &str = "tenx_web";

/// The prefix of every browser tab's grouped session: `tenx-web-<id>`.
pub const SESSION_PREFIX: &str = "tenx-web-";

/// Equal, in time that depends only on the lengths — a token compared with
/// `==` stops at the first wrong byte, which is measurable from outside.
pub fn token_matches(given: &str, expected: &str) -> bool {
    if expected.is_empty() || given.len() != expected.len() {
        return false;
    }
    given.bytes().zip(expected.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

/// The value of cookie `name` in a `Cookie` request header
/// (`a=1; tenx_web=…`).
pub fn cookie_value<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').find_map(|pair| {
        let (k, v) = pair.trim().split_once('=')?;
        (k == name).then_some(v)
    })
}

/// Paths served without the cookie: the web app manifest and the icons it
/// names. Browsers fetch a manifest without credentials (unless the page asks
/// otherwise), and an installed PWA's icons the same way, so gating them only
/// breaks installing the page; they are the same bytes as the public logo.
pub fn public_path(path: &str) -> bool {
    matches!(
        path,
        "/manifest.webmanifest"
            | "/favicon.svg"
            | "/favicon-16.png"
            | "/favicon-32.png"
            | "/tenx-mark-256.png"
            | "/icon-192.png"
            | "/icon-512.png"
            | "/icon-maskable-512.png"
            | "/apple-touch-icon.png"
            | "/badge-96.png"
            // The service worker: static code with nothing in it, and an app
            // installed to an iPhone's Home Screen has a cookie jar of its own
            // until its first `?token=`.
            | "/sw.js"
    )
}

/// The manifest as a signed-in page gets it: `start_url` carries the token.
/// An app installed to an iPhone or iPad Home Screen keeps its own cookies,
/// apart from Safari's, so without this its first launch would land on the
/// "open the address tenx web printed" page; with it, the launch is the
/// token swap every browser starts with. Anything that isn't a JSON object
/// comes back as it was.
pub fn manifest_with_token(manifest: &str, token: &str) -> String {
    let Ok(mut v) = serde_json::from_str::<serde_json::Value>(manifest) else { return manifest.to_string() };
    let Some(obj) = v.as_object_mut() else { return manifest.to_string() };
    obj.insert("start_url".into(), serde_json::Value::String(format!("/?token={token}")));
    // The app's identity stays "/", whatever its start URL: rotating the
    // token must not make the installed app a different one.
    obj.entry("id").or_insert_with(|| serde_json::Value::String("/".into()));
    v.to_string()
}

/// The largest image `/paste` takes: a phone camera's full-size JPEG fits;
/// anything bigger is refused before it is written.
pub const PASTE_MAX_BYTES: usize = 25 * 1024 * 1024;

/// How long a pasted image is kept. It only has to outlive the agent reading
/// it; older ones are swept on the next paste.
pub const PASTE_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// The file extension a pasted image is saved under, by its `Content-Type` —
/// only the formats an agent can read (PNG, JPEG, GIF, WebP); anything else
/// is refused.
pub fn paste_ext(content_type: &str) -> Option<&'static str> {
    let mime = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    match mime.as_str() {
        "image/png" => Some("png"),
        "image/jpeg" | "image/jpg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

/// Whether a signed-in request for `path` renews the cookie: loading the page
/// itself (`/`, an `.html` page) does, so a page or an installed app that is
/// used keeps its year-long sign-in for good — only `--rotate-token` ends it.
/// Assets and API calls don't: once per visit is enough.
pub fn renews_cookie(path: &str) -> bool {
    path == "/" || path.ends_with(".html")
}

/// Whether a request's cookie header carries the token.
pub fn cookie_ok(cookie_header: Option<&str>, token: &str) -> bool {
    cookie_header.and_then(|h| cookie_value(h, COOKIE)).is_some_and(|v| token_matches(v, token))
}

/// The `Set-Cookie` that hands the page the token: only ever sent back to
/// this server (`SameSite=Strict`), never readable by script (`HttpOnly`),
/// for the whole site. No `Expires`: it lasts as long as the token does,
/// which `--rotate-token` ends.
pub fn set_cookie(token: &str) -> String {
    format!("{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000")
}

/// The `Origin` a WebSocket upgrade must come from: the page this server
/// served (`http(s)://<Host>`), or one of the `--dev-origin`s (`next dev` on
/// another port). No `Origin` at all is refused — every browser sends one on
/// a WebSocket, so its absence means something that isn't a page of ours,
/// and any other page could otherwise drive your terminal through your
/// cookie (cross-site WebSocket hijacking; `SameSite` doesn't cover
/// WebSockets in every browser).
pub fn origin_allowed(origin: Option<&str>, host: Option<&str>, dev_origins: &[String]) -> bool {
    let Some(origin) = origin else { return false };
    let origin = origin.trim_end_matches('/');
    if dev_origins.iter().any(|d| d.trim_end_matches('/') == origin) {
        return true;
    }
    let Some(host) = host else { return false };
    origin == format!("http://{host}") || origin == format!("https://{host}")
}

/// Whether `origin` is one of the `--dev-origin`s.
pub fn is_dev_origin(origin: Option<&str>, dev_origins: &[String]) -> bool {
    origin.is_some_and(|o| dev_origins.iter().any(|d| d.trim_end_matches('/') == o.trim_end_matches('/')))
}

/// Whether a WebSocket upgrade may proceed: from an allowed origin, and
/// either with the cookie or — only from a dev origin, whose page can't have
/// the cookie of another port's server — with the token as `?token=`.
pub fn ws_authorized(cookie_ok: bool, query_token_ok: bool, origin_allowed: bool, dev_origin: bool) -> bool {
    origin_allowed && (cookie_ok || (dev_origin && query_token_ok))
}

/// Whether `host` (an address to listen on) only takes connections from this
/// machine — anything else gets a warning, since the page is a shell.
pub fn is_loopback(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    h == "localhost" || h == "::1" || h.starts_with("127.")
}

/// The column's layout for a page `page_cols` cells wide: its width
/// (`column::width`, the TUI's rule) and whether the page is narrow — under
/// `small_cols`, where the column is an overlay over the terminal rather
/// than beside it.
pub fn layout(page_cols: u16, configured: u16, small_cols: u16) -> (u16, bool) {
    let narrow = page_cols < small_cols;
    (crate::column::width(page_cols, configured), narrow)
}

/// Whether `id` can name a tab's session: lowercase hex, 8–64 long. It comes
/// from the page (`?session=`), so it is checked before it reaches a tmux
/// target, where `:`/`.` and the like mean something.
pub fn valid_session_id(id: &str) -> bool {
    (8..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The grouped session a tab with session id `id` attaches to.
pub fn session_name(id: &str) -> String {
    format!("{SESSION_PREFIX}{id}")
}

/// Lowercase hex of `bytes` — tokens and session ids.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_matches_only_the_same_token() {
        assert!(token_matches("abc123", "abc123"));
        assert!(!token_matches("abc124", "abc123"));
        assert!(!token_matches("abc12", "abc123"));
        assert!(!token_matches("", ""), "an empty token never authorises");
    }

    #[test]
    fn cookie_is_found_among_others() {
        let h = "theme=dark; tenx_web=s3cret; other=1";
        assert_eq!(cookie_value(h, COOKIE), Some("s3cret"));
        assert!(cookie_ok(Some(h), "s3cret"));
        assert!(!cookie_ok(Some(h), "other"));
        assert!(!cookie_ok(Some("tenx_webx=s3cret"), "s3cret"));
        assert!(!cookie_ok(None, "s3cret"));
    }

    #[test]
    fn set_cookie_is_strict_and_http_only() {
        let c = set_cookie("t");
        assert!(c.starts_with("tenx_web=t;"));
        assert!(c.contains("HttpOnly") && c.contains("SameSite=Strict") && c.contains("Path=/"));
    }

    #[test]
    fn origin_must_be_the_page_or_a_dev_origin() {
        let dev = vec!["http://localhost:3000".to_string()];
        assert!(origin_allowed(Some("http://127.0.0.1:7070"), Some("127.0.0.1:7070"), &[]));
        assert!(origin_allowed(Some("https://mbal.tail.ts.net"), Some("mbal.tail.ts.net"), &[]));
        assert!(origin_allowed(Some("http://localhost:3000/"), Some("127.0.0.1:7070"), &dev));
        assert!(!origin_allowed(Some("http://evil.example"), Some("127.0.0.1:7070"), &dev));
        assert!(!origin_allowed(None, Some("127.0.0.1:7070"), &dev), "no Origin is refused");
        assert!(!origin_allowed(Some("http://127.0.0.1:7070"), None, &[]));
    }

    #[test]
    fn query_token_only_counts_from_a_dev_origin() {
        assert!(ws_authorized(true, false, true, false));
        assert!(ws_authorized(false, true, true, true));
        assert!(!ws_authorized(false, true, true, false), "a ?token= from the page itself needs the cookie");
        assert!(!ws_authorized(true, true, false, true), "the origin check comes first");
    }

    #[test]
    fn loopback_hosts() {
        for h in ["127.0.0.1", "127.1.2.3", "localhost", "::1", "[::1]"] {
            assert!(is_loopback(h), "{h}");
        }
        for h in ["0.0.0.0", "100.64.0.1", "::", "mbal"] {
            assert!(!is_loopback(h), "{h}");
        }
    }

    #[test]
    fn layout_follows_the_tui() {
        assert_eq!(layout(200, 0, 100), (crate::column::width(200, 0), false));
        assert!(layout(60, 0, 100).1, "under 100 cells the column is an overlay");
    }

    #[test]
    fn session_ids_are_short_hex() {
        assert!(valid_session_id("0123abcd"));
        assert!(!valid_session_id("0123abc"));
        assert!(!valid_session_id("0123ABCD"));
        assert!(!valid_session_id("tenx:1.0"));
        assert_eq!(session_name("0123abcd"), "tenx-web-0123abcd");
        assert_eq!(hex(&[0, 15, 255]), "000fff");
    }

    #[test]
    fn only_the_install_bits_skip_the_cookie() {
        assert!(public_path("/manifest.webmanifest"));
        assert!(public_path("/tenx-mark-256.png"));
        assert!(!public_path("/"));
        assert!(!public_path("/index.html"));
        assert!(!public_path("/_next/static/chunks/main.js"));
        assert!(!public_path("/manifest.webmanifest/../index.html"));
    }

    #[test]
    fn only_images_an_agent_reads_can_be_pasted() {
        assert_eq!(paste_ext("image/png"), Some("png"));
        assert_eq!(paste_ext("image/JPEG; charset=binary"), Some("jpg"));
        assert_eq!(paste_ext("image/webp"), Some("webp"));
        assert_eq!(paste_ext("image/svg+xml"), None, "a script can hide in an SVG");
        assert_eq!(paste_ext("text/plain"), None);
        assert_eq!(paste_ext(""), None);
    }

    #[test]
    fn a_signed_in_manifest_starts_with_the_token() {
        let m = manifest_with_token(r#"{"name":"tenx","start_url":"/","id":"/"}"#, "abc");
        let v: serde_json::Value = serde_json::from_str(&m).unwrap();
        assert_eq!(v["start_url"], "/?token=abc");
        assert_eq!(v["id"], "/", "the app's identity doesn't change with the token");
        assert_eq!(manifest_with_token("not json", "abc"), "not json");
        assert!(public_path("/sw.js"));
        assert!(public_path("/icon-maskable-512.png"));
    }

    #[test]
    fn loading_the_page_renews_the_sign_in() {
        assert!(renews_cookie("/"));
        assert!(renews_cookie("/index.html"));
        assert!(!renews_cookie("/_next/static/chunks/app.js"));
        assert!(!renews_cookie("/manifest.webmanifest"));
        assert!(!renews_cookie("/push/key"));
    }
}
