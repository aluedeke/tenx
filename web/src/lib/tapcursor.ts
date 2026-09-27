// Tap to place the cursor. A tap on a phone reaches tmux as a mouse click,
// and a line editor that doesn't ask for mouse events — Claude Code's input,
// a shell prompt — never sees it. So the page does what mobile terminal apps
// do: it works out which cell was tapped and sends the arrow keys that walk
// the cursor there.
//
// Arrow keys are only safe where they move a cursor within text you're
// editing. ← → on the cursor's own line are (a shell or an editor moves along
// it). ↑ ↓ are not in general — at the top of Claude's input ↑ recalls
// history — so they are only used inside a box drawn around the cursor
// (Claude's input rules), between its top and bottom edges.
//
// Pure: the screen comes in as text lines, so it tests without a terminal.

export interface Cell {
  x: number;
  y: number;
}

/** What a tap should do: move the cursor to `target`, `vertical` saying
 * whether rows may change (inside an input box) or only the column. */
export interface Plan {
  target: Cell;
  vertical: boolean;
}

/** tmux's pane border. */
const PANE_BORDER = '│';

/** A row that is an edge of an input box: mostly horizontal rule. */
function isRule(text: string): boolean {
  const t = text.trim();
  if (t.length < 8) return false;
  let rule = 0;
  for (const ch of t) if ('─━═╭╮╰╯┌┐└┘'.includes(ch)) rule++;
  return rule / [...t].length >= 0.8;
}

/** The columns of the pane the cursor is in: between the tmux borders on
 * either side of it on its row, [left, right). */
export function paneColumns(row: string, x: number): [number, number] {
  const chars = [...row];
  let left = 0;
  for (let i = Math.min(x, chars.length) - 1; i >= 0; i--) {
    if (chars[i] === PANE_BORDER) {
      left = i + 1;
      break;
    }
  }
  let right = chars.length;
  for (let i = x; i < chars.length; i++) {
    if (chars[i] === PANE_BORDER) {
      right = i;
      break;
    }
  }
  return [left, Math.max(right, x + 1)];
}

/** The rows strictly inside the rules above and below the cursor, within the
 * pane's columns — Claude's input box — or null when there is none. */
export function inputBox(rows: string[], cursor: Cell, cols: [number, number]): [number, number] | null {
  const slice = (y: number) => [...(rows[y] ?? '')].slice(cols[0], cols[1]).join('');
  let top = -1;
  for (let y = cursor.y - 1; y >= 0; y--) {
    if (isRule(slice(y))) {
      top = y;
      break;
    }
  }
  let bottom = -1;
  for (let y = cursor.y + 1; y < rows.length; y++) {
    if (isRule(slice(y))) {
      bottom = y;
      break;
    }
  }
  return top >= 0 && bottom >= 0 ? [top + 1, bottom - 1] : null;
}

/** Where a tap at `tap` should move the cursor, or null when it is outside
 * anything the cursor can safely walk to (another pane, the output above
 * Claude's input, a different row with no box around it). */
export function planTap(rows: string[], cursor: Cell, tap: Cell): Plan | null {
  const cols = paneColumns(rows[cursor.y] ?? '', cursor.x);
  if (tap.x < cols[0] || tap.x >= cols[1]) return null;
  if (tap.y === cursor.y) return tap.x === cursor.x ? null : { target: tap, vertical: false };
  const box = inputBox(rows, cursor, cols);
  if (!box || tap.y < box[0] || tap.y > box[1]) return null;
  return { target: tap, vertical: true };
}

/** The bytes of one arrow key, honouring DECCKM (application cursor keys). */
export function arrow(dir: 'up' | 'down' | 'left' | 'right', application: boolean): string {
  const c = { up: 'A', down: 'B', right: 'C', left: 'D' }[dir];
  return (application ? '\x1bO' : '\x1b[') + c;
}
