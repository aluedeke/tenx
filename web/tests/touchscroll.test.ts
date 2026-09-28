// Unit tests for src/lib/touchscroll.ts: `node --test tests/touchscroll.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { Accumulator, FLING_STOP, decay, wheelNotch } from '../src/lib/touchscroll.ts';

test('a notch is an SGR wheel report at the 1-based cell', () => {
  assert.equal(wheelNotch(true, 0, 0), '\x1b[<64;1;1M');
  assert.equal(wheelNotch(false, 9, 4), '\x1b[<65;10;5M');
});

test('dragging down scrolls back, a notch per step, remainder carried', () => {
  const acc = new Accumulator(50);
  assert.equal(acc.add(30), 0, 'not a notch yet');
  assert.equal(acc.add(30), 1, 'the carry makes it one');
  assert.equal(acc.add(120), 2);
  assert.equal(acc.add(-130), -2, 'dragging up scrolls forward (30 carried + -130)');
});

test('reset forgets the carry', () => {
  const acc = new Accumulator(50);
  acc.add(40);
  acc.reset();
  assert.equal(acc.add(40), 0);
});

test('a flick slows down and stops', () => {
  let v = 2;
  let frames = 0;
  while (Math.abs(v) > FLING_STOP && frames < 1000) {
    v = decay(v, 16);
    frames++;
  }
  assert.ok(frames > 10 && frames < 200, `stopped after ${frames} frames`);
});
