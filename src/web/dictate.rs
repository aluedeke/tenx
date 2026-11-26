//! `/speech`: live dictation, one WebSocket per recording. The page sends
//! the microphone as it hears it (binary frames: 16-bit PCM, 16 kHz mono)
//! and `{"type":"stop"}` when the user stops; this answers with
//! `{"type":"live","done":…,"committed":…,"tentative":…,"corrected":bool}`
//! about once a second while they talk (`tenx_core::dictation` decides
//! what's settled), then `{"type":"text","text":…}` — what to type — or
//! `{"type":"error","error":…}`, and closes.
//!
//! With `[speech] url` the speech server hears each run (`Listener`): its
//! text alone, cut at pauses (`Dictation::finish_at_pause`), in the language
//! the page set or the first run heard clearly. When it fails, `tenx-whisper`
//! hears the rest of the recording, with its segments.
//!
//! With `TENX_WEB_STT_URL` (a `whisper-server`) there is no live text: the
//! whole recording goes there on stop, as `/transcribe` does.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use axum::extract::ws::{Message, WebSocket};
use tenx_core::dictation::{Dictation, PauseCut, SERVER_TRIM_SECS, STEP_SECS, has_speech, pauses};
use tenx_core::whisper::{self, RATE, Request};

use crate::workspace::SpeechConfig;

use super::transcribe::{Engine, Listener, engine};

/// Pauses tried per run for a cut, latest first: each is a request.
const CUT_TRIES: usize = 2;

/// The longest recording: what `/transcribe` takes, some 13 minutes.
const MAX_SAMPLES: usize = tenx_core::web::STT_MAX_BYTES / 2;

enum FromPage {
    Audio(Vec<f32>),
    Stop,
}

