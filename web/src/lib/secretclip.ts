// A secret value from a clipboard, for the secrets form's Paste button: the
// clipboard of the device the user holds — the iOS app reads UIPasteboard
// itself (lib/native), a browser asks (`navigator.clipboard.readText`) —
// and the Mac's (tenx web's own, lib/macclip) only when the device's can't
// be read or holds no text. On a phone that is where the value was copied;
// the Mac's would be whatever the clipboard sync last carried over.

import { choose, htmlText } from './clipboard.ts';
import { nativeApp, nativeClipboard } from './native.ts';
import { fetchFromMac } from './macclip.ts';

export type ClipSource = 'device' | 'mac';

export interface SecretClip {
  text: string;
  source: ClipSource;
}

/** The ways to read each clipboard; the defaults are the real ones. */
export interface ClipReaders {
  device(): Promise<string>;
  mac(): Promise<string>;
}

/** The value a copy holds: one trailing line ending dropped, as the
 * terminal's prompt does (`clean_typed_value`). */
export function valueOf(text: string): string {
  return text.replace(/\r?\n$/, '');
}

export async function readSecretClip(readers: ClipReaders = defaultReaders): Promise<SecretClip> {
  try {
    const text = valueOf(await readers.device());
    if (text) return { text, source: 'device' };
  } catch {
    // Refused or not supported: the Mac's next.
  }
  let text: string;
  try {
    text = valueOf(await readers.mac());
  } catch {
    throw new Error('The clipboard holds no text. Copy the value first.');
  }
  if (!text) throw new Error('The clipboard holds no text. Copy the value first.');
  return { text, source: 'mac' };
}

/** What the form shows under a value: its length, and the last four
 * characters of a long one (`describe_value` in tenx-core). */
export function describeValue(value: string): string {
  const chars = [...value];
  const n = chars.length;
  return n >= 16 ? `${n} characters, ending …${chars.slice(n - 4).join('')}` : `${n} characters`;
}

const defaultReaders: ClipReaders = {
  async device() {
    if (nativeApp()) {
      const clip = await choose(await nativeClipboard(), htmlText);
      return clip?.kind === 'text' ? clip.text : '';
    }
    if (!navigator.clipboard?.readText) throw new Error('no clipboard read');
    return navigator.clipboard.readText();
  },
  mac: fetchFromMac,
};
