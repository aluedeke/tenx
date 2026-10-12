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
}

export interface Layout {
  /** An on-screen keyboard is up (the key bar sits on it). */
  keyboard: boolean;
  /** The page's height and top. */
  height: number;
  top: number;
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
  return plausible && covered > 1
    ? { keyboard, height: m.height, top: m.top }
    : { keyboard: false, height: m.innerHeight, top: 0 };
}

/** The key bar (Esc, Ctrl, arrows, paste, 📎) on a touch screen: over the
 * on-screen keyboard; on an iPad whenever a text field (the terminal) has
 * the focus, since a hardware keyboard never raises an on-screen one; and
 * while recording, since the microphone's stop button is on it. */
export function keyBarShown(s: { touch: boolean; ipad: boolean; typing: boolean; keyboard: boolean; recording: boolean }): boolean {
  return s.touch && ((s.ipad && s.typing) || s.keyboard || s.recording);
}
