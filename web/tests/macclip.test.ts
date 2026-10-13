// Unit tests for src/lib/macclip.ts: `node --test tests/macclip.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { copyFromMac, type WritableClipboard } from '../src/lib/macclip.ts';

class FakeItem {
  parts: Record<string, Promise<Blob>>;
  constructor(parts: Record<string, Promise<Blob>>) {
    this.parts = parts;
  }
}

test('the write starts at once and waits for the text, where a ClipboardItem takes a promise', async () => {
  (globalThis as { ClipboardItem?: unknown }).ClipboardItem = FakeItem;
  let release!: (t: string) => void;
  const pending = new Promise<string>((r) => (release = r));
  let written: Promise<Blob> | undefined;
  const clipboard: WritableClipboard = {
    write: async ([item]) => {
      written = (item as unknown as FakeItem).parts['text/plain'];
      await written;
    },
    writeText: async () => assert.fail('writeText is the fallback'),
  };
  const done = copyFromMac(clipboard, () => pending);
  // Called inside the tap, before the text has arrived.
  assert.ok(written, 'write was called before the fetch finished');
  release('grüße ✓');
  assert.equal(await done, 7);
  assert.equal(await (await written!).text(), 'grüße ✓');
  delete (globalThis as { ClipboardItem?: unknown }).ClipboardItem;
});

test('without ClipboardItem the text is written once it is there', async () => {
  let got = '';
  const clipboard: WritableClipboard = { writeText: async (t) => void (got = t) };
  assert.equal(await copyFromMac(clipboard, async () => 'hello'), 5);
  assert.equal(got, 'hello');
});

test('a failed fetch fails the write and writes nothing', async () => {
  let wrote = false;
  const clipboard: WritableClipboard = { writeText: async () => void (wrote = true) };
  await assert.rejects(copyFromMac(clipboard, async () => Promise.reject(new Error('502'))), /502/);
  assert.equal(wrote, false);
});
