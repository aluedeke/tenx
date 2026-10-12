// The iOS app (mobile/ios): the page in a WKWebView, with a message handler
// `tenx` the page can call. Only what a browser can't do goes through it:
// reading the clipboard without iOS's Paste bubble (the app reads
// UIPasteboard itself). Everything else is the same page as in Safari.

import type { ClipItem } from './clipboard';

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

/** Open the app's settings (server address, sign out). */
export function openNativeSettings(): void {
  void handler()?.postMessage({ action: 'settings' });
}
