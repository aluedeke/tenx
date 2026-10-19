// Text that arrives without key presses — iOS dictation, predictive-text
// suggestions, autocorrect — doesn't type: it rewrites the text field, and
// dictation rewrites its whole guess on every word ("fix the" → "fix the
// login" → "fix the login timeout"). Forwarding each rewrite as new input
// repeats the sentence per word. What the program behind the terminal needs
// is the edit between two states of the field: so many backspaces, then the
// new characters.
//
// Pure, for the unit tests.

export interface Edit {
  /** Characters to erase from the end of what was sent before. */
  erase: number;
  /** What to type after erasing. */
  insert: string;
}

/** The edit that turns `before` into `after`, keeping their common start
 * (by code point, so an emoji is one backspace). */
export function textEdit(before: string, after: string): Edit {
  const a = Array.from(before);
  const b = Array.from(after);
  let common = 0;
  while (common < a.length && common < b.length && a[common] === b[common]) common++;
  return { erase: a.length - common, insert: b.slice(common).join('') };
}

/** The same, but never erasing into the first `keep` characters of
 * `before` — text the user has typed after since, or moved the cursor
 * into. Words of `after` up to as many as that kept text has are taken as
 * its version of it, and only what follows them is typed. */
export function keptEdit(before: string, after: string, keep: number): Edit {
  const edit = textEdit(before, after);
  const a = Array.from(before);
  if (a.length - edit.erase >= keep) return edit;
  const words = a.slice(0, keep).join('').split(/\s+/).filter(Boolean).length;
  // After's text from the end of its `words`-th word: " login".
  let at = 0;
  for (let n = 0; n < words; n++) {
    const m = /\S+/.exec(after.slice(at));
    if (!m) {
      at = after.length;
      break;
    }
    at += m.index + m[0].length;
  }
  return { erase: a.length - keep, insert: after.slice(at) };
}

/** `value` with `edit` applied at its end (by code point). */
export function applyEdit(value: string, edit: Edit): string {
  const chars = Array.from(value);
  return chars.slice(0, Math.max(0, chars.length - edit.erase)).join('') + edit.insert;
}
