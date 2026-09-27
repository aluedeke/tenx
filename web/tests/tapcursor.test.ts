// Unit tests for src/lib/tapcursor.ts: `node --test tests/tapcursor.test.ts`
// (Node strips the types).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { arrow, inputBox, paneColumns, planTap } from '../src/lib/tapcursor.ts';

// A tmux window: Claude on the left, a shell on the right, Claude's input
// between two rules with the cursor on its second line.
const W = 30;
const pad = (s: string) => s.padEnd(W);
const rows = [
  pad('⏺ Done, the tests pass.') + '│' + pad('$ ls'),
  pad('') + '│' + pad('Cargo.toml  src'),
  pad('──────────────────────────────') + '│' + pad('$ '),
  pad('> fix the login timeout and') + '│' + pad(''),
  pad('  add a test for it') + '│' + pad(''),
  pad('──────────────────────────────') + '│' + pad(''),
  pad('  ? for shortcuts') + '│' + pad(''),
];
const cursor = { x: 10, y: 4 };

test('the pane is bounded by the tmux borders on the cursor row', () => {
  assert.deepEqual(paneColumns(rows[4], 10), [0, W]);
  assert.deepEqual(paneColumns(rows[4], W + 3), [W + 1, 2 * W + 1]);
});

test('the input box is the rows between the rules around the cursor', () => {
  assert.deepEqual(inputBox(rows, cursor, [0, W]), [3, 4]);
  // The shell pane has no rules: no box.
  assert.equal(inputBox(rows, { x: 33, y: 2 }, [W + 1, 2 * W + 1]), null);
});

test('a tap on the cursor row moves along it', () => {
  assert.deepEqual(planTap(rows, cursor, { x: 4, y: 4 }), { target: { x: 4, y: 4 }, vertical: false });
  assert.equal(planTap(rows, cursor, cursor), null, 'already there');
});

test('a tap on another line of the input box may change rows', () => {
  assert.deepEqual(planTap(rows, cursor, { x: 6, y: 3 }), { target: { x: 6, y: 3 }, vertical: true });
});

test('taps outside the box or the pane do nothing', () => {
  assert.equal(planTap(rows, cursor, { x: 3, y: 0 }), null, 'the output above: ↑ would recall history');
  assert.equal(planTap(rows, cursor, { x: 3, y: 6 }), null, 'the hint line below');
  assert.equal(planTap(rows, cursor, { x: W + 4, y: 4 }), null, 'the other pane');
});

test('a shell prompt only moves along its own line', () => {
  const shell = ['$ git commit -m "wip"', '', ''];
  const at = { x: 21, y: 0 };
  assert.deepEqual(planTap(shell, at, { x: 6, y: 0 }), { target: { x: 6, y: 0 }, vertical: false });
  assert.equal(planTap(shell, at, { x: 6, y: 1 }), null);
});

test('arrows follow the cursor-key mode', () => {
  assert.equal(arrow('left', false), '\x1b[D');
  assert.equal(arrow('up', true), '\x1bOA');
});
