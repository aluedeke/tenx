// Opening a link from the terminal outside the app: a new window with no
// `opener`, which a browser tab, or an app installed on Android or a desktop,
// opens in the default browser. An app installed to an iOS Home Screen can't
// be made to: iOS shows every link in its in-app browser sheet (whose Safari
// button hands the page over). The `x-safari-https://…` scheme native apps
// use for that is rejected as an invalid address when a web app follows it,
// so it isn't used.
//
// Pure (`isWebUrl`, `linkHref`, `linkKey`), for the unit tests.

import type { ColumnView, LinkView } from '@/protocol';

/** Only web links leave the app; anything else (javascript:, data:, a
 * file path) is not followed at all. */
export function isWebUrl(url: string): boolean {
  return /^https?:\/\//i.test(url);
}

/** Open `url` outside the app, as far as the platform allows. */
export function openExternal(url: string): void {
  if (!isWebUrl(url)) return;
  const w = window.open(url, '_blank', 'noopener,noreferrer');
  if (w) w.opener = null;
}

// ── A task's links (`L`, `f`, the ↗ button, the chips) ───────────────────


/** Where `link` is from this page. A port's link names the machine tenx runs
 * on as `localhost`; from a phone that is the phone, so it gets the page's
 * own host instead (the machine `tenx web` serves from). */
export function linkHref(link: LinkView, hostname: string): string {
  if (link.kind !== 'port') return link.url;
  const host = hostname.includes(':') ? `[${hostname}]` : hostname;
  return link.url.replace(/^http:\/\/(localhost|127\.0\.0\.1)(?=[:/]|$)/, `http://${host}`);
}

export type LinkPick = { do: 'open' | 'copy'; link: LinkView };

/** A key in the column's `L` or `f` mode that names a link — the page acts
 * on it itself, inside the key press, since a browser only lets a page open
 * a window then. Everything else (moving, Escape) goes to the server. */
export function linkKey(view: ColumnView, key: string, mod: boolean): LinkPick | null {
  if (mod) return null;
  const m = view.mode;
  if (m.kind === 'links') {
    const at = (i: number) => m.links[i];
    let link: LinkView | undefined;
    let act: LinkPick['do'] = 'open';
    if (key === 'Enter' || key === 'o' || key === 'l') link = at(m.selected);
    else if (key === 'y') {
      link = at(m.selected);
      act = 'copy';
    } else if (/^[1-9]$/.test(key)) link = at(Number(key) - 1);
    return link ? { do: act, link } : null;
  }
  if (m.kind === 'hints' && /^[a-zA-Z]$/.test(key)) {
    const want = key.toLowerCase();
    for (const item of view.items) {
      if (item.kind !== 'task') continue;
      const chip = item.chips.find((c) => c.hint === want && c.link);
      if (chip?.link) return { do: key === want ? 'open' : 'copy', link: chip.link };
    }
  }
  return null;
}

/** Put `text` on this device's clipboard; false when the browser refused. */
export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}
