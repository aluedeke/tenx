// Unit tests for src/lib/dragfilter.ts: `node --test tests/dragfilter.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { DragFilter } from '../src/lib/dragfilter.ts';

test("an iPad click — press, a drag in the same cell, release — reaches tmux as a click", () => {
  const f = new DragFilter();
  assert.equal(f.pass('\x1b[<0;203;37M'), true);
  assert.equal(f.pass('\x1b[<32;203;37M'), false, 'no movement: not a drag');
  assert.equal(f.pass('\x1b[<32;203;37M'), false);
  assert.equal(f.pass('\x1b[<0;203;37m'), true);
});

test('a real drag passes from the first cell it reaches, back over the start too', () => {
  const f = new DragFilter();
  f.pass('\x1b[<0;10;5M');
  assert.equal(f.pass('\x1b[<32;10;5M'), false);
  assert.equal(f.pass('\x1b[<32;11;5M'), true);
  assert.equal(f.pass('\x1b[<32;10;5M'), true, 'a selection dragged back to where it began');
  assert.equal(f.pass('\x1b[<0;10;5m'), true);
});

test('after a release, a drag report is not held to the old press', () => {
  const f = new DragFilter();
  f.pass('\x1b[<0;10;5M');
  f.pass('\x1b[<0;10;5m');
  assert.equal(f.pass('\x1b[<32;10;5M'), true);
});

test('other input passes: keys, wheel, motion with no button, other buttons', () => {
  const f = new DragFilter();
  assert.equal(f.pass('a'), true);
  assert.equal(f.pass('\x1b[A'), true);
  f.pass('\x1b[<0;10;5M');
  assert.equal(f.pass('\x1b[<64;10;5M'), true, 'wheel');
  assert.equal(f.pass('\x1b[<35;10;5M'), true, 'motion without a button');
  assert.equal(f.pass('\x1b[<2;10;5M'), true, 'right button press');
  assert.equal(f.pass('\x1b[<34;10;5M'), false, 'right button drag in its pressed cell');
});
