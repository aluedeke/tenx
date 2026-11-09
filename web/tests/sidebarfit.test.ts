// Unit tests for src/lib/sidebarfit.ts: `node --test tests/sidebarfit.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  COLUMN_MIN_COLS,
  DRAWER_MARGIN,
  MIN_TASK_COLS,
  SIDEBAR_MIN,
  SIDEBAR_PX,
  columnDrag,
  columnPx,
  dragTo,
  parseColumnCols,
  parseWidth,
  sidebarFits,
} from '../src/lib/sidebarfit.ts';

test('the sidebar fits only while the task keeps its columns', () => {
  const cell = 8;
  const column = 300;
  const exact = column + SIDEBAR_PX + MIN_TASK_COLS * cell;
  assert.equal(sidebarFits(exact, cell, column), true);
  assert.equal(sidebarFits(exact - 1, cell, column), false);
  // A 1024 px page with the task list beside: no room; without it, room.
  assert.equal(sidebarFits(1024, 7.8, 301), false);
  assert.equal(sidebarFits(1024, 7.8, 0), true);
  // A wider sidebar needs a wider page.
  assert.equal(sidebarFits(exact, cell, column, SIDEBAR_PX + 1), false);
});

test('a panel is dragged no wider than leaves the task its columns', () => {
  const cell = 8;
  const page = 1440;
  const column = 300;
  const widest = page - column - MIN_TASK_COLS * cell; // 500
  assert.deepEqual(dragTo(440, false, page, cell, column), { width: 440, stop: false, cols: 87 });
  assert.deepEqual(dragTo(widest, false, page, cell, column), { width: widest, stop: false, cols: MIN_TASK_COLS });
  assert.deepEqual(dragTo(900, false, page, cell, column), { width: widest, stop: true, cols: MIN_TASK_COLS });
  // What the drag leaves fits as a panel.
  assert.equal(sidebarFits(page, cell, column, dragTo(900, false, page, cell, column).width), true);
  // Never narrower than the narrowest.
  assert.deepEqual(dragTo(100, false, page, cell, column), { width: SIDEBAR_MIN, stop: false, cols: 112 });
});

test('a drawer is dragged up to the margin it leaves of the page', () => {
  assert.deepEqual(dragTo(460, true, 820, 8, 0), { width: 460, stop: false, cols: null });
  assert.deepEqual(dragTo(900, true, 820, 8, 0), { width: 820 - DRAWER_MARGIN, stop: true, cols: null });
});

test('a remembered width is a number no narrower than the narrowest', () => {
  assert.equal(parseWidth('440'), 440);
  assert.equal(parseWidth(null), null);
  assert.equal(parseWidth(''), null);
  assert.equal(parseWidth('wide'), null);
  assert.equal(parseWidth('100'), null);
});

test('the task list is dragged in whole columns, up to half the page', () => {
  const cell = 8;
  // 340 px is 40 columns and its padding.
  assert.deepEqual(columnDrag(340, 1440, cell), { width: columnPx(40, cell), stop: false, cols: 40 });
  assert.equal(columnPx(40, cell), 340);
  // Half of 180 columns.
  assert.deepEqual(columnDrag(1200, 1440, cell), { width: columnPx(90, cell), stop: true, cols: 90 });
  assert.deepEqual(columnDrag(50, 1440, cell), { width: columnPx(COLUMN_MIN_COLS, cell), stop: false, cols: COLUMN_MIN_COLS });
});

test('a remembered task list width is whole columns, no narrower than the narrowest', () => {
  assert.equal(parseColumnCols('40'), 40);
  assert.equal(parseColumnCols(null), null);
  assert.equal(parseColumnCols('40.5'), null);
  assert.equal(parseColumnCols('12'), null);
});
