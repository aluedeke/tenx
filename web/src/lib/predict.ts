// Predicting the column's answer, so it moves at the speed of the finger
// rather than of the round trip (the server owns the column; on a relayed
// connection an answer can take a second).
//
// The page numbers every input it sends (`seq`); each view says the highest
// one it reflects (`ack`). What's drawn is the last view with the inputs it
// doesn't reflect yet replayed on top — only the few whose effect is certain
// from the view alone: the cursor moving through the list (↓ ↑ j k, ^j ^k,
// a click on a row) and with it what the task area shows (the "current"
// marker, a closed task's empty screen), the search field taking or losing
// the cursor, the filter as it's typed — on the Tasks tab the rows too:
// narrowed from the rows in hand, widened from the last unfiltered list the
// page kept —, opening the task under the cursor (⏎, o, l, the open action),
// and the help overlay. A tab click switches the highlighted tab and marks
// the list as loading. Everything else (actions, forms, the Repos tab's
// filtered rows) waits for the server; an action on a row marks that row
// busy meanwhile. The first input that
// can't be predicted stops the replay, so nothing after it is guessed on a
// state that may be wrong. When the real view arrives it replaces all of it.
//
// Mirrors src/tui/column.rs `step_down`/`step_up`/`handle_insert_key`/
// `handle_normal_key`/`jump`/`handle_help_key` and `is_shown`, view.rs
// `task_items` and tenx-core `column::filter_matches`; pure, for the unit
// tests.

import type { ClientMessage, ColumnView, Item } from '../protocol.ts';

export interface Pending {
  seq: number;
  msg: ClientMessage;
}

export interface Predicted {
  view: ColumnView;
  /** A tab switch is on its way: the rows shown are the old tab's. */
  loading: boolean;
  /** The task in the session's current window, when known: what the task
   * area shows once the cursor leaves a closed task. `undefined`: unknown
   * (the server's view showed a closed task's screen). */
  window?: string | null;
  /** The `seq` of a predicted open: the page hands the keyboard to the
   * terminal (what the server's `focus_terminal` would do). */
  opened?: number;
  /** A closed task being opened: its window and agent are starting. */
  opening?: string | null;
  /** The row an unanswered action acts on, and the action. */
  busy?: { id: string; action: string } | null;
  /** The filter the rows shown are filtered by; `null`: not known (the
   * rows are the server's for an older filter). */
  rowsFor?: string | null;
}

/** What the replay filters rows from: the last unfiltered Tasks list the
 * page got (for a filter that widens), and what the rows in hand reflect. */
export interface Rows {
  full: Item[] | null;
  rowsFor: string | null;
}

/** tenx-core `column::filter_matches`: the needle's characters, spaces
 * skipped, in order in the haystack, case ignored. */
export function filterMatches(needle: string, haystack: string): boolean {
  const hay = Array.from(haystack.toLowerCase());
  let at = 0;
  for (const nc of Array.from(needle.toLowerCase())) {
    if (nc === ' ') continue;
    while (at < hay.length && hay[at] !== nc) at++;
    if (at === hay.length) return false;
    at++;
  }
  return true;
}

/** The Tasks list filtered by `needle`, as `task_items` draws it: a task
 * matches on "<workspace> <title>" (`Row::matches`) and keeps its subagent
 * lines; a section's header shows while any of its tasks does, counting
 * them. */
export function filterTasks(items: Item[], needle: string): Item[] {
  const out: Item[] = [];
  let header: Extract<Item, { kind: 'header' }> | null = null;
  let group: Item[] = [];
  let count = 0;
  let keep = false;
  const flush = () => {
    if (count > 0) {
      if (header) out.push({ ...header, count });
      out.push(...group);
    }
    group = [];
    count = 0;
  };
  for (const item of items) {
    if (item.kind === 'header') {
      flush();
      header = item;
    } else if (item.kind === 'task') {
      keep = needle === '' || filterMatches(needle, `${item.ws} ${item.title}`);
      if (keep) {
        group.push(item);
        count++;
      }
    } else if (item.kind === 'sub') {
      if (keep) group.push(item);
    }
  }
  flush();
  return out.length ? out : [{ kind: 'empty', lines: ['no tasks yet — :n to create one'] }];
}

