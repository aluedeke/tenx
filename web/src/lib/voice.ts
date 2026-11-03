// Talk mode: a conversation with tenx's voice assistant (`tenx-voice`, a
// Pipecat pipeline that `tenx web` runs — src/web/voice.rs). The audio goes
// over WebRTC straight between this page and that program, with the
// browser's echo cancellation, so it can listen while it speaks and stop
// when interrupted; only the setup goes through `tenx web`, behind the
// cookie: the offer to `POST /voice/offer`, late ICE candidates with `PATCH`
// on the same path, which task the page shows to `POST /voice/context`, and
// a pause to `POST /voice/pause` (docs/web-protocol.md).
//
// No Pipecat client library: its WebRTC transport brings Daily's SDK along.
// What this needs of Pipecat's protocol is small — a data channel that says
// "ping" every second (the program's only way to notice a page that went
// away), `client-ready`, and the RTVI messages it reads for its captions.

import { apiUrl } from './api.ts';

/** What the conversation is doing, for the header. */
export type VoiceState = 'connecting' | 'listening' | 'hearing' | 'thinking' | 'speaking' | 'paused';

/** A line of the conversation, for the captions. */
export interface Caption {
  who: 'you' | 'tenx' | 'tool';
  text: string;
}

/** What an RTVI message from the program means here; null for the rest. */
export type VoiceEvent =
  | { kind: 'state'; state: VoiceState }
  | { kind: 'caption'; caption: Caption }
  | { kind: 'renegotiate' };

/** Tool names as the captions say them: `tenx_send_message` → "send message". */
export function toolLabel(name: string): string {
  return name.replace(/^tenx_/, '').replace(/_/g, ' ');
}

/** One data channel message → what it changes, or null. */
export function voiceEvent(raw: unknown): VoiceEvent | null {
  if (!raw || typeof raw !== 'object') return null;
  const msg = raw as { label?: string; type?: string; data?: Record<string, unknown>; message?: { type?: string } };
  if (msg.type === 'signalling') return msg.message?.type === 'renegotiate' ? { kind: 'renegotiate' } : null;
  if (msg.label !== 'rtvi-ai') return null;
  const data = msg.data ?? {};
  switch (msg.type) {
    case 'bot-ready':
    case 'bot-stopped-speaking':
    case 'user-stopped-speaking':
      return { kind: 'state', state: 'listening' };
    case 'user-started-speaking':
      return { kind: 'state', state: 'hearing' };
    case 'bot-llm-started':
      return { kind: 'state', state: 'thinking' };
    case 'bot-started-speaking':
      return { kind: 'state', state: 'speaking' };
    case 'user-transcription':
      return data.final && typeof data.text === 'string' && data.text.trim()
        ? { kind: 'caption', caption: { who: 'you', text: data.text.trim() } }
        : null;
    case 'bot-output':
      // Sentence by sentence, as spoken; the word-by-word ones repeat them.
      return typeof data.text === 'string' && data.text.trim() && data.spoken !== false && data.aggregated_by !== 'word'
        ? { kind: 'caption', caption: { who: 'tenx', text: data.text.trim() } }
        : null;
    case 'llm-function-call-started':
      return typeof data.function_name === 'string'
        ? { kind: 'caption', caption: { who: 'tool', text: toolLabel(data.function_name) } }
        : null;
    default:
      return null;
  }
}

/** Captions kept on screen: the last few lines are enough to follow. */
export const CAPTIONS = 4;

/** Add a caption; a line from the same speaker right after joins it. */
export function addCaption(list: Caption[], c: Caption): Caption[] {
  const last = list[list.length - 1];
  const next = last && last.who === c.who && c.who !== 'tool' ? [...list.slice(0, -1), { who: c.who, text: `${last.text} ${c.text}` }] : [...list, c];
  return next.slice(-CAPTIONS);
}

interface Handlers {
  onState(state: VoiceState): void;
  onCaption(c: Caption): void;
  /** The conversation ended: on its own (`error` set) or by `stop`. */
  onEnd(error?: string): void;
}

/** How long to wait for ICE candidates before sending the offer anyway
 * (the rest follow with PATCH). */
const GATHER_MS = 1500;

export class Conversation {
  private pc: RTCPeerConnection | null = null;
  private dc: RTCDataChannel | null = null;
  private mic: MediaStream | null = null;
  private pcId: string | null = null;
  private ping: ReturnType<typeof setInterval> | null = null;
  private ended = false;
  private early: RTCIceCandidateInit[] = [];
  private task: string | null;
  private paused = false;
  /** The program's last state, shown again when the pause ends. */
  private last: VoiceState = 'listening';
  private h: Handlers;
  readonly audio: HTMLAudioElement;

  /** Call straight from the tap: iOS lets audio play, and asks for the
   * microphone, only inside one. */
  constructor(task: string | null, h: Handlers) {
    this.task = task;
    this.h = h;
    this.audio = new Audio();
    this.audio.autoplay = true;
    // Unlocks playback for when the stream arrives.
    void this.audio.play().catch(() => {});
    void this.start().catch((e) => this.end(e instanceof Error ? e.message : String(e)));
  }

