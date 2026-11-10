// The iOS app's clipboard, kept in step with the Mac's while the page is on
// screen: a copy on either side lands on the other within a tick, with no
// key to press. The app reads and writes UIPasteboard itself (no Paste
// bubble; lib/native), the page carries the text or the image to and from
// the Mac (lib/macclip). An image goes over when the copy has no text of
// its own, as the paste key chooses (lib/clipboard).
//
// Each tick asks the app whether the device's clipboard changed (its change
// count, which reads nothing), and only then for what it holds; else it asks
// the Mac, which names an image by its content and hands over the bytes
// only when asked. A copy is sent on only when it differs from the last one
// synced, so the two sides never echo each other — nor Apple's Universal
// Clipboard, where it still runs.

import { namesImage } from './clipboard.ts';

/** The largest image carried over (the server's PASTE_MAX_BYTES). */
export const MAX_IMAGE_BYTES = 25 * 1024 * 1024;

/** How often a copy that failed to reach the Mac is sent again. */
const SEND_RETRIES = 2;

/** An image on a clipboard: PNG, JPEG, GIF or WebP. */
export interface ClipImage {
  type: string;
  bytes: Uint8Array<ArrayBuffer>;
}

/** What the Mac's clipboard holds (POST /clipboard/get): its text ('' for
 * none), or an image named by its content. */
export interface MacClip {
  text: string;
  image?: { id: string; type: string; bytes: number };
}

/** How the Mac names an image (tenx_core `clip_image_id`): the start of its
 * SHA-256, as hex. */
export async function imageId(bytes: Uint8Array<ArrayBuffer>): Promise<string> {
  const hash = new Uint8Array(await crypto.subtle.digest('SHA-256', bytes));
  return Array.from(hash.subarray(0, 16), (b) => b.toString(16).padStart(2, '0')).join('');
}

/** What the app says about the device's clipboard (lib/native `nativePeek`). */
export interface Peek {
  /** The sync is switched on in the app's settings. */
  enabled: boolean;
  /** UIPasteboard's change count. */
  count: number;
  /** The text, when `count` moved since the count asked about: null when
   * there is none to send — no text, a password manager's concealed item,
   * or iOS keeping it from the app ("Paste from Other Apps: Ask"). */
  text?: string | null;
  /** The image, when `count` moved and the clipboard holds one. */
  image?: ClipImage | null;
  /** iOS kept the clipboard from the app. */
  blocked?: boolean;
}

export interface SyncDeps {
  /** The device's clipboard: what it holds only if the count moved since `since`. */
  peek(since: number | null): Promise<Peek>;
  /** Write `clip` to the device's clipboard unless its count moved past
   * `ifCount` (a copy made meanwhile wins); the new count, or null. */
  write(clip: string | ClipImage, ifCount: number): Promise<number | null>;
  getMac(): Promise<MacClip>;
  /** The bytes of the image `getMac` named; null when the Mac's clipboard
   * holds something else by now. */
  getMacImage(id: string): Promise<ClipImage | null>;
  setMac(text: string): Promise<void>;
  /** The id the Mac names the image by from now on. */
  setMacImage(image: ClipImage): Promise<string>;
}

/** What one tick did, for the diagnostics log. */
export type Step = 'off' | 'start' | 'same' | 'to-mac' | 'from-mac' | 'skipped' | 'blocked';

/** A copy as `last` remembers it; null for a Mac clipboard with nothing to
 * bring over. */
function macKey(mac: MacClip): string | null {
  if (mac.image) return `i:${mac.image.id}`;
  return mac.text === '' ? null : `t:${mac.text}`;
}

export class ClipSync {
  /** The device's change count last seen; null before the first tick. */
  private count: number | null = null;
  /** The copy last known on both sides (`macKey`). */
  private last: string | null = null;
  /** How often the copy now on the device failed to reach the Mac. */
  private retries = 0;
  private readonly deps: SyncDeps;

  constructor(deps: SyncDeps) {
    this.deps = deps;
  }

  async tick(): Promise<Step> {
    const peek = await this.deps.peek(this.count);
    if (!peek.enabled) {
      // Switched back on later: start over, sending nothing old.
      this.count = null;
      return 'off';
    }
    if (this.count === null) {
      // Whatever is on either side now was there before the page: leave
      // both. The Mac first: while it can't be asked, nothing has started.
      this.last = macKey(await this.deps.getMac());
      this.count = peek.count;
      return 'start';
    }
    if (peek.count !== this.count) {
      const before = this.count;
      this.count = peek.count;
      if (peek.blocked) return 'blocked';
      try {
        const step = await this.toMac(peek);
        this.retries = 0;
        return step;
      } catch (e) {
        // The Mac out of reach for a moment: the copy is asked for again,
        // a few times — not an upload every second for as long as it fails.
        if (this.retries < SEND_RETRIES) {
          this.retries += 1;
          this.count = before;
        } else {
          this.retries = 0;
        }
        throw e;
      }
    }
    const mac = await this.deps.getMac();
    const key = macKey(mac);
    if (key === null || key === this.last) return 'same';
    let count: number | null;
    if (mac.image) {
      if (mac.image.bytes > MAX_IMAGE_BYTES) {
        this.last = key;
        return 'skipped';
      }
      const image = await this.deps.getMacImage(mac.image.id);
      // Something else was copied on the Mac: the next tick brings that.
      if (!image) return 'skipped';
      try {
        count = await this.deps.write(image, this.count);
      } catch (e) {
        // An app that can't take the image (one from before images): it is
        // not fetched again every second.
        this.last = key;
        throw e;
      }
    } else {
      count = await this.deps.write(mac.text, this.count);
    }
    // A copy on the device came first: the next tick sends that instead.
    if (count === null) return 'skipped';
    this.count = count;
    this.last = key;
    return 'from-mac';
  }

  /** A new copy on the device, sent to the Mac unless it is the one last
   * synced or there is nothing to send. */
  private async toMac(peek: Peek): Promise<Step> {
    const text = typeof peek.text === 'string' ? peek.text : '';
    // Text of its own wins; a file name or an address only names the image.
    if (peek.image && (!text.trim() || namesImage(text))) {
      if (peek.image.bytes.length > MAX_IMAGE_BYTES) return 'skipped';
      if (`i:${await imageId(peek.image.bytes)}` === this.last) return 'skipped';
      this.last = `i:${await this.deps.setMacImage(peek.image)}`;
      return 'to-mac';
    }
    if (text === '' || `t:${text}` === this.last) return 'skipped';
    await this.deps.setMac(text);
    this.last = `t:${text}`;
    return 'to-mac';
  }
}
