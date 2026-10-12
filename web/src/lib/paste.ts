// Images for the agent. The browser's clipboard (or photo library) is on
// another device than the agent, so an image is uploaded to `tenx web`
// (POST /paste), saved on its machine, and its path is pasted into the
// terminal — Claude Code attaches a pasted image path as an image.

import { apiUrl } from './api';

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
