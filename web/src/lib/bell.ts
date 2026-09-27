// The terminal bell (`\a`, which tmux passes on for a window that rang):
// the tab's title and favicon blink for a moment, so a bell in a background
// tab is still seen.

const ALERT_ICON =
  'data:image/svg+xml,' +
  encodeURIComponent(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64" rx="14" fill="#171820"/><circle cx="32" cy="32" r="14" fill="#e4a854"/></svg>',
  );

let timer: ReturnType<typeof setInterval> | null = null;

export function bell() {
  if (timer) return;
  const title = document.title;
  const link = document.querySelector<HTMLLinkElement>('link[rel="icon"]');
  const icon = link?.href ?? '';
  let on = false;
  let flips = 0;
  timer = setInterval(() => {
    on = !on;
    document.title = on ? `● ${title}` : title;
    if (link) link.href = on ? ALERT_ICON : icon;
    flips += 1;
    // Keep blinking while the tab is in the background; stop a few blinks
    // after it is looked at.
    if (flips >= 6 && document.visibilityState === 'visible' && !on) {
      clearInterval(timer!);
      timer = null;
      document.title = title;
      if (link) link.href = icon;
    }
  }, 400);
}
