// Speech to text for the terminal: the page records with the microphone and
// `tenx web` has it transcribed on its machine (POST /transcribe → a
// whisper.cpp server there; docs/web-protocol.md). Nothing is transcribed in
// the browser: a model good enough — tuned to its user's voice and terms —
// is too big for a phone, and Safari's own recognition doesn't exist in a
// Home Screen app.
//
// One setting in localStorage: tenx.stt.language, a Whisper language code
// ("de", "en") or "auto"; empty leaves it to the server.

import { apiUrl } from './api';

/** Whisper's input: 16 kHz mono. */
const RATE = 16_000;

export function sttLanguage(): string {
  try {
    return localStorage.getItem('tenx.stt.language') || '';
  } catch {
    return '';
  }
}

/** Why the microphone can't be used here, or null when it can. */
export function sttUnsupported(): string | null {
  if (typeof window === 'undefined') return 'no window';
  if (!window.isSecureContext) return 'the microphone needs HTTPS — open tenx web through `tailscale serve`';
  if (!navigator.mediaDevices?.getUserMedia) return 'this browser has no microphone access';
  if (typeof MediaRecorder === 'undefined') return 'this browser cannot record audio';
  return null;
}

/** One recording: started on a tap, stopped on the next. */
export class Recording {
  private chunks: Blob[] = [];
  private constructor(
    private stream: MediaStream,
    private rec: MediaRecorder,
  ) {
    rec.ondataavailable = (e) => {
      if (e.data.size) this.chunks.push(e.data);
    };
  }

  static async start(): Promise<Recording> {
    const stream = await navigator.mediaDevices.getUserMedia({
      audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true },
    });
    const r = new Recording(stream, new MediaRecorder(stream));
    r.rec.start();
    return r;
  }

  /** Stop and give the audio, 16 kHz mono. */
  async stop(): Promise<Float32Array> {
    const done = new Promise<void>((resolve) => (this.rec.onstop = () => resolve()));
    this.rec.stop();
    await done;
    this.release();
    const blob = new Blob(this.chunks, { type: this.rec.mimeType });
    return toMono16k(await blob.arrayBuffer());
  }

  /** The microphone off — and with it the browser's recording indicator. */
  private release() {
    for (const t of this.stream.getTracks()) t.stop();
  }
}

/** Decode whatever MediaRecorder made (WebM/Opus, MP4/AAC on Safari) and
 * resample it to 16 kHz mono. */
async function toMono16k(data: ArrayBuffer): Promise<Float32Array> {
  const ctx = new AudioContext();
  let decoded: AudioBuffer;
  try {
    decoded = await ctx.decodeAudioData(data);
  } finally {
    void ctx.close();
  }
  const length = Math.max(1, Math.ceil(decoded.duration * RATE));
  const off = new OfflineAudioContext(1, length, RATE);
  const src = off.createBufferSource();
  src.buffer = decoded;
  src.connect(off.destination);
  src.start();
  const out = await off.startRendering();
  return out.getChannelData(0);
}

/** Whether a recording has speech in it: at least a quarter second of 30 ms
 * frames louder than room noise. Whisper given silence makes text up, so
 * silence isn't sent. */
export function hasSpeech(audio: Float32Array, threshold = 0.01, minSeconds = 0.25): boolean {
  const frame = Math.round(RATE * 0.03);
  let loud = 0;
  for (let i = 0; i + frame <= audio.length; i += frame) {
    let sum = 0;
    for (let j = i; j < i + frame; j++) sum += audio[j] * audio[j];
    if (Math.sqrt(sum / frame) > threshold) loud++;
  }
  return loud * 0.03 >= minSeconds;
}

/** 16-bit PCM WAV: what a Whisper server reads without a converter. */
export function wav(audio: Float32Array): Blob {
  const buf = new ArrayBuffer(44 + audio.length * 2);
  const v = new DataView(buf);
  const str = (at: number, s: string) => [...s].forEach((c, i) => v.setUint8(at + i, c.charCodeAt(0)));
  str(0, 'RIFF');
  v.setUint32(4, 36 + audio.length * 2, true);
  str(8, 'WAVE');
  str(12, 'fmt ');
  v.setUint32(16, 16, true);
  v.setUint16(20, 1, true); // PCM
  v.setUint16(22, 1, true); // mono
  v.setUint32(24, RATE, true);
  v.setUint32(28, RATE * 2, true);
  v.setUint16(32, 2, true);
  v.setUint16(34, 16, true);
  str(36, 'data');
  v.setUint32(40, audio.length * 2, true);
  for (let i = 0; i < audio.length; i++) {
    const s = Math.max(-1, Math.min(1, audio[i]));
    v.setInt16(44 + i * 2, s < 0 ? s * 0x8000 : s * 0x7fff, true);
  }
  return new Blob([buf], { type: 'audio/wav' });
}

/** The text of a recording, from the speech server on tenx web's machine. */
export async function transcribe(audio: Float32Array): Promise<string> {
  const url = new URL(apiUrl('/transcribe'), location.href);
  const language = sttLanguage();
  if (language) url.searchParams.set('language', language);
  const res = await fetch(url, { method: 'POST', body: wav(audio), headers: { 'content-type': 'audio/wav' }, credentials: 'include' });
  if (!res.ok) throw new Error((await res.text()) || `transcribe failed: ${res.status}`);
  const { text } = (await res.json()) as { text: string };
  return text;
}
