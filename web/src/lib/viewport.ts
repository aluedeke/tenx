// Where the page ends while a keyboard covers part of it. A phone's keyboard
// covers the page rather than shrinking it (iOS ignores
// `interactive-widget`), so the page pins itself to the visual viewport —
// but only when that viewport is a keyboard's doing. Pure, so the rules test
// without a browser.

export interface Measure {
  /** A text field has the focus: no keyboard comes up without one. */
  editing: boolean;
  /** `visualViewport.height` / `.offsetTop`. */
  height: number;
  top: number;
  /** `window.innerHeight`. */
  innerHeight: number;
  /** The tallest the viewport has been at this width: the height with no
   * keyboard. */
  fullHeight: number;
  /** Keep the room of a small cover after it has gone (an iPad, whose key
   * bar is always there): `reserve` is that room so far, at this width. */
  keep: boolean;
  reserve: number;
}

export interface Layout {
  /** An on-screen keyboard is up (the key bar sits on it). */
  keyboard: boolean;
  /** The page's height and top. */
  height: number;
  top: number;
  /** Room to keep under the key bar: the part of `reserve` nothing covers
   * now. */
  gap: number;
  /** `reserve` for the next measure. */
  reserve: number;
}

/** Less than this off the full height is no on-screen keyboard. */
export const KEYBOARD_MIN_PX = 120;

export function layout(m: Measure): Layout {
  // iPadOS hands out a sliver while switching apps, for the switcher's
  // snapshot: never a keyboard, which covers at most two thirds.
  const plausible = m.editing && m.height >= m.innerHeight * 0.35;
  const covered = m.fullHeight - m.height;
  const keyboard = plausible && covered > KEYBOARD_MIN_PX;
  // Smaller than a keyboard but still covering the page: the shortcut bar
  // iPadOS shows over a hardware keyboard. The page ends above it too, so
  // the key bar isn't under it.
  const small = plausible && covered > 1 && !keyboard;
  // It comes and goes with the focus. Keeping its room while it's gone
  // keeps the terminal one height, instead of resizing (and tmux redrawing
  // every line) on every tap.
  const reserve = m.keep && small ? Math.max(m.reserve, covered) : m.reserve;
  const gap = m.keep && !keyboard ? Math.max(0, reserve - (small ? covered : 0)) : 0;
  return plausible && covered > 1
    ? { keyboard, height: m.height, top: m.top, gap, reserve }
    : { keyboard: false, height: m.innerHeight, top: 0, gap, reserve };
}

/** Whether the page keeps the room of iPadOS's shortcut bar (`Measure.keep`):
 * on an iPad's touch screen, where the key bar is always there to hold it —
 * but not in an app (the Home Screen app, the tenx iOS app, which hides
 * that bar), which needs no such room. */
export function keepsShortcutRoom(s: { touch: boolean; ipad: boolean; app: boolean }): boolean {
  return s.touch && s.ipad && !s.app;
}

/** The page runs inside the tenx iOS app (mobile/ios): a WKWebView with the
 * app's `tenx` message handler. */
export function inIosApp(w: { webkit?: { messageHandlers?: { tenx?: unknown } } }): boolean {
  return !!w.webkit?.messageHandlers?.tenx;
}

/** The key bar (Esc, Ctrl, arrows, paste, 📎) on a touch screen: over the
 * on-screen keyboard; on an iPad always, since a hardware keyboard never
 * raises an on-screen one, and a bar that came and went with the focus
 * resized the terminal on every tap; and while recording, since the
 * microphone's stop button is on it. */
export function keyBarShown(s: { touch: boolean; ipad: boolean; keyboard: boolean; recording: boolean }): boolean {
  return s.touch && (s.ipad || s.keyboard || s.recording);
}
