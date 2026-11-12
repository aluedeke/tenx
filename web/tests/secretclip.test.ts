// Unit tests for src/lib/secretclip.ts: `node --test tests/secretclip.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { describeValue, readSecretClip } from '../src/lib/secretclip.ts';

const fail = async (): Promise<string> => {
  throw new Error('refused');
};

test("the device's clipboard comes first, without its line ending", async () => {
  const clip = await readSecretClip({ device: async () => 'from-the-phone\n', mac: async () => 'from-the-mac' });
  assert.deepEqual(clip, { text: 'from-the-phone', source: 'device' });
});

test("the Mac's when the device's is refused or empty", async () => {
  assert.deepEqual(await readSecretClip({ device: fail, mac: async () => 'from-the-mac' }), { text: 'from-the-mac', source: 'mac' });
  assert.deepEqual(await readSecretClip({ device: async () => '', mac: async () => 'from-the-mac' }), { text: 'from-the-mac', source: 'mac' });
});

test('nothing on either says to copy the value first', async () => {
  await assert.rejects(readSecretClip({ device: async () => '', mac: async () => '\n' }), /Copy the value first/);
  await assert.rejects(readSecretClip({ device: fail, mac: fail }), /Copy the value first/);
});

test('a value is described by its length, and the tail of a long one', () => {
  assert.equal(describeValue('short'), '5 characters');
  assert.equal(describeValue('clip-value-ONE-0123456789-aaaa'), '30 characters, ending …aaaa');
});
