// The iOS app's clipboard, kept in step with the Mac's while the page is on
// screen: a copy on either side lands on the other within a tick, with no
// key to press. The app reads and writes UIPasteboard itself (no Paste
// bubble; lib/native), the page carries the text to and from the Mac
// (lib/macclip). Text only, like /clipboard/get and /set.
//
// Each tick asks the app whether the device's clipboard changed (its change
// count, which reads nothing), and only then for the text; else it asks the
// Mac. A text is sent on only when it differs from the last one synced, so
// the two sides never echo each other — nor Apple's Universal Clipboard,
// where it still runs.

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
  /** iOS kept the clipboard from the app. */
  blocked?: boolean;
}

export interface SyncDeps {
  /** The device's clipboard: the text only if the count moved since `since`. */
  peek(since: number | null): Promise<Peek>;
  /** Write `text` to the device's clipboard unless its count moved past
   * `ifCount` (a copy made meanwhile wins); the new count, or null. */
  write(text: string, ifCount: number): Promise<number | null>;
  /** The Mac's clipboard text ('' for none). */
  getMac(): Promise<string>;
  setMac(text: string): Promise<void>;
}

/** What one tick did, for the diagnostics log. */
export type Step = 'off' | 'start' | 'same' | 'to-mac' | 'from-mac' | 'skipped' | 'blocked';

export class ClipSync {
  /** The device's change count last seen; null before the first tick. */
  private count: number | null = null;
  /** The text last known on both sides. */
  private last: string | null = null;
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
      // Whatever is on either side now was there before the page: leave both.
      this.count = peek.count;
      this.last = await this.deps.getMac();
      return 'start';
    }
    if (peek.count !== this.count) {
      this.count = peek.count;
      if (peek.blocked) return 'blocked';
      const text = peek.text;
      if (typeof text !== 'string' || text === '' || text === this.last) return 'skipped';
      await this.deps.setMac(text);
      this.last = text;
      return 'to-mac';
    }
    const mac = await this.deps.getMac();
    if (mac === '' || mac === this.last) return 'same';
    const count = await this.deps.write(mac, this.count);
    // A copy on the device came first: the next tick sends that instead.
    if (count === null) return 'skipped';
    this.count = count;
    this.last = mac;
    return 'from-mac';
  }
}
