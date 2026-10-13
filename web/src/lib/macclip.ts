// The clipboard of the machine `tenx web` runs on (POST /clipboard/get and
// /clipboard/set), for the key bar's two clipboard keys: this device's
// clipboard to the Mac's, and the Mac's to this device's. Text only.

import { apiUrl } from './api.ts';

/** The Mac's clipboard has no text to bring over. */
export class EmptyClipboard extends Error {
  constructor() {
    super("the Mac's clipboard has no text");
  }
}

/** Put `text` on the Mac's clipboard. */
export async function sendToMac(text: string): Promise<void> {
  const res = await fetch(apiUrl('/clipboard/set'), {
    method: 'POST',
    body: text,
    headers: { 'content-type': 'text/plain; charset=utf-8' },
    credentials: 'include',
  });
  if (!res.ok) throw new Error((await res.text()) || `failed: ${res.status}`);
}

/** The text on the Mac's clipboard; `EmptyClipboard` when there is none. */
export async function fetchFromMac(): Promise<string> {
  const res = await fetch(apiUrl('/clipboard/get'), { method: 'POST', credentials: 'include' });
  if (!res.ok) throw new Error((await res.text()) || `failed: ${res.status}`);
  const { text } = (await res.json()) as { text: string };
  if (!text) throw new EmptyClipboard();
  return text;
}

/** The part of the Clipboard API `copyFromMac` writes with. */
export interface WritableClipboard {
  write?(items: ClipboardItem[]): Promise<void>;
  writeText(text: string): Promise<void>;
}

/** The Mac's clipboard onto this device's; the number of characters.
 * Called inside the tap: Safari allows a clipboard write only there, and the
 * fetch would outlast it — so where the browser takes a promise in a
 * `ClipboardItem`, the write starts now and waits for the text. */
export async function copyFromMac(
  clipboard: WritableClipboard = navigator.clipboard,
  fetchText: () => Promise<string> = fetchFromMac,
): Promise<number> {
  const text = fetchText();
  if (clipboard.write && typeof ClipboardItem !== 'undefined') {
    const blob = text.then((t) => new Blob([t], { type: 'text/plain' }));
    await clipboard.write([new ClipboardItem({ 'text/plain': blob })]);
  } else {
    await clipboard.writeText(await text);
  }
  return (await text).length;
}
