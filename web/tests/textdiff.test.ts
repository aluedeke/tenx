// Unit tests for src/lib/textdiff.ts: `node --test tests/textdiff.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { keptEdit, textEdit } from '../src/lib/textdiff.ts';

test('dictation growing its guess only sends the new words', () => {
  assert.deepEqual(textEdit('', 'Fix'), { erase: 0, insert: 'Fix' });
  assert.deepEqual(textEdit('Fix', 'Fix the'), { erase: 0, insert: ' the' });
  assert.deepEqual(textEdit('Fix the', 'Fix the login'), { erase: 0, insert: ' login' });
});

test('a revised guess erases back to where it differs', () => {
  assert.deepEqual(textEdit('Fix the log in', 'Fix the login'), { erase: 3, insert: 'in' });
  assert.deepEqual(textEdit('fix', 'Fix'), { erase: 3, insert: 'Fix' });
});

test('an unchanged field sends nothing', () => {
  assert.deepEqual(textEdit('same', 'same'), { erase: 0, insert: '' });
});

test('an emoji is one character to erase', () => {
  assert.deepEqual(textEdit('ok 👍', 'ok'), { erase: 2, insert: '' });
});

test('a kept start is never erased: later text is edited after it', () => {
  // Live dictation: "the" was typed and the user typed after it; a later
  // correction of "the" can't reach back there.
  assert.deepEqual(keptEdit('fix the log', 'fix the login', 7), { erase: 0, insert: 'in' });
  assert.deepEqual(keptEdit('fix the log', 'fix a login', 7), { erase: 4, insert: ' login' });
  assert.deepEqual(keptEdit('fix the log', 'fix', 7), { erase: 4, insert: '' });
  assert.deepEqual(keptEdit('fix the', 'fix the', 7), { erase: 0, insert: '' });
});
