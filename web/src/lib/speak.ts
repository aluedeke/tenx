// The header's speaker: the current task's agent's last reply, read aloud.
// `tenx web` turns it into speech on its machine (Kokoro, in `tenx-tts`), a
// few sentences at a time: POST /speak gives the reply's pieces, and POST
// /speak/audio one piece's audio, asked for while the piece before it plays
// — so a long reply starts at once. The page plays them through one <audio>
// element, which, unlike Web Audio, iOS plays with the ringer switched to
// silent.

import { apiUrl } from './api.ts';

/** A WAV of a few silent samples: played inside the tap, it lets the
 * element play the real audio later, once the request is answered — iOS
 * allows an element to start playing only inside a tap, and the request
 * outlasts it. */
function silence(): string {
  const samples = 8;
  const buf = new ArrayBuffer(44 + samples * 2);
  const v = new DataView(buf);
  const text = (at: number, s: string) => [...s].forEach((c, i) => v.setUint8(at + i, c.charCodeAt(0)));
  text(0, 'RIFF');
  v.setUint32(4, 36 + samples * 2, true);
  text(8, 'WAVEfmt ');
  v.setUint32(16, 16, true); // fmt chunk size
  v.setUint16(20, 1, true); // PCM
  v.setUint16(22, 1, true); // mono
  v.setUint32(24, 8000, true); // sample rate
  v.setUint32(28, 16000, true); // byte rate
  v.setUint16(32, 2, true); // block align
  v.setUint16(34, 16, true); // bits per sample
  text(36, 'data');
  v.setUint32(40, samples * 2, true);
  return URL.createObjectURL(new Blob([buf], { type: 'audio/wav' }));
}

/** What a reading is doing. */
export type ReadingState = 'loading' | 'playing' | 'paused';

/** One reading: started on a tap, paused and resumed by later ones, ended
 * by the last piece's end, `stop`, or a failure. */
export class Reading {
  private audio = new Audio();
  private url: string | null = null;
  private stopped = false;
  private ended = false;
  private paused = false;
  private started = false;
  private onState: (s: ReadingState) => void;
  /** Ends the piece playing now (its `ended`, or `stop`). */
  private pieceDone: (() => void) | null = null;
  private onEnd: (error?: string) => void;

  /** Straight from the tap. `onState` follows it from `loading` (until the
   * first piece plays) on; `onEnd` runs once, with the reason when it
   * failed. */
  constructor(taskId: string, onState: (s: ReadingState) => void, onEnd: (error?: string) => void) {
    this.onEnd = onEnd;
    this.onState = onState;
    const unlock = silence();
    this.audio.src = unlock;
    this.audio.play().catch(() => {});
    this.audio.onended = () => this.pieceDone?.();
    void this.run(taskId)
      .then(() => this.end())
      .catch((e) => this.end(e instanceof Error ? e.message : String(e)))
      .finally(() => URL.revokeObjectURL(unlock));
  }

  /** Hold the voice where it is; the next piece still gets made. */
  pause() {
    if (this.ended || this.paused) return;
    this.paused = true;
    this.audio.pause();
    this.onState('paused');
  }

  /** Go on from where it paused. From a tap or a key: iOS lets audio start
   * again only inside one. */
  resume() {
    if (this.ended || !this.paused) return;
    this.paused = false;
    // Paused before the first piece came: it starts when it comes.
    if (!this.started) return this.onState('loading');
    this.onState('playing');
    this.audio.play().catch((e) => this.end(e instanceof Error ? e.message : String(e)));
  }

  /** Stop reading, or stop waiting for the audio. */
  stop() {
    this.stopped = true;
    this.audio.pause();
    this.pieceDone?.();
    this.end();
  }

  private async run(taskId: string) {
    const pieces = await fetchPieces(taskId);
    let next = fetchAudio(pieces[0]);
    for (let i = 0; i < pieces.length; i++) {
      const blob = await next;
      if (this.stopped) return;
      // The next piece is made while this one plays.
      if (i + 1 < pieces.length) {
        next = fetchAudio(pieces[i + 1]);
        next.catch(() => {}); // awaited next round, unless stopped first
      }
      await this.play(blob);
      if (this.stopped) return;
    }
  }

  /** Plays one piece — or, while paused, puts it in place for `resume`;
   * returns when it has ended (or the reading stopped). */
  private async play(blob: Blob) {
    if (this.url) URL.revokeObjectURL(this.url);
    this.url = URL.createObjectURL(blob);
    const done = new Promise<void>((resolve) => (this.pieceDone = resolve));
    this.audio.src = this.url;
    this.started = true;
    if (!this.paused) {
      await this.audio.play();
      if (!this.ended && !this.paused) this.onState('playing');
    }
    await done;
    this.pieceDone = null;
  }

  private end(error?: string) {
    if (this.ended) return;
    this.ended = true;
    this.audio.onended = null;
    this.audio.removeAttribute('src');
    if (this.url) URL.revokeObjectURL(this.url);
    this.url = null;
    this.onEnd(error);
  }
}

async function fetchPieces(taskId: string): Promise<string[]> {
  const res = await fetch(apiUrl(`/speak?task=${encodeURIComponent(taskId)}`), { method: 'POST', credentials: 'include' });
  if (!res.ok) throw new Error((await res.text()) || `failed: ${res.status}`);
  const { pieces } = (await res.json()) as { pieces: string[] };
  if (!pieces.length) throw new Error('nothing to read aloud');
  return pieces;
}

async function fetchAudio(text: string): Promise<Blob> {
  const res = await fetch(apiUrl('/speak/audio'), {
    method: 'POST',
    body: text,
    headers: { 'content-type': 'text/plain; charset=utf-8' },
    credentials: 'include',
  });
  if (!res.ok) throw new Error((await res.text()) || `failed: ${res.status}`);
  return res.blob();
}
