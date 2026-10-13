// Speech to text for the terminal, live: while the user talks, the page
// streams the microphone to `tenx web` (the `/speech` WebSocket,
// docs/web-protocol.md), which transcribes it on its machine about once a
// second and answers with the text so far — settled words and a tentative
// tail. On stop it answers with the final text, which the page types.
// Nothing is transcribed in the browser: a model good enough — tuned to its
// user's voice and terms — is too big for a phone, and Safari's own
// recognition doesn't exist in a Home Screen app.
//
// One setting in localStorage: tenx.stt.language, a Whisper language code
// ("de", "en") or "auto"; empty leaves it to the server.

/** Whisper's input: 16 kHz mono. */
const RATE = 16_000;
/** Audio goes out in pieces this long (samples): 100 ms. */
const CHUNK = RATE / 10;
/** The "speak now" chime: two short rising tones (C5, G5), in seconds. */
const CHIME = 0.18;

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
  if (typeof AudioWorkletNode === 'undefined') return 'this browser cannot stream audio';
  return null;
}

/** The text so far: `done` sentences and `committed` words won't change,
 * `tentative` may. `corrected`: a sentence's final pass just changed it. */
export interface LiveText {
  done: string;
  committed: string;
  tentative: string;
  corrected: boolean;
}

/** `/speech` on this page's server, or on `NEXT_PUBLIC_TENX_URL` under
 * `next dev` (with the token, since the cookie belongs to the other origin). */
function speechUrl(): string {
  const params = new URLSearchParams();
  const language = sttLanguage();
  if (language) params.set('language', language);
  const dev = process.env.NEXT_PUBLIC_TENX_URL;
  let url: URL;
  if (dev) {
    url = new URL('/speech', dev);
    const token = process.env.NEXT_PUBLIC_TENX_TOKEN || new URLSearchParams(location.search).get('token');
    if (token) params.set('token', token);
  } else {
    url = new URL('/speech', location.href);
  }
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  url.search = params.toString();
  return url.toString();
}

/** Turns the microphone's blocks (at the audio context's rate) into 16 kHz
 * 16-bit pieces: each output sample the mean of the input samples it covers,
 * which also keeps what's above 8 kHz from folding back in. */
class Downsampler {
  private pos = 0; // where the next output sample starts, in input samples
  private sum = 0;
  private n = 0;
  private out: number[] = [];
  constructor(private ratio: number) {}

  push(block: Float32Array): Int16Array[] {
    const pieces: Int16Array[] = [];
    for (let i = 0; i < block.length; i++) {
      this.sum += block[i];
      this.n++;
      if (++this.pos >= this.ratio) {
        this.pos -= this.ratio;
        this.out.push(this.sum / this.n);
        this.sum = 0;
        this.n = 0;
        if (this.out.length >= CHUNK) pieces.push(this.take());
      }
    }
    return pieces;
  }

  take(): Int16Array {
    const pcm = Int16Array.from(this.out, (s) => {
      const c = Math.max(-1, Math.min(1, s));
      return c < 0 ? c * 0x8000 : c * 0x7fff;
    });
    this.out = [];
    return pcm;
  }
}

/** Safari's Audio Session API (16.4+): how iOS mixes and routes this page's
 * sound. Missing from TypeScript's DOM types and from other browsers. */
type AudioSession = { type: string };
const audioSession = (): AudioSession | undefined =>
  typeof navigator === 'undefined' ? undefined : (navigator as unknown as { audioSession?: AudioSession }).audioSession;

/** Plays the "speak now" chime now; returns when it has ended, on the
 * context's clock. */
function chime(ctx: AudioContext): number {
  const t = ctx.currentTime;
  const gain = ctx.createGain();
  gain.connect(ctx.destination);
  gain.gain.setValueAtTime(0, t);
  for (const [i, hz] of [523, 784].entries()) {
    const at = t + i * (CHIME / 2);
    const osc = ctx.createOscillator();
    osc.type = 'sine';
    osc.frequency.value = hz;
    osc.connect(gain);
    gain.gain.setTargetAtTime(0.5, at, 0.005);
    gain.gain.setTargetAtTime(0, at + CHIME / 2 - 0.03, 0.01);
    osc.start(at);
    osc.stop(at + CHIME / 2);
  }
  // A little after the last tone, for the room's echo of it.
  return t + CHIME + 0.05;
}

