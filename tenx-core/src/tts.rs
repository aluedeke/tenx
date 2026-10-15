//! Text to speech for `tenx web`'s speaker: the Kokoro voices, and the
//! conversation between `tenx web` and `tenx-tts`, its text-to-speech
//! program, over the program's stdin and stdout.
//!
//! `tenx-tts` says `{"ready":true,"sample_rate":24000}` (or `{"error":…}` and
//! exits) once its model is loaded. Then, per piece of text, `tenx web`
//! writes one line, `{"text":…,"speed":1.0}`, and reads one line back:
//! `{"samples":N}` followed by N little-endian `f32` samples, or
//! `{"error":…}`. EOF on stdin ends the program.
//!
//! The voice is fixed when the program starts (`--voice`): Kokoro's
//! language — which lexicon and which espeak-ng voice spell out the words —
//! goes with the voice, and is set when the model loads.

use serde::{Deserialize, Serialize};

/// The model's voices (Kokoro v1.0, as sherpa-onnx's `voices.bin` orders
/// them): a voice's speaker id is its place here. The first letter is the
/// language (`a` American English, `b` British English, `e` Spanish, `f`
/// French, `h` Hindi, `i` Italian, `j` Japanese, `p` Brazilian Portuguese,
/// `z` Mandarin), the second `f`emale or `m`ale.
pub const VOICES: [&str; 54] = [
    "af_alloy", "af_aoede", "af_bella", "af_heart", "af_jessica", "af_kore", "af_nicole", "af_nova", "af_river",
    "af_sarah", "af_sky", "am_adam", "am_echo", "am_eric", "am_fenrir", "am_liam", "am_michael", "am_onyx", "am_puck",
    "am_santa", "bf_alice", "bf_emma", "bf_isabella", "bf_lily", "bm_daniel", "bm_fable", "bm_george", "bm_lewis",
    "ef_dora", "em_alex", "ff_siwis", "hf_alpha", "hf_beta", "hm_omega", "hm_psi", "if_sara", "im_nicola", "jf_alpha",
    "jf_gongitsune", "jf_nezumi", "jf_tebukuro", "jm_kumo", "pf_dora", "pm_alex", "pm_santa", "zf_xiaobei",
    "zf_xiaoni", "zf_xiaoxiao", "zf_xiaoyi", "zm_yunjian", "zm_yunxi", "zm_yunxia", "zm_yunyang", "em_santa",
];

/// The voice when none is set: Kokoro's best-rated English one.
pub const DEFAULT_VOICE: &str = "af_heart";

/// A voice by name (`af_heart`) or speaker id (`3`).
pub fn voice_id(voice: &str) -> Option<i32> {
    let v = voice.trim();
    if let Ok(n) = v.parse::<usize>() {
        return (n < VOICES.len()).then_some(n as i32);
    }
    VOICES.iter().position(|&name| name == v).map(|i| i as i32)
}

/// What a voice speaks: the espeak-ng language (named as in the model's
/// `espeak-ng-data/lang`, where British English is plain `en`) for words no
/// lexicon has, and the model directory's lexicon file for it, if it ships
/// one.
pub fn voice_language(id: i32) -> (&'static str, Option<&'static str>) {
    let name = usize::try_from(id).ok().and_then(|i| VOICES.get(i)).copied().unwrap_or(DEFAULT_VOICE);
    match name.as_bytes()[0] {
        b'b' => ("en", Some("lexicon-gb-en.txt")),
        b'e' => ("es", None),
        b'f' => ("fr", None),
        b'h' => ("hi", None),
        b'i' => ("it", None),
        b'j' => ("ja", None),
        b'p' => ("pt-br", None),
        b'z' => ("cmn", Some("lexicon-zh.txt")),
        _ => ("en-us", Some("lexicon-us-en.txt")),
    }
}

/// One piece of text to speak.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub text: String,
    /// 1.0 is the voice's own pace.
    #[serde(default = "one")]
    pub speed: f32,
}

fn one() -> f32 {
    1.0
}

/// The program's one-line answers: to starting, and to each piece.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Reply {
    Ready { ready: bool, sample_rate: u32 },
    /// Followed by this many `f32` samples.
    Audio { samples: usize },
    Error { error: String },
}

