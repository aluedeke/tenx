// Unit tests for src/lib/viewport.ts: `node --test tests/viewport.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { inIosApp, keepsShortcutRoom, keyBarShown, layout } from '../src/lib/viewport.ts';

// An iPad in landscape: 1024 × 768 with no keyboard.
const ipad = { editing: true, top: 0, innerHeight: 768, fullHeight: 768, keep: true, reserve: 0 };

test('an on-screen keyboard: the page ends on top of it', () => {
  assert.deepEqual(layout({ ...ipad, height: 420, top: 12 }), { keyboard: true, height: 420, top: 12, gap: 0, reserve: 0 });
});

test("iPadOS's shortcut bar over a hardware keyboard: the page ends above it, but it is no keyboard", () => {
  assert.deepEqual(layout({ ...ipad, height: 713 }), { keyboard: false, height: 713, top: 0, gap: 0, reserve: 55 });
});

test("the shortcut bar gone: its room stays under the key bar, so the terminal keeps its height", () => {
  assert.deepEqual(layout({ ...ipad, height: 768, reserve: 55 }), { keyboard: false, height: 768, top: 0, gap: 55, reserve: 55 });
  assert.deepEqual(layout({ ...ipad, editing: false, height: 768, reserve: 55 }), { keyboard: false, height: 768, top: 0, gap: 55, reserve: 55 });
  // A smaller one: the rest of the room.
  assert.deepEqual(layout({ ...ipad, height: 728, reserve: 55 }), { keyboard: false, height: 728, top: 0, gap: 15, reserve: 55 });
});

test('an on-screen keyboard takes no extra room under the key bar', () => {
  assert.deepEqual(layout({ ...ipad, height: 420, reserve: 55 }), { keyboard: true, height: 420, top: 0, gap: 0, reserve: 55 });
});

test('a phone keeps no room: the page follows what covers it', () => {
  const phone = { ...ipad, keep: false };
  assert.deepEqual(layout({ ...phone, height: 713 }), { keyboard: false, height: 713, top: 0, gap: 0, reserve: 0 });
  assert.deepEqual(layout({ ...phone, height: 768 }), { keyboard: false, height: 768, top: 0, gap: 0, reserve: 0 });
});

test('nothing covering the page: the whole window', () => {
  assert.deepEqual(layout({ ...ipad, height: 768 }), { keyboard: false, height: 768, top: 0, gap: 0, reserve: 0 });
});

test("the app switcher's sliver is no keyboard: the whole window", () => {
  assert.deepEqual(layout({ ...ipad, height: 80 }), { keyboard: false, height: 768, top: 0, gap: 0, reserve: 0 });
});

test('without a focused text field nothing is a keyboard', () => {
  assert.deepEqual(layout({ ...ipad, editing: false, height: 420 }), { keyboard: false, height: 768, top: 0, gap: 0, reserve: 0 });
});

test('the key bar: always on an iPad, over the keyboard on a phone, while recording', () => {
  const none = { touch: true, ipad: false, keyboard: false, recording: false };
  assert.equal(keyBarShown(none), false, 'a phone with its keyboard down');
  assert.equal(keyBarShown({ ...none, ipad: true }), true, 'an iPad, focused or not');
  assert.equal(keyBarShown({ ...none, keyboard: true }), true);
  assert.equal(keyBarShown({ ...none, recording: true }), true);
  assert.equal(keyBarShown({ ...none, touch: false, ipad: true, keyboard: true }), false, 'never without a touch screen');
});

test("the shortcut bar's room is kept on an iPad in Safari, not in an app", () => {
  assert.equal(keepsShortcutRoom({ touch: true, ipad: true, app: false }), true);
  assert.equal(keepsShortcutRoom({ touch: true, ipad: true, app: true }), false, 'the Home Screen app or the iOS app');
  assert.equal(keepsShortcutRoom({ touch: true, ipad: false, app: false }), false, 'a phone');
  assert.equal(keepsShortcutRoom({ touch: false, ipad: true, app: false }), false, 'no touch screen');
});

test('the tenx iOS app is known by its message handler', () => {
  assert.equal(inIosApp({ webkit: { messageHandlers: { tenx: {} } } }), true);
  assert.equal(inIosApp({ webkit: { messageHandlers: {} } }), false, 'another WKWebView');
  assert.equal(inIosApp({}), false, 'Safari');
});
