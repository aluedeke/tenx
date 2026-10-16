// Unit tests for src/lib/predict.ts: `node --test tests/predict.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { filterMatches, filterTasks, predict, unacked, type Pending } from '../src/lib/predict.ts';
import type { ColumnView, Item } from '../src/protocol.ts';

const task = (id: string, extra: Record<string, unknown> = {}): Item =>
  ({
    kind: 'task', id, ws: 'w', ws_color: '#fff', slug: id, title: id, title_color: '#fff', glyph: '·', glyph_color: '#fff',
    status: 'idle', selected: false, current: false, closed: false, pending: false, reason: null, answerable: false,
    locked: false, agent: null, age: null, prs: [], ports: [], ...extra,
  }) as unknown as Item;
const sub = (taskId: string, id: string): Item =>
  ({ kind: 'sub', task: taskId, id, glyph: '◐', glyph_color: '#fff', label: id, extras: [], finished: false, selected: false }) as unknown as Item;
const header = (label: string): Item => ({ kind: 'header', label, count: 1, color: '#fff' });
// Titled tasks for the filter: "login" with a subagent, "build", "logs".
const named = () => [
  { ...header('WORKING'), count: 2 } as Item, task('login', { title: 'Login' }), sub('login', 'l1'), task('build', { title: 'Build log' }),
  header('INACTIVE'), task('logs', { title: 'Logs' }),
];
const ids = (v: ColumnView) =>
  v.items.map((i) => (i.kind === 'header' ? `${i.label} ${i.count}` : i.kind === 'sub' ? i.id : i.kind === 'task' ? i.id : i.kind));

function view(items: Item[], focus: 'search' | 'list' = 'list', filter = ''): ColumnView {
  return {
    tabs: [
      { label: 'Tasks', active: true, running: 0 },
      { label: 'Repos', active: false, running: 0 },
      { label: 'Work', active: false, running: 0 },
    ],
    focus, filter, current: null, items,
    mode: { kind: 'list' },
    footer: { kind: 'hint', tag: focus === 'search' ? 'INSERT' : 'NORMAL', text: '', hint: null, warn: null },
    help: [],
  } as unknown as ColumnView;
}

const key = (k: string, ctrl = false) => ({ type: 'key' as const, key: k, ctrl, alt: false, shift: false });
const ops = (...msgs: Pending['msg'][]): Pending[] => msgs.map((msg, i) => ({ seq: i + 1, msg }));
const cursor = (v: ColumnView) =>
  v.items.filter((i) => 'selected' in i && i.selected).map((i) => (i.kind === 'sub' ? `${i.task}>${i.id}` : (i as { id: string }).id));

// A: two subagents; B plain; C last.
const rows = () => [header('WORKING'), task('A', { selected: true }), sub('A', 'a1'), sub('A', 'a2'), header('INACTIVE'), task('B'), task('C')];

test('j steps through a task, its subagents, then the next task, skipping headers', () => {
  const steps = [1, 2, 3, 4].map((n) => cursor(predict(view(rows()), ops(...Array(n).fill(key('j')))).view));
  assert.deepEqual(steps, [['A>a1'], ['A>a2'], ['B'], ['C']]);
});

test('j is clamped at the bottom, like the server', () => {
  assert.deepEqual(cursor(predict(view(rows()), ops(...Array(9).fill(key('j')))).view), ['C']);
});

test('k goes back up; from a task onto the task above lands on its last subagent', () => {
  const v = view([header('W'), task('A'), sub('A', 'a1'), sub('A', 'a2'), task('B', { selected: true })]);
  assert.deepEqual(cursor(predict(v, ops(key('k'))).view), ['A>a2']);
  assert.deepEqual(cursor(predict(v, ops(key('ArrowUp'), key('ArrowUp'), key('ArrowUp'))).view), ['A']);
});

test('k off the top goes into the search field', () => {
  const out = predict(view(rows()), ops(key('k'))).view;
  assert.equal(out.focus, 'search');
  assert.deepEqual(cursor(out), []);
  assert.equal(out.footer.tag, 'INSERT');
});

