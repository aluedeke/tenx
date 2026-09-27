// Notifications: the service worker (public/sw.js), the browser's push
// subscription, and telling `tenx web` about it (POST /push/subscribe). The
// server decides when to push — on the same edges as tenx's desktop
// notifications — this only turns them on and off for this browser.

import { apiUrl } from './api';

export type PushState =
  /** No service worker / Push API here (or not HTTPS). */
  | 'unsupported'
  /** iPhone / iPad Safari: Push only works in the Home Screen app. */
  | 'needs-install'
  | 'off'
  | 'on'
  /** The browser (or the user, earlier) refused; only its settings undo it. */
  | 'denied';

export function isIos(): boolean {
  return /iPad|iPhone|iPod/.test(navigator.userAgent) || (navigator.userAgent.includes('Macintosh') && navigator.maxTouchPoints > 1);
}

export function standalone(): boolean {
  return window.matchMedia('(display-mode: standalone)').matches || (navigator as { standalone?: boolean }).standalone === true;
}

function supported(): boolean {
  return window.isSecureContext && 'serviceWorker' in navigator && 'PushManager' in window && 'Notification' in window;
}

/** Register the worker (at page load; also what makes the page installable). */
export async function register(): Promise<void> {
  if (!window.isSecureContext || !('serviceWorker' in navigator)) return;
  try {
    await navigator.serviceWorker.register('/sw.js', { scope: '/' });
  } catch (e) {
    console.warn('tenx web: service worker', e);
  }
}

async function subscription(): Promise<PushSubscription | null> {
  const reg = await navigator.serviceWorker.getRegistration('/');
  return (await reg?.pushManager.getSubscription()) ?? null;
}

async function post(path: string, body?: unknown): Promise<Response> {
  const res = await fetch(apiUrl(path), {
    method: 'POST',
    credentials: 'include',
    headers: { 'content-type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!res.ok) throw new Error((await res.text()) || `${path}: ${res.status}`);
  return res;
}

export async function state(): Promise<PushState> {
  if (!supported()) return isIos() && !standalone() ? 'needs-install' : 'unsupported';
  if (Notification.permission === 'denied') return 'denied';
  const sub = await subscription();
  if (sub && Notification.permission === 'granted') {
    // Tell the server again: it may have lost the file, or be another run.
    post('/push/subscribe', sub.toJSON()).catch(() => {});
    return 'on';
  }
  return 'off';
}

function keyBytes(b64url: string): Uint8Array<ArrayBuffer> {
  const b64 = b64url.replace(/-/g, '+').replace(/_/g, '/').padEnd(Math.ceil(b64url.length / 4) * 4, '=');
  const raw = atob(b64);
  const out = new Uint8Array(new ArrayBuffer(raw.length));
  for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
  return out;
}

/** Turn notifications on. Call straight from the click: the permission
 * prompt only appears inside a user gesture. */
export async function enable(): Promise<PushState> {
  const permission = await Notification.requestPermission();
  if (permission !== 'granted') return permission === 'denied' ? 'denied' : 'off';
  await register();
  const reg = await navigator.serviceWorker.ready;
  const res = await fetch(apiUrl('/push/key'), { credentials: 'include' });
  if (!res.ok) throw new Error(`/push/key: ${res.status}`);
  const { key } = (await res.json()) as { key: string };
  let sub = await reg.pushManager.getSubscription();
  // A subscription made with another server key can't receive this one's.
  if (sub && sub.options.applicationServerKey) {
    const had = new Uint8Array(sub.options.applicationServerKey);
    const want = keyBytes(key);
    if (had.length !== want.length || had.some((b, i) => b !== want[i])) {
      await sub.unsubscribe();
      sub = null;
    }
  }
  sub ??= await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: keyBytes(key) });
  await post('/push/subscribe', sub.toJSON());
  return 'on';
}

export async function disable(): Promise<PushState> {
  const sub = await subscription();
  if (sub) {
    await post('/push/unsubscribe', { endpoint: sub.endpoint }).catch(() => {});
    await sub.unsubscribe();
  }
  return 'off';
}

/** A test notification to every browser that turned them on. */
export async function test(): Promise<{ sent: number; subscriptions: number }> {
  return (await post('/push/test')).json();
}

/** The app icon's badge: how many tasks need you (0 clears it). */
export function badge(count: number) {
  const nav = navigator as Navigator & { setAppBadge?(n: number): Promise<void>; clearAppBadge?(): Promise<void> };
  if (count > 0) nav.setAppBadge?.(count).catch(() => {});
  else nav.clearAppBadge?.().catch(() => {});
}
