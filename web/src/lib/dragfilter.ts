// A click on iPad Safari reaches xterm as a press, a drag that stays in the
// pressed cell, then the release — the pointer reports a move without
// moving. tmux takes any drag in a pane that doesn't ask for the mouse (a
// shell) as the start of a selection: the pane enters copy-mode (the cursor
// jumps), and the release copies what is selected — nothing — so tmux sends
// an empty OSC 52, which wipes the browser's clipboard. A drag only means
// something once it leaves the pressed cell, so until then it is dropped.
//
// Pure: the page feeds in what xterm sends, this says whether it goes on.

/** One SGR mouse report: `CSI < b ; x ; y M|m`. */
const SGR = /^\x1b\[<(\d+);(\d+);(\d+)([Mm])$/;

export class DragFilter {
  /** The cell of the button held down, until the pointer leaves it. */
  private pressed: string | null = null;

  /** Whether `data` (what xterm produced) goes on to the terminal. */
  pass(data: string): boolean {
    const m = SGR.exec(data);
    if (!m) return true;
    const b = Number(m[1]);
    const cell = `${m[2]};${m[3]}`;
    if (m[4] === 'm' || b >= 64) {
      // A release, or the wheel.
      if (m[4] === 'm') this.pressed = null;
      return true;
    }
    if (!(b & 32)) {
      this.pressed = cell;
      return true;
    }
    // Motion: a drag (a button held) still in the pressed cell is no drag.
    if ((b & 3) === 3 || this.pressed === null) return true;
    if (cell === this.pressed) return false;
    this.pressed = null;
    return true;
  }
}
