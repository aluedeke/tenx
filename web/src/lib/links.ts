// Opening a link from the terminal outside the app: a new window with no
// `opener`, which a browser tab, or an app installed on Android or a desktop,
// opens in the default browser. An app installed to an iOS Home Screen can't
// be made to: iOS shows every link in its in-app browser sheet (whose Safari
// button hands the page over). The `x-safari-https://…` scheme native apps
// use for that is rejected as an invalid address when a web app follows it,
// so it isn't used.
//
// Pure (`isWebUrl`), for the unit tests.

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
