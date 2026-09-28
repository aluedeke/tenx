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
