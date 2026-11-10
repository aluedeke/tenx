//! `POST /clipboard/get`, `/clipboard/image` and `/clipboard/set`: the
//! clipboard of the machine `tenx web` runs on, for the key bar's two
//! clipboard keys and the iOS app's sync — text or an image copied on a
//! phone goes to the Mac's clipboard, and what is copied on the Mac comes to
//! the phone's.
//!
//! macOS: the pasteboard through `osascript` (`pasteboard-get.js`,
//! `pasteboard-set.js`) — `pbpaste` knows text only, and the pasteboard's
//! change count is what lets a sync ask once a second and read an image once
//! per copy. Text is written with `pbcopy`, with a UTF-8 locale set — under
//! launchd (`tenx web service`) there may be none, and its text encoding
//! follows it. Elsewhere: `wl-copy`/`wl-paste`, else `xclip` — they have no
//! change count, so an image there is read on every request.
//! `TENX_WEB_CLIPBOARD` names a file to use instead (an image in
//! `<file>.image`, its type in `<file>.image-type`), for the end-to-end test.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use axum::body::Bytes;
use tenx_core::web;

/// What a clipboard holds for the other device.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Clip {
    /// Empty when it holds nothing to carry over.
    Text(String),
    Image(Image),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Image {
    /// `web::clip_image_id` of `bytes`.
    pub id: String,
    /// `image/png`, `image/jpeg`, `image/gif` or `image/webp`.
    pub mime: String,
    pub bytes: Bytes,
}

impl Image {
    fn new(mime: &str, bytes: Vec<u8>) -> Self {
        Image { id: web::clip_image_id(&bytes), mime: mime.to_string(), bytes: bytes.into() }
    }
}

/// A clipboard program and its arguments.
type Program<'a> = (&'static str, &'a [&'a str]);

/// The programs that read or write text, in the order they are tried.
fn programs(write: bool) -> &'static [Program<'static>] {
    if cfg!(target_os = "macos") {
        if write { &[("pbcopy", &[])] } else { &[("pbpaste", &[])] }
    } else if write {
        &[("wl-copy", &[]), ("xclip", &["-selection", "clipboard", "-in"])]
    } else {
        &[("wl-paste", &["--no-newline"]), ("xclip", &["-selection", "clipboard", "-out"])]
    }
}

/// `programs` that can work here: not wl-clipboard in an X11 session —
/// installed beside xclip there, it would fail before xclip got its turn.
fn usable<'a, 'p>(programs: &'a [Program<'p>]) -> impl Iterator<Item = Program<'p>> + 'a {
    let set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    // With neither named (a service's environment), wl-clipboard still
    // finds the default Wayland socket: it keeps its turn.
    let x11_only = set("DISPLAY") && !set("WAYLAND_DISPLAY");
    programs.iter().copied().filter(move |p| !(x11_only && p.0.starts_with("wl-")))
}

fn command((program, args): Program) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args);
    if cfg!(target_os = "macos") {
        cmd.env("LC_ALL", "en_US.UTF-8");
    }
    cmd
}

/// What the first of `programs` that is installed prints.
fn read_with(programs: &[Program]) -> Result<Vec<u8>> {
    let mut last = None;
    for program in usable(programs) {
        match command(program).stdin(Stdio::null()).stderr(Stdio::null()).output() {
            Ok(out) if out.status.success() => return Ok(out.stdout),
            // wl-paste fails on an empty clipboard; that is no reason to try xclip.
            Ok(out) => last = Some(anyhow::anyhow!("{} exited with {}", program.0, out.status)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("run {}", program.0)),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("no clipboard program found ({})", names(programs))))
}

/// Hand `bytes` to the first of `programs` that is installed.
fn write_with(programs: &[Program], bytes: &[u8]) -> Result<()> {
    for program in usable(programs) {
        let mut child = match command(program).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("run {}", program.0)),
        };
        // Dropped after the write, so the program sees the end of its input.
        child.stdin.take().context("clipboard program's stdin")?.write_all(bytes)?;
        let status = child.wait()?;
        if !status.success() {
            bail!("{} exited with {status}", program.0);
        }
        return Ok(());
    }
    bail!("no clipboard program found ({})", names(programs))
}

