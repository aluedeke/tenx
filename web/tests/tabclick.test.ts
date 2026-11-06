// Unit tests for src/lib/tabclick.ts: `node --test tests/tabclick.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { TabClicks, tabAt } from '../src/lib/tabclick.ts';

const strip = '   1 agent   2 shell   3 nvim                   ';

test('the tab under a column', () => {
  assert.equal(tabAt(strip, 3), 1);
  assert.equal(tabAt(strip, 13), 2);
  assert.equal(tabAt(strip, 26), 3);
  assert.equal(tabAt(strip, 40), null);
  assert.equal(tabAt("the agent's own screen", 5), null);
});

test('a click on a tab is its Alt key, and its release is dropped', () => {
  const t = new TabClicks();
  assert.equal(t.translate('\x1b[<0;14;1M', () => strip), '\x1b2');
  assert.equal(t.translate('\x1b[<0;14;1m', () => strip), '');
});

test('anything else goes on as it is', () => {
  const t = new TabClicks();
  assert.equal(t.translate('\x1b[<0;14;2M', () => strip), '\x1b[<0;14;2M', 'not the top line');
  assert.equal(t.translate('\x1b[<0;14;2m', () => strip), '\x1b[<0;14;2m');
  assert.equal(t.translate('\x1b[<0;40;1M', () => strip), '\x1b[<0;40;1M', 'beside the tabs');
  assert.equal(t.translate('\x1b[<2;14;1M', () => strip), '\x1b[<2;14;1M', 'the right button');
  assert.equal(t.translate('\x1b[<64;14;1M', () => strip), '\x1b[<64;14;1M', 'the wheel');
  assert.equal(t.translate('ls\r', () => strip), 'ls\r');
});
