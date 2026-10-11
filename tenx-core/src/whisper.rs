//! The conversation between `tenx web` and `tenx-whisper`, its speech-to-text
//! program, over the program's stdin and stdout — and the one audio format
//! that travels: what the page records, 16 kHz mono 16-bit WAV.
//!
//! `tenx-whisper` says `{"ready":true}` (or `{"error":…}` and exits) once its
//! model is loaded. Then, per recording, `tenx web` writes a header line,
//! `{"samples":N,"language":"de"}` (`language` optional: the program's
//! default), followed by N little-endian `f32` samples, and reads one line
//! back: `{"text":…}` or `{"error":…}`. EOF on stdin ends the program.
//!
//! Live dictation (`dictation`) adds to the header: `prompt` (text before
//! this audio, after the program's glossary), `fallback: false` (no
//! temperature fallback — for final passes, where it cut long text short)
//! and `segments: true`, which adds `"segments":[{"text":…,"end":secs}]` to
//! the reply.

use serde::{Deserialize, Serialize};

/// The sample rate everything here is at: Whisper's.
pub const RATE: u32 = 16_000;

/// The line before a recording's samples.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub samples: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Whisper's temperature fallback; on unless `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<bool>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub segments: bool,
}

/// One of Whisper's segments: its text and where it ends in the audio.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub text: String,
    /// Seconds from the start of the request's audio.
    pub end: f32,
}

/// The program's one-line answers: to starting, and to each recording.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Reply {
    Ready { ready: bool },
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        segments: Vec<Segment>,
    },
    Error { error: String },
}

/// A reply as one line, newline included.
pub fn reply_line(reply: &Reply) -> String {
    let mut s = serde_json::to_string(reply).expect("a reply serializes");
    s.push('\n');
    s
}

