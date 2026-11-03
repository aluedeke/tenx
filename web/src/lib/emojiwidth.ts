// tmux gives every emoji two cells; xterm draws a glyph into the cells its
// width table gives it and clips the rest. The graphemes add-on's table
// (Unicode 15) still makes some newer emojis one cell (🫠, 🪿), so their
// right half was cut off and the next character drawn over it. The
// browser's own Unicode data is current: an emoji it knows, which the table
// makes one cell, gets two.
//
// Pure: wraps the add-on's provider, which the page catches as it registers.

import type { IUnicodeVersionProvider } from '@xterm/xterm';

export const VERSION = 'tenx-emoji';

const EMOJI = /^\p{Emoji_Presentation}$/u;

/** Whether `codepoint` is an emoji shown as a picture by default. Regional
 * indicators are left out: one alone is no flag, a pair is already two. */
export function isWideEmoji(codepoint: number): boolean {
  if (codepoint < 0x2000 || (codepoint >= 0x1f1e6 && codepoint <= 0x1f1ff)) return false;
  return EMOJI.test(String.fromCodePoint(codepoint));
}

// xterm's packed char properties: grapheme state << 3 | width << 1 | join.
const WIDTH = 0b110;

/** `base` with every one-cell emoji made two cells. */
export function widerEmoji(base: IUnicodeVersionProvider): IUnicodeVersionProvider {
  return {
    version: VERSION,
    wcwidth: (codepoint) => {
      const w = base.wcwidth(codepoint);
      return w === 1 && isWideEmoji(codepoint) ? 2 : w;
    },
    charProperties: (codepoint, preceding) => {
      const p = base.charProperties(codepoint, preceding);
      return (p & WIDTH) === 1 << 1 && isWideEmoji(codepoint) ? (p & ~WIDTH) | (2 << 1) : p;
    },
  };
}
