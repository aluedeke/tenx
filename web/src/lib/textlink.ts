// The link under a finger: press and hold on a link in the terminal opens a
// menu to copy or open it (a phone has no mouse to hover the link with, and
// a long URL is often for pasting somewhere else). The page hands in the
// screen's cells; this finds the URL that covers the held one. A URL longer
// than the line goes on in the next row — wrapped by the terminal, or by the
// agent, which starts the rest at its indent — so a row that ends in its last
// cell is read on into the next one when that one goes on with URL text.
// Pure, for the unit tests.

import type { Cell } from './touchselect.ts';

/** A web or Obsidian link in a line: the web-links add-on's pattern with
 * both schemes (lib/links `OBSIDIAN_TEXT_URL`). */
const TEXT_URL = /(?:https?|obsidian):[/]{2}[^\s"'!*(){}|\\^<>`]*[^\s"':,.!?{}|\\^~[\]`()<>]/gi;

/** Text a URL can go on with in the next row: not a border, not a bullet. */
const URL_GOES_ON = /^[\w/%?#&=.~:+@-]/;

/** The URL that covers `at`, or null. `rows` are the screen's cells, row by
 * row: each cell's text, '' for the right half of a wide character. */
export function urlAt(rows: string[][], at: Cell): string | null {
  const full = (y: number) => {
    const row = rows[y] ?? [];
    const last = row[row.length - 1] ?? '';
    return last.trim() !== '';
  };
  const goesOn = (y: number) => y + 1 < rows.length && full(y) && URL_GOES_ON.test(rowText(rows[y + 1]).trimStart());
  // The rows that read as one line with the held one.
  let top = at.y;
  while (top > 0 && goesOn(top - 1)) top--;
  let bottom = at.y;
  while (goesOn(bottom)) bottom++;

  let text = '';
  const cells: Cell[] = [];
  for (let y = top; y <= bottom; y++) {
    const row = rows[y] ?? [];
    let x = 0;
    if (y > top) while (x < row.length && (row[x] ?? '').trim() === '') x++;
    for (; x < row.length; x++) {
      const ch = row[x] ?? '';
      if (ch === '') continue;
      for (let i = 0; i < ch.length; i++) cells.push({ x, y });
      text += ch;
    }
  }
  for (const m of text.matchAll(TEXT_URL)) {
    const start = m.index ?? 0;
    for (let i = start; i < start + m[0].length; i++) {
      const c = cells[i];
      if (c && c.y === at.y && c.x === at.x) return m[0];
    }
  }
  return null;
}

function rowText(row: string[] | undefined): string {
  return (row ?? []).join('');
}
