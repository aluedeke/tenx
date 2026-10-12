import { test } from 'node:test';
import assert from 'node:assert/strict';
import { clipItems } from '../src/lib/native.ts';
import { choose } from '../src/lib/clipboard.ts';

test('text from the app pastes as text', async () => {
  const items = clipItems([{ types: ['text/plain'], data: { 'text/plain': 'hello' } }]);
  assert.deepEqual(await choose(items, (h) => h), { kind: 'text', text: 'hello' });
});

test('an image from the app comes back as its bytes', async () => {
  const bytes = [0x89, 0x50, 0x4e, 0x47];
  const b64 = btoa(String.fromCharCode(...bytes));
  const items = clipItems([{ types: ['image/png', 'text/plain'], data: { 'image/png': b64, 'text/plain': 'IMG_1.png' } }]);
  const clip = await choose(items, (h) => h);
  assert.equal(clip?.kind, 'images');
  if (clip?.kind !== 'images') return;
  assert.equal(clip.files[0].type, 'image/png');
  assert.deepEqual([...new Uint8Array(await clip.files[0].arrayBuffer())], bytes);
});

test('a type the item lacks is an error', async () => {
  const [item] = clipItems([{ types: [], data: {} }]);
  await assert.rejects(item.getType('text/plain'));
});
