// Keyboard plumbing shared by the page and the touch key bar.

import type { KeyMessage } from '@/protocol';

/** Cmd, not Ctrl, carries the browser's own shortcuts on a Mac, so ^w and ^n
 * reach the page there; elsewhere Ctrl+W closes the tab before any page sees
 * it, and Alt+w / Alt+n stand in (docs/web-protocol.md). */
export function isMac(): boolean {
  if (typeof navigator === 'undefined') return false;
  const platform = (navigator as Navigator & { userAgentData?: { platform?: string } }).userAgentData?.platform ?? navigator.platform;
  return /mac|iphone|ipad|ipod/i.test(platform);
}

/** An iPad. Safari there says it is a Mac (`MacIntel`); a Mac has no
 * touch screen, though. */
export function isIPad(platform: string, maxTouchPoints: number): boolean {
  return /ipad/i.test(platform) || (platform === 'MacIntel' && maxTouchPoints > 1);
}

const MODIFIERS = new Set(['Shift', 'Control', 'Alt', 'Meta', 'CapsLock', 'AltGraph', 'Fn', 'Dead', 'Unidentified', 'Process']);

/** A lone modifier or composition key — nothing the column could act on. */
export function isModifierOnly(ev: KeyboardEvent): boolean {
  return MODIFIERS.has(ev.key) || ev.isComposing;
}

/** `event.code` → the letter it would type, for Alt combinations on a Mac
 * keyboard layout where Alt+w types `∑`. */
function baseLetter(ev: KeyboardEvent): string | null {
  const m = /^Key([A-Z])$/.exec(ev.code);
  return m ? m[1].toLowerCase() : null;
}

/** ^w, or its alias off macOS. */
export function isFocusCycle(ev: KeyboardEvent, mac: boolean): boolean {
  const letter = baseLetter(ev) ?? ev.key.toLowerCase();
  if (letter !== 'w' || ev.metaKey || ev.shiftKey) return false;
  return (ev.ctrlKey && !ev.altKey) || (!mac && ev.altKey && !ev.ctrlKey);
}

/** Ctrl+M: start or stop the microphone, wherever the keyboard is. By the
 * physical key. In a terminal Ctrl+M is the same byte as Enter, which the
 * page takes before xterm does: Enter itself stays Enter (and ends a
 * dictation with a send, `enterWhileDictating`). */
export function isMicToggle(ev: KeyboardEvent): boolean {
  return ev.ctrlKey && !ev.altKey && !ev.metaKey && !ev.shiftKey && baseLetter(ev) === 'm';
}

/** Ctrl+.: read the current task's last reply aloud, pause, go on — the
 * speaker in the header. By the physical key (the period key is in the
 * same place on US and German keyboards). A terminal has no use for it: it
 * sends no byte of its own, so no agent or shell key is taken. */
export function isSpeakerToggle(ev: KeyboardEvent): boolean {
  return ev.ctrlKey && !ev.altKey && !ev.metaKey && !ev.shiftKey && ev.code === 'Period';
}

/** Ctrl+V, the key Claude Code pastes an image with. It reads the clipboard
 * of the machine it runs on — not the one of a browser on another device,
 * where the image is. By the physical key, like the others. */
export function isImagePaste(ev: KeyboardEvent): boolean {
  return ev.ctrlKey && !ev.altKey && !ev.metaKey && !ev.shiftKey && baseLetter(ev) === 'v';
}

/** Whether the page takes this ^V for an image paste: in the terminal, on an
 * Apple device, away from the machine `tenx web` runs on. Off a Mac ^V is
 * the browser's own paste, which reaches the page as a paste event. */
export function takesImagePaste(ev: KeyboardEvent, s: { terminal: boolean; mac: boolean; hostname: string }): boolean {
  return s.terminal && s.mac && isImagePaste(ev) && isRemote(s.hostname);
}

/** The browser runs on another machine than `tenx web` (a phone or a
 * tablet, through `tailscale serve`): its clipboard isn't the agent's. */
export function isRemote(hostname: string): boolean {
  return !['localhost', '127.0.0.1', '[::1]', '::1'].includes(hostname);
}

/** Alt+n off macOS: ^n for the column (Alt+Shift+n: ^⇧n, which the
 * browser keeps too). */
export function isNewTaskAlias(ev: KeyboardEvent, mac: boolean): boolean {
  return !mac && ev.altKey && !ev.ctrlKey && !ev.metaKey && (baseLetter(ev) ?? ev.key.toLowerCase()) === 'n';
}

export function keyMessage(ev: KeyboardEvent): KeyMessage {
  // With Ctrl held a browser may report the letter shifted or, on some
  // layouts, a control character; the physical key is what a terminal sends.
  const key = ev.ctrlKey && baseLetter(ev) ? baseLetter(ev)! : ev.key;
  return { type: 'key', key, ctrl: ev.ctrlKey, alt: ev.altKey, shift: ev.shiftKey };
}

/** Enter (a lone `\r`: Shift+Enter is CSI u) while the microphone is on:
 * the natural "send". `stop`: stop the recording, then send Enter after its
 * final text; `wait`: the final text is on its way, send Enter after it. */
export function enterWhileDictating(data: string, mic: 'idle' | 'recording' | 'transcribing'): 'stop' | 'wait' | null {
  if (data !== '\r' || mic === 'idle') return null;
  return mic === 'recording' ? 'stop' : 'wait';
}

/** The bytes a terminal sends for the key bar's keys. */
export const TERMINAL_BYTES: Record<string, string> = {
  Escape: '\x1b',
  Tab: '\t',
  ArrowUp: '\x1b[A',
  ArrowDown: '\x1b[B',
  Enter: '\r',
};

/** Ctrl+<char> as a terminal sends it: the control code for letters and the
 * few punctuation keys that have one; anything else passes unchanged. */
export function ctrlChar(ch: string): string {
  if (ch.length !== 1) return ch;
  const c = ch.toLowerCase().charCodeAt(0);
  if (c >= 97 && c <= 122) return String.fromCharCode(c - 96);
  const map: Record<string, number> = { '@': 0, ' ': 0, '[': 27, '\\': 28, ']': 29, '^': 30, _: 31, '?': 127 };
  return ch in map ? String.fromCharCode(map[ch]) : ch;
}
