// Unit tests for src/lib/textlink.ts: `node --test tests/textlink.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { urlAt } from '../src/lib/textlink.ts';

/** A screen of `cols` cells per row from plain lines (one cell per
 * character, padded with spaces). */
function screen(cols: number, ...lines: string[]): string[][] {
  return lines.map((l) => [...l.padEnd(cols, ' ')].slice(0, cols));
}

test('a URL in a line: held anywhere on it, the whole URL', () => {
  const rows = screen(40, 'see https://example.com/a?b=1 for more');
  assert.equal(urlAt(rows, { x: 4, y: 0 }), 'https://example.com/a?b=1');
  assert.equal(urlAt(rows, { x: 28, y: 0 }), 'https://example.com/a?b=1');
  assert.equal(urlAt(rows, { x: 30, y: 0 }), null, 'past its end');
  assert.equal(urlAt(rows, { x: 1, y: 0 }), null, 'before it');
});

test('a full stop or a closing bracket after the URL is not part of it', () => {
  const rows = screen(40, '(at https://example.com/x).');
  assert.equal(urlAt(rows, { x: 10, y: 0 }), 'https://example.com/x');
});

test('a URL wrapped at the edge goes on in the next row, at its indent', () => {
  const rows = screen(19, '⏺ https://github.co', '  m/a/b/pull/7', 'next line');
  assert.equal(urlAt(rows, { x: 5, y: 0 }), 'https://github.com/a/b/pull/7');
  assert.equal(urlAt(rows, { x: 6, y: 1 }), 'https://github.com/a/b/pull/7', 'held on the rest');
  assert.equal(urlAt(rows, { x: 1, y: 2 }), null);
});

test('a row that ends short of the edge does not go on', () => {
  const rows = screen(30, 'https://a.example/x', 'more');
  assert.equal(urlAt(rows, { x: 3, y: 0 }), 'https://a.example/x');
});

test('a border under a URL that fills its row is not read on into', () => {
  const rows = screen(20, 'http://a.example/abc', '────────────────────');
  assert.equal(urlAt(rows, { x: 3, y: 0 }), 'http://a.example/abc');
  assert.equal(urlAt(rows, { x: 3, y: 1 }), null);
});

test('wide characters before the URL: cells, not characters, are counted', () => {
  // 👍 takes two cells; the right half is ''.
  const rows = [['👍', '', ' ', ...'https://x.io/'], ['']];
  assert.equal(urlAt(rows, { x: 3, y: 0 }), 'https://x.io/');
  assert.equal(urlAt(rows, { x: 0, y: 0 }), null);
});

test('an Obsidian link', () => {
  const rows = screen(60, 'open obsidian://open?vault=brain&file=x now');
  assert.equal(urlAt(rows, { x: 10, y: 0 }), 'obsidian://open?vault=brain&file=x');
});
