// Images for the agent. The browser's clipboard (or photo library) is on
// another device than the agent, so an image is uploaded to `tenx web`
// (POST /paste), saved on its machine, and its path is pasted into the
// terminal — Claude Code attaches a pasted image path as an image.

import { apiUrl } from './api';

/** The images among a clipboard's or a drop's items. */
export function imagesIn(data: DataTransfer | null): File[] {
  if (!data) return [];
  const files: File[] = [];
  for (const item of Array.from(data.items ?? [])) {
    if (item.kind === 'file' && item.type.startsWith('image/')) {
      const f = item.getAsFile();
      if (f) files.push(f);
    }
  }
  if (files.length === 0) {
    for (const f of Array.from(data.files ?? [])) if (f.type.startsWith('image/')) files.push(f);
  }
  return files;
}

/** Upload one image; the path it was saved at on the agent's machine. */
export async function upload(file: Blob): Promise<string> {
  const res = await fetch(apiUrl('/paste'), {
    method: 'POST',
    body: file,
    headers: { 'content-type': file.type || 'application/octet-stream' },
    credentials: 'include',
  });
  if (!res.ok) throw new Error((await res.text()) || `paste failed: ${res.status}`);
  const { path } = (await res.json()) as { path: string };
  return path;
}

/** A path as it should be typed: quoted if it has anything a shell or an
 * agent's path detection would split on. */
export function asTyped(path: string): string {
  return /^[\w./~+-]+$/.test(path) ? path : `'${path.replace(/'/g, `'\\''`)}'`;
}