pub(super) async fn session(mut socket: WebSocket, language: Option<String>) {
    let (to_worker, from_page) = mpsc::channel();
    let (to_page, mut from_worker) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
    let worker = tokio::task::spawn_blocking(move || {
        let reply = match listen(&from_page, &to_page, language.as_deref()) {
            Ok(text) => serde_json::json!({ "type": "text", "text": text }),
            Err(e) => serde_json::json!({ "type": "error", "error": format!("{e:#}") }),
        };
        let _ = to_page.send(reply);
    });
    loop {
        tokio::select! {
            msg = socket.recv() => match msg {
                Some(Ok(Message::Binary(bytes))) => { let _ = to_worker.send(FromPage::Audio(whisper::pcm16_samples(&bytes))); }
                Some(Ok(Message::Text(text))) => {
                    if serde_json::from_str::<serde_json::Value>(&text).is_ok_and(|v| v["type"] == "stop") {
                        let _ = to_worker.send(FromPage::Stop);
                    }
                }
                // Gone: the worker sees its channel close and gives up.
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            out = from_worker.recv() => match out {
                Some(v) => {
                    let last = v["type"] != "live";
                    if socket.send(Message::Text(v.to_string().into())).await.is_err() || last {
                        break;
                    }
                }
                None => break,
            },
        }
    }
    drop(to_worker);
    let _ = worker.await;
    let _ = socket.send(Message::Close(None)).await;
}

/// The recording, live: runs over the buffer once a second while the page
/// sends audio, then the final text once it says stop. An error when the
/// page went away first.
fn listen(from_page: &mpsc::Receiver<FromPage>, to_page: &tokio::sync::mpsc::UnboundedSender<serde_json::Value>, language: Option<&str>) -> Result<String> {
    let engine = engine()?;
    match &engine {
        Engine::Local(config) => super::whisper::warm(config),
        Engine::Speech(server) => server.warm_fallback(),
        Engine::Server(_) => {}
    }
    let mut ears = match &engine {
        Engine::Speech(server) => Some(Listener::new(server, language)),
        _ => None,
    };
    let mut audio: Vec<f32> = Vec::new();
    let mut d = Dictation::default();
    let step = Duration::from_secs_f32(STEP_SECS);
    let mut next = Instant::now() + step;
    let mut heard_at_last_run = 0;
    loop {
        match from_page.recv_timeout(next.saturating_duration_since(Instant::now())) {
            Ok(FromPage::Audio(samples)) => {
                audio.extend(samples);
                anyhow::ensure!(audio.len() <= MAX_SAMPLES, "recording too long (over {} minutes)", MAX_SAMPLES / RATE as usize / 60);
                continue;
            }
            Ok(FromPage::Stop) => break,
            Err(mpsc::RecvTimeoutError::Disconnected) => anyhow::bail!("the page went away"),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        next = Instant::now() + step;
        if matches!(engine, Engine::Server(_)) {
            continue;
        }
        let buffer = &audio[d.start.min(audio.len())..];
        // Only when something new was said: Whisper given silence invents text.
        let new = &audio[heard_at_last_run.min(audio.len())..];
        heard_at_last_run = audio.len();
        if buffer.len() < RATE as usize / 2 || !has_speech(new, 0.01, 0.1) {
            continue;
        }
        let corrected = match ears.as_mut() {
            Some(ears) => match server_run(ears, &mut d, buffer) {
                Some(corrected) => corrected,
                None => {
                    let (config, language) = ears.fallback()?;
                    whisper_run(config, &mut d, buffer, language.as_deref())?
                }
            },
            None => {
                let Engine::Local(config) = &engine else { unreachable!("a whisper-server has no live runs") };
                whisper_run(config, &mut d, buffer, language)?
            }
        };
        let v = d.view();
        let _ = to_page.send(serde_json::json!({
            "type": "live", "done": v.done, "committed": v.committed, "tentative": v.tentative, "corrected": corrected,
        }));
    }
    // Whatever came in with the stop.
    while let Ok(FromPage::Audio(samples)) = from_page.try_recv() {
        audio.extend(samples);
    }
    if !has_speech(&audio, 0.01, 0.25) {
        return Ok(String::new());
    }
    let tail = &audio[d.start.min(audio.len())..];
    let text = match &engine {
        Engine::Server(url) => return super::transcribe::from_server(url, "audio/wav", &whisper::wav(&audio), language),
        _ if !has_speech(tail, 0.01, 0.25) => String::new(),
        Engine::Speech(_) => {
            let ears = ears.as_mut().expect("a speech server's listener");
            match ears.ask(tail, &d.prompt()) {
                Some(text) => text,
                None => {
                    let (config, language) = ears.fallback()?;
                    whisper_final(config, &d, tail, language.as_deref())?
                }
            }
        }
        Engine::Local(config) => whisper_final(config, &d, tail, language)?,
    };
    Ok(d.finish(&text))
}

/// One run over the buffer by the speech server, and a cut at a pause when
/// a committed sentence is over: whether the cut's final pass changed a
/// word, or `None` when the server failed (now or before).
fn server_run(ears: &mut Listener, d: &mut Dictation, buffer: &[f32]) -> Option<bool> {
    let text = ears.ask(buffer, &d.prompt())?;
    d.update_text(&text);
    let secs = buffer.len() as f32 / RATE as f32;
    if !d.wants_cut(secs) {
        return Some(false);
    }
    let long = secs > SERVER_TRIM_SECS;
    // A pause too close to the start holds no sentence.
    for end in pauses(buffer, 0.01).into_iter().rev().filter(|&p| p >= RATE as usize / 2).take(CUT_TRIES) {
        let text = ears.ask(&buffer[..end], &d.prompt())?;
        match d.finish_at_pause(end, &text, long) {
            PauseCut::Done { changed } => return Some(changed),
            PauseCut::TooFar => continue,
            PauseCut::NoFit => break,
        }
    }
    Some(false)
}

/// One run over the buffer by `tenx-whisper`, cut where a committed segment
/// ends a sentence: whether the cut's final pass changed a word.
fn whisper_run(config: &SpeechConfig, d: &mut Dictation, buffer: &[f32], language: Option<&str>) -> Result<bool> {
    let prompt = d.prompt();
    let req = |fallback| Request { samples: 0, language: language.map(str::to_string), prompt: Some(prompt.clone()), fallback, segments: true };
    let (_, segments) = super::whisper::run(config, buffer, req(None))?;
    let Some(cut) = d.update(&segments, buffer.len() as f32 / RATE as f32) else { return Ok(false) };
    let sentence = &buffer[..cut.end.min(buffer.len())];
    let (text, _) = super::whisper::run(config, sentence, Request { segments: false, ..req(Some(false)) })?;
    Ok(d.finish_sentence(cut, &text))
}

/// `tenx-whisper`'s final pass over what follows the last cut.
fn whisper_final(config: &SpeechConfig, d: &Dictation, tail: &[f32], language: Option<&str>) -> Result<String> {
    let req = Request { samples: 0, language: language.map(str::to_string), prompt: Some(d.prompt()), fallback: Some(false), segments: false };
    Ok(super::whisper::run(config, tail, req)?.0)
}
