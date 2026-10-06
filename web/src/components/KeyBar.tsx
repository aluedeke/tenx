'use client';

import { useEffect, useRef } from 'react';

// The keys a phone keyboard lacks, above it: Esc, a sticky Ctrl for the next
// key, Tab, arrows, Enter, and A/D to answer the task in front of you. They go
// wherever the keyboard is — the column as key messages, the terminal as bytes.

interface Props {
  /** The column has the keyboard (adds `?`, drops the sticky Ctrl). */
  column: boolean;
  ctrlSticky: boolean;
  onCtrl(): void;
  onKey(key: string, shift: boolean): void;
  /** Images picked from the photo library or the camera. */
  onImages(files: File[]): void;
  /** Paste what's on the clipboard — an image or text. */
  onPaste(): void;
  /** Put the keyboard focus back where it was (the terminal, or the
   * column's text input) — called inside the tap, the only place iOS lets a
   * focus keep its on-screen keyboard up. */
  onRefocus(): void;
  /** A diagnostic line for the server's log. */
  onDiag?(message: string): void;
}

const KEYS: { key: string; label: string; shift?: boolean; cls?: string }[] = [
  { key: 'Escape', label: 'esc' },
  { key: 'Tab', label: '⇥' },
  { key: 'ArrowUp', label: '↑' },
  { key: 'ArrowDown', label: '↓' },
  { key: 'Enter', label: '⏎' },
  { key: 'A', label: 'A', shift: true, cls: 'warn' },
  { key: 'D', label: 'D', shift: true, cls: 'warn' },
];

export function KeyBar({ column, ctrlSticky, onCtrl, onKey, onImages, onPaste, onRefocus, onDiag }: Props) {
  const bar = useRef<HTMLDivElement>(null);
  /** A touch already pasted on lifting; the click after it, if any, mustn't. */
  const pastedByTouch = useRef(false);
  // iOS moves focus off the terminal — and closes the keyboard — on a tap
  // anywhere else, whatever pointerdown does. Cancelling the touch itself
  // stops that; it has to be a native, non-passive listener (React's are
  // passive). Not for paste and 📎, which need their click.
  useEffect(() => {
    const el = bar.current;
    if (!el) return;
    const hold = (ev: TouchEvent) => {
      if ((ev.target as Element | null)?.closest('[data-keep-focus]')) ev.preventDefault();
    };
    el.addEventListener('touchstart', hold, { passive: false });
    el.addEventListener('touchend', hold, { passive: false });
    return () => {
      el.removeEventListener('touchstart', hold);
      el.removeEventListener('touchend', hold);
    };
  }, []);
  // Pointer-down, not click, and no default: a tap mustn't take focus from
  // the terminal (and with it the on-screen keyboard).
  const press = (fn: () => void) => (ev: React.PointerEvent) => {
    ev.preventDefault();
    fn();
    onRefocus();
  };
  return (
    <div ref={bar} className="keybar" data-testid="keybar">
      <button type="button" className="key" data-keep-focus onPointerDown={press(() => onKey('Escape', false))}>
        esc
      </button>
      {!column && (
        <button type="button" className={ctrlSticky ? 'key on' : 'key'} data-keep-focus onPointerDown={press(onCtrl)}>
          ctrl
        </button>
      )}
      {KEYS.slice(1).map((k) => (
        <button
          key={k.key}
          type="button"
          className={k.cls ? `key ${k.cls}` : 'key'}
          data-keep-focus
          onPointerDown={press(() => onKey(k.key, !!k.shift))}
        >
          {k.label}
        </button>
      ))}
      {column && (
        <button type="button" className="key" data-keep-focus onPointerDown={press(() => onKey('?', true))}>
          ?
        </button>
      )}
      {!column && (
        // A click, not a pointer-down: reading the clipboard needs the
        // gesture to have completed (iOS then shows its own Paste bubble).
        <button
          type="button"
          className="key"
          aria-label="paste"
          data-testid="paste"
          // No default on the press: the terminal keeps focus, and with it
          // the on-screen keyboard. That also means iOS fires no click for a
          // touch, so a finger pastes on lifting — still the tap the
          // clipboard read needs — and a mouse on the click.
          onPointerDown={(e) => {
            e.preventDefault();
            onDiag?.(`paste: pointerdown (${e.pointerType})`);
          }}
          onPointerUp={(e) => {
            if (e.pointerType === 'mouse') return;
            pastedByTouch.current = true;
            onDiag?.(`paste: pointerup (${e.pointerType})`);
            onPaste();
          }}
          onClick={() => {
            // The click that may still follow a touch: already pasted.
            if (pastedByTouch.current) {
              pastedByTouch.current = false;
              return;
            }
            onRefocus();
            onPaste();
          }}
        >
          <PasteIcon />
        </button>
      )}
      {!column && (
        // A label, not a pointer-down button: the file picker only opens from
        // a real click on its input.
        <label className="key" aria-label="attach an image" data-testid="attach">
          📎
          <input
            type="file"
            accept="image/*"
            multiple
            hidden
            onChange={(e) => {
              const files = Array.from(e.currentTarget.files ?? []);
              e.currentTarget.value = '';
              if (files.length) onImages(files);
            }}
          />
        </label>
      )}
    </div>
  );
}

/** Paste, not copy: a clipboard with an arrow going into it. Drawn rather
 * than an emoji — 📋 alone reads as "copy". */
function PasteIcon() {
  return (
    <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M9 4H7a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V6a2 2 0 0 0-2-2h-2" />
      <rect x="9" y="2.5" width="6" height="3.5" rx="1" />
      <path d="M12 9.5v7" />
      <path d="M9 13.5l3 3 3-3" />
    </svg>
  );
}
