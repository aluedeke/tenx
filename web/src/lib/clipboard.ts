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