fn names(programs: &[Program]) -> String {
    programs.iter().map(|p| p.0).collect::<Vec<_>>().join(", ")
}

/// A Linux clipboard: the types it offers decide what is read, since asked
/// for text, both programs hand over an image's bytes when there is none.
fn linux_get() -> Result<Clip> {
    let offered = read_with(&[("wl-paste", &["--list-types"]), ("xclip", &["-selection", "clipboard", "-t", "TARGETS", "-out"])]);
    let offered = offered.map(|out| String::from_utf8_lossy(&out).into_owned()).unwrap_or_default();
    let image = web::clip_image_type(&offered);
    // Types that name no text: nothing to read as text, image or not.
    let text = if !offered.trim().is_empty() && !web::clip_offers_text(&offered) { String::new() } else { get_text()? };
    if let Some(mime) = image.filter(|_| web::clip_prefers_image(&text)) {
        let bytes = read_with(&[("wl-paste", &["--type", mime]), ("xclip", &["-selection", "clipboard", "-t", mime, "-out"])])?;
        if !bytes.is_empty() && bytes.len() <= web::PASTE_MAX_BYTES {
            return Ok(Clip::Image(Image::new(mime, bytes)));
        }
    }
    Ok(Clip::Text(text))
}

/// The test's stand-in for the clipboard, when set.
fn file() -> Option<PathBuf> {
    std::env::var_os("TENX_WEB_CLIPBOARD").filter(|v| !v.is_empty()).map(Into::into)
}

/// Where the stand-in keeps an image, and its type.
fn image_files(path: &Path) -> (PathBuf, PathBuf) {
    let with = |suffix: &str| PathBuf::from(format!("{}{suffix}", path.display()));
    (with(".image"), with(".image-type"))
}

/// What the macOS pasteboard held at its change count: the sync asks every
/// second, and reads the pasteboard only after a copy.
static SEEN: Mutex<Option<(i64, Clip)>> = Mutex::new(None);

/// What is on this machine's clipboard: an image when it holds one and no
/// text of its own (`web::clip_prefers_image`), else its text.
pub(super) fn get() -> Result<Clip> {
    if let Some(path) = file() {
        let (image, mime) = image_files(&path);
        if let (Ok(bytes), Ok(mime)) = (std::fs::read(&image), std::fs::read_to_string(&mime)) {
            return Ok(Clip::Image(Image::new(mime.trim(), bytes)));
        }
        return Ok(Clip::Text(std::fs::read_to_string(&path).unwrap_or_default()));
    }
    if cfg!(target_os = "macos") {
        // Held across the read: two requests at once read the pasteboard once.
        let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        let (count, clip) = read_pasteboard("", seen.as_ref().map(|s| s.0))?;
        return Ok(match (clip, seen.as_ref()) {
            (Some(clip), _) => {
                *seen = Some((count, clip.clone()));
                clip
            }
            (None, Some((_, clip))) => clip.clone(),
            (None, None) => bail!("the pasteboard script named no content"),
        });
    }
    linux_get()
}

/// The image on this machine's clipboard, if it is still the one named `id`.
pub(super) fn image(id: &str) -> Result<Option<Image>> {
    Ok(match get()? {
        Clip::Image(image) if image.id == id => Some(image),
        _ => None,
    })
}

fn get_text() -> Result<String> {
    read_with(programs(false)).map(|out| String::from_utf8_lossy(&out).into_owned())
}

/// Put `text` on this machine's clipboard.
pub(super) fn set(text: &str) -> Result<()> {
    if let Some(path) = file() {
        let (image, mime) = image_files(&path);
        let _ = std::fs::remove_file(image);
        let _ = std::fs::remove_file(mime);
        return std::fs::write(&path, text).with_context(|| format!("write {}", path.display()));
    }
    write_with(programs(true), text.as_bytes())
}