  /** Starts in `connecting`, which the caller shows: nothing is reported
   * from inside the constructor, before the caller holds the object. */
  private async start() {
    this.mic = await navigator.mediaDevices.getUserMedia({
      audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
    });
    if (this.ended) return this.release();
    const pc = new RTCPeerConnection();
    this.pc = pc;
    // The program expects the microphone on the first transceiver.
    pc.addTransceiver(this.mic.getAudioTracks()[0], { direction: 'sendrecv', streams: [this.mic] });
    pc.ontrack = (ev) => {
      this.audio.srcObject = ev.streams[0] ?? new MediaStream([ev.track]);
      void this.audio.play().catch(() => {});
    };
    pc.onconnectionstatechange = () => {
      if (pc.connectionState === 'failed') this.end('the audio connection failed');
      if (pc.connectionState === 'closed') this.end();
    };
    pc.onicecandidate = (ev) => {
      if (!ev.candidate) return;
      if (this.pcId) void this.trickle([ev.candidate.toJSON()]);
      else this.early.push(ev.candidate.toJSON());
    };
    const dc = pc.createDataChannel('chat', { ordered: true });
    this.dc = dc;
    dc.onopen = () => {
      dc.send(JSON.stringify({ label: 'rtvi-ai', type: 'client-ready', id: crypto.randomUUID(), data: { version: '1.0.0', about: { library: 'tenx-web' } } }));
      this.ping = setInterval(() => dc.readyState === 'open' && dc.send(`ping: ${Date.now()}`), 1000);
    };
    dc.onmessage = (ev) => {
      let parsed: unknown;
      try {
        parsed = JSON.parse(typeof ev.data === 'string' ? ev.data : '');
      } catch {
        return;
      }
      const e = voiceEvent(parsed);
      if (!e) return;
      if (e.kind === 'state') {
        this.last = e.state;
        if (!this.paused) this.h.onState(e.state);
      }
      else if (e.kind === 'caption') this.h.onCaption(e.caption);
      else void this.negotiate().catch((err) => this.end(err instanceof Error ? err.message : String(err)));
    };
    await this.negotiate();
  }

  /** An offer, its answer: the first connection or a renegotiation. */
  private async negotiate() {
    const pc = this.pc;
    if (!pc) return;
    await pc.setLocalDescription(await pc.createOffer());
    await gathered(pc, GATHER_MS);
    const local = pc.localDescription;
    if (!local || this.ended) return;
    const res = await fetch(apiUrl('/voice/offer'), {
      method: 'POST',
      credentials: 'include',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ sdp: local.sdp, type: local.type, pc_id: this.pcId, restart_pc: false, task: this.task }),
    });
    if (!res.ok) throw new Error((await res.text()) || `HTTP ${res.status}`);
    const answer = (await res.json()) as { sdp: string; type: RTCSdpType; pc_id: string };
    if (this.ended) return;
    this.pcId = answer.pc_id;
    await pc.setRemoteDescription({ sdp: answer.sdp, type: answer.type });
    if (this.early.length) void this.trickle(this.early.splice(0));
  }

  private async trickle(candidates: RTCIceCandidateInit[]) {
    if (!this.pcId || this.ended) return;
    await fetch(apiUrl('/voice/offer'), {
      method: 'PATCH',
      credentials: 'include',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        pc_id: this.pcId,
        candidates: candidates.map((c) => ({ candidate: c.candidate ?? '', sdp_mid: c.sdpMid ?? '0', sdp_mline_index: c.sdpMLineIndex ?? 0 })),
      }),
    }).catch(() => {});
  }

  /** The page now shows `task`: "it" means that one. */
  lookAt(task: string | null) {
    if (task === this.task) return;
    this.task = task;
    if (!this.pcId || this.ended) return;
    void fetch(apiUrl('/voice/context'), {
      method: 'POST',
      credentials: 'include',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ pc_id: this.pcId, task }),
    }).catch(() => {});
  }

  /** Pause: the microphone sends silence and the speaker is muted at
   * once, here, and the program stops mid-sentence and keeps its news for
   * later. Again: go on. */
  togglePause() {
    if (!this.pcId || this.ended) return;
    this.paused = !this.paused;
    this.mic?.getAudioTracks().forEach((t) => (t.enabled = !this.paused));
    this.audio.muted = this.paused;
    this.h.onState(this.paused ? 'paused' : this.last);
    void fetch(apiUrl('/voice/pause'), {
      method: 'POST',
      credentials: 'include',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ pc_id: this.pcId, paused: this.paused }),
    }).catch(() => {});
  }

  stop() {
    this.end();
  }

  private end(error?: string) {
    if (this.ended) return;
    this.ended = true;
    this.release();
    this.h.onEnd(error);
  }

  private release() {
    if (this.ping) clearInterval(this.ping);
    this.ping = null;
    this.dc?.close();
    this.pc?.close();
    this.mic?.getTracks().forEach((t) => t.stop());
    this.audio.pause();
    this.audio.srcObject = null;
  }
}

/** Resolves when ICE gathering is complete, or after `ms`. */
function gathered(pc: RTCPeerConnection, ms: number): Promise<void> {
  if (pc.iceGatheringState === 'complete') return Promise.resolve();
  return new Promise((resolve) => {
    const done = () => {
      clearTimeout(t);
      pc.removeEventListener('icegatheringstatechange', check);
      resolve();
    };
    const check = () => pc.iceGatheringState === 'complete' && done();
    const t = setTimeout(done, ms);
    pc.addEventListener('icegatheringstatechange', check);
  });
}
