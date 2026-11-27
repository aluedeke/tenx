// A tabbed task's pages (`view.pages`, tenx_core::pages): web pages listed in
// its tab bar after agent, shell and nvim. The list is the task's, the same on
// every device; which page a device shows is its own. The iOS app shows one in
// a web view of its own with the workspace's logins (PagePane); a browser opens
// it in a tab of its own, since most sites refuse to be shown inside another
// page.
//
// Pure, for the unit tests.

import type { PageView } from '@/protocol';

/** Whether opening `url` is `page`: where it is, or what it was opened with. */
export function isPage(page: PageView, url: string): boolean {
  return page.url === url || page.opened === url;
}

/** The web view's name in the app: page ids are per task. */
export function pageKey(task: string, id: string): string {
  return `${task}#${id}`;
}

/** What a chip says: the page's title, else its host and path. */
export function chipLabel(page: PageView): string {
  if (page.title.trim()) return page.title.trim();
  try {
    const u = new URL(page.url);
    const path = u.pathname === '/' ? '' : u.pathname;
    return u.host + path;
  } catch {
    return page.url;
  }
}

/** What was typed into the bar's address field, as an address: a bare
 * `linear.app/acme` gets `https://`; text with spaces is no address. */
export function typedUrl(text: string): string | null {
  const t = text.trim();
  if (!t || /\s/.test(t)) return null;
  if (/^https?:\/\//i.test(t)) return t;
  if (/^[a-z][a-z0-9+.-]*:/i.test(t) && !/^[^:/]+:\d+(\/|$)/.test(t)) return null;
  const local = /^(localhost|127\.|\[::1\])/i.test(t);
  return `${local ? 'http' : 'https'}://${t}`;
}

/** The workspace of a row id (`ws/slug`): the name the app's settings give
 * the workspace's cookie store. */
export function workspaceOf(task: string): string {
  const i = task.lastIndexOf('/');
  return i < 0 ? task : task.slice(0, i);
}

/** Whether a bookmark fits what is typed in the `+` field: every word in
 * its name or address, case aside. Nothing typed: every bookmark. */
export function matchesBookmark(b: { name: string; url: string }, typed: string): boolean {
  const hay = `${b.name} ${b.url}`.toLowerCase();
  return typed
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean)
    .every((w) => hay.includes(w));
}

/** What Enter in the `+` field opens: an address when what is typed looks
 * like one (a dot, a colon or a slash), else the first bookmark it matches,
 * else what `typedUrl` makes of it. */
export function plusTarget(typed: string, bookmarks: { name: string; url: string }[]): string | null {
  const t = typed.trim();
  if (!t) return null;
  if (!/[.:/]/.test(t)) {
    const hit = bookmarks.find((b) => matchesBookmark(b, t));
    if (hit) return hit.url;
  }
  return typedUrl(t);
}

/** Whether two addresses are the same page: a trailing slash aside, which
 * the app's web view adds (`https://linear.app` → `https://linear.app/`). */
export function sameAddress(a: string, b: string): boolean {
  const bare = (u: string) => u.trim().replace(/\/+$/, '');
  return bare(a) === bare(b);
}
