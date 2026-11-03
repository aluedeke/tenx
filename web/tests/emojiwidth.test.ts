// Unit tests for src/lib/emojiwidth.ts: `node --test tests/emojiwidth.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { IUnicodeVersionProvider } from '@xterm/xterm';
import { isWideEmoji, widerEmoji, VERSION } from '../src/lib/emojiwidth.ts';

const cp = (s: string) => s.codePointAt(0)!;
const pack = (state: number, width: number, join: boolean) => (state << 3) | (width << 1) | (join ? 1 : 0);

// A table that makes everything one cell, joins nothing, and keeps a state.
const narrow: IUnicodeVersionProvider = {
  version: 'narrow',
  wcwidth: () => 1,
  charProperties: () => pack(5, 1, false),
};

test('emojis newer than the table are wide; text and symbols are not', () => {
  for (const e of ['🫠', '🪿', '📌', '🥲']) assert.ok(isWideEmoji(cp(e)), e);
  for (const c of ['A', 'ä', '⏺', '✻', '⚠', '✔', '│', '中']) assert.ok(!isWideEmoji(cp(c)), c);
});

test('a lone regional indicator is left alone', () => {
  assert.ok(!isWideEmoji(cp('🇩')));
});

test('the wrapper widens a one-cell emoji and keeps the rest of its properties', () => {
  const p = widerEmoji(narrow);
  assert.equal(p.version, VERSION);
  assert.equal(p.wcwidth(cp('🫠')), 2);
  assert.equal(p.wcwidth(cp('A')), 1);
  assert.equal(p.charProperties(cp('🫠'), 0), pack(5, 2, false));
  assert.equal(p.charProperties(cp('A'), 0), pack(5, 1, false));
});

test('what the table already decides is kept: zero width, joins, two cells', () => {
  const base: IUnicodeVersionProvider = {
    version: 'b',
    wcwidth: (c) => (c === 0x200d ? 0 : 2),
    charProperties: (_c, preceding) => (preceding ? pack(7, 2, true) : pack(3, 2, false)),
  };
  const p = widerEmoji(base);
  assert.equal(p.wcwidth(0x200d), 0);
  assert.equal(p.charProperties(cp('🫠'), 0), pack(3, 2, false));
  assert.equal(p.charProperties(cp('🫠'), pack(3, 2, false)), pack(7, 2, true));
});