/// The samples of a 16 kHz mono 16-bit PCM WAV — what the page sends — as
/// Whisper's `f32`s. Anything else is refused, not converted: there is no
/// other sender.
pub fn wav_samples(wav: &[u8]) -> Result<Vec<f32>, String> {
    if wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return Err("not a WAV file".into());
    }
    let u16_at = |i: usize| u16::from_le_bytes([wav[i], wav[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([wav[i], wav[i + 1], wav[i + 2], wav[i + 3]]);
    let mut format_ok = false;
    let mut at = 12;
    while at + 8 <= wav.len() {
        let id = &wav[at..at + 4];
        let len = u32_at(at + 4) as usize;
        let body = at + 8;
        let end = body.saturating_add(len).min(wav.len());
        if id == b"fmt " {
            if len < 16 || body + 16 > wav.len() {
                return Err("a WAV format chunk too short".into());
            }
            let (format, channels, rate, bits) = (u16_at(body), u16_at(body + 2), u32_at(body + 4), u16_at(body + 14));
            if (format, channels, rate, bits) != (1, 1, RATE, 16) {
                return Err(format!("WAV must be 16 kHz mono 16-bit PCM, not format {format}, {channels} channel(s), {rate} Hz, {bits}-bit"));
            }
            format_ok = true;
        } else if id == b"data" {
            if !format_ok {
                return Err("WAV data before its format".into());
            }
            return Ok(wav[body..end].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect());
        }
        // Chunks are padded to an even length.
        at = body.saturating_add(len + (len & 1));
    }
    Err("no audio in the WAV file".into())
}

/// Samples as a 16 kHz mono 16-bit WAV, what [`wav_samples`] reads — and a
/// `whisper-server` (live dictation's audio, when one is set).
pub fn wav(samples: &[f32]) -> Vec<u8> {
    let data = (samples.len() * 2) as u32;
    let mut v = Vec::with_capacity(44 + data as usize);
    v.extend(b"RIFF");
    v.extend((36 + data).to_le_bytes());
    v.extend(b"WAVEfmt ");
    v.extend(16u32.to_le_bytes());
    v.extend(1u16.to_le_bytes()); // PCM
    v.extend(1u16.to_le_bytes()); // mono
    v.extend(RATE.to_le_bytes());
    v.extend((RATE * 2).to_le_bytes());
    v.extend(2u16.to_le_bytes());
    v.extend(16u16.to_le_bytes());
    v.extend(b"data");
    v.extend(data.to_le_bytes());
    for s in samples {
        let s = s.clamp(-1.0, 1.0);
        v.extend((if s < 0.0 { s * 32768.0 } else { s * 32767.0 } as i16).to_le_bytes());
    }
    v
}

/// The page's live audio frames (16-bit little-endian PCM at 16 kHz) as
/// samples.
pub fn pcm16_samples(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect()
}

/// Samples as the bytes that follow a [`Request`].
pub fn samples_bytes(samples: &[f32]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

/// The bytes after a [`Request`] back as samples.
pub fn bytes_samples(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
}

/// Whisper's segments joined into what's typed: trimmed, single-spaced
/// between segments.
pub fn join_segments<'a>(segments: impl IntoIterator<Item = &'a str>) -> String {
    segments.into_iter().map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(channels: u16, rate: u32, samples: &[i16]) -> Vec<u8> {
        let mut v = b"RIFF\0\0\0\0WAVE".to_vec();
        v.extend(b"fmt ");
        v.extend(16u32.to_le_bytes());
        v.extend(1u16.to_le_bytes());
        v.extend(channels.to_le_bytes());
        v.extend(rate.to_le_bytes());
        v.extend((rate * 2).to_le_bytes());
        v.extend(2u16.to_le_bytes());
        v.extend(16u16.to_le_bytes());
        // A chunk to skip, odd-sized (padded).
        v.extend(b"LIST");
        v.extend(3u32.to_le_bytes());
        v.extend([1, 2, 3, 0]);
        v.extend(b"data");
        v.extend(((samples.len() * 2) as u32).to_le_bytes());
        for s in samples {
            v.extend(s.to_le_bytes());
        }
        v
    }

    #[test]
    fn wav_samples_reads_the_pages_format_and_skips_other_chunks() {
        let got = wav_samples(&wav(1, 16_000, &[0, 16384, -32768])).unwrap();
        assert_eq!(got, vec![0.0, 0.5, -1.0]);
    }

    #[test]
    fn wav_samples_refuses_other_formats() {
        assert!(wav_samples(&wav(2, 16_000, &[0, 0])).unwrap_err().contains("2 channel"));
        assert!(wav_samples(&wav(1, 48_000, &[0])).unwrap_err().contains("48000 Hz"));
        assert_eq!(wav_samples(b"OggS...."), Err("not a WAV file".into()));
        assert_eq!(wav_samples(b"RIFF\0\0\0\0WAVE"), Err("no audio in the WAV file".into()));
    }

    #[test]
    fn wav_round_trips_and_pcm_frames_decode() {
        assert_eq!(wav_samples(&super::wav(&[0.0, 0.5, -1.0])).unwrap(), vec![0.0, 16383.0 / 32768.0, -1.0]);
        assert_eq!(pcm16_samples(&[0, 0x40, 0, 0x80, 7]), vec![0.5, -1.0]);
    }

    #[test]
    fn samples_round_trip_as_bytes() {
        let s = [0.25f32, -0.5, 1.0];
        assert_eq!(bytes_samples(&samples_bytes(&s)), s);
    }

    #[test]
    fn requests_and_replies_are_one_json_line() {
        let r = Request { samples: 3, language: None, prompt: None, fallback: None, segments: false };
        assert_eq!(serde_json::to_string(&r).unwrap(), r#"{"samples":3}"#);
        let de: Request = serde_json::from_str(r#"{"samples":3,"language":"de"}"#).unwrap();
        assert_eq!(de.language.as_deref(), Some("de"));
        assert_eq!(reply_line(&Reply::Text { text: "hi".into(), segments: vec![] }), "{\"text\":\"hi\"}\n");
        let live: Reply = serde_json::from_str(r#"{"text":"a.","segments":[{"text":"a.","end":1.5}]}"#).unwrap();
        assert_eq!(live, Reply::Text { text: "a.".into(), segments: vec![Segment { text: "a.".into(), end: 1.5 }] });
        assert_eq!(serde_json::from_str::<Reply>(r#"{"ready":true}"#).unwrap(), Reply::Ready { ready: true });
        assert_eq!(serde_json::from_str::<Reply>(r#"{"error":"x"}"#).unwrap(), Reply::Error { error: "x".into() });
    }

    #[test]
    fn segments_join_trimmed() {
        assert_eq!(join_segments([" Fix the login", " timeout. ", ""]), "Fix the login timeout.");
    }
}
