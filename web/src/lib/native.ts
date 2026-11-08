// The iOS app (mobile/ios): the page in a WKWebView, with a message handler
// `tenx` the page can call. Only what a browser can't do goes through it:
// reading and writing the clipboard without iOS's Paste bubble (the app uses
// UIPasteboard itself), and whether a hardware keyboard is attached.
// Everything else is the same page as in Safari.

import type { ClipItem } from './clipboard';
import type { Peek } from './clipsync';

/** One pasteboard item as the app sends it: per MIME type, text as is and
 * images as base64. */
export interface NativeItem {
  types: string[];
  data: Record<string, string>;
}

interface Handler {
  postMessage(message: unknown): Promise<unknown>;
}

function handler(): Handler | null {
  if (typeof window === 'undefined') return null;
  const w = window as Window & { webkit?: { messageHandlers?: { tenx?: Handler } } };
  return w.webkit?.messageHandlers?.tenx ?? null;
}

/** The page runs inside the tenx iOS app. */
export function nativeApp(): boolean {
  return handler() !== null;
}

/** The device's clipboard, read by the app, in the shape
 * `navigator.clipboard.read()` gives (so lib/clipboard chooses the same way). */
export async function nativeClipboard(): Promise<ClipItem[]> {
  const h = handler();
  if (!h) throw new Error('not in the app');
  return clipItems((await h.postMessage({ action: 'clipboard' })) as NativeItem[]);
}

export function clipItems(items: NativeItem[]): ClipItem[] {
  return items.map((item) => ({
    types: item.types,
    async getType(type: string): Promise<Blob> {
      const value = item.data[type];
      if (value === undefined) throw new Error(`no ${type}`);
      if (type.startsWith('text/')) return new Blob([value], { type });
      return new Blob([Uint8Array.from(atob(value), (c) => c.charCodeAt(0))], { type });
    },
  }));
}

/** The device's clipboard for lib/clipsync: its change count, and the text
 * only when the count moved since `since`. */
export async function nativePeek(since: number | null): Promise<Peek> {
  const h = handler();
  if (!h) throw new Error('not in the app');
  return (await h.postMessage({ action: 'clipboardPeek', since })) as Peek;
}

/** Write `text` to the device's clipboard unless its change count moved past
 * `ifCount`; the new count, or null when a copy came first. */
export async function nativeWrite(text: string, ifCount: number): Promise<number | null> {
  const h = handler();
  if (!h) throw new Error('not in the app');
  const count = await h.postMessage({ action: 'clipboardWrite', text, ifCount });
  return typeof count === 'number' ? count : null;
}

/** The event the app fires on the window when a hardware keyboard comes or
 * goes; `detail` is whether one is attached now. */
export const HARDWARE_KEYBOARD_EVENT = 'tenx-hardware-keyboard';

/** Whether a hardware keyboard is attached, as the app sees it (false
 * outside the app: a page can't tell). */
export async function nativeHardwareKeyboard(): Promise<boolean> {
  const h = handler();
  if (!h) return false;
  return (await h.postMessage({ action: 'hardwareKeyboard' })) === true;
}

/** Open the app's settings (server address, sign out). */
export function openNativeSettings(): void {
  void handler()?.postMessage({ action: 'settings' });
}
