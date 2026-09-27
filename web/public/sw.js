// tenx web's service worker: Web Push only (docs/web-protocol.md). It shows
// what `tenx web` pushes when a task starts needing you, and a tap on the
// notification brings the page to that task.
//
// No fetch handler, deliberately: the page is a live terminal, useless
// offline, and a cache here could keep serving an old page after tenx is
// upgraded. Installing to a Home Screen doesn't need one.

self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (event) => event.waitUntil(self.clients.claim()));

self.addEventListener('push', (event) => {
  let msg = {};
  try {
    msg = event.data ? event.data.json() : {};
  } catch {
    msg = { body: event.data ? event.data.text() : '' };
  }
  // Always show one: iOS revokes the subscription of a worker that receives
  // a push and shows nothing.
  event.waitUntil(
    self.registration.showNotification(msg.title || 'tenx', {
      body: msg.body || '',
      tag: msg.tag || 'tenx',
      renotify: true,
      icon: '/icon-192.png',
      badge: '/badge-96.png',
      data: { url: msg.url || '/' },
    }),
  );
});

self.addEventListener('notificationclick', (event) => {
  event.notification.close();
  const url = new URL((event.notification.data && event.notification.data.url) || '/', self.location.origin);
  const task = url.searchParams.get('task');
  event.waitUntil(
    (async () => {
      const windows = await self.clients.matchAll({ type: 'window', includeUncontrolled: true });
      const open = windows.find((c) => new URL(c.url).origin === self.location.origin);
      if (open) {
        await open.focus();
        if (task) open.postMessage({ type: 'open-task', task });
        return;
      }
      await self.clients.openWindow(url.href);
    })(),
  );
});
