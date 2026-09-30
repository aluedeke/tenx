// Unit tests for src/lib/predict.ts: `node --test tests/predict.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { predict, unacked, type Pending } from '../src/lib/predict.ts';
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

test('typing in the search field updates the filter text, not the rows', () => {
  const v = view(rows(), 'search', 'lo');
  const out = predict(v, ops(key('g'), key('i'), key('Backspace'), key('n'))).view;
  assert.equal(out.filter, 'logn');
  assert.equal(out.items, v.items, 'the rows are the server’s to filter');
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
  // Enter opens (server-only); the j after it isn't guessed.
  assert.deepEqual(cursor(predict(view(rows()), ops(key('Enter'), key('j'))).view), ['A']);
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
