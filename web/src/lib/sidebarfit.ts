// Whether the sidebar fits beside the terminal as a panel: the same rule as
// the terminal client's (tenx_core::sidebar::width) — the task keeps at
// least MIN_TASK_COLS columns beside the task list and the sidebar —, in
// pixels. Where it doesn't fit, the sidebar opens over the terminal.

/** The sidebar panel's width (globals.css `.sidebar`). */
export const SIDEBAR_PX = 300;
/** tenx_core::sidebar::MIN_TASK_COLS. */
export const MIN_TASK_COLS = 80;

/** A page `pageWidth` wide, with `columnPx` of it the task list beside the
 * terminal (0 when it isn't) and terminal cells `cell` wide. */
export function sidebarFits(pageWidth: number, cell: number, columnPx: number): boolean {
  return (pageWidth - columnPx - SIDEBAR_PX) / cell >= MIN_TASK_COLS;
}
