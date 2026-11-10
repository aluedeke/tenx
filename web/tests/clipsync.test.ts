// Unit tests for src/lib/clipsync.ts: `node --test tests/clipsync.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { ClipSync, MAX_IMAGE_BYTES, imageId, type ClipImage, type MacClip, type Peek, type SyncDeps } from '../src/lib/clipsync.ts';

function png(...bytes: number[]): ClipImage {
  return { type: 'image/png', bytes: new Uint8Array(bytes) };
}

/** A device clipboard and a Mac clipboard, as the app and tenx web keep them. */
class World implements SyncDeps {
  enabled = true;
  count = 1;
  device: string | ClipImage = 'old on the device';
  /** The text a copy put next to its image (a file name, an address). */
  deviceText = '';
  concealed = false;
  blocked = false;
  mac: string | ClipImage = 'old on the Mac';
  /** The Mac says an image is this big, whatever its bytes. */
  macSize: number | null = null;
  /** Runs inside `write`, before the count check: a copy made meanwhile. */
  beforeWrite: (() => void) | null = null;
  /** The app can't take an image (one from before images). */
  refusesImages = false;
  sent: (string | ClipImage)[] = [];
  fetched = 0;

  copyOnDevice(clip: string | ClipImage, concealed = false, text = '') {
    this.device = clip;
    this.deviceText = text;
    this.concealed = concealed;
    this.count += 1;
  }

  async peek(since: number | null): Promise<Peek> {
    const peek: Peek = { enabled: this.enabled, count: this.count };
    if (since !== null && since !== this.count) {
      if (this.blocked) peek.blocked = true;
      const hidden = this.blocked || this.concealed;
      if (typeof this.device === 'string') {
        peek.text = hidden ? null : this.device;
      } else {
        peek.text = hidden ? null : this.deviceText || null;
        if (!hidden) peek.image = this.device;
      }
    }
    return peek;
  }

  async write(clip: string | ClipImage, ifCount: number): Promise<number | null> {
    this.beforeWrite?.();
    this.beforeWrite = null;
    if (typeof clip !== 'string' && this.refusesImages) throw new Error('no text');
    if (this.count !== ifCount) return null;
    this.device = clip;
    this.count += 1;
    return this.count;
  }

  async getMac(): Promise<MacClip> {
    if (typeof this.mac === 'string') return { text: this.mac };
    const bytes = this.macSize ?? this.mac.bytes.length;
    return { text: '', image: { id: await imageId(this.mac.bytes), type: this.mac.type, bytes } };
  }

  async getMacImage(id: string) {
    this.fetched += 1;
    return typeof this.mac !== 'string' && (await imageId(this.mac.bytes)) === id ? this.mac : null;
  }

  async setMac(text: string) {
    this.mac = text;
    this.sent.push(text);
  }

