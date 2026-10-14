// What the page was like when a clipboard call ran, for the server's log:
// iOS refuses the clipboard (NotAllowedError) on some page loads and not on
// others, and only the device can see why. Never the clipboard's content.

/** The page around a clipboard call. */
export interface PageState {
  /** The document has the focus (`document.hasFocus()`). */
  focused: boolean;
  visible: boolean;
  /** Opened from the Home Screen, not in a Safari tab. */
  standalone: boolean;
  /** The tap still counts as one (`navigator.userActivation`), if known. */
  activation?: boolean;
  /** Since the page loaded. */
  ageMs: number;
  /** Since the page last came back to the screen, if it ever left. */
  shownMs?: number;
}

let shownAt: number | undefined;

/** Starts noting when the page comes back to the screen (after the app
 * switcher, a lock, a sleep). */
export function trackShown(): void {
  if (typeof document === 'undefined') return;
  const mark = () => {
    if (document.visibilityState === 'visible') shownAt = performance.now();
  };
  document.addEventListener('visibilitychange', mark);
  window.addEventListener('pageshow', (ev) => {
    if (ev.persisted) mark();
  });
}

/** The page as it is now. */
export function pageState(): PageState {
  const activation = (navigator as { userActivation?: { isActive: boolean } }).userActivation?.isActive;
  const now = performance.now();
  return {
    focused: document.hasFocus(),
    visible: document.visibilityState === 'visible',
    standalone:
      matchMedia('(display-mode: standalone)').matches || (navigator as { standalone?: boolean }).standalone === true,
    activation,
    ageMs: now,
    shownMs: shownAt === undefined ? undefined : now - shownAt,
  };
}

/** One line for the log: `focus=yes visible=yes app=safari activation=yes age=12s shown=3s`. */
export function describe(s: PageState): string {
  const yes = (b: boolean) => (b ? 'yes' : 'no');
  const parts = [
    `focus=${yes(s.focused)}`,
    `visible=${yes(s.visible)}`,
    `app=${s.standalone ? 'home-screen' : 'safari'}`,
    `activation=${s.activation === undefined ? '?' : yes(s.activation)}`,
    `age=${seconds(s.ageMs)}`,
  ];
  if (s.shownMs !== undefined) parts.push(`shown=${seconds(s.shownMs)}`);
  return parts.join(' ');
}

function seconds(ms: number): string {
  return ms < 10_000 ? `${(ms / 1000).toFixed(1)}s` : `${Math.round(ms / 1000)}s`;
}

/** A failed clipboard call for the log: the error, how long after the tap
 * (`since`, a `performance.now()`) it came, and the page then. */
export function failure(e: unknown, since: number): string {
  const error = e instanceof Error ? `${e.name}: ${e.message}` : String(e);
  return `${error} (after ${Math.round(performance.now() - since)} ms; ${describe(pageState())})`;
}
