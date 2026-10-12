// What the key bar's paste key pastes from a clipboard read
// (`navigator.clipboard.read()`): text when there is any — copied text or
// a web page's selection — and an image only when nothing textual is there,
// or when the text only names the image (its address or file name: what
// iOS's Copy puts next to an image from Safari or the share sheet).
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
  const files = await images(items);
  if (text.trim() && !(files.length && namesImage(text))) return { kind: 'text', text };
  return files.length ? { kind: 'images', files } : null;
}

/** The images on a clipboard, whatever text came with them. */
export async function images(items: ClipItem[]): Promise<File[]> {
  const files: File[] = [];
  for (const item of items) {
    const image = item.types.find((t) => t.startsWith('image/'));
    if (image) files.push(new File([await item.getType(image)], 'clipboard', { type: image }));
  }
  return files;
}

/** One word that is a web address or an image's file name. */
export function namesImage(text: string): boolean {
  const t = text.trim();
  if (!t || /\s/.test(t)) return false;
  return /^[a-z][\w+.-]*:\/\//i.test(t) || /\.(png|jpe?g|gif|heic|heif|webp|tiff?|bmp)$/i.test(t);
}

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

/** What a paste event carries: its images (uploaded, their paths typed),
 * else its text, else nothing. */
export function pasteOf(data: DataTransfer | null): { kind: 'images'; files: File[] } | { kind: 'text'; text: string } | null {
  const files = imagesIn(data);
  if (files.length) return { kind: 'images', files };
  const text = data?.getData('text/plain') ?? '';
  return text ? { kind: 'text', text } : null;
}

/** A web page's HTML as the text it shows, for pasting into a terminal. */
export function htmlText(html: string): string {
  const doc = new DOMParser().parseFromString(html, 'text/html');
  return (doc.body?.innerText ?? doc.body?.textContent ?? '').trim();
}

/** The part of `navigator.clipboard` the terminal's copy needs. */
export interface TextClipboard {
  readText(): Promise<string>;
  writeText(text: string): Promise<void>;
}

/** Where an OSC 52 copy goes: the one clipboard a browser has, whatever
 * target the program named. tmux names none (`ESC ]52;;<text>`) when it
 * copies a selection, and the addon's own provider keeps only `c` — so a
 * copy in nvim or a shell was dropped. Reads stay `c`-only: a program asking
 * for the clipboard gets it only when it asked for exactly that. */
export class TerminalClipboard {
  private clipboard: () => TextClipboard;

  constructor(clipboard: () => TextClipboard = () => navigator.clipboard) {
    this.clipboard = clipboard;
  }

  readText(selection: string): Promise<string> {
    return selection === 'c' ? this.clipboard().readText() : Promise.resolve('');
  }

  writeText(_selection: string, text: string): Promise<void> {
    return this.clipboard().writeText(text);
  }
}