/** `view` with `filter` typed, its Tasks rows filtered when the rows to
 * filter are known: the ones in hand when the filter only grew (a match
 * for the longer filter matches the shorter), else the kept full list. */
function refilter(view: ColumnView, filter: string, rows: Rows, onTasks: boolean): Predicted {
  const source =
    !onTasks ? null
    : rows.rowsFor !== null && filter.startsWith(rows.rowsFor) ? view.items
    : rows.full;
  if (!source) return { view: { ...view, filter }, loading: false, rowsFor: null };
  return { view: { ...view, filter, items: filterTasks(source, filter) }, loading: false, rowsFor: filter };
}

/** The inputs a view with `ack` doesn't reflect yet. */
export function unacked(pending: Pending[], ack: number): Pending[] {
  return pending.filter((p) => p.seq > ack);
}

function selectable(view: ColumnView, item: Item): boolean {
  const tab = view.tabs.find((t) => t.active)?.label ?? 'Tasks';
  return tab === 'Tasks' ? item.kind === 'task' || item.kind === 'sub' : item.kind === 'repo' || item.kind === 'job';
}

function isSelected(item: Item): boolean {
  return (item.kind === 'task' || item.kind === 'sub' || item.kind === 'repo' || item.kind === 'job') && item.selected;
}

/** The view with the cursor on item `at` (an index into `items`), or on
 * nothing (`at` = -1), and focus as given. */
function withCursor(view: ColumnView, at: number, focus: 'search' | 'list'): ColumnView {
  const items = view.items.map((item, i) => {
    if (!(item.kind === 'task' || item.kind === 'sub' || item.kind === 'repo' || item.kind === 'job')) return item;
    const on = i === at;
    return item.selected === on ? item : { ...item, selected: on };
  });
  const footer =
    view.footer.kind === 'hint' && view.footer.tag
      ? { ...view.footer, tag: focus === 'search' ? ('INSERT' as const) : ('NORMAL' as const) }
      : view.footer;
  return { ...view, items, focus, footer };
}

type TaskOf = Extract<Item, { kind: 'task' }>;

/** The task the cursor is on (a subagent line's task too), when the list
 * has the cursor on the Tasks tab. */
function cursorTask(view: ColumnView): TaskOf | null {
  if (view.focus !== 'list' || (view.tabs.find((t) => t.active)?.label ?? 'Tasks') !== 'Tasks') return null;
  const on = view.items.find((i) => (i.kind === 'task' || i.kind === 'sub') && i.selected);
  if (!on) return null;
  const id = on.kind === 'sub' ? on.task : (on as TaskOf).id;
  return (view.items.find((i) => i.kind === 'task' && i.id === id) as TaskOf | undefined) ?? null;
}

/** What the task area shows after the cursor moved (`Column::is_shown`):
 * a closed task under the cursor, else the current window — which a move
 * onto an open task switches to (`follow_selection`, when `follow`). */
function show(p: Predicted, follow: boolean): Predicted {
  const { view } = p;
  const on = cursorTask(view);
  let window = p.window;
  if (on && !on.closed && follow) window = on.id;
  const closed = on && on.closed ? on : null;
  const current = closed ? closed.id : (window ?? null);
  const items = view.items.map((it) => {
    if (it.kind !== 'task') return it;
    const cur = it.id === current;
    return it.current === cur ? it : { ...it, current: cur };
  });
  const shown_closed = closed ? { id: closed.id, title: closed.title, ws: closed.ws, ws_color: closed.ws_color } : null;
  return { ...p, window, view: { ...view, items, current, shown_closed } };
}

/** ⏎ on the task under the cursor (`Column::jump`): its window comes up (made
 * first if it was closed), the keyboard goes to it, and the column is left
 * with the search field focused and the filter cleared. */
function open(view: ColumnView, rows: Rows): Predicted | null {
  const on = cursorTask(view);
  if (!on || on.pending) return null;
  // A subagent line opens its view, not the task.
  if (view.items.some((i) => i.kind === 'sub' && i.selected)) return null;
  // The filter is cleared: the whole list comes back.
  let all = view;
  if (view.filter !== '') {
    if (!rows.full) return null;
    all = { ...view, filter: '', items: rows.full };
  }
  const p = show({ view: withCursor(all, -1, 'search'), loading: false, window: on.id }, false);
  return { ...p, opening: on.closed ? on.id : null, rowsFor: '' };
}

