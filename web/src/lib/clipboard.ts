// What the key bar's paste key pastes from a clipboard read
// (`navigator.clipboard.read()`): text when there is any — copied text or
// a web page's selection — and an image only when nothing textual is there.
// A page selection on iOS can come as `text/html` alone, so HTML counts as
// text (its readable text, not the markup). Pure apart from the HTML
// conversion, which the caller supplies, so it tests without a DOM.

export interface ClipItem {
  types: readonly string[];
  getType(type: string): Promise<Blob>;
}

export type Clip = { kind: 'text'; text: string } | { kind: 'images'; files: File[] } | null;

export async function choose(items: ClipItem[], htmlToText: (html: string) => string): Promise<Clip> {
  let text = '';
  for (const item of items) {
    if (item.types.includes('text/plain')) text += await (await item.getType('text/plain')).text();
    else if (item.types.includes('text/html')) text += htmlToText(await (await item.getType('text/html')).text());
  }
  if (text.trim()) return { kind: 'text', text };
  const files: File[] = [];
  for (const item of items) {
    const image = item.types.find((t) => t.startsWith('image/'));
    if (image) files.push(new File([await item.getType(image)], 'clipboard', { type: image }));
  }
  return files.length ? { kind: 'images', files } : null;
}

/** A web page's HTML as the text it shows, for pasting into a terminal. */
export function htmlText(html: string): string {
  const doc = new DOMParser().parseFromString(html, 'text/html');
  return (doc.body?.innerText ?? doc.body?.textContent ?? '').trim();
}
