// Opening a link from the terminal in the system browser, not inside the app.
//
// An app installed to the iOS Home Screen opens every link in an in-app
// browser sheet, whatever `target` says. The `x-safari-` scheme prefix
// (`x-safari-https://…`) is how an iOS app hands a URL to Safari itself; it
// isn't documented by Apple but is what iOS apps rely on. Everywhere else —
// a browser tab, or an installed app on Android or a desktop — a new window
// with no `opener` already lands in the default browser.
//
// Pure (`browserUrl`), for the unit tests.

/** Only web links leave the app; anything else (javascript:, data:, a
 * file path) is not followed at all. */
export function isWebUrl(url: string): boolean {
  return /^https?:\/\//i.test(url);
}

/** What to open for `url`: in an installed iOS app, the Safari hand-off. */
export function browserUrl(url: string, iosStandalone: boolean): string {
  return iosStandalone ? `x-safari-${url}` : url;
}

/** The page runs as an app installed to an iPhone or iPad Home Screen. */
export function isIosStandalone(): boolean {
  const nav = navigator as Navigator & { standalone?: boolean };
  const ios = /iPad|iPhone|iPod/.test(navigator.userAgent) || (navigator.platform === 'MacIntel' && navigator.maxTouchPoints > 1);
  return ios && (nav.standalone === true || window.matchMedia('(display-mode: standalone)').matches);
}

/** Open `url` in the system browser. */
export function openExternal(url: string): void {
  if (!isWebUrl(url)) return;
  const target = browserUrl(url, isIosStandalone());
  if (target !== url) {
    // A navigation, not a new window: the scheme leaves the app to Safari
    // and this page stays where it is.
    location.href = target;
    return;
  }
  const w = window.open(url, '_blank', 'noopener,noreferrer');
  if (w) w.opener = null;
}
