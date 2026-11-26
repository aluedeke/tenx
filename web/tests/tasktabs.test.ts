// Unit tests for src/lib/tasktabs.ts: `node --test tests/tasktabs.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { GIVE_UP_MS, litTab } from '../src/lib/tasktabs.ts';

test('without a click the bar lights what the window shows', () => {
  assert.equal(litTab('agent', null, 0), 'agent');
});

test('a click lights its tab at once, whatever the views say meanwhile', () => {
  const asked = { tab: 'nvim' as const, at: 1000 };
  assert.equal(litTab('agent', asked, 1001), 'nvim', 'the view from before the click');
  assert.equal(litTab('shell', asked, 1500), 'nvim', 'a view for an earlier click');
  assert.equal(litTab('nvim', asked, 1600), 'nvim');
});

test('a click the view never confirms gives up', () => {
  const asked = { tab: 'nvim' as const, at: 1000 };
  assert.equal(litTab('agent', asked, 1000 + GIVE_UP_MS), 'agent');
});
