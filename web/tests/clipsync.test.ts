// Unit tests for src/lib/clipsync.ts: `node --test tests/clipsync.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { ClipSync, type Peek, type SyncDeps } from '../src/lib/clipsync.ts';

/** A device clipboard and a Mac clipboard, as the app and tenx web keep them. */
class World implements SyncDeps {
  enabled = true;
  count = 1;
  device = 'old on the device';
  concealed = false;
  blocked = false;
  mac = 'old on the Mac';
  /** Runs inside `write`, before the count check: a copy made meanwhile. */
  beforeWrite: (() => void) | null = null;
  sent: string[] = [];

  copyOnDevice(text: string, concealed = false) {
    this.device = text;
    this.concealed = concealed;
    this.count += 1;
  }

  async peek(since: number | null): Promise<Peek> {
    const peek: Peek = { enabled: this.enabled, count: this.count };
    if (since !== null && since !== this.count) {
      if (this.blocked) peek.blocked = true;
      peek.text = this.blocked || this.concealed ? null : this.device;
    }
    return peek;
  }

  async write(text: string, ifCount: number): Promise<number | null> {
    this.beforeWrite?.();
    this.beforeWrite = null;
    if (this.count !== ifCount) return null;
    this.device = text;
    this.count += 1;
    return this.count;
  }

  async getMac() {
    return this.mac;
  }

  async setMac(text: string) {
    this.mac = text;
    this.sent.push(text);
  }
}

test('the first tick moves nothing that was there before', async () => {
  const w = new World();
  const sync = new ClipSync(w);
  assert.equal(await sync.tick(), 'start');
  assert.equal(await sync.tick(), 'same');
  assert.equal(w.mac, 'old on the Mac');
  assert.equal(w.device, 'old on the device');
});

test('a copy on the device goes to the Mac, once', async () => {
  const w = new World();
  const sync = new ClipSync(w);
  await sync.tick();
  w.copyOnDevice('npm test');
  assert.equal(await sync.tick(), 'to-mac');
  assert.equal(w.mac, 'npm test');
  assert.equal(await sync.tick(), 'same', 'the Mac now has it: nothing comes back');
  assert.deepEqual(w.sent, ['npm test']);
});

test('a copy on the Mac comes to the device, and is not sent back', async () => {
  const w = new World();
  const sync = new ClipSync(w);
  await sync.tick();
  w.mac = 'git status';
  assert.equal(await sync.tick(), 'from-mac');
  assert.equal(w.device, 'git status');
  assert.equal(await sync.tick(), 'same', 'our own write moved the count: no echo');
  assert.deepEqual(w.sent, []);
});

test('a copy on the device made while the Mac text was on its way wins', async () => {
  const w = new World();
  const sync = new ClipSync(w);
  await sync.tick();
  w.mac = 'from the Mac';
  w.beforeWrite = () => w.copyOnDevice('from the device');
  assert.equal(await sync.tick(), 'skipped');
  assert.equal(w.device, 'from the device');
  assert.equal(await sync.tick(), 'to-mac');
  assert.equal(w.mac, 'from the device');
});

test('the same text arriving on the device from elsewhere (Universal Clipboard) is not sent again', async () => {
  const w = new World();
  const sync = new ClipSync(w);
  await sync.tick();
  w.mac = 'shared';
  await sync.tick();
  w.copyOnDevice('shared');
  assert.equal(await sync.tick(), 'skipped');
  assert.deepEqual(w.sent, []);
});

test("a password manager's concealed copy stays on the device", async () => {
  const w = new World();
  const sync = new ClipSync(w);
  await sync.tick();
  w.copyOnDevice('hunter2', true);
  assert.equal(await sync.tick(), 'skipped');
  assert.equal(w.mac, 'old on the Mac');
});

test('a clipboard iOS keeps from the app is reported, not sent', async () => {
  const w = new World();
  const sync = new ClipSync(w);
  await sync.tick();
  w.blocked = true;
  w.copyOnDevice('secret');
  assert.equal(await sync.tick(), 'blocked');
  assert.deepEqual(w.sent, []);
});

test('an empty Mac clipboard never empties the device', async () => {
  const w = new World();
  const sync = new ClipSync(w);
  await sync.tick();
  w.mac = '';
  assert.equal(await sync.tick(), 'same');
  assert.equal(w.device, 'old on the device');
});

test('switched off, nothing moves; switched on again, it starts over', async () => {
  const w = new World();
  const sync = new ClipSync(w);
  await sync.tick();
  w.enabled = false;
  w.copyOnDevice('while off');
  assert.equal(await sync.tick(), 'off');
  w.enabled = true;
  assert.equal(await sync.tick(), 'start');
  assert.equal(await sync.tick(), 'same');
  assert.deepEqual(w.sent, []);
});
