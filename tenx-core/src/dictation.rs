//! Live dictation: what `tenx web` decides while someone talks into the
//! page's microphone — which words are settled, which may still change, and
//! where a sentence is over.
//!
//! Whisper isn't a streaming model, so about once a second the audio since
//! the last finished sentence (the *buffer*) is transcribed again. A word is
//! *committed* when two runs in a row agree on it (LocalAgreement-2, as in
//! ufal/whisper_streaming), and committed words never change; the rest of the
//! latest run is *tentative*. When a committed segment ends a sentence, the
//! buffer is cut there and that sentence gets a final pass of its own (no
//! temperature fallback), whose text replaces its committed words — so a
//! correction lands where the user is reading, and stopping is quick.
//!
//! Learned the hard way (the fine-tune-whisper task's measurements): runs are
//! matched to committed words by the words, not by Whisper's word times,
//! which shift between runs; a cut inside a sentence makes Whisper invent
//! text, so that happens only when the buffer nears Whisper's 30 s window.

use crate::whisper::{RATE, Segment};

/// How often the buffer is transcribed again.
pub const STEP_SECS: f32 = 1.0;
/// A buffer longer than this is cut at any committed segment, sentence end
/// or not: Whisper sees at most 30 s.
pub const TRIM_SECS: f32 = 25.0;
/// How much finished text goes into the prompt for what follows.
const PROMPT_CHARS: usize = 300;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Dictation {
    /// Finished sentences, as their final pass read them.
    done: Vec<String>,
    /// The buffer's committed words.
    committed: Vec<String>,
    /// The latest run's words after the committed ones.
    tentative: Vec<String>,
    /// Where the buffer starts in the whole recording, in samples.
    pub start: usize,
}

/// Where to end a finished sentence: `end` samples into the buffer, taking
/// the buffer's first `words` committed words with it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cut {
    pub end: usize,
    words: usize,
}

/// What the page shows: finished and committed text in full colour, the
/// tentative rest greyed.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct View {
    pub done: String,
    pub committed: String,
    pub tentative: String,
}

