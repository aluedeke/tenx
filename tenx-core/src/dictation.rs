//! Live dictation: what `tenx web` decides while someone talks into the
//! page's microphone — which words are settled, which may still change, and
//! where a sentence is over.
//!
//! Neither engine streams, so about once a second the audio since the last
//! finished sentence (the *buffer*) is transcribed again. A word is
//! *committed* when two runs in a row agree on it (LocalAgreement-2, as in
//! ufal/whisper_streaming), and committed words never change; the rest of the
//! latest run is *tentative*. When a committed sentence is over, the buffer is
//! cut there and that sentence gets a final pass of its own, whose text
//! replaces its committed words — so a correction lands where the user is
//! reading, and stopping is quick.
//!
//! Where to cut depends on the engine:
//!
//! - `tenx-whisper` gives segments with end times: the buffer is cut where a
//!   committed segment ends a sentence ([`Dictation::update`]).
//! - The speech server gives text only. Its cuts are at the buffer's pauses
//!   ([`pauses`]): once a committed word ends a sentence, the audio up to a
//!   pause gets the final pass, and the cut is made only when that text ends
//!   on the committed sentence end ([`Dictation::finish_at_pause`]) — a run
//!   over audio that stops mid-sentence ends in a full stop too, so its own
//!   punctuation proves nothing.
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
/// The speech server's buffer is cut at a pause past this, sentence end or
/// not: every run takes longer with the buffer, and the server writes at
/// most 256 tokens.
pub const SERVER_TRIM_SECS: f32 = 20.0;
/// Past this, the speech server's buffer is cut at its latest pause even
/// when no final pass fits ([`Dictation::force_cut`]): the server writes at
/// most 256 tokens, and a run over a long buffer nears its time limit.
pub const SERVER_MAX_SECS: f32 = 25.0;
/// Pauses tried per run for a cut ([`cut_candidates`]): each is a request.
pub const CUT_TRIES: usize = 3;
/// A quiet stretch at least this long is a place to cut.
pub const PAUSE_SECS: f32 = 0.3;
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
    /// How many words were committed when a cut at a pause last didn't
    /// fit: no new try before more are committed.
    cut_tried_at: Option<usize>,
}

/// Where to end a finished sentence: `end` samples into the buffer, taking
/// the buffer's first `words` committed words with it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cut {
    pub end: usize,
    words: usize,
}

/// How a final pass over the audio up to a pause fits the committed words
/// ([`Dictation::finish_at_pause`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PauseCut {
    /// The sentence is finished; whether the final pass changed a word.
    Done { changed: bool },
    /// The text runs past the committed sentence end, or into words not
    /// committed yet: an earlier pause may fit.
    TooFar,
    /// The text doesn't end where a committed sentence does: no cut now.
    NoFit,
}

/// What the page shows: finished and committed text in full colour, the
/// tentative rest greyed.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct View {
    pub done: String,
    pub committed: String,
    pub tentative: String,
}

/// Whether two texts have the same words, casing and punctuation aside.
fn same_words(a: &str, b: &str) -> bool {
    let words = |t: &str| t.split_whitespace().map(key).filter(|k| !k.is_empty()).collect::<Vec<_>>();
    words(a) == words(b)
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
    align(committed, hyp).map_or(committed.len().min(hyp.len()), |(_, _, j)| j + 1)
}

