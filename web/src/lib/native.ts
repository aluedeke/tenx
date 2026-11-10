// The iOS app (mobile/ios): the page in a WKWebView, with a message handler
// `tenx` the page can call. Only what a browser can't do goes through it:
// reading and writing the clipboard without iOS's Paste bubble (the app uses
// UIPasteboard itself), and whether a hardware keyboard is attached.
// Everything else is the same page as in Safari.

import type { ClipItem } from './clipboard';
import type { ClipImage, Peek } from './clipsync';

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
      return new Blob([fromBase64(value)], { type });
    },
  }));
}

/** By index: an image is tens of millions of characters, too many to walk
 * as an iterable on the page's own thread. */
export function fromBase64(value: string): Uint8Array<ArrayBuffer> {
  const binary = atob(value);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

/** In pieces: an image's bytes are too many arguments for one call. */
export function toBase64(bytes: Uint8Array): string {
  let binary = '';
  for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(binary);
}

/** The device's clipboard for lib/clipsync: its change count, and its text
 * and image only when the count moved since `since`. */
export async function nativePeek(since: number | null): Promise<Peek> {
  const h = handler();
  if (!h) throw new Error('not in the app');
  const { image, ...peek } = (await h.postMessage({ action: 'clipboardPeek', since })) as Omit<Peek, 'image'> & {
    image?: { type: string; data: string } | null;
  };
  return image ? { ...peek, image: { type: image.type, bytes: fromBase64(image.data) } } : peek;
}

/** Write text or an image to the device's clipboard unless its change count
 * moved past `ifCount`; the new count, or null when a copy came first. */
export async function nativeWrite(clip: string | ClipImage, ifCount: number): Promise<number | null> {
  const h = handler();
  if (!h) throw new Error('not in the app');
  const what = typeof clip === 'string' ? { text: clip } : { image: { type: clip.type, data: toBase64(clip.bytes) } };
  const count = await h.postMessage({ action: 'clipboardWrite', ...what, ifCount });
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
