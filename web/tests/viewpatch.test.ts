// Unit tests for src/lib/viewpatch.ts: `node --test tests/viewpatch.test.ts`.
// The cases are tenx-core/src/viewpatch.rs's, from the receiving side.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { applyPatch } from '../src/lib/viewpatch.ts';
import type { ColumnView, Item } from '../src/protocol.ts';

const task = (id: string, selected: boolean): Item => ({ kind: 'task', id, title: `Task ${id}`, selected }) as unknown as Item;
const header = (label: string): Item => ({ kind: 'header', label, count: 1, color: '#fff' });
const view = (items: Item[], focus = 'list'): ColumnView =>
  ({ tabs: [{ label: 'Tasks', active: true, running: 0 }], focus, items, help: [] }) as unknown as ColumnView;

test('a cursor move: two new rows, the rest by index', () => {
  const old = view([task('a', true), task('b', false), task('c', false)]);
  const got = applyPatch(old, { items: [task('a', false), task('b', true), 2] });
  assert.deepEqual(got, view([task('a', false), task('b', true), task('c', false)]));
  assert.equal(got.items[2], old.items[2], 'an unchanged row is the same object, so React can skip it');
});

test('rows that moved are found by their old index', () => {
  const old = view([header('WORKING'), task('a', false), task('b', false), header('IDLE'), task('c', false)]);
  const got = applyPatch(old, { items: [0, 2, 3, 1, 4] });
  assert.deepEqual(got.items.map((i) => (i.kind === 'task' ? i.id : i.kind === 'header' ? i.label : '')), [
    'WORKING',
    'b',
    'IDLE',
    'a',
    'c',
  ]);
});

test('top-level fields are set whole and dropped ones removed; no items keeps the rows', () => {
  const old = view([task('a', false)]);
  const got = applyPatch(old, { set: { focus: 'search', status: 'saved' }, unset: ['help'] });
  assert.equal(got.focus, 'search');
  assert.equal(got.status, 'saved');
  assert.equal(got.items, old.items);
  assert.ok(!Object.hasOwn(got, 'help'));
});

test('an empty patch (only the ack moved) changes nothing', () => {
  const old = view([task('a', false)]);
  assert.equal(applyPatch(old, {}), old, 'the same view: nothing to redraw');
});
