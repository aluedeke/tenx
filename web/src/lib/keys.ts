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

/** Option+M (Alt+M): start or stop the microphone, wherever the keyboard
 * is. By the physical key — on a Mac layout Option+M types `µ`. */
export function isMicToggle(ev: KeyboardEvent): boolean {
  return ev.altKey && !ev.ctrlKey && !ev.metaKey && !ev.shiftKey && baseLetter(ev) === 'm';
}

/** Alt+n off macOS: ^n for the column. */
export function isNewTaskAlias(ev: KeyboardEvent, mac: boolean): boolean {
  return !mac && ev.altKey && !ev.ctrlKey && !ev.metaKey && (baseLetter(ev) ?? ev.key.toLowerCase()) === 'n';
}

export function keyMessage(ev: KeyboardEvent): KeyMessage {
  // With Ctrl held a browser may report the letter shifted or, on some
  // layouts, a control character; the physical key is what a terminal sends.
  const key = ev.ctrlKey && baseLetter(ev) ? baseLetter(ev)! : ev.key;
  return { type: 'key', key, ctrl: ev.ctrlKey, alt: ev.altKey, shift: ev.shiftKey };
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
