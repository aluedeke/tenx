// Unit tests for src/lib/touchselect.ts: `node --test tests/touchselect.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { SelectGesture } from '../src/lib/touchselect.ts';

test('hold, drag, lift: the left button pressed, dragged and released, as a mouse sends it', () => {
  const g = new SelectGesture();
  assert.equal(g.down(100, 100, { x: 2, y: 3 }), true, 'start the hold timer');
  assert.equal(g.move(103, 101, { x: 2, y: 3 }).cancel, false, 'a wobble is still a hold');
  assert.equal(g.hold(), '\x1b[<0;3;4M');
  assert.equal(g.selecting, true);
  assert.deepEqual(g.move(160, 100, { x: 9, y: 3 }), { send: '\x1b[<32;10;4M', cancel: false });
  assert.deepEqual(g.move(161, 100, { x: 9, y: 3 }), { send: '', cancel: false }, 'same cell: nothing new');
  assert.equal(g.up({ x: 9, y: 3 }), '\x1b[<0;10;4m');
  assert.equal(g.selecting, false);
});

test('moving before the hold is up is a scroll: the timer stops and nothing is sent', () => {
  const g = new SelectGesture();
  g.down(100, 100, { x: 2, y: 3 });
  assert.deepEqual(g.move(100, 130, { x: 2, y: 5 }), { send: '', cancel: true });
  assert.equal(g.hold(), '', 'a late timer selects nothing');
  assert.equal(g.up({ x: 2, y: 5 }), '');
});

test('a quick tap selects nothing', () => {
  const g = new SelectGesture();
  g.down(100, 100, { x: 2, y: 3 });
  assert.equal(g.up({ x: 2, y: 3 }), '');
});

test('a touch off the grid starts no hold', () => {
  assert.equal(new SelectGesture().down(0, 0, null), false);
});

test('a cancelled touch still releases the button, where the drag last was', () => {
  const g = new SelectGesture();
  g.down(100, 100, { x: 2, y: 3 });
  g.hold();
  g.move(160, 100, { x: 9, y: 3 });
  assert.equal(g.up(null), '\x1b[<0;10;4m');
});

test('the next touch starts fresh', () => {
  const g = new SelectGesture();
  g.down(100, 100, { x: 2, y: 3 });
  g.hold();
  g.up({ x: 2, y: 3 });
  g.down(10, 10, { x: 0, y: 0 });
  assert.equal(g.selecting, false);
  assert.equal(g.hold(), '\x1b[<0;1;1M');
});