test('↓ from the search field enters the list after your task', () => {
  const v = view([header('W'), task('A', { current: true }), sub('A', 'a1'), task('B'), task('C')], 'search');
  const out = predict(v, ops(key('ArrowDown'))).view;
  assert.equal(out.focus, 'list');
  assert.deepEqual(cursor(out), ['B']);
});

test('typing in the search field narrows the rows from those in hand', () => {
  const out = predict(view(named(), 'search'), ops(key('l'), key('o'), key('g'), key('i'))).view;
  assert.equal(out.filter, 'logi');
  assert.deepEqual(ids(out), ['WORKING 1', 'login', 'l1'], 'a section with no match loses its header');
});

test('a shorter filter widens from the kept full list; without one the rows wait for the server', () => {
  const narrowed = { ...view(filterTasks(named(), 'log'), 'search', 'log') } as ColumnView;
  assert.deepEqual(ids(predict(narrowed, ops(key('Backspace')), named()).view), ['WORKING 2', 'login', 'l1', 'build', 'INACTIVE 1', 'logs']);
  const out = predict(narrowed, ops(key('Backspace'), key('g')));
  assert.equal(out.view.filter, 'log');
  assert.equal(out.rowsFor, null, 'rows for an older filter: not filtered further');
});

test('a filter that matches nothing shows the empty line', () => {
  assert.deepEqual(predict(view(named(), 'search'), ops(key('z'))).view.items, [{ kind: 'empty', lines: ['no tasks yet — :n to create one'] }]);
});

test('the match is filter_matches over "<workspace> <title>": in order, spaces skipped, any case', () => {
  assert.equal(filterMatches('w lgn', 'w Login'), true);
  assert.equal(filterMatches('nigol', 'w login'), false);
  assert.equal(filterMatches('', 'anything'), true);
});

test('⏎ on a filtered list clears the filter: the full list comes back', () => {
  const v = { ...view(filterTasks(named(), 'logs'), 'list', 'logs') } as ColumnView;
  (v.items[1] as { selected: boolean }).selected = true;
  const out = predict(v, ops(key('Enter')), named());
  assert.equal(out.view.filter, '');
  assert.equal(out.view.current, 'logs');
  assert.equal(ids(out.view).length, named().length);
  assert.equal(predict(v, ops(key('Enter'))).opened, undefined, 'no full list: not guessed');
});

test('a click on a subagent line selects it', () => {
  const out = predict(view(rows(), 'search'), ops({ type: 'click', kind: 'task', id: 'A', sub: 'a2' })).view;
  assert.deepEqual(cursor(out), ['A>a2']);
  assert.equal(out.focus, 'list');
});

test('a tab click switches the tab and marks the rows as loading; nothing after it is guessed', () => {
  const out = predict(view(rows()), ops({ type: 'click', kind: 'tab', index: 1 }, key('j')));
  assert.equal(out.loading, true);
  assert.equal(out.view.tabs[1].active, true);
  assert.deepEqual(cursor(out.view), ['A'], 'the j after it is not applied');
});

test('an input that can’t be predicted stops the replay', () => {
  // x closes the window (server-only); the j after it isn't guessed.
  assert.deepEqual(cursor(predict(view(rows()), ops(key('x'), key('j'))).view), ['A']);
  // Nor anything outside list mode.
  const form = { ...view(rows()), mode: { kind: 'rename', buffer: 'x' } } as unknown as ColumnView;
  assert.deepEqual(cursor(predict(form, ops(key('j'))).view), ['A']);
});

test('a view that acknowledges an input drops it from the replay', () => {
  const pending = ops(key('j'), key('j'), key('j'));
  assert.deepEqual(unacked(pending, 2).map((p) => p.seq), [3]);
  // The server has applied the first two: its view already shows A>a2.
  const server = view([header('W'), task('A'), sub('A', 'a1'), sub('A', 'a2', ), task('B'), task('C')]);
  (server.items[3] as { selected: boolean }).selected = true;
  assert.deepEqual(cursor(predict(server, unacked(pending, 2)).view), ['B']);
});