  async setMacImage(image: ClipImage) {
    this.mac = image;
    this.sent.push(image);
    return imageId(image.bytes);
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

test('an image is named as the Mac names it: the start of its SHA-256', async () => {
  assert.equal(await imageId(new TextEncoder().encode('abc')), 'ba7816bf8f01cfea414140de5dae2223');
});

test('an image copied on the device goes to the Mac, once', async () => {
  const w = new World();
  const sync = new ClipSync(w);
  await sync.tick();
  w.copyOnDevice(png(1, 2, 3));
  assert.equal(await sync.tick(), 'to-mac');
  assert.deepEqual(w.mac, png(1, 2, 3));
  assert.equal(await sync.tick(), 'same', 'the Mac now has it: nothing comes back');
  assert.equal(w.sent.length, 1);
  assert.equal(w.fetched, 0);
});

test('an image copied on the Mac comes to the device, and is not sent back', async () => {
  const w = new World();
  const sync = new ClipSync(w);
  await sync.tick();
  w.mac = png(4, 5, 6);
  assert.equal(await sync.tick(), 'from-mac');
  assert.deepEqual(w.device, png(4, 5, 6));
  assert.equal(await sync.tick(), 'same');
  assert.equal(w.fetched, 1, 'its bytes are fetched once, not every tick');
  assert.deepEqual(w.sent, []);
});

test('an image that was on the Mac before the page stays there', async () => {
  const w = new World();
  w.mac = png(7);
  const sync = new ClipSync(w);
  assert.equal(await sync.tick(), 'start');
  assert.equal(await sync.tick(), 'same');
  assert.equal(w.device, 'old on the device');
  assert.equal(w.fetched, 0);
});

test('the same image arriving on the device from elsewhere (Universal Clipboard) is not sent again', async () => {
  const w = new World();
  const sync = new ClipSync(w);
  await sync.tick();
  w.mac = png(8, 9);
  await sync.tick();
  w.copyOnDevice(png(8, 9));
  assert.equal(await sync.tick(), 'skipped');
  assert.deepEqual(w.sent, []);
});

test('text that only names the image leaves the image; text of its own wins', async () => {
  const w = new World();
  const sync = new ClipSync(w);
  await sync.tick();
  w.copyOnDevice(png(1), false, 'IMG_0042.png');
  assert.equal(await sync.tick(), 'to-mac');
  assert.deepEqual(w.mac, png(1));
  w.copyOnDevice(png(2), false, 'Q1\t12');
  assert.equal(await sync.tick(), 'to-mac');
  assert.equal(w.mac, 'Q1\t12');
});

test('text after an image, and the image again, both go over', async () => {
  const w = new World();
  const sync = new ClipSync(w);
  await sync.tick();
  w.mac = png(1);
  assert.equal(await sync.tick(), 'from-mac');
  w.mac = 'words';
  assert.equal(await sync.tick(), 'from-mac');
  assert.equal(w.device, 'words');
  w.mac = png(1);
  assert.equal(await sync.tick(), 'from-mac');
  assert.deepEqual(w.device, png(1));
});

test('an image too big to carry is skipped without fetching it', async () => {
  const w = new World();
  const sync = new ClipSync(w);
  await sync.tick();
  w.mac = png(1);
  w.macSize = MAX_IMAGE_BYTES + 1;
  assert.equal(await sync.tick(), 'skipped');
  assert.equal(await sync.tick(), 'same');
  assert.equal(w.fetched, 0);
  assert.equal(w.device, 'old on the device');
});

test('an image the app refuses is not fetched again every tick', async () => {
  const w = new World();
  w.refusesImages = true;
  const sync = new ClipSync(w);
  await sync.tick();
  w.mac = png(1);
  await assert.rejects(sync.tick(), /no text/);
  assert.equal(await sync.tick(), 'same');
  assert.equal(w.fetched, 1);
});

test("an image whose bytes couldn't be fetched is fetched again", async () => {
  const w = new World();
  const sync = new ClipSync(w);
  await sync.tick();
  w.mac = png(1);
  const fetch = w.getMacImage.bind(w);
  w.getMacImage = async () => Promise.reject(new Error('502'));
  await assert.rejects(sync.tick(), /502/);
  w.getMacImage = fetch;
  assert.equal(await sync.tick(), 'from-mac');
  assert.deepEqual(w.device, png(1));
});

test('a first tick that could not ask the Mac starts again, and moves nothing old', async () => {
  const w = new World();
  const sync = new ClipSync(w);
  const getMac = w.getMac.bind(w);
  w.getMac = async () => Promise.reject(new Error('offline'));
  await assert.rejects(sync.tick(), /offline/);
  w.getMac = getMac;
  assert.equal(await sync.tick(), 'start');
  assert.equal(await sync.tick(), 'same');
  assert.equal(w.device, 'old on the device');
});

test('a copy that failed to reach the Mac is sent again, but not forever', async () => {
  const w = new World();
  const sync = new ClipSync(w);
  await sync.tick();
  const setMac = w.setMac.bind(w);
  let tries = 0;
  w.setMac = async () => {
    tries += 1;
    throw new Error('offline');
  };
  w.copyOnDevice('once more');
  await assert.rejects(sync.tick(), /offline/);
  w.setMac = setMac;
  assert.equal(await sync.tick(), 'to-mac');
  assert.equal(w.mac, 'once more');
  // A Mac that keeps refusing: three tries, then the copy is let go.
  w.setMac = async () => {
    tries += 1;
    throw new Error('refused');
  };
  tries = 0;
  w.copyOnDevice(png(9));
  w.setMacImage = w.setMac as never;
  for (let i = 0; i < 3; i++) await assert.rejects(sync.tick(), /refused/);
  assert.equal(await sync.tick(), 'same');
  assert.equal(tries, 3);
});

test('an image gone from the Mac before its bytes were fetched is left for the next tick', async () => {
  const w = new World();
  const sync = new ClipSync(w);
  await sync.tick();
  w.mac = png(1);
  w.getMacImage = async () => null;
  assert.equal(await sync.tick(), 'skipped');
  w.mac = 'copied since';
  assert.equal(await sync.tick(), 'from-mac');
  assert.equal(w.device, 'copied since');
});
