'use client';

// The paste key's fallback: a real text field to paste into. iOS refuses a
// page's clipboard read of text copied in another app (`NotAllowedError`,
// even after its Paste bubble was tapped), and a press and hold on the
// terminal opens no menu — the terminal is a drawing, not a field. A field
// always takes iOS's own Paste (and ⌘V, dictation, the password keyboard).
// Text goes to the terminal as one paste; an image pasted here goes up like
// one from the paste key.

import { useEffect, useRef, useState } from 'react';
import { imagesIn } from '@/lib/clipboard';

interface Props {
  /** Why the box opened instead of pasting at once, if it did. */
  why?: string;
  onText(text: string): void;
  onImages(files: File[]): void;
  onClose(): void;
}

export function PasteBox({ why, onText, onImages, onClose }: Props) {
  const ref = useRef<HTMLTextAreaElement>(null);
  const [text, setText] = useState('');
  // Outside a tap iOS raises no keyboard for this, but the field is ready
  // for a press and hold; a desktop browser can type into it at once.
  useEffect(() => ref.current?.focus({ preventScroll: true }), []);
  const send = () => {
    if (!text) return;
    onClose();
    onText(text);
  };
  return (
    <div className="popup-layer" data-popup="paste">
      <div className="dim" onPointerDown={onClose} />
      <form
        className="wf webform pastebox"
        data-testid="paste-box"
        onSubmit={(e) => {
          e.preventDefault();
          send();
        }}
        onKeyDown={(e) => {
          if (e.key === 'Escape') {
            e.preventDefault();
            e.stopPropagation();
            onClose();
          } else if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
            e.preventDefault();
            send();
          }
        }}
      >
        <div className="wf-top">
          <span className="wf-title">paste</span>
          <span className="grow" />
          <button type="button" className="wf-x" aria-label="cancel" onClick={onClose}>
            ✕
          </button>
        </div>
        <section className="wf-sec">
          <textarea
            ref={ref}
            className="wf-in wf-area"
            aria-label="text to paste"
            placeholder="press and hold here, then Paste"
            value={text}
            autoComplete="off"
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
            onChange={(e) => setText(e.currentTarget.value)}
            onPaste={(e) => {
              const files = imagesIn(e.clipboardData);
              if (!files.length) return;
              e.preventDefault();
              onClose();
              onImages(files);
            }}
          />
          <div className="wf-hint">{why ?? 'goes to the terminal as typed, without Enter'}</div>
        </section>
        <div className="wf-bar">
          <button type="button" className="wf-btn ghost" onClick={onClose}>
            Cancel <span className="k">esc</span>
          </button>
          <button type="submit" className="wf-btn pri" disabled={!text}>
            Paste <span className="k">⌘⏎</span>
          </button>
        </div>
      </form>
    </div>
  );
}
