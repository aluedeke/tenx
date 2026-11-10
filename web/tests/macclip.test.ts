// Unit tests for src/lib/macclip.ts: `node --test tests/macclip.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { OtherKind, copyFromMac, copyImageFromMac, type WritableClipboard } from '../src/lib/macclip.ts';

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

test('an image is written as PNG, the write started before it arrived', async () => {
  (globalThis as { ClipboardItem?: unknown }).ClipboardItem = FakeItem;
  let release!: (b: Blob) => void;
  const pending = new Promise<Blob>((r) => (release = r));
  let written: Promise<Blob> | undefined;
  const clipboard: WritableClipboard = {
    write: async ([item]) => {
      written = (item as unknown as FakeItem).parts['image/png'];
      await written;
    },
    writeText: async () => assert.fail('an image is no text'),
  };
  const done = copyImageFromMac(clipboard, () => pending);
  assert.ok(written, 'write was called before the fetch finished');
  release(new Blob([new Uint8Array([1, 2, 3])], { type: 'image/png' }));
  assert.equal(await done, 3);
  delete (globalThis as { ClipboardItem?: unknown }).ClipboardItem;
});

test('the Mac holding the other kind is what the failed write reports, not the browser\'s error', async () => {
  (globalThis as { ClipboardItem?: unknown }).ClipboardItem = FakeItem;
  const clipboard: WritableClipboard = {
    // As a browser does when the promise it was handed rejects.
    write: async ([item]) => {
      await Object.values((item as unknown as FakeItem).parts)[0].catch(() => {});
      throw new DOMException('write failed', 'NotAllowedError');
    },
    writeText: async () => assert.fail('writeText is the fallback'),
  };
  await assert.rejects(
    copyFromMac(clipboard, () => Promise.reject(new OtherKind('image'))),
    (e) => e instanceof OtherKind && e.kind === 'image',
  );
  await assert.rejects(
    copyImageFromMac(clipboard, () => Promise.reject(new OtherKind('text'))),
    (e) => e instanceof OtherKind && e.kind === 'text',
  );
  delete (globalThis as { ClipboardItem?: unknown }).ClipboardItem;
});

test('a browser that writes no images still learns what the Mac holds', async () => {
  const clipboard: WritableClipboard = { writeText: async () => {} };
  await assert.rejects(copyImageFromMac(clipboard, async () => new Blob([])), /writes no images/);
  await assert.rejects(copyImageFromMac(clipboard, () => Promise.reject(new OtherKind('text'))), OtherKind);
});
