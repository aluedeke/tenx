//! The speech server `tenx web`'s microphone asks first when `[speech] url`
//! is set: OpenAI's transcription API (`POST <url>/audio/transcriptions`,
//! multipart `file` as WAV) plus `languages` — the accepted languages, of
//! which the server says which one it heard — or `language`, fixed. msal's
//! stt-server (the fine-tuned Qwen3-ASR) is one; it answers `{"text": …,
//! "language": "de", …}` and gives no segments, so live dictation cuts at
//! pauses (`dictation`).
//!
//! The rules are tenx-agent's (its `SpeechServerSTT`): a request gets a time
//! limit that grows with the audio; a server that fails, times out or can't
//! be reached is left alone for [`PAUSE_SECS`] and `tenx-whisper` hears in
//! its place; a refusal (4xx) means a setting is wrong, so it isn't asked
//! again for that recording, but nothing waits.

use serde::Deserialize;

/// Seconds to connect, on top of the work.
pub const CONNECT_SECS: f32 = 1.5;
/// Seconds of work for any request, plus [`SECS_PER_AUDIO_SEC`] per second of
/// audio, at most [`MAX_SECS`].
pub const BASE_SECS: f32 = 1.0;
pub const SECS_PER_AUDIO_SEC: f32 = 0.1;
pub const MAX_SECS: f32 = 2.5;
/// How long a server that failed is left alone.
pub const PAUSE_SECS: u64 = 30;
/// A dictation keeps the language of its first run with this many words.
pub const LANGUAGE_MIN_WORDS: usize = 2;

/// The endpoint for `[speech] url`: the API's base (`…/v1`) or the endpoint
/// itself.
pub fn endpoint(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    if url.ends_with("/audio/transcriptions") { url.to_string() } else { format!("{url}/audio/transcriptions") }
}

/// (seconds to connect, seconds in all) for `audio_secs` of audio.
pub fn timeout(audio_secs: f32) -> (f32, f32) {
    let work = (BASE_SECS + SECS_PER_AUDIO_SEC * audio_secs).min(MAX_SECS);
    (CONNECT_SECS, CONNECT_SECS + work)
}

/// The form field that says which language to hear: `language` when the
/// page or an earlier run fixed it, else `languages` (when set; else the
/// server's own).
pub fn language_field(fixed: Option<&str>, languages: &[String]) -> Option<(&'static str, String)> {
    match fixed {
        Some(code) => Some(("language", code.to_string())),
        None if !languages.is_empty() => Some(("languages", languages.join(","))),
        None => None,
    }
}

/// What the server heard.
#[derive(Debug, Clone, PartialEq)]
pub struct Heard {
    pub text: String,
    /// Its language code (`de`), when it said one.
    pub language: Option<String>,
}

impl Heard {
    /// The language a recording keeps from now on: this run's, once it has
    /// enough words to tell — and only a code it may send back (one of
    /// `accepted`, or any code when that's empty), never a name.
    pub fn settled_language(&self, accepted: &[String]) -> Option<&str> {
        let words = self.text.split_whitespace().filter(|w| w.chars().any(char::is_alphanumeric)).count();
        let code = self.language.as_deref().filter(|l| is_code(l))?;
        (words >= LANGUAGE_MIN_WORDS && (accepted.is_empty() || accepted.iter().any(|a| a == code))).then_some(code)
    }
}

fn is_code(lang: &str) -> bool {
    (2..=3).contains(&lang.len()) && lang.bytes().all(|b| b.is_ascii_lowercase())
}

/// The language a recording starts with: the page's, else `[speech]
/// language`; `auto` (or nothing) leaves it to the server.
pub fn start_language(page: Option<&str>, config: &str) -> Option<String> {
    [page.unwrap_or(""), config].into_iter().map(str::trim).find(|l| !l.is_empty()).filter(|l| *l != "auto").map(str::to_string)
}

/// Why the server gave no text.
#[derive(Debug, Clone, PartialEq)]
pub enum Failure {
    /// It answered 4xx: a setting (`languages`, `url`) is wrong. Not
    /// asked again for this recording; no pause for others.
    Refused(String),
    /// Unreachable, too slow, 5xx or a reply that isn't one.
    Down(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Refused(e) => write!(f, "refused: {e} (check [speech] url and languages)"),
            Failure::Down(e) => write!(f, "{e}"),
        }
    }
}

