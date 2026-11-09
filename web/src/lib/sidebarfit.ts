// Whether the sidebar fits beside the terminal as a panel: the same rule as
// the terminal client's (tenx_core::sidebar::width) — the task keeps at
// least MIN_TASK_COLS columns beside the task list and the sidebar —, in
// pixels. Where it doesn't fit, the sidebar opens over the terminal.
//
// Its width is the user's (dragging its left edge, remembered per browser):
// `dragTo` keeps a drag between SIDEBAR_MIN and what still leaves the task
// its columns, or, over the terminal, DRAWER_MARGIN of the page.

/** The sidebar panel's width until one is dragged (globals.css `.sidebar`). */
export const SIDEBAR_PX = 300;
/** tenx_core::sidebar::MIN_TASK_COLS. */
export const MIN_TASK_COLS = 80;
/** The narrowest sidebar: a PR's title and its check squares still fit. */
export const SIDEBAR_MIN = 240;
/** What a drawer leaves of the page at its left, to see and tap the
 * terminal behind it. */
export const DRAWER_MARGIN = 60;

/** A page `pageWidth` wide, with `columnPx` of it the task list beside the
 * terminal (0 when it isn't), terminal cells `cell` wide and the sidebar
 * `sidebarPx` wide. */
export function sidebarFits(pageWidth: number, cell: number, columnPx: number, sidebarPx = SIDEBAR_PX): boolean {
  return (pageWidth - columnPx - sidebarPx) / cell >= MIN_TASK_COLS;
}

/** Where a drag of the sidebar's edge lands: `width`, and `stop` when the
 * drag went past the widest it may be. `cols` is what the task keeps beside
 * a panel (null for a drawer, which the terminal doesn't make room for). */
export interface Drag {
  width: number;
  stop: boolean;
  cols: number | null;
}

/** The sidebar dragged to `want` px. */
export function dragTo(want: number, drawer: boolean, pageWidth: number, cell: number, columnPx: number): Drag {
  const widest = drawer ? pageWidth - DRAWER_MARGIN : Math.floor(pageWidth - columnPx - MIN_TASK_COLS * cell);
  const max = Math.max(SIDEBAR_MIN, widest);
  const width = Math.round(Math.min(Math.max(want, SIDEBAR_MIN), max));
  const cols = drawer ? null : Math.floor((pageWidth - columnPx - width) / cell);
  return { width, stop: want > max, cols };
}

/** A remembered width, or null for none (or one that isn't a width). */
export function parseWidth(stored: string | null): number | null {
  const n = Number(stored);
  return stored && Number.isFinite(n) && n >= SIDEBAR_MIN ? Math.round(n) : null;
}
