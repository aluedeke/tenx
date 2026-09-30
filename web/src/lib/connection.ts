// The one WebSocket a tab keeps to `tenx web` (docs/web-protocol.md): binary
// frames are the terminal, text frames are JSON messages. It reconnects on
// its own with a backoff, handing the server the grouped session it had so a
// reload or a dropped phone connection lands back where it was.

import type { ClientMessage, ServerMessage } from '@/protocol';

export type Status = 'connecting' | 'open' | 'reconnecting';

const SESSION_KEY = 'tenx-web-session';
const BACKOFF_MS = [250, 500, 1000, 2000, 4000, 8000];

export interface Handlers {
  message(msg: ServerMessage): void;
  output(bytes: Uint8Array): void;
  status(status: Status, failures: number): void;
}

/** Where the socket lives: this page's own origin when `tenx web` serves it,
 * or `NEXT_PUBLIC_TENX_URL` under `next dev` (with the token in the URL,
 * since the cookie belongs to the other origin). */
function socketUrl(session: string | null): string {
  const params = new URLSearchParams();
  if (session) params.set('session', session);
  const dev = process.env.NEXT_PUBLIC_TENX_URL;
  let base: string;
  if (dev) {
    const url = new URL(dev);
    url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
    url.pathname = '/ws';
    base = url.toString();
    const token = process.env.NEXT_PUBLIC_TENX_TOKEN || new URLSearchParams(location.search).get('token');
    if (token) params.set('token', token);
  } else {
    base = `${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${location.host}/ws`;
  }
  const qs = params.toString();
  return qs ? `${base}?${qs}` : base;
}

// localStorage, not sessionStorage: an iOS Home Screen app that is closed and
// reopened starts a new session storage, and would never find its tmux
// session again. Two desktop tabs asking for the same id is fine — the
// server gives the second one a session of its own.
function storedSession(): string | null {
  try {
    return localStorage.getItem(SESSION_KEY);
  } catch {
    return null;
  }
}

function storeSession(id: string) {
  try {
    localStorage.setItem(SESSION_KEY, id);
  } catch {
    // Private mode or blocked storage: a reload just starts a new session.
  }
}

export class Connection {
  private ws: WebSocket | null = null;
  private failures = 0;
  private timer: ReturnType<typeof setTimeout> | null = null;
  /** The session the last socket asked for. */
  private asked: string | null = null;
  /** The last `hello` came back with the session asked for: the page is back
   * where it was (within the server's grace period), not on a new session. */
  resumed = false;
  private closed = false;
  private encoder = new TextEncoder();

  constructor(private handlers: Handlers) {}

  start() {
    this.closed = false;
    this.open();
  }

  stop() {
    this.closed = true;
    if (this.timer) clearTimeout(this.timer);
    this.ws?.close();
    this.ws = null;
  }

  get isOpen(): boolean {
    return this.ws?.readyState === WebSocket.OPEN;
  }

  /** Whether it went out (the socket was open). */
  send(msg: ClientMessage): boolean {
    if (!this.isOpen) return false;
    this.ws!.send(JSON.stringify(msg));
    return true;
  }

  /** Keyboard input for the terminal. */
  input(data: string | Uint8Array) {
    if (!this.isOpen) return;
    this.ws!.send(typeof data === 'string' ? this.encoder.encode(data) : data);
  }

  private open() {
    this.handlers.status(this.failures === 0 ? 'connecting' : 'reconnecting', this.failures);
    this.asked = storedSession();
    const ws = new WebSocket(socketUrl(this.asked));
    ws.binaryType = 'arraybuffer';
    this.ws = ws;
    ws.onopen = () => {
      this.failures = 0;
      this.handlers.status('open', 0);
    };
    ws.onmessage = (ev) => {
      if (typeof ev.data === 'string') {
        let msg: ServerMessage;
        try {
          msg = JSON.parse(ev.data);
        } catch {
          return;
        }
        if (msg.type === 'hello') {
          this.resumed = msg.session === this.asked;
          storeSession(msg.session);
        }
        this.handlers.message(msg);
      } else {
        this.handlers.output(new Uint8Array(ev.data as ArrayBuffer));
      }
    };
    ws.onclose = () => {
      if (this.ws !== ws) return;
      this.ws = null;
      if (this.closed) return;
      this.failures += 1;
      this.handlers.status('reconnecting', this.failures);
      const delay = BACKOFF_MS[Math.min(this.failures - 1, BACKOFF_MS.length - 1)];
      this.timer = setTimeout(() => this.open(), delay);
    };
  }
}
