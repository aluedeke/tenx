// Whether the sidebar fits beside the terminal as a panel: the same rule as
// the terminal client's (tenx_core::sidebar::width) — the task keeps at
// least MIN_TASK_COLS columns beside the task list and the sidebar —, in
// pixels. Where it doesn't fit, the sidebar opens over the terminal.
//
// Its width is the user's (dragging its left edge, remembered per browser):
// `dragTo` keeps a drag between SIDEBAR_MIN and what still leaves the task
// its columns, or, over the terminal, DRAWER_MARGIN of the page. The task
// list's right edge is dragged the same way (`columnDrag`), in whole
// columns, within the server's rule for a configured width
// (tenx_core::column::width): at least MIN_COLS, at most half the page.

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

/** Where a drag of an edge lands: `width`, and `stop` when the drag went
 * past the widest it may be. `cols`: for the sidebar, what the task keeps
 * beside a panel (null for a drawer, which the terminal doesn't make room
 * for); for the task list, its own columns. */
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

/** tenx_core::column::MIN_COLS: the narrowest task list. */
export const COLUMN_MIN_COLS = 30;
/** The task list's padding beside its `cols` cells. */
export const COLUMN_PAD = 20;

/** The task list's width in px for `cols` cells `cell` wide. */
export function columnPx(cols: number, cell: number): number {
  return Math.ceil(cols * cell + COLUMN_PAD);
}

/** The widest task list on a page `pageWidth` wide: half of it. */
export function columnMaxCols(pageWidth: number, cell: number): number {
  return Math.max(COLUMN_MIN_COLS, Math.floor(pageWidth / cell / 2));
}

/** The task list dragged to `want` px: whole columns (`cols`), `stop` past
 * the widest. */
export function columnDrag(want: number, pageWidth: number, cell: number): Drag {
  const asked = Math.round((want - COLUMN_PAD) / cell);
  const max = columnMaxCols(pageWidth, cell);
  const cols = Math.min(Math.max(asked, COLUMN_MIN_COLS), max);
  return { width: columnPx(cols, cell), stop: asked > max, cols };
}

/** A remembered task list width in columns, or null for automatic. */
export function parseColumnCols(stored: string | null): number | null {
  const n = Number(stored);
  return stored && Number.isInteger(n) && n >= COLUMN_MIN_COLS ? n : null;
}