/** A key in the help overlay (`handle_help_key`): it scrolls like a pager,
 * anything else closes it. */
function helpKey(view: ColumnView, key: string, ctrl: boolean): Predicted {
  const scroll = view.mode.kind === 'help' ? view.mode.scroll : 0;
  const to = (n: number) => ({ view: { ...view, mode: { kind: 'help' as const, scroll: Math.max(0, Math.min(n, 65535)) } }, loading: false });
  if (key === 'j' || key === 'ArrowDown') return to(scroll + 1);
  if (key === 'k' || key === 'ArrowUp') return to(scroll - 1);
  if (ctrl && key === 'd') return to(scroll + 10);
  if (ctrl && key === 'u') return to(scroll - 10);
  if (key === 'PageDown' || key === ' ') return to(scroll + 10);
  if (key === 'PageUp') return to(scroll - 10);
  if (key === 'g' || key === 'Home') return to(0);
  if (key === 'G' || key === 'End') return to(65535);
  return { view: { ...view, mode: { kind: 'list' } }, loading: false };
}

/** Actions and keys whose effect needs the server, on the row under the
 * cursor: drawn busy until the answer. */
const ROW_ACTIONS = new Set(['approve', 'deny', 'delete', 'close', 'unlock', 'rename', 'edit_repos', 'transcript', 'reject']);
const ROW_KEYS: Record<string, string> = { A: 'approve', D: 'deny', x: 'close', u: 'unlock', r: 'rename', e: 'edit_repos', t: 'transcript', d: 'delete' };

function busyOn(view: ColumnView, msg: ClientMessage): { id: string; action: string } | null {
  if (view.mode.kind !== 'list') return null;
  const action =
    msg.type === 'action' ? (ROW_ACTIONS.has(msg.name) ? msg.name : null)
    : msg.type === 'key' && !msg.ctrl && !msg.alt && view.focus === 'list' ? (ROW_KEYS[msg.key] ?? null)
    : null;
  if (!action) return null;
  // An action focuses the list first (`handle_action`); the cursor row is
  // the one it acts on.
  const on = cursorTask({ ...view, focus: 'list' });
  return on ? { id: on.id, action } : null;
}

