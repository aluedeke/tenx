//! The page: `web/out` (the static Next.js export) embedded at build time,
//! or the stub build.rs writes when it wasn't built (`TENX_WEB_DIR`).

use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::response::Response;

#[derive(rust_embed::RustEmbed)]
#[folder = "$TENX_WEB_DIR"]
struct Page;

/// The embedded file for a request path: `/` is `index.html`, and a
/// directory-style route of the export (`/foo`) is `foo.html` or
/// `foo/index.html`.
pub fn get(path: &str) -> Option<Response> {
    let path = path.trim_start_matches('/');
    let candidates = if path.is_empty() {
        vec!["index.html".to_string()]
    } else {
        vec![path.to_string(), format!("{path}.html"), format!("{}/index.html", path.trim_end_matches('/'))]
    };
    candidates.iter().find_map(|p| {
        let file = Page::get(p)?;
        let mime = file.metadata.mimetype().to_string();
        // Next's hashed bundles never change under a name; the pages do.
        let cache = if p.starts_with("_next/static/") { "public, max-age=31536000, immutable" } else { "no-cache" };
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime)
            .header(header::CACHE_CONTROL, cache)
            .body(Body::from(file.data.into_owned()))
            .ok()
    })
}

/// An embedded file's text, for the few the server rewrites (the manifest).
pub fn text(path: &str) -> Option<String> {
    let file = Page::get(path.trim_start_matches('/'))?;
    String::from_utf8(file.data.into_owned()).ok()
}