/// The outcome of one request: whether `curl` got an answer (else its
/// error), the HTTP status and the body.
pub fn outcome(answered: Result<(), String>, status: &str, body: &str) -> Result<Heard, Failure> {
    answered.map_err(|e| Failure::Down(format!("unreachable: {e}")))?;
    let short = || body.chars().take(200).collect::<String>();
    match status.trim().parse::<u16>() {
        Ok(200) => {}
        Ok(code @ 400..=499) => return Err(Failure::Refused(format!("{code} {}", short()))),
        _ => return Err(Failure::Down(format!("answered {} {}", status.trim(), short()))),
    }
    #[derive(Deserialize)]
    struct Reply {
        text: Option<String>,
        language: Option<String>,
    }
    let r: Reply = serde_json::from_str(body).map_err(|_| Failure::Down(format!("not JSON: {}", short())))?;
    let text = r.text.ok_or_else(|| Failure::Down(format!("no text: {}", short())))?;
    let language = r.language.map(|l| l.trim().to_lowercase()).filter(|l| !l.is_empty());
    Ok(Heard { text: text.trim().to_string(), language })
}

/// The `prompt` field: the glossary, then what was said before this audio.
pub fn prompt(glossary: &str, before: &str) -> String {
    match (glossary.trim(), before.trim()) {
        (g, "") => g.to_string(),
        ("", b) => b.to_string(),
        (g, b) => format!("{g}\n\n{b}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_endpoint_is_added_to_a_base_url_once() {
        assert_eq!(endpoint("http://h:8890/v1"), "http://h:8890/v1/audio/transcriptions");
        assert_eq!(endpoint("http://h:8890/v1/"), "http://h:8890/v1/audio/transcriptions");
        assert_eq!(endpoint("http://h:8890/v1/audio/transcriptions"), "http://h:8890/v1/audio/transcriptions");
    }

    #[test]
    fn the_time_limit_grows_with_the_audio_up_to_a_cap() {
        assert_eq!(timeout(0.0), (1.5, 2.5));
        assert_eq!(timeout(10.0), (1.5, 3.5));
        assert_eq!(timeout(60.0), (1.5, 4.0));
    }

    #[test]
    fn a_fixed_language_wins_over_the_accepted_ones() {
        let langs = vec!["en".to_string(), "de".to_string()];
        assert_eq!(language_field(Some("de"), &langs), Some(("language", "de".into())));
        assert_eq!(language_field(None, &langs), Some(("languages", "en,de".into())));
        assert_eq!(language_field(None, &[]), None);
    }

    #[test]
    fn replies_and_failures() {
        let ok = outcome(Ok(()), "200", r#"{"text":" Wie geht's? ","language":"de","duration":1.2}"#);
        assert_eq!(ok, Ok(Heard { text: "Wie geht's?".into(), language: Some("de".into()) }));
        assert!(matches!(outcome(Ok(()), "400", "unknown language(s) xx"), Err(Failure::Refused(e)) if e.starts_with("400 unknown")));
        assert!(matches!(outcome(Ok(()), "500", "boom"), Err(Failure::Down(_))));
        assert!(matches!(outcome(Ok(()), "200", "<html>"), Err(Failure::Down(e)) if e.starts_with("not JSON")));
        assert!(matches!(outcome(Err("timed out".into()), "000", ""), Err(Failure::Down(e)) if e.contains("timed out")));
        assert_eq!(outcome(Ok(()), "200", r#"{"text":"hi"}"#).unwrap().language, None);
    }

    #[test]
    fn a_language_settles_on_two_words_and_an_accepted_code() {
        let langs = vec!["en".to_string(), "de".to_string()];
        let heard = |t: &str, l: &str| Heard { text: t.into(), language: Some(l.into()) };
        assert_eq!(heard("Super.", "de").settled_language(&langs), None);
        assert_eq!(heard("Super, danke.", "de").settled_language(&langs), Some("de"));
        assert_eq!(heard("Super - .", "de").settled_language(&langs), None);
        assert_eq!(heard("Super, danke.", "nl").settled_language(&langs), None);
        assert_eq!(heard("Super, danke.", "nl").settled_language(&[]), Some("nl"));
        assert_eq!(heard("Super, danke.", "german").settled_language(&[]), None);
        assert_eq!(Heard { text: "a b".into(), language: None }.settled_language(&langs), None);
    }

    #[test]
    fn a_recording_starts_with_the_pages_language_else_the_configs() {
        assert_eq!(start_language(Some("de"), "en"), Some("de".into()));
        assert_eq!(start_language(None, "de"), Some("de".into()));
        assert_eq!(start_language(Some("auto"), "de"), None);
        assert_eq!(start_language(None, "auto"), None);
        assert_eq!(start_language(None, ""), None);
    }

    #[test]
    fn the_prompt_is_the_glossary_then_what_came_before() {
        assert_eq!(prompt("Xcode, tenx", ""), "Xcode, tenx");
        assert_eq!(prompt("", "Fix it."), "Fix it.");
        assert_eq!(prompt("Xcode\n", "Fix it."), "Xcode\n\nFix it.");
    }
}