/** One dictation: started on a tap, stopped on the next. */
export class Dictation {
  private finished: Promise<string>;
  private constructor(
    private ctx: AudioContext,
    private stream: MediaStream,
    private node: AudioWorkletNode,
    private ws: WebSocket,
    private down: Downsampler,
    onText: (t: LiveText) => void,
    onError: (message: string) => void,
    log: (message: string) => void,
  ) {
    let stopping = false;
    this.finished = new Promise((resolve, reject) => {
      ws.onmessage = (e) => {
        const m = JSON.parse(String(e.data));
        if (m.type === 'live') onText(m);
        else if (m.type === 'text') resolve(m.text ?? '');
        else if (m.type === 'error') {
          reject(new Error(m.error));
          if (!stopping) onError(m.error);
        }
      };
      ws.onclose = () => {
        reject(new Error('the connection to tenx web closed'));
        if (!stopping) onError('the connection to tenx web closed');
      };
    });
    this.finished.catch(() => {});
    this.stopping = () => (stopping = true);
    // The first block from the microphone means it records: the chime says
    // "speak now". The microphone hears the chime too, so what it records
    // until the chime has ended is dropped, not sent to Whisper.
    let speakFrom = Infinity;
    node.port.onmessage = (e: MessageEvent<Float32Array>) => {
      if (speakFrom === Infinity) {
        speakFrom = chime(ctx);
        log(`chime: context ${ctx.state} at ${ctx.sampleRate} Hz, audio session ${audioSession()?.type ?? 'none'}`);
      }
      if (ctx.currentTime < speakFrom) return;
      for (const piece of this.down.push(e.data)) this.send(piece);
    };
  }

  private stopping: () => void;

  private send(pcm: Int16Array) {
    if (this.ws.readyState === WebSocket.OPEN) this.ws.send(pcm.buffer);
  }

  /** Straight from a tap: iOS lets audio start only inside one, so the
   * audio context is made before anything is awaited. `log` gets
   * diagnostics for the server's log. */
  static async start(
    onText: (t: LiveText) => void,
    onError: (message: string) => void,
    log: (message: string) => void = () => {},
  ): Promise<Dictation> {
    // iOS: with the microphone on, the page's sound would go to the earpiece
    // or be muted, and the chime unheard. Play-and-record sends it to the
    // speaker, also with the ringer switched to silent.
    const session = audioSession();
    if (session) session.type = 'play-and-record';
    const ctx = new AudioContext();
    void ctx.resume();
    let stream: MediaStream | undefined;
    try {
      stream = await navigator.mediaDevices.getUserMedia({
        audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true },
      });
      const ws = new WebSocket(speechUrl());
      ws.binaryType = 'arraybuffer';
      const open = new Promise<void>((resolve, reject) => {
        ws.onopen = () => resolve();
        ws.onerror = () => reject(new Error('cannot reach tenx web for speech'));
      });
      await ctx.audioWorklet.addModule('/pcm-worklet.js');
      const node = new AudioWorkletNode(ctx, 'pcm');
      ctx.createMediaStreamSource(stream).connect(node);
      node.connect(ctx.destination); // it outputs silence; connected, it runs
      await open;
      return new Dictation(ctx, stream, node, ws, new Downsampler(ctx.sampleRate / RATE), onText, onError, log);
    } catch (e) {
      for (const t of stream?.getTracks() ?? []) t.stop();
      void ctx.close();
      if (session) session.type = 'auto';
      throw e;
    }
  }

  /** The microphone off, then the final text. */
  async stop(): Promise<string> {
    this.stopping();
    this.release();
    const rest = this.down.take();
    if (rest.length) this.send(rest);
    if (this.ws.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify({ type: 'stop' }));
    try {
      return await this.finished;
    } finally {
      this.ws.close();
    }
  }

  /** Give up: the microphone off, nothing typed. */
  cancel() {
    this.stopping();
    this.release();
    this.ws.close();
  }

  /** The microphone off — and with it the browser's recording indicator. */
  private release() {
    this.node.port.onmessage = null;
    this.node.disconnect();
    for (const t of this.stream.getTracks()) t.stop();
    void this.ctx.close();
    const session = audioSession();
    if (session) session.type = 'auto';
  }
}
