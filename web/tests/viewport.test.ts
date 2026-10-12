// Unit tests for src/lib/viewport.ts: `node --test tests/viewport.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { keyBarShown, layout } from '../src/lib/viewport.ts';

// An iPad in landscape: 1024 × 768 with no keyboard.
const ipad = { editing: true, top: 0, innerHeight: 768, fullHeight: 768 };

test('an on-screen keyboard: the page ends on top of it', () => {
  assert.deepEqual(layout({ ...ipad, height: 420, top: 12 }), { keyboard: true, height: 420, top: 12 });
});

test("iPadOS's shortcut bar over a hardware keyboard: the page ends above it, but it is no keyboard", () => {
  assert.deepEqual(layout({ ...ipad, height: 713 }), { keyboard: false, height: 713, top: 0 });
});

test('nothing covering the page: the whole window', () => {
  assert.deepEqual(layout({ ...ipad, height: 768 }), { keyboard: false, height: 768, top: 0 });
});

test("the app switcher's sliver is no keyboard: the whole window", () => {
  assert.deepEqual(layout({ ...ipad, height: 80 }), { keyboard: false, height: 768, top: 0 });
});

test('without a focused text field nothing is a keyboard', () => {
  assert.deepEqual(layout({ ...ipad, editing: false, height: 420 }), { keyboard: false, height: 768, top: 0 });
});

test('the key bar: on an iPad while the terminal has the focus, over the keyboard on a phone, while recording', () => {
  const none = { touch: true, ipad: false, typing: false, keyboard: false, recording: false };
  assert.equal(keyBarShown(none), false, 'a phone with its keyboard down');
  assert.equal(keyBarShown({ ...none, typing: true }), false, 'a phone focused by code: no keyboard, no bar');
  assert.equal(keyBarShown({ ...none, ipad: true, typing: true }), true, 'an iPad with a hardware keyboard');
  assert.equal(keyBarShown({ ...none, ipad: true }), false, 'an iPad with nothing focused');
  assert.equal(keyBarShown({ ...none, keyboard: true }), true);
  assert.equal(keyBarShown({ ...none, recording: true }), true);
  assert.equal(keyBarShown({ ...none, touch: false, ipad: true, typing: true, keyboard: true }), false, 'never without a touch screen');
});
