// Scrolling by touch. The history lives in tmux, not in xterm.js (its own
// scrollback is 0), and tmux scrolls it on the mouse wheel: a wheel over a
// pane enters copy-mode and moves 5 lines per notch, and scrolling back to
// the bottom leaves it (`copy-mode -e`). A program that asked for the mouse
// itself (nvim, less) gets the wheel instead and scrolls on its own. So a
// finger drag becomes wheel notches — SGR mouse reports, the encoding tmux
// asks its clients for — at the cell under the finger, which is how tmux
// knows which pane to scroll.
//
// Pure: the page feeds in finger movement, this says how many notches.

/** Lines tmux moves per wheel notch (its default WheelUpPane binding). */
export const LINES_PER_NOTCH = 5;

/** A drag counts as a scroll once it has moved this far, mostly vertically;
 * shorter is a tap (which places the cursor). */
export const SCROLL_SLOP_PX = 10;

/** One wheel notch at a 0-based cell: an SGR report (`CSI < 64|65 ; x ; y M`),
 * 64 = up (back through the history), 65 = down. */
export function wheelNotch(up: boolean, x: number, y: number): string {
  return `\x1b[<${up ? 64 : 65};${x + 1};${y + 1}M`;
}

/**
 * Finger movement → notches. Dragging down pulls older lines into view, as a
 * touch list does, so it scrolls back (wheel up). Movement that doesn't add
 * up to a notch is carried over, so a slow drag still scrolls.
 */
export class Accumulator {
  private carry = 0;
  private readonly pxPerNotch: number;

  // A plain field, not a parameter property: the unit tests run this file
  // through Node's type stripping, which doesn't rewrite those.
  constructor(pxPerNotch: number) {
    this.pxPerNotch = pxPerNotch;
  }

  /** Add `dy` pixels (positive = finger moved down); the notches to send,
   * positive = up. */
  add(dy: number): number {
    this.carry += dy;
    const notches = Math.trunc(this.carry / this.pxPerNotch);
    this.carry -= notches * this.pxPerNotch;
    return notches;
  }

  reset() {
    this.carry = 0;
  }
}

/** Momentum after a flick: the velocity (px/ms) `dt` ms later. */
export function decay(velocity: number, dt: number): number {
  return velocity * Math.pow(0.95, dt / 16);
}

/** Below this (px/ms) a flick has run out. */
export const FLING_STOP = 0.05;
