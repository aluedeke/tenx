// Predicting the column's answer, so it moves at the speed of the finger
// rather than of the round trip (the server owns the column; on a relayed
// connection an answer can take a second).
//
// The page numbers every input it sends (`seq`); each view says the highest
// one it reflects (`ack`). What's drawn is the last view with the inputs it
// doesn't reflect yet replayed on top — only the few whose effect is certain
// from the view alone: the cursor moving through the list (↓ ↑ j k, ^j ^k,
// a click on a row), the search field taking or losing the cursor, and the
// filter text as it's typed. A tab click switches the highlighted tab and
// marks the list as loading; everything else (opening, actions, forms, the
// filtered rows themselves) waits for the server. The first input that
// can't be predicted stops the replay, so nothing after it is guessed on a
// state that may be wrong. When the real view arrives it replaces all of it.
//
// Mirrors src/tui/column.rs `step_down`/`step_up`/`handle_insert_key`/
// `handle_normal_key`; pure, for the unit tests.

import type { ClientMessage, ColumnView, Item } from '../protocol.ts';

export interface Pending {
  seq: number;
  msg: ClientMessage;
}

export interface Predicted {
  view: ColumnView;
  /** A tab switch is on its way: the rows shown are the old tab's. */
  loading: boolean;
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

/** One input applied to `view`, or null when its effect can't be known. */
export function applyOne(view: ColumnView, msg: ClientMessage): Predicted | null {
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
        if (key === 'Backspace') return { view: { ...view, filter: [...view.filter].slice(0, -1).join('') }, loading: false };
        // `:` opens the command line; Tab switches tabs; Enter opens.
        if ([...key].length === 1 && key !== ':') return { view: { ...view, filter: view.filter + key }, loading: false };
        return null;
      }
      if (key === 'j') return down();
      if (key === 'k') return up();
      if (key === 'i' || key === '/') return { view: withCursor(view, -1, 'search'), loading: false };
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
    default:
      return null;
  }
}

/** `view` with `pending` replayed on it, stopping at the first input that
 * can't be predicted (or after a tab switch, whose rows are unknown). */
export function predict(view: ColumnView, pending: Pending[]): Predicted {
  let out: Predicted = { view, loading: false };
  for (const p of pending) {
    if (out.loading) break;
    const next = applyOne(out.view, p.msg);
    if (!next) break;
    out = next;
  }
  return out;
}
