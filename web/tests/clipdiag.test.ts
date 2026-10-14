// Unit tests for src/lib/clipdiag.ts: `node --test tests/clipdiag.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { describe } from '../src/lib/clipdiag.ts';

test('a page state reads as one log line', () => {
  const line = describe({ focused: true, visible: true, standalone: false, activation: true, ageMs: 2_345, shownMs: 61_200 });
  assert.equal(line, 'focus=yes visible=yes app=safari activation=yes age=2.3s shown=61s');
});

test('an unknown activation and a page that never left the screen', () => {
  const line = describe({ focused: false, visible: true, standalone: true, ageMs: 500 });
  assert.equal(line, 'focus=no visible=yes app=home-screen activation=? age=0.5s');
});
