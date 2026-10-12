// Unit tests for src/lib/keys.ts: `node --test tests/keys.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { isIPad, isImagePaste, isRemote, takesImagePaste } from '../src/lib/keys.ts';

const key = (code: string, mods: Partial<KeyboardEvent> = {}) =>
  ({ code, key: code.slice(3).toLowerCase(), ctrlKey: false, altKey: false, metaKey: false, shiftKey: false, ...mods }) as KeyboardEvent;

test('Ctrl+V is the image paste key, by the physical key', () => {
  assert.equal(isImagePaste(key('KeyV', { ctrlKey: true })), true);
  assert.equal(isImagePaste(key('KeyV', { ctrlKey: true, key: '\x16' })), true);
});

test('Cmd+V, Ctrl+Shift+V and a bare v are not', () => {
  assert.equal(isImagePaste(key('KeyV', { metaKey: true })), false);
  assert.equal(isImagePaste(key('KeyV', { ctrlKey: true, shiftKey: true })), false);
  assert.equal(isImagePaste(key('KeyV')), false);
});

test('a browser on this machine shares its clipboard; one elsewhere does not', () => {
  for (const host of ['localhost', '127.0.0.1', '[::1]']) assert.equal(isRemote(host), false, host);
  assert.equal(isRemote('mac.tail1234.ts.net'), true);
});

test('the page takes ^V only in the terminal, on an Apple device, from another machine', () => {
  const ctrlV = key('KeyV', { ctrlKey: true });
  const ipad = { terminal: true, mac: true, hostname: 'mac.tail1234.ts.net' };
  assert.equal(takesImagePaste(ctrlV, ipad), true);
  assert.equal(takesImagePaste(ctrlV, { ...ipad, terminal: false }), false, 'the column has the keys');
  assert.equal(takesImagePaste(ctrlV, { ...ipad, mac: false }), false, "off a Mac ^V is the browser's paste");
  assert.equal(takesImagePaste(ctrlV, { ...ipad, hostname: '127.0.0.1' }), false, 'same machine: Claude Code finds it');
  assert.equal(takesImagePaste(key('KeyV', { metaKey: true }), ipad), false);
});

test('an iPad is iPad, or a "Mac" with a touch screen (iPadOS Safari)', () => {
  assert.equal(isIPad('MacIntel', 5), true);
  assert.equal(isIPad('iPad', 5), true);
  assert.equal(isIPad('MacIntel', 0), false, 'a Mac');
  assert.equal(isIPad('iPhone', 5), false);
  assert.equal(isIPad('Linux armv8l', 5), false);
});
