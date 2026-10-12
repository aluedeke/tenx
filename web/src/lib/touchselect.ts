// Press and hold, then drag: a text selection by finger. A finger drag sends
// no mouse events, so tmux, which selects on a mouse drag, never saw one; the
// finger's path is sent as the left-button press, drags and release a mouse
// would send, and tmux selects and copies on release (OSC 52). Moving before
// the hold is up is a scroll or a tap instead. Pure: the page feeds in the
// finger and a timer, this says what to send.

import { SCROLL_SLOP_PX, leftButton } from './touchscroll.ts';

/** How long to hold still before a drag selects. */
export const SELECT_HOLD_MS = 400;

export interface Cell {
  x: number;
  y: number;
}

export class SelectGesture {
  /** Where the finger went down, while the hold may still turn into a
   * selection. */
  private start: { px: number; py: number; cell: Cell } | null = null;
  private last: Cell | null = null;
  selecting = false;

  /** A finger down on `cell` (null: off the grid). True: start the hold
   * timer, which calls `hold`. */
  down(px: number, py: number, cell: Cell | null): boolean {
    this.selecting = false;
    this.last = null;
    this.start = cell ? { px, py, cell } : null;
    return !!cell;
  }

  /** The hold timer ran out with the finger still close: the press. */
  hold(): string {
    if (!this.start) return '';
    this.selecting = true;
    this.last = this.start.cell;
    return leftButton('press', this.last.x, this.last.y);
  }

  /** The finger moved to (`px`, `py`), over `cell` (held inside the grid).
   * `cancel`: stop the hold timer (moved too far to be a hold). */
  move(px: number, py: number, cell: Cell): { send: string; cancel: boolean } {
    if (!this.selecting) {
      const far = !!this.start && Math.hypot(px - this.start.px, py - this.start.py) >= SCROLL_SLOP_PX;
      if (far) this.start = null;
      return { send: '', cancel: far };
    }
    if (this.last && cell.x === this.last.x && cell.y === this.last.y) return { send: '', cancel: false };
    this.last = cell;
    return { send: leftButton('drag', cell.x, cell.y), cancel: false };
  }

  /** The finger lifted (or the touch was cancelled) over `cell`. */
  up(cell: Cell | null): string {
    this.start = null;
    if (!this.selecting) return '';
    this.selecting = false;
    const at = cell ?? this.last;
    this.last = null;
    return at ? leftButton('release', at.x, at.y) : '';
  }
}