/// The longest common subsequence of `a` and `b`: its length and the
/// positions of its last pair, or `None` when they share no word.
fn align(a: &[String], b: &[String]) -> Option<(usize, usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    let (mut i, mut j, mut last) = (0, 0, None);
    while i < n && j < m {
        if a[i] == b[j] {
            last = Some((i, j));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    last.map(|(i, j)| (lcs[0][0] as usize, i, j))
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
        let committed_before = self.committed.len();
        let (skip, agreed) = self.agree(hyp);

        // The run's word i is committed word i - offset.
        let offset = skip as isize - committed_before as isize;
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

    /// Commit what this run (`hyp`) and the one before agree on; the rest is
    /// tentative. The run's words that repeat committed ones, and how many
    /// more it committed.
    fn agree(&mut self, hyp: Vec<String>) -> (usize, usize) {
        let keys: Vec<String> = hyp.iter().map(|w| key(w)).collect();
        let committed_keys: Vec<String> = self.committed.iter().map(|w| key(w)).collect();
        let skip = skip_committed(&committed_keys, &keys);
        let agreed = keys[skip..].iter().zip(&self.tentative).take_while(|(k, t)| **k == key(t)).count();
        self.committed.extend(hyp[skip..skip + agreed].iter().cloned());
        self.tentative = hyp[skip + agreed..].to_vec();
        (skip, agreed)
    }

    /// Take one run over the buffer as text alone (the speech server's).
    pub fn update_text(&mut self, text: &str) {
        self.agree(text.split_whitespace().map(str::to_string).collect());
    }

    /// Whether to look for a pause to cut at (the speech server's buffer,
    /// `buffer_secs` long): a committed word ends a sentence, or the buffer
    /// is long and something is committed.
    /// Not again before more words are committed after a try that didn't
    /// fit ([`Dictation::cut_missed`]): "Dr." ends no sentence, and every try
    /// is a request.
    pub fn wants_cut(&self, buffer_secs: f32) -> bool {
        self.cut_tried_at != Some(self.committed.len())
            && (self.committed.iter().any(|w| ends_sentence(w)) || (buffer_secs > SERVER_TRIM_SECS && !self.committed.is_empty()))
    }

    /// No pause fitted this time.
    pub fn cut_missed(&mut self) {
        self.cut_tried_at = Some(self.committed.len());
    }

    /// Close a sentence at a pause `end` samples into the buffer, given the
    /// final pass over the audio before it — when that text is the committed
    /// words up to one that ends a sentence (any committed word, when the
    /// buffer is `long`; then a full stop the cut alone put there is
    /// dropped). See [`PauseCut`].
    pub fn finish_at_pause(&mut self, end: usize, final_text: &str, long: bool) -> PauseCut {
        let fin: Vec<String> = final_text.split_whitespace().map(key).filter(|k| !k.is_empty()).collect();
        let committed: Vec<String> = self.committed.iter().map(|w| key(w)).collect();
        if fin.is_empty() || committed.is_empty() {
            return PauseCut::NoFit;
        }
        let Some((matched, last_c, last_f)) = align(&committed[..committed.len().min(fin.len() + 3)], &fin) else {
            return PauseCut::TooFar;
        };
        // Its words after the last match stand for as many committed ones
        // (the final pass may hear the last word differently).
        let last = last_c + (fin.len() - 1 - last_f);
        if last >= self.committed.len() {
            return PauseCut::TooFar;
        }
        // Most of its words, and most of the committed ones it replaces,
        // match: a pass that heard only "login." mustn't erase "Fix the".
        if (matched as f32) < 0.6 * fin.len().max(last + 1) as f32 {
            return PauseCut::NoFit;
        }
        let sentence_end = ends_sentence(&self.committed[last]);
        if !sentence_end && !long {
            return if self.committed[..last].iter().any(|w| ends_sentence(w)) { PauseCut::TooFar } else { PauseCut::NoFit };
        }
        let words: Vec<String> = self.committed.drain(..=last).collect();
        let mut text = final_text.trim().to_string();
        if !sentence_end && text.ends_with(['.', '?', '!']) && !text.ends_with("..") {
            text.pop();
        }
        let changed = !same_words(&words.join(" "), &text);
        self.done.push(text);
        self.start += end;
        self.cut_tried_at = None;
        PauseCut::Done { changed }
    }

    /// The buffer is past [`SERVER_MAX_SECS`] and no pause fitted: close
    /// everything heard before `end` with the final pass over it (the
    /// committed words when that came back empty), sentence or not, and
    /// start the agreement over after it. Whether a shown word changed.
    pub fn force_cut(&mut self, end: usize, final_text: &str) -> bool {
        let before = self.committed.join(" ");
        let text = if final_text.trim().is_empty() { before.clone() } else { final_text.trim().to_string() };
        let changed = !same_words(&before, &text);
        if !text.is_empty() {
            self.done.push(text);
        }
        self.committed.clear();
        self.tentative.clear();
        self.start += end;
        self.cut_tried_at = None;
        changed
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
        prompt_tail(&self.done.join(" "))
    }

    pub fn view(&self) -> View {
        View { done: self.done.join(" "), committed: self.committed.join(" "), tentative: self.tentative.join(" ") }
    }
}

/// Whether audio has speech in it: at least `min_secs` of 30 ms frames
/// louder than room noise. Whisper given silence makes text up, so silence
/// isn't transcribed (the page checks the same way, lib/stt.ts).
pub fn has_speech(audio: &[f32], threshold: f32, min_secs: f32) -> bool {
    frame_levels(audio).into_iter().filter(|&l| l > threshold).count() as f32 * 0.03 >= min_secs
}

/// The end of what was said, as a prompt for what follows.
pub fn prompt_tail(said: &str) -> String {
    let skip = said.chars().count().saturating_sub(PROMPT_CHARS);
    said.chars().skip(skip).collect()
}

/// The pauses worth a final pass for a cut, latest first, at most
/// [`CUT_TRIES`]: not in the buffer's first half second (no sentence fits
/// there), and not in its last [`STEP_SECS`], whose words the run before
/// didn't hear, so none of them is committed yet.
pub fn cut_candidates(pauses: &[usize], buffer_len: usize) -> Vec<usize> {
    let latest = buffer_len.saturating_sub((STEP_SECS * RATE as f32) as usize);
    pauses.iter().rev().copied().filter(|&p| p >= RATE as usize / 2 && p <= latest).take(CUT_TRIES).collect()
}

/// Where to force a cut in a buffer of `buffer_len` samples: its latest
/// pause in the cut candidates' range, else a second before its end.
pub fn forced_cut(pauses: &[usize], buffer_len: usize) -> usize {
    let latest = buffer_len.saturating_sub((STEP_SECS * RATE as f32) as usize);
    pauses.iter().rev().copied().find(|&p| p >= RATE as usize / 2 && p <= latest).unwrap_or(latest)
}

/// The 30 ms frames' loudness (RMS).
fn frame_levels(audio: &[f32]) -> Vec<f32> {
    let frame = (RATE as f32 * 0.03) as usize;
    audio.chunks_exact(frame).map(|f| (f.iter().map(|s| s * s).sum::<f32>() / frame as f32).sqrt()).collect()
}

/// Where the audio pauses, in samples (the middle of each quiet stretch of
/// at least [`PAUSE_SECS`] after speech), first to last. Quiet is under
/// `threshold` or, in a noisy room, under twice its noise floor (the
/// quietest 1% of the frames).
pub fn pauses(audio: &[f32], threshold: f32) -> Vec<usize> {
    let frame = (RATE as f32 * 0.03) as usize;
    let levels = frame_levels(audio);
    if levels.is_empty() {
        return Vec::new();
    }
    let mut sorted = levels.clone();
    sorted.sort_by(f32::total_cmp);
    let quiet = threshold.max(2.0 * sorted[sorted.len() / 100]);
    let min_frames = (PAUSE_SECS / 0.03).ceil() as usize;
    let (mut out, mut heard, mut run) = (Vec::new(), false, 0usize);
    for (i, &l) in levels.iter().chain([&f32::INFINITY]).enumerate() {
        if l < quiet {
            run += 1;
            continue;
        }
        if heard && run >= min_frames {
            out.push((i - run / 2) * frame);
        }
        if i < levels.len() {
            heard = true;
        }
        run = 0;
    }
    out
}

/// A long recording in pieces of at most `max_secs` for one request each,
/// cut at its last pause before that length (anywhere, if it has none).
pub fn split_at_pauses(audio: &[f32], threshold: f32, max_secs: f32) -> Vec<std::ops::Range<usize>> {
    let max = (max_secs * RATE as f32) as usize;
    let mut out = Vec::new();
    let mut start = 0;
    while audio.len() - start > max {
        let piece = &audio[start..start + max];
        let cut = pauses(piece, threshold).into_iter().rev().find(|&p| p > max / 4).unwrap_or(max);
        out.push(start..start + cut);
        start += cut;
    }
    out.push(start..audio.len());
    out
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

    fn speech(secs: f32) -> Vec<f32> {
        (0..(secs * RATE as f32) as usize).map(|i| (i as f32 / 10.0).sin() * 0.2).collect()
    }

    fn quiet(secs: f32) -> Vec<f32> {
        vec![0.0; (secs * RATE as f32) as usize]
    }

    #[test]
    fn text_runs_commit_like_segment_runs() {
        let mut d = Dictation::default();
        d.update_text("Please fix");
        d.update_text("Please fix the log");
        d.update_text("please fix the login timeout");
        assert_eq!(d.view().committed, "Please fix the");
        assert_eq!(d.view().tentative, "login timeout");
        assert!(!d.wants_cut(3.0));
    }

    #[test]
    fn a_committed_sentence_end_is_cut_at_the_pause_its_final_pass_ends_on() {
        let mut d = Dictation::default();
        d.update_text("Fix the login. Then");
        d.update_text("Fix the login. Then open");
        assert!(d.wants_cut(3.0));
        // A pause after "Then": the text runs past the sentence end.
        assert_eq!(d.finish_at_pause(40_000, "Fix the login. Then.", false), PauseCut::TooFar);
        assert_eq!(d.view().done, "");
        // The pause after "login.": cut, the final pass's spelling kept.
        assert_eq!(d.finish_at_pause(24_000, "Fix the logins.", false), PauseCut::Done { changed: true });
        assert_eq!(d.start, 24_000);
        assert_eq!(d.view(), View { done: "Fix the logins.".into(), committed: "Then".into(), tentative: "open".into() });
    }

    #[test]
    fn a_full_stop_the_cut_put_there_is_no_sentence_end() {
        let mut d = Dictation::default();
        d.update_text("and then we go on and on");
        d.update_text("and then we go on and on");
        assert!(!d.wants_cut(10.0), "no sentence end committed");
        assert!(d.wants_cut(21.0), "long buffer");
        // A short buffer: "we." ends mid-sentence.
        assert_eq!(d.finish_at_pause(16_000, "And then we.", false), PauseCut::NoFit);
        // A long one: cut anyway, the cut's full stop dropped.
        assert_eq!(d.finish_at_pause(16_000, "And then we.", true), PauseCut::Done { changed: false });
        assert_eq!(d.view().done, "And then we");
        assert_eq!(d.view().committed, "go on and on");
    }

    #[test]
    fn a_forced_cut_closes_everything_before_it_and_starts_over() {
        let mut d = Dictation::default();
        d.update_text("and then we go on and on");
        d.update_text("and then we go on and on and");
        assert!(!d.force_cut(400_000, "and then we go on and on"));
        assert_eq!(d.view(), View { done: "and then we go on and on".into(), committed: "".into(), tentative: "".into() });
        assert_eq!(d.start, 400_000);
        assert!(d.force_cut(10, "Und dann."));
        let s = RATE as usize;
        assert_eq!(forced_cut(&[2 * s, 20 * s, 26 * s], 26 * s), 20 * s);
        assert_eq!(forced_cut(&[], 26 * s), 25 * s);
    }

    #[test]
    fn a_long_cut_drops_any_end_mark_the_cut_put_there() {
        let mut d = Dictation::default();
        d.update_text("and then we go on and on");
        d.update_text("and then we go on and on");
        assert_eq!(d.finish_at_pause(16_000, "and then we?", true), PauseCut::Done { changed: false });
        assert_eq!(d.view().done, "and then we");
        assert!(same_words("a - b", "a b"));
    }

    #[test]
    fn a_final_pass_that_heard_part_of_the_sentence_does_not_cut() {
        let mut d = Dictation::default();
        d.update_text("Fix the login. Then");
        d.update_text("Fix the login. Then open");
        assert_eq!(d.finish_at_pause(24_000, "login.", false), PauseCut::NoFit);
        assert_eq!(d.view().committed, "Fix the login. Then");
    }

    #[test]
    fn a_cut_that_did_not_fit_waits_for_more_committed_words() {
        let mut d = Dictation::default();
        d.update_text("Ask Dr. Smith");
        d.update_text("Ask Dr. Smith about");
        assert!(d.wants_cut(3.0));
        d.cut_missed();
        assert!(!d.wants_cut(4.0));
        d.update_text("Ask Dr. Smith about it");
        assert!(d.wants_cut(5.0));
    }

    #[test]
    fn cut_candidates_skip_the_start_and_the_last_second() {
        let s = RATE as usize;
        let p = [s / 4, s, 2 * s, 3 * s, 4 * s, 5 * s + s / 2];
        assert_eq!(cut_candidates(&p, 6 * s), vec![4 * s, 3 * s, 2 * s]);
        assert_eq!(cut_candidates(&p, s), Vec::<usize>::new());
        assert_eq!(prompt_tail(&"a".repeat(500)).len(), PROMPT_CHARS);
    }

    #[test]
    fn a_final_pass_that_hears_other_words_or_nothing_does_not_cut() {
        let mut d = Dictation::default();
        d.update_text("Fix the login.");
        d.update_text("Fix the login.");
        assert_eq!(d.finish_at_pause(8_000, "Something else entirely.", false), PauseCut::TooFar);
        assert_eq!(d.finish_at_pause(8_000, "", false), PauseCut::NoFit);
        assert_eq!(d.finish_at_pause(8_000, "Fix the login.", false), PauseCut::Done { changed: false });
        assert_eq!(d.finish(""), "Fix the login.");
    }

    #[test]
    fn pauses_are_quiet_stretches_after_speech() {
        let audio = [quiet(1.0), speech(1.0), quiet(0.6), speech(1.0), quiet(0.1), speech(0.5), quiet(0.4)].concat();
        let p = pauses(&audio, 0.01);
        assert_eq!(p.len(), 2, "{p:?}");
        // The middle of the 0.6 s pause at 2.0–2.6 s, and of the trailing one at 4.2–4.6 s.
        assert!((p[0] as f32 / RATE as f32 - 2.3).abs() < 0.05, "{p:?}");
        assert!((p[1] as f32 / RATE as f32 - 4.4).abs() < 0.05, "{p:?}");
    }

    #[test]
    fn pauses_in_a_noisy_room_are_under_its_noise_floor_twice() {
        let noise = |secs: f32| (0..(secs * RATE as f32) as usize).map(|i| if i % 2 == 0 { 0.02 } else { -0.02 }).collect::<Vec<f32>>();
        let audio = [noise(1.0), speech(1.0), noise(0.6), speech(1.0)].concat();
        assert_eq!(pauses(&audio, 0.01).len(), 1);
    }

    #[test]
    fn long_recordings_split_at_their_pauses() {
        let audio = [speech(10.0), quiet(0.5), speech(10.0), quiet(0.5), speech(10.0)].concat();
        let pieces = split_at_pauses(&audio, 0.01, 25.0);
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0].start, 0);
        assert!((pieces[0].end as f32 / RATE as f32 - 20.75).abs() < 0.05, "{pieces:?}");
        assert_eq!(pieces[1].end, audio.len());
        // Without a pause: at the limit.
        assert_eq!(split_at_pauses(&speech(30.0), 0.01, 25.0)[0], 0..400_000);
        assert_eq!(split_at_pauses(&speech(3.0), 0.01, 25.0), vec![0..48_000]);
    }
}
