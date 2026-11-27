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

/** iOS's own menu for a link held in the terminal — Copy, Open, Share — at
 * (`x`, `y`) on the page. Rejects outside the app and in an app too old to
 * have it: the page shows its own menu then. */
export async function nativeLinkMenu(url: string, x: number, y: number): Promise<void> {
  const h = handler();
  if (!h) throw new Error('not in the app');
  await h.postMessage({ action: 'linkMenu', url, x, y });
}

/** Open the app's settings (server address, sign out). */
export function openNativeSettings(): void {
  void handler()?.postMessage({ action: 'settings' });
}

/** The app's notifications: iOS's permission for them, and the device token
 * once the app registered with APNs, with the APNs environment it belongs
 * to (an Xcode build: sandbox; TestFlight: production). */
export interface NativePush {
  permission: 'granted' | 'denied' | 'notDetermined';
  token?: string;
  env: 'sandbox' | 'production';
}

/** What the app knows about its notifications, without asking the user.
 * Rejects in an app too old to have them. */
export async function nativePushState(): Promise<NativePush> {
  const h = handler();
  if (!h) throw new Error('not in the app');
  return (await h.postMessage({ action: 'pushState' })) as NativePush;
}

/** Ask iOS for permission (the prompt shows once) and register with APNs. */
export async function nativePushEnable(): Promise<NativePush> {
  const h = handler();
  if (!h) throw new Error('not in the app');
  return (await h.postMessage({ action: 'pushEnable' })) as NativePush;
}

/** The event the app fires on the window when a notification is tapped;
 * `detail` is the address it carries (`/?task=<id>`, `&unlock=1`). */
export const OPEN_TASK_EVENT = 'tenx-open-task';

/** The address of a notification tapped before the page ran (one that
 * launched the app), once; null when there is none. */
export async function nativeTakeTap(): Promise<string | null> {
  const h = handler();
  if (!h) return null;
  const url = await h.postMessage({ action: 'takeTap' });
  return typeof url === 'string' ? url : null;
}

// ── A task's pages (lib/pages, PagePane) ─────────────────

/** One of a task's pages, shown by the app in a web view of its own over
 * `rect` (the page's CSS pixels, which are the app's points: the page is
 * never zoomed). `key` names the web view (task and page id), so a page
 * shown again comes back as it was left; `store` is the workspace's cookie
 * store (`tenx_core::pages::store_id`), `storeName` what the app's settings
 * call it. */
export interface NativePage {
  key: string;
  url: string;
  store: string;
  storeName: string;
  rect: { x: number; y: number; width: number; height: number };
}

/** Show (or move) a page. Rejects outside the app and in an app too old to
 * show pages: the page opens the address outside it then. */
export async function nativePageShow(page: NativePage): Promise<void> {
  const h = handler();
  if (!h) throw new Error('not in the app');
  await h.postMessage({ action: 'pageShow', ...page });
}

/** Whether the app shows pages: an app too old to answers no action of
 * theirs. */
export async function nativePagesSupported(): Promise<boolean> {
  const h = handler();
  if (!h) return false;
  try {
    return (await h.postMessage({ action: 'pageHide' })) === true;
  } catch {
    return false;
  }
}

/** Open a web link in the app it belongs to, when one is installed (iOS's
 * universal links). False when none took it, outside the app, and in an
 * app too old to try: the caller opens it as a page then. */
export async function nativeOpenApp(url: string): Promise<boolean> {
  const h = handler();
  if (!h) return false;
  try {
    return (await h.postMessage({ action: 'openApp', url })) === true;
  } catch {
    return false;
  }
}

/** Hide the page shown, if any; it keeps its place for the next show. */
export function nativePageHide(): void {
  void handler()
    ?.postMessage({ action: 'pageHide' })
    .catch(() => {});
}

/** Back, forward or reload in the page `key`. */
export function nativePageDo(key: string, what: 'back' | 'forward' | 'reload'): void {
  void handler()
    ?.postMessage({ action: 'pageDo', key, what })
    .catch(() => {});
}

/** Take the page `key` to `url`; false when the app refused it. */
export async function nativePageGo(key: string, url: string): Promise<boolean> {
  try {
    return (await handler()?.postMessage({ action: 'pageDo', key, what: 'go', url })) === true;
  } catch {
    return false;
  }
}

/** The page `key` was closed: the app drops its web view. */
export function nativePageClose(key: string): void {
  void handler()
    ?.postMessage({ action: 'pageClose', key })
    .catch(() => {});
}

/** The event the app fires on the window when a page it shows moves on:
 * `detail` is a `NativePageState`. */
export const PAGE_EVENT = 'tenx-page';

export interface NativePageState {
  key: string;
  url: string;
  title: string;
  canGoBack: boolean;
  canGoForward: boolean;
  loading: boolean;
}