/// A word as compared between runs: casing and punctuation don't count.
fn key(word: &str) -> String {
    word.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

fn ends_sentence(text: &str) -> bool {
    text.trim_end().ends_with(['.', '?', '!'])
}

/// How many of `hyp`'s leading words repeat the `committed` ones: the
/// position after the last word of the longest common subsequence, looking
/// only a little past the committed words' count (a common word like "the"
/// further on is not a match).
fn skip_committed(committed: &[String], hyp: &[String]) -> usize {
    if committed.is_empty() {
        return 0;
    }
    let hyp = &hyp[..hyp.len().min(committed.len() + 3)];
    let (n, m) = (committed.len(), hyp.len());
    let mut lcs = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if committed[i] == hyp[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    let (mut i, mut j, mut last) = (0, 0, None);
    while i < n && j < m {
        if committed[i] == hyp[j] {
            last = Some(j);
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    last.map_or(n.min(m), |j| j + 1)
}

impl Dictation {
    /// Take one run over the buffer (`buffer_secs` long): commit what it
    /// and the run before agree on, and say where to cut, if anywhere.
    pub fn update(&mut self, segments: &[Segment], buffer_secs: f32) -> Option<Cut> {
        let mut hyp = Vec::new();
        let mut ends = Vec::new(); // words through each segment
        for seg in segments {
            hyp.extend(seg.text.split_whitespace().map(str::to_string));
            ends.push(hyp.len());
        }
        let keys: Vec<String> = hyp.iter().map(|w| key(w)).collect();
        let committed_keys: Vec<String> = self.committed.iter().map(|w| key(w)).collect();
        let skip = skip_committed(&committed_keys, &keys);
        let agreed = keys[skip..].iter().zip(&self.tentative).take_while(|(k, t)| **k == key(t)).count();
        self.committed.extend(hyp[skip..skip + agreed].iter().cloned());
        self.tentative = hyp[skip + agreed..].to_vec();

        // The run's word i is committed word i - offset.
        let offset = skip as isize - committed_keys.len() as isize;
        let settled = skip + agreed;
        let finished: Vec<usize> = (0..segments.len().saturating_sub(1)).filter(|&i| ends[i] > 0 && ends[i] <= settled).collect();
        let at = finished
            .iter()
            .rev()
            .find(|&&i| ends_sentence(&segments[i].text))
            .or_else(|| finished.last().filter(|_| buffer_secs > TRIM_SECS))?;
        let words = (ends[*at] as isize - offset).clamp(0, self.committed.len() as isize) as usize;
        Some(Cut { end: (segments[*at].end.max(0.0) * RATE as f32) as usize, words })
    }

    /// Close the sentence a cut ends with its final pass's text (its
    /// committed words if that came back empty). Whether the final pass
    /// changed a word — worth showing.
    pub fn finish_sentence(&mut self, cut: Cut, final_text: &str) -> bool {
        let words: Vec<String> = self.committed.drain(..cut.words.min(self.committed.len())).collect();
        let before = words.join(" ");
        let text = if final_text.trim().is_empty() { before.clone() } else { final_text.trim().to_string() };
        let changed = before.split_whitespace().map(key).ne(text.split_whitespace().map(key));
        if !text.is_empty() {
            self.done.push(text);
        }
        self.start += cut.end;
        changed
    }

    /// The whole text once the user stops: the finished sentences and the
    /// final pass over the rest (`tail`; the buffer's words if empty).
    pub fn finish(&self, tail: &str) -> String {
        let rest = if tail.trim().is_empty() { self.committed.iter().chain(&self.tentative).cloned().collect::<Vec<_>>().join(" ") } else { tail.trim().to_string() };
        self.done.iter().map(String::as_str).chain([rest.as_str()]).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
    }

    /// The prompt for the buffer: the end of what was said before it.
    pub fn prompt(&self) -> String {
        let done = self.done.join(" ");
        let skip = done.chars().count().saturating_sub(PROMPT_CHARS);
        done.chars().skip(skip).collect()
    }

    pub fn view(&self) -> View {
        View { done: self.done.join(" "), committed: self.committed.join(" "), tentative: self.tentative.join(" ") }
    }
}

/// Whether audio has speech in it: at least `min_secs` of 30 ms frames
/// louder than room noise. Whisper given silence makes text up, so silence
/// isn't transcribed (the page checks the same way, lib/stt.ts).
pub fn has_speech(audio: &[f32], threshold: f32, min_secs: f32) -> bool {
    let frame = (RATE as f32 * 0.03) as usize;
    let loud = audio.chunks_exact(frame).filter(|f| (f.iter().map(|s| s * s).sum::<f32>() / frame as f32).sqrt() > threshold).count();
    loud as f32 * 0.03 >= min_secs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segs(parts: &[(&str, f32)]) -> Vec<Segment> {
        parts.iter().map(|(t, e)| Segment { text: t.to_string(), end: *e }).collect()
    }

    #[test]
    fn a_word_is_committed_once_two_runs_agree_on_it() {
        let mut d = Dictation::default();
        assert_eq!(d.update(&segs(&[("Please fix", 1.0)]), 1.0), None);
        assert_eq!(d.view(), View { done: "".into(), committed: "".into(), tentative: "Please fix".into() });
        d.update(&segs(&[("Please fix the log", 2.0)]), 2.0);
        assert_eq!(d.view().committed, "Please fix");
        assert_eq!(d.view().tentative, "the log");
        // "log" became "login": only the agreed words commit.
        d.update(&segs(&[("please fix the login timeout", 3.0)]), 3.0);
        assert_eq!(d.view().committed, "Please fix the");
        assert_eq!(d.view().tentative, "login timeout");
    }

    #[test]
    fn committed_words_are_matched_by_the_words_not_their_place() {
        let mut d = Dictation::default();
        d.update(&segs(&[("so fix the login", 2.0)]), 2.0);
        d.update(&segs(&[("so fix the login now", 3.0)]), 3.0);
        assert_eq!(d.view().committed, "so fix the login");
        // A word dropped from the front this run: still aligned.
        d.update(&segs(&[("fix the login now please", 4.0)]), 4.0);
        assert_eq!(d.view().committed, "so fix the login now");
        assert_eq!(d.view().tentative, "please");
    }

    #[test]
    fn the_buffer_is_cut_after_a_committed_sentence_and_finalized() {
        let mut d = Dictation::default();
        d.update(&segs(&[("Fix the login.", 1.5), ("Then", 2.0)]), 2.0);
        // Not yet: nothing committed.
        let cut = d.update(&segs(&[("Fix the login.", 1.5), ("Then open", 3.0)]), 3.0).expect("a sentence is committed");
        assert_eq!(cut.end, 24_000);
        assert!(d.finish_sentence(cut, "Fix the logs."));
        assert_eq!(d.start, 24_000);
        assert_eq!(d.view(), View { done: "Fix the logs.".into(), committed: "Then".into(), tentative: "open".into() });
        assert_eq!(d.prompt(), "Fix the logs.");
        // The next run is over the new buffer.
        d.update(&segs(&[("Then open a PR", 2.0)]), 2.0);
        assert_eq!(d.view().committed, "Then open");
        assert_eq!(d.finish("Then open a PR."), "Fix the logs. Then open a PR.");
    }

    #[test]
    fn no_cut_inside_a_sentence_until_the_buffer_is_long() {
        let mut d = Dictation::default();
        let run = segs(&[("and then we", 10.0), ("go on", 20.0)]);
        d.update(&run, 20.0);
        assert_eq!(d.update(&run, 20.0), None);
        let cut = d.update(&run, 26.0).expect("long buffer");
        assert_eq!(cut.end, 160_000);
        assert!(!d.finish_sentence(cut, ""), "empty final keeps the committed words");
        assert_eq!(d.view().done, "and then we");
        assert_eq!(d.view().committed, "go on");
    }

    #[test]
    fn the_last_segment_is_never_cut_it_may_still_grow() {
        let mut d = Dictation::default();
        let run = segs(&[("Done.", 1.0)]);
        d.update(&run, 1.0);
        assert_eq!(d.update(&run, 2.0), None);
        assert_eq!(d.finish(""), "Done.");
    }

    #[test]
    fn the_prompt_is_the_end_of_the_finished_text() {
        let d = Dictation { done: vec!["ä".repeat(400)], ..Default::default() };
        assert_eq!(d.prompt().chars().count(), PROMPT_CHARS);
    }

    #[test]
    fn silence_is_not_speech() {
        assert!(!has_speech(&vec![0.0; 16_000], 0.01, 0.25));
        let tone: Vec<f32> = (0..16_000).map(|i| (i as f32 / 10.0).sin() * 0.2).collect();
        assert!(has_speech(&tone, 0.01, 0.25));
    }
}
