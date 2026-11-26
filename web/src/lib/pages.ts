// A tabbed task's pages (`view.pages`, tenx_core::pages): web pages listed in
// its tab bar after agent, shell and nvim. The list is the task's, the same on
// every device; which page a device shows is its own. The iOS app shows one in
// a web view of its own with the workspace's logins (PagePane); a browser opens
// it in a tab of its own, since most sites refuse to be shown inside another
// page.
//
// Pure, for the unit tests.

import type { PageView } from '@/protocol';

/** The web view's name in the app: page ids are per task. */
export function pageKey(task: string, id: string): string {
  return `${task}#${id}`;
}

/** A `localhost` address: the Mac's own, which a phone can't reach. */
export function isLocalUrl(url: string): boolean {
  try {
    const host = new URL(url).hostname.replace(/^\[|\]$/g, '');
    return host === 'localhost' || host.endsWith('.localhost') || host === '::1' || /^127\./.test(host);
  } catch {
    return false;
  }
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