/// A reply as one line, newline included.
pub fn reply_line(reply: &Reply) -> String {
    let mut s = serde_json::to_string(reply).expect("a reply serializes");
    s.push('\n');
    s
}

/// The first piece is short, so the voice starts soon: making audio takes
/// Kokoro about a sixth of its length, and the next piece is made while
/// this one plays.
pub const FIRST_PIECE: usize = 160;
/// Later pieces may be longer: there is a whole piece's playing time to
/// make them in.
pub const PIECE: usize = 600;

/// Spoken text (`readaloud::spoken_text`, one unit per line) in pieces to
/// make and play one after the other: whole sentences, joined until a piece
/// would pass its size — the first [`FIRST_PIECE`] characters, later ones
/// [`PIECE`]. A sentence longer than that is a piece of its own.
pub fn pieces(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for sentence in text.lines().flat_map(sentences) {
        let limit = if out.is_empty() { FIRST_PIECE } else { PIECE };
        if !cur.is_empty() && cur.chars().count() + 1 + sentence.chars().count() > limit {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(sentence);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// A line split after each `.`, `!` or `?` that a space follows.
fn sentences(line: &str) -> impl Iterator<Item = &str> {
    let mut rest = line.trim();
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let cut = rest
            .char_indices()
            .find(|&(i, c)| matches!(c, '.' | '!' | '?') && rest[i + 1..].starts_with(' '))
            .map_or(rest.len(), |(i, _)| i + 1);
        let (sentence, after) = rest.split_at(cut);
        rest = after.trim_start();
        Some(sentence.trim())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voices_by_name_or_number() {
        assert_eq!(voice_id("af_heart"), Some(3));
        assert_eq!(voice_id(" bm_george "), Some(26));
        assert_eq!(voice_id("53"), Some(53));
        assert_eq!(voice_id("em_santa"), Some(53));
        assert_eq!(voice_id("54"), None);
        assert_eq!(voice_id("Samantha"), None);
        assert_eq!(voice_id(DEFAULT_VOICE), Some(3));
    }

    #[test]
    fn a_voice_brings_its_language() {
        assert_eq!(voice_language(3), ("en-us", Some("lexicon-us-en.txt")));
        assert_eq!(voice_language(voice_id("bf_emma").unwrap()), ("en", Some("lexicon-gb-en.txt")));
        assert_eq!(voice_language(voice_id("ff_siwis").unwrap()), ("fr", None));
        assert_eq!(voice_language(-1), ("en-us", Some("lexicon-us-en.txt")));
    }

    #[test]
    fn replies_are_one_line_each() {
        assert_eq!(reply_line(&Reply::Ready { ready: true, sample_rate: 24000 }), "{\"ready\":true,\"sample_rate\":24000}\n");
        let audio: Reply = serde_json::from_str("{\"samples\":5}").unwrap();
        assert_eq!(audio, Reply::Audio { samples: 5 });
        let err: Reply = serde_json::from_str("{\"error\":\"no\"}").unwrap();
        assert_eq!(err, Reply::Error { error: "no".into() });
        let req: Request = serde_json::from_str("{\"text\":\"hi\"}").unwrap();
        assert_eq!(req.speed, 1.0);
    }

    #[test]
    fn pieces_keep_sentences_whole_and_start_short() {
        let s = "One two three. ".repeat(30);
        let p = pieces(s.trim());
        assert!(p[0].chars().count() <= FIRST_PIECE, "{}", p[0]);
        assert!(p[1].chars().count() > FIRST_PIECE);
        assert!(p.iter().all(|x| x.chars().count() <= PIECE && x.ends_with('.')));
        assert_eq!(p.join(" "), s.trim());
    }

    #[test]
    fn pieces_split_lines_and_sentences_but_not_numbers() {
        assert_eq!(pieces("Version 1.5 is out. Yes!\nNext line"), vec!["Version 1.5 is out. Yes! Next line"]);
        let long = "x".repeat(FIRST_PIECE + 10);
        assert_eq!(pieces(&format!("Hi. {long}. Bye.")), vec!["Hi.".to_string(), format!("{long}. Bye.")]);
        assert!(pieces("").is_empty());
    }
}
