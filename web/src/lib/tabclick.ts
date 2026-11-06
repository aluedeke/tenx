// A tabbed task window's tab strip is the top line of its pane, drawn by
// tmux (`tenx_core::tabs::strip_format`). tmux sends a click there to no key
// binding, so the page turns a click on a tab into the key that shows it —
// Alt+1/2/3, which tmux does bind. The TUI client does the same in Rust
// (`tenx_core::tabs::tab_at`); the labels here must match `Tab::label`.
//
// Pure: the page feeds in what xterm sends and the strip's line.

/** What each tab shows on the strip, spaces included, in key order. */
const TABS = [' 1 agent ', ' 2 shell ', ' 3 nvim '];

/** One SGR mouse report: `CSI < b ; x ; y M|m`. */
const SGR = /^\x1b\[<(\d+);(\d+);(\d+)([Mm])$/;

/** The tab (1-based) under 0-based column `x` of the strip's `line`. */
export function tabAt(line: string, x: number): number | null {
  const chars = Array.from(line);
  for (let t = 0; t < TABS.length; t++) {
    const text = Array.from(TABS[t]);
    for (let i = 0; i + text.length <= chars.length; i++) {
      if (x >= i && x < i + text.length && text.every((c, j) => chars[i + j] === c)) return t + 1;
    }
  }
  return null;
}

export class TabClicks {
  /** A press went to a tab: its release is dropped too. */
  private pressed = false;

  /** What goes to the terminal for `data`: a left press on a tab of the top
   * line (`topLine`) becomes that tab's Alt key, its release nothing;
   * anything else as it is. */
  translate(data: string, topLine: () => string): string {
    const m = SGR.exec(data);
    if (!m) return data;
    const [b, x, y, kind] = [Number(m[1]), Number(m[2]), Number(m[3]), m[4]];
    if (kind === 'm') {
      if (this.pressed && (b & 3) === 0) {
        this.pressed = false;
        return '';
      }
      return data;
    }
    if (b !== 0 || y !== 1) return data;
    const tab = tabAt(topLine(), x - 1);
    if (tab === null) return data;
    this.pressed = true;
    return `\x1b${tab}`;
  }
}
