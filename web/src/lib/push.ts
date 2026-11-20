// Notifications: the service worker (public/sw.js), the browser's push
// subscription, and telling `tenx web` about it (POST /push/subscribe). The
// server decides when to push — on the same edges as tenx's desktop
// notifications, and when an agent runs `tenx notify` — this only turns them
// on and off for this browser. Inside the tenx iOS app, which has no Web
// Push, the app registers with APNs itself and this hands its device token
// to the server (POST /push/apns/subscribe).

import { apiUrl } from './api';
import { nativePushEnable, nativePushState, type NativePush } from './native';
import { inIosApp } from './viewport';

export type PushState =
  /** No service worker / Push API here (or not HTTPS). */
  | 'unsupported'
  /** iPhone / iPad Safari: Push only works in the Home Screen app. */
  | 'needs-install'
  /** Inside a tenx iOS app too old for notifications: a WKWebView has no
   * Web Push, and nothing to install — the page offers neither. */
  | 'app'
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

/** Turned off in the app: iOS keeps the app registered, so the page
 * remembers that the user doesn't want them. */
const APP_OFF_KEY = 'tenx-app-push-off';

function appOff(): boolean {
  try {
    return localStorage.getItem(APP_OFF_KEY) === '1';
  } catch {
    return false;
  }
}

function setAppOff(off: boolean) {
  try {
    if (off) localStorage.setItem(APP_OFF_KEY, '1');
    else localStorage.removeItem(APP_OFF_KEY);
  } catch {
    // A page that can't store it asks again next time.
  }
}

function inApp(): boolean {
  return inIosApp(window as Parameters<typeof inIosApp>[0]);
}

async function appState(): Promise<PushState> {
  let app: NativePush | undefined;
  try {
    app = await nativePushState();
  } catch {
    return 'app';
  }
  // An app too old for notifications answers nothing, or refuses the action.
  if (!app || typeof app.permission !== 'string') return 'app';
  if (app.permission === 'denied') return 'denied';
  if (app.permission !== 'granted' || !app.token || appOff()) return 'off';
  // Tell the server again: it may have lost the file, or be another run.
  post('/push/apns/subscribe', { token: app.token, env: app.env }).catch(() => {});
  return 'on';
}

export async function state(): Promise<PushState> {
  if (inApp()) return appState();
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
  if (inApp()) {
    const app = await nativePushEnable();
    if (app.permission !== 'granted') return app.permission === 'denied' ? 'denied' : 'off';
    if (!app.token) throw new Error('iOS gave the app no device token');
    await post('/push/apns/subscribe', { token: app.token, env: app.env });
    setAppOff(false);
    return 'on';
  }
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
  if (inApp()) {
    setAppOff(true);
    const app = await nativePushState().catch(() => null);
    if (app?.token) await post('/push/apns/unsubscribe', { token: app.token }).catch(() => {});
    return 'off';
  }
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