// What the task area shows: the marker, the closed screen, opening.
const shown = (v: ColumnView) => v.items.filter((i) => i.kind === 'task' && i.current).map((i) => (i as { id: string }).id);
const marked = () => {
  const v = view([header('W'), task('A', { selected: true, current: true }), task('B'), task('C', { closed: true })]);
  return { ...v, current: 'A' } as ColumnView;
};

test('moving onto an open task switches the window: the marker follows', () => {
  const out = predict(marked(), ops(key('j'))).view;
  assert.equal(out.current, 'B');
  assert.deepEqual(shown(out), ['B']);
  assert.equal(out.shown_closed, null);
});

test('resting on a closed task shows its empty screen; leaving it shows the window again', () => {
  const on = predict(marked(), ops(key('j'), key('j'))).view;
  assert.equal(on.current, 'C');
  assert.equal(on.shown_closed?.id, 'C');
  const back = predict(marked(), ops(key('j'), key('j'), key('k'))).view;
  assert.equal(back.shown_closed, null);
  assert.equal(back.current, 'B', 'the window B switched to is still behind');
});

test('from a server view on a closed task, the window behind it is unknown: no marker', () => {
  const v = { ...view([header('W'), task('A'), task('C', { closed: true, selected: true, current: true })]), current: 'C' } as ColumnView;
  (v as { shown_closed: unknown }).shown_closed = { id: 'C', title: 'C', ws: 'w', ws_color: '#fff' };
  const out = predict(v, ops(key('i'))).view;
  assert.equal(out.focus, 'search');
  assert.equal(out.current, null);
  assert.equal(out.shown_closed, null);
});

test('⏎ on an open task: its window, the keyboard to the terminal, the search field focused', () => {
  const out = predict(marked(), ops(key('j'), key('Enter')));
  assert.equal(out.view.current, 'B');
  assert.equal(out.view.focus, 'search');
  assert.deepEqual(cursor(out.view), []);
  assert.equal(out.opened, 2);
  assert.equal(out.opening, null, 'an open window comes up at once');
});

test('opening a closed task says so while its window starts', () => {
  const out = predict(marked(), ops({ type: 'click', kind: 'task', id: 'C' }, { type: 'action', name: 'open' }));
  assert.equal(out.opening, 'C');
  assert.equal(out.view.current, 'C');
  assert.equal(out.view.shown_closed, null);
});

test('no open is guessed on a filtered list, a task being set up, or a subagent line', () => {
  assert.equal(predict(view(rows(), 'list', 'x'), ops(key('Enter'))).opened, undefined);
  assert.equal(predict(view([task('A', { selected: true, pending: true })]), ops(key('Enter'))).opened, undefined);
  assert.equal(predict(view(rows()), ops(key('j'), key('Enter'))).opened, undefined);
});

test('? opens help, which scrolls and closes on any other key', () => {
  const v = view(rows());
  assert.deepEqual(predict(v, ops(key('?'))).view.mode, { kind: 'help', scroll: 0 });
  assert.deepEqual(predict(v, ops({ type: 'action', name: 'help' }, key('j'), key('j'), key('k'))).view.mode, { kind: 'help', scroll: 1 });
  assert.deepEqual(predict(v, ops(key('?'), key('Escape'))).view.mode, { kind: 'list' });
});

test('an action on a row marks the row busy until the answer', () => {
  const out = predict(view(rows()), ops({ type: 'click', kind: 'task', id: 'B' }, { type: 'action', name: 'approve' }));
  assert.deepEqual(out.busy, { id: 'B', action: 'approve' });
  assert.deepEqual(predict(view(rows()), ops(key('x'))).busy, { id: 'A', action: 'close' });
  assert.equal(predict(view(rows()), ops({ type: 'form', op: 'submit' })).busy, null, 'not a row action');
});
