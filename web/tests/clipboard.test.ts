// Unit tests for src/lib/clipboard.ts: `node --test tests/clipboard.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { choose, type ClipItem } from '../src/lib/clipboard.ts';

const item = (parts: Record<string, string>): ClipItem => ({
  types: Object.keys(parts),
  getType: async (t: string) => new Blob([parts[t]], { type: t }),
});
const strip = (html: string) => html.replace(/<[^>]+>/g, '');

test('plain text is pasted as it is', async () => {
  assert.deepEqual(await choose([item({ 'text/plain': 'hello' })], strip), { kind: 'text', text: 'hello' });
});

test('a page selection that is only HTML is pasted as its text', async () => {
  assert.deepEqual(await choose([item({ 'text/html': '<p>Fix <b>login</b></p>' })], strip), { kind: 'text', text: 'Fix login' });
});

test('text wins over an image copied with it', async () => {
  const both = item({ 'text/plain': 'caption', 'image/png': 'png-bytes' });
  assert.deepEqual(await choose([both], strip), { kind: 'text', text: 'caption' });
});

test('an image alone is uploaded', async () => {
  const clip = await choose([item({ 'image/png': 'png-bytes' })], strip);
  assert.equal(clip?.kind, 'images');
  assert.equal(clip?.kind === 'images' && clip.files[0].type, 'image/png');
});

test('nothing usable is nothing', async () => {
  assert.equal(await choose([item({ 'text/plain': '   ' })], strip), null);
  assert.equal(await choose([], strip), null);
});
