// The clipboard of the machine `tenx web` runs on (POST /clipboard/get,
// /clipboard/image and /clipboard/set), for the key bar's two clipboard keys
// — this device's clipboard to the Mac's, and the Mac's to this device's —
// and for the iOS app's sync (lib/clipsync). Text or an image.

import { apiUrl } from './api.ts';
import type { ClipImage, MacClip } from './clipsync.ts';

/** The Mac's clipboard has nothing to bring over. */
export class EmptyClipboard extends Error {
  constructor() {
    super("the Mac's clipboard is empty");
  }
}

export type MacKind = 'text' | 'image';

/** The Mac's clipboard holds the other kind than the copy asked for: a
 * browser must be told which kind it writes before the Mac has answered
 * (`copyFromMac`), so the caller starts over with `kind`. */
export class OtherKind extends Error {
  readonly kind: MacKind;
  constructor(kind: MacKind) {
    super(`the Mac's clipboard holds ${kind === 'image' ? 'an image' : 'text'}`);
    this.kind = kind;
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

/** Put `image` on the Mac's clipboard; the id the Mac names it by. */
export async function sendImageToMac(image: ClipImage): Promise<string> {
  const res = await fetch(apiUrl('/clipboard/set'), {
    method: 'POST',
    body: image.bytes,
    headers: { 'content-type': image.type },
    credentials: 'include',
  });
  if (!res.ok) throw new Error((await res.text()) || `failed: ${res.status}`);
  return ((await res.json()) as { id: string }).id;
}

/** What the Mac's clipboard holds: its text, or an image by name. */
export async function peekMac(): Promise<MacClip> {
  const res = await fetch(apiUrl('/clipboard/get'), { method: 'POST', credentials: 'include' });
  if (!res.ok) throw new Error((await res.text()) || `failed: ${res.status}`);
  return (await res.json()) as MacClip;
}

/** The image `peekMac` named `id`; null when the Mac's clipboard holds
 * something else by now. */
export async function fetchMacImage(id: string): Promise<ClipImage | null> {
  const res = await fetch(apiUrl(`/clipboard/image?id=${encodeURIComponent(id)}`), { method: 'POST', credentials: 'include' });
  if (res.status === 404) return null;
  if (!res.ok) throw new Error((await res.text()) || `failed: ${res.status}`);
  const type = (res.headers.get('content-type') ?? 'image/png').split(';')[0].trim();
  return { type, bytes: new Uint8Array(await res.arrayBuffer()) };
}

/** The text on the Mac's clipboard; `EmptyClipboard` when it holds
 * nothing, `OtherKind` when it holds an image. */
export async function fetchFromMac(): Promise<string> {
  const { text, image } = await peekMac();
  if (image) throw new OtherKind('image');
  if (!text) throw new EmptyClipboard();
  return text;
}

/** The image on the Mac's clipboard, as PNG: the one image format every
 * browser writes to a clipboard. `OtherKind` when it holds text. */
export async function fetchPngFromMac(png: (image: Blob) => Promise<Blob> = toPng): Promise<Blob> {
  const { text, image } = await peekMac();
  if (!image) throw text ? new OtherKind('text') : new EmptyClipboard();
  const got = await fetchMacImage(image.id);
  if (!got) throw new Error("the Mac's clipboard changed meanwhile");
  return png(new Blob([got.bytes], { type: got.type }));
}

/** An image as PNG, drawn by the browser. */
export async function toPng(image: Blob): Promise<Blob> {
  if (image.type === 'image/png') return image;
  const bitmap = await createImageBitmap(image);
  const canvas = document.createElement('canvas');
  canvas.width = bitmap.width;
  canvas.height = bitmap.height;
  canvas.getContext('2d')?.drawImage(bitmap, 0, 0);
  return new Promise((resolve, reject) =>
    canvas.toBlob((blob) => (blob ? resolve(blob) : reject(new Error("the browser couldn't draw the image"))), 'image/png'),
  );
}

/** The part of the Clipboard API `copyFromMac` writes with. */
export interface WritableClipboard {
  write?(items: ClipboardItem[]): Promise<void>;
  writeText(text: string): Promise<void>;
}

/** Start the write now and let it wait for `content`. When it fails because
 * `content` did, that reason says more than the browser's. */
async function writeWaiting(clipboard: WritableClipboard, type: string, content: Promise<Blob>): Promise<void> {
  try {
    await clipboard.write!([new ClipboardItem({ [type]: content })]);
  } catch (e) {
    await content;
    throw e;
  }
}

/** The Mac's clipboard text onto this device's; the number of characters.
 * Called inside the tap: Safari allows a clipboard write only there, and the
 * fetch would outlast it — so where the browser takes a promise in a
 * `ClipboardItem`, the write starts now and waits for the text. */
export async function copyFromMac(
  clipboard: WritableClipboard = navigator.clipboard,
  fetchText: () => Promise<string> = fetchFromMac,
): Promise<number> {
  const text = fetchText();
  if (clipboard.write && typeof ClipboardItem !== 'undefined') {
    await writeWaiting(
      clipboard,
      'text/plain',
      text.then((t) => new Blob([t], { type: 'text/plain' })),
    );
  } else {
    await clipboard.writeText(await text);
  }
  return (await text).length;
}

/** The same for the Mac's image; its size in bytes. */
export async function copyImageFromMac(
  clipboard: WritableClipboard = navigator.clipboard,
  fetchPng: () => Promise<Blob> = fetchPngFromMac,
): Promise<number> {
  const png = fetchPng();
  if (!clipboard.write || typeof ClipboardItem === 'undefined') {
    // No write, but still the right answer about what the Mac holds.
    await png;
    throw new Error('this browser writes no images to the clipboard');
  }
  await writeWaiting(clipboard, 'image/png', png);
  return (await png).size;
}
