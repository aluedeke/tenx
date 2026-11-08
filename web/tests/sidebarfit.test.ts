// Unit tests for src/lib/sidebarfit.ts: `node --test tests/sidebarfit.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { MIN_TASK_COLS, SIDEBAR_PX, sidebarFits } from '../src/lib/sidebarfit.ts';

test('the sidebar fits only while the task keeps its columns', () => {
  const cell = 8;
  const column = 300;
  const exact = column + SIDEBAR_PX + MIN_TASK_COLS * cell;
  assert.equal(sidebarFits(exact, cell, column), true);
  assert.equal(sidebarFits(exact - 1, cell, column), false);
  // A 1024 px page with the task list beside: no room; without it, room.
  assert.equal(sidebarFits(1024, 7.8, 301), false);
  assert.equal(sidebarFits(1024, 7.8, 0), true);
});