/** One input applied to `view`, or null when its effect can't be known. */
export function applyOne(view: ColumnView, msg: ClientMessage, rows: Rows = { full: null, rowsFor: view.filter }): Predicted | null {
  if (view.mode.kind === 'help' && msg.type === 'key' && !msg.alt) return helpKey(view, msg.key, msg.ctrl);
  if (view.mode.kind !== 'list') return null;
  const order = view.items.map((it, i) => (selectable(view, it) ? i : -1)).filter((i) => i >= 0);
  const cursor = view.focus === 'list' ? order.findIndex((i) => isSelected(view.items[i])) : -1;
  const tasks = order.filter((i) => view.items[i].kind === 'task');
  const onTasks = (view.tabs.find((t) => t.active)?.label ?? 'Tasks') === 'Tasks';

  const down = (): Predicted | null => {
    if (view.focus === 'search') {
      // Into the list at the task after yours, else the top.
      const rows = onTasks ? tasks : order;
      if (rows.length === 0) return null;
      const own = onTasks ? tasks.findIndex((i) => (view.items[i] as { current?: boolean }).current) : -1;
      const to = Math.min(own >= 0 ? own + 1 : 0, rows.length - 1);
      return { view: withCursor(view, rows[to], 'list'), loading: false };
    }
    if (cursor < 0) return null;
    return { view: withCursor(view, order[Math.min(cursor + 1, order.length - 1)], 'list'), loading: false };
  };
  const up = (): Predicted | null => {
    if (view.focus === 'search') {
      if (!onTasks) return { view, loading: false };
      const own = tasks.findIndex((i) => (view.items[i] as { current?: boolean }).current);
      if (own < 0) return { view, loading: false };
      return { view: withCursor(view, tasks[Math.max(own - 1, 0)], 'list'), loading: false };
    }
    if (cursor < 0) return null;
    // Off the top of the list: into the search field.
    if (cursor === 0) return { view: withCursor(view, -1, 'search'), loading: false };
    return { view: withCursor(view, order[cursor - 1], 'list'), loading: false };
  };

  switch (msg.type) {
    case 'key': {
      const { key, ctrl, alt } = msg;
      if (alt) return null;
      if (key === 'ArrowDown' || (ctrl && key === 'j')) return down();
      if (key === 'ArrowUp' || (ctrl && key === 'k')) return up();
      if (ctrl) return null;
      if (view.focus === 'search') {
        if (key === 'Escape') return { view: withCursor(view, -1, 'list'), loading: false };
        if (key === 'Backspace') return refilter(view, [...view.filter].slice(0, -1).join(''), rows, onTasks);
        // `:` opens the command line; Tab switches tabs; Enter opens.
        if ([...key].length === 1 && key !== ':') return refilter(view, view.filter + key, rows, onTasks);
        return null;
      }
      if (key === 'j') return down();
      if (key === 'k') return up();
      if (key === 'i' || key === '/') return { view: withCursor(view, -1, 'search'), loading: false };
      if (key === 'Enter' || key === 'o' || key === 'l') return onTasks ? open(view, rows) : null;
      if (key === '?') return { view: { ...view, mode: { kind: 'help', scroll: 0 } }, loading: false };
      return null;
    }
    case 'click': {
      if (msg.kind === 'search') return { view: withCursor(view, -1, 'search'), loading: false };
      if (msg.kind === 'task') {
        const at = view.items.findIndex((it) =>
          msg.sub ? it.kind === 'sub' && it.task === msg.id && it.id === msg.sub : it.kind === 'task' && it.id === msg.id,
        );
        return at >= 0 && onTasks ? { view: withCursor(view, at, 'list'), loading: false } : null;
      }
      if (msg.kind === 'item') {
        const at = view.items.findIndex((it) => (it.kind === 'repo' || it.kind === 'job') && it.pos === msg.pos);
        return at >= 0 && !onTasks ? { view: withCursor(view, at, 'list'), loading: false } : null;
      }
      if (msg.kind === 'tab') {
        if (!view.tabs[msg.index]) return null;
        const tabs = view.tabs.map((t, i) => ({ ...t, active: i === msg.index }));
        return { view: { ...view, tabs }, loading: !view.tabs[msg.index].active };
      }
      return null;
    }
    case 'action': {
      if (msg.name === 'open') return onTasks && view.focus === 'list' ? open(view, rows) : null;
      if (msg.name === 'help') return { view: { ...view, mode: { kind: 'help', scroll: 0 } }, loading: false };
      return null;
    }
    default:
      return null;
  }
}

/** Whether `msg` moves the cursor the way `follow_selection` follows: the
 * window switches to an open task the cursor lands on. */
function follows(msg: ClientMessage): boolean {
  if (msg.type === 'click') return msg.kind === 'task';
  return msg.type === 'key' && !msg.alt && ['ArrowDown', 'ArrowUp', 'j', 'k'].includes(msg.key);
}

/** `view` with `pending` replayed on it, stopping at the first input that
 * can't be predicted (or after a tab switch, whose rows are unknown). */
export function predict(view: ColumnView, pending: Pending[], full: Item[] | null = null): Predicted {
  // The server's view shows a closed task's screen: which window is behind
  // it isn't in the view.
  let out: Predicted = { view, loading: false, window: view.shown_closed ? undefined : view.current, rowsFor: view.filter };
  for (const p of pending) {
    if (out.loading) break;
    const next = applyOne(out.view, p.msg, { full, rowsFor: out.rowsFor ?? null });
    if (!next) {
      out = { ...out, busy: busyOn(out.view, p.msg) };
      break;
    }
    const moved = next.view.items !== out.view.items || next.view.focus !== out.view.focus;
    if (next.rowsFor !== undefined) out = { ...out, rowsFor: next.rowsFor };
    if (next.opening !== undefined) {
      out = { ...out, ...next, opened: p.seq };
    } else if (moved && next.view.mode.kind === 'list') {
      out = show({ ...next, window: out.window, opened: out.opened, opening: out.opening, rowsFor: out.rowsFor }, follows(p.msg));
    } else {
      out = { ...out, view: next.view, loading: next.loading };
    }
  }
  return out;
}
