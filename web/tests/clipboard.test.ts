// Unit tests for src/lib/clipboard.ts: `node --test tests/clipboard.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { choose, TerminalClipboard, type ClipItem } from '../src/lib/clipboard.ts';

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

const fakeClipboard = (initial = '') => {
  const state = { text: initial };
  return { state, clipboard: { readText: async () => state.text, writeText: async (t: string) => void (state.text = t) } };
};

test('a copy with no target (how tmux copies) reaches the clipboard', async () => {
  const { state, clipboard } = fakeClipboard();
  await new TerminalClipboard(() => clipboard).writeText('', 'from nvim');
  assert.equal(state.text, 'from nvim');
});

test('a copy to the clipboard target still reaches it', async () => {
  const { state, clipboard } = fakeClipboard();
  await new TerminalClipboard(() => clipboard).writeText('c', 'from claude');
  assert.equal(state.text, 'from claude');
});

test('a program reads the clipboard only when it asks for c', async () => {
  const { clipboard } = fakeClipboard('secret');
  const provider = new TerminalClipboard(() => clipboard);
  assert.equal(await provider.readText('c'), 'secret');
  assert.equal(await provider.readText(''), '');
});