/// Put an image of `content_type` on this machine's clipboard; the id
/// `get` names it by from now on.
pub(super) fn set_image(content_type: &str, bytes: &[u8]) -> Result<String> {
    let uti = web::clip_image_uti(content_type).with_context(|| format!("not an image the clipboard takes: {content_type:?}"))?;
    if let Some(path) = file() {
        let (image, mime) = image_files(&path);
        std::fs::write(&image, bytes).with_context(|| format!("write {}", image.display()))?;
        std::fs::write(&mime, content_type).with_context(|| format!("write {}", mime.display()))?;
        return Ok(web::clip_image_id(bytes));
    }
    if !cfg!(target_os = "macos") {
        let mime = web::clip_image_mime(content_type).unwrap_or("image/png");
        write_with(&[("wl-copy", &["--type", mime]), ("xclip", &["-selection", "clipboard", "-t", mime, "-in"])], bytes)?;
        return Ok(web::clip_image_id(bytes));
    }
    write_pasteboard_image("", uti, bytes)?;
    // As the pasteboard hands it back, so the sender knows its own image.
    Ok(match get()? {
        Clip::Image(image) => image.id,
        Clip::Text(_) => web::clip_image_id(bytes),
    })
}

fn osascript(script: &str) -> Command {
    let mut cmd = Command::new("osascript");
    cmd.args(["-l", "JavaScript", "-e", script]);
    cmd
}

/// The macOS pasteboard `board` ("" for the general one): its change count,
/// and what it holds unless the count is still `since`.
fn read_pasteboard(board: &str, since: Option<i64>) -> Result<(i64, Option<Clip>)> {
    let since = since.map(|c| c.to_string()).unwrap_or_default();
    let out = osascript(include_str!("pasteboard-get.js"))
        .args([board, &since, &web::PASTE_MAX_BYTES.to_string()])
        .stdin(Stdio::null())
        .output()
        .context("run osascript")?;
    if !out.status.success() {
        bail!("osascript: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let end = out.stdout.iter().position(|&b| b == b'\n').context("the pasteboard script printed nothing")?;
    let head: serde_json::Value = serde_json::from_slice(&out.stdout[..end]).context("the pasteboard script's reply")?;
    let count = head["count"].as_i64().context("the pasteboard's change count")?;
    let Some(text) = head["text"].as_str() else { return Ok((count, None)) };
    let clip = match head["image"]["type"].as_str() {
        // A file's name is no text of its own, whatever it looks like.
        Some(mime) if head["file"].as_bool() == Some(true) || web::clip_prefers_image(text) => {
            Clip::Image(Image::new(mime, out.stdout[end + 1..].to_vec()))
        }
        _ => Clip::Text(text.to_string()),
    };
    Ok((count, Some(clip)))
}

fn write_pasteboard_image(board: &str, uti: &str, bytes: &[u8]) -> Result<()> {
    let mut child = osascript(include_str!("pasteboard-set.js"))
        .args([board, uti])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("run osascript")?;
    // Dropped after the write, so the script sees the end of its input.
    child.stdin.take().context("osascript's stdin")?.write_all(bytes)?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("osascript: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1×1 PNG.
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf,
        0xc0, 0xf0, 0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60,
        0x82,
    ];

    /// On a pasteboard of its own, never the user's clipboard.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_image_written_to_a_pasteboard_is_read_back_once_per_copy() {
        let board = format!("tenx-test-{}", std::process::id());
        write_pasteboard_image(&board, "public.png", PNG).unwrap();
        let (count, clip) = read_pasteboard(&board, None).unwrap();
        assert_eq!(clip, Some(Clip::Image(Image::new("image/png", PNG.to_vec()))));
        let (again, clip) = read_pasteboard(&board, Some(count)).unwrap();
        assert_eq!((again, clip), (count, None), "no copy since: nothing is read");
    }

    /// Writes the clipboard, so only by hand and where it is nobody's:
    /// `xvfb-run cargo test -p tenx-cli --bin tenx linux_clipboard -- --ignored`
    /// (xclip), or under a headless Wayland compositor (wl-clipboard).
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "writes the clipboard; needs a display"]
    fn a_linux_clipboard_carries_an_image_and_then_text_again() {
        let id = set_image("image/png", PNG).unwrap();
        assert_eq!(get().unwrap(), Clip::Image(Image::new("image/png", PNG.to_vec())));
        assert_eq!(image(&id).unwrap().map(|i| i.bytes.to_vec()), Some(PNG.to_vec()));
        set("grüße").unwrap();
        assert_eq!(get().unwrap(), Clip::Text("grüße".into()));
        assert_eq!(image(&id).unwrap(), None);
    }
}
