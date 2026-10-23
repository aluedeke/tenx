'use client';

import { useEffect, useRef } from 'react';
import { MicIcon, formatSeconds, type MicState } from './Header';

// The keys a phone keyboard lacks, above it: Esc, a sticky Ctrl for the next
// key, Tab, arrows, Enter, `?` (the column's help). They go wherever the keyboard is — the column as
// key messages, the terminal as bytes. Then the clipboard: this device's to
// the Mac's (→⌘), the Mac's to this device's (←⌘), paste into the terminal.

interface Props {
  /** The column has the keyboard: ctrl, paste, the microphone and 📎 are
   * off (still there, so no key moves). */
  column: boolean;
  ctrlSticky: boolean;
  onCtrl(): void;
  onKey(key: string, shift: boolean): void;
  /** The id of the page's file input for images from the photo library or
   * the camera (kept outside the bar, which may go while picking). */
  attachInput: string;
  /** Paste what's on the clipboard — an image or text. */
  onPaste(): void;
  /** This device's clipboard onto the Mac's. */
  onToMac(): void;
  /** The Mac's clipboard onto this device's. */
  onFromMac(): void;
  /** Put the keyboard focus back where it was (the terminal, or the
   * column's text input) — called inside the tap, the only place iOS lets a
   * focus keep its on-screen keyboard up. */
  onRefocus(): void;
  /** A diagnostic line for the server's log. */
  onDiag?(message: string): void;
  mic: MicState;
  micSeconds: number;
  /** Start or stop the recording (the header's microphone). */
  onMic(): void;
}

/** The page's file input the 📎 opens. */
export const ATTACH_INPUT = 'kb-attach';

const KEYS: { key: string; label: string }[] = [
  { key: 'Escape', label: 'esc' },
  { key: 'Tab', label: '⇥' },
  { key: 'ArrowUp', label: '↑' },
  { key: 'ArrowDown', label: '↓' },
  { key: 'Enter', label: '⏎' },
];

export function KeyBar({ column, ctrlSticky, onCtrl, onKey, attachInput, onPaste, onToMac, onFromMac, onRefocus, onDiag, mic, micSeconds, onMic }: Props) {
  const bar = useRef<HTMLDivElement>(null);
  /** The microphone was started by a touch; the click after it, if any, mustn't. */
  const micByTouch = useRef(false);
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
  // the terminal (and with it the on-screen keyboard). A key that is off
  // does nothing — the touch is still cancelled, so the focus stays.
  const press = (fn: () => void, on = true) => (ev: React.PointerEvent) => {
    ev.preventDefault();
    if (!on) return;
    flash(ev.currentTarget);
    fn();
    onRefocus();
  };
  // Every key, always, in the same place: which keys work changes with the
  // focus, the bar doesn't.
  return (
    <div ref={bar} className="keybar" data-testid="keybar">
      <button type="button" className="key" data-keep-focus onPointerDown={press(() => onKey('Escape', false))}>
        esc
      </button>
      <button
        type="button"
        className={ctrlSticky && !column ? 'key on' : 'key'}
        data-keep-focus
        aria-disabled={column}
        aria-pressed={ctrlSticky && !column}
        onPointerDown={press(onCtrl, !column)}
      >
        ctrl
      </button>
      {KEYS.slice(1).map((k) => (
        <button key={k.key} type="button" className="key" data-keep-focus onPointerDown={press(() => onKey(k.key, false))}>
          {k.label}
        </button>
      ))}
      <button type="button" className="key" data-keep-focus onPointerDown={press(() => onKey('?', true))}>
        ?
      </button>
      <ClipboardKey label="copy to the Mac" testId="to-mac" onDiag={onDiag} onRefocus={onRefocus} onTap={onToMac}>
        →⌘
      </ClipboardKey>
      <ClipboardKey label="copy from the Mac" testId="from-mac" onDiag={onDiag} onRefocus={onRefocus} onTap={onFromMac}>
        ←⌘
      </ClipboardKey>
      <ClipboardKey label="paste" testId="paste" off={column} onDiag={onDiag} onRefocus={onRefocus} onTap={onPaste}>
        <PasteIcon />
      </ClipboardKey>
      {/* Keeps the keyboard like the keys around it (data-keep-focus: the
          touch is cancelled, so iOS moves no focus), so the same button
          stops the recording. A finger acts on lifting — a completed tap,
          which iOS wants before it opens the microphone. Off in the column,
          unless a recording is running: its stop button is here. */}
      {(() => {
        const off = mic === 'transcribing' || (column && mic === 'idle');
        return (
          <button
            type="button"
            className={`key mic ${mic}`}
            data-keep-focus
            aria-label={mic === 'recording' ? 'stop and type what you said' : 'speak instead of typing'}
            aria-pressed={mic === 'recording'}
            aria-disabled={off}
            data-testid="kb-mic"
            onPointerDown={(e) => {
              e.preventDefault();
              if (!off) flash(e.currentTarget);
            }}
            onPointerUp={(e) => {
              if (e.pointerType === 'mouse' || off) return;
              micByTouch.current = true;
              onRefocus();
              onMic();
            }}
            onClick={() => {
              if (micByTouch.current) {
                micByTouch.current = false;
                return;
              }
              if (!off) onMic();
            }}
          >
            {mic === 'recording' ? (
              <>
                <span className="micdot" />
                {formatSeconds(micSeconds)}
              </>
            ) : mic === 'transcribing' ? (
              '…'
            ) : (
              <MicIcon />
            )}
          </button>
        );
      })()}
      {/* A label, not a pointer-down button: the file picker only opens from
          a real click on its input. Off in the column: no click reaches it. */}
      <label
        className="key"
        aria-label="attach an image"
        aria-disabled={column}
        data-testid="attach"
        htmlFor={column ? undefined : attachInput}
        onPointerDown={(e) => {
          if (!column) flash(e.currentTarget);
        }}
      >
        📎
      </label>
    </div>
  );
}

/** A short highlight on a pressed key: the keys cancel their touch (so the
 * focus stays), and with it iOS's own :active look. */
function flash(el: Element) {
  el.classList.remove('pressed');
  void (el as HTMLElement).offsetWidth;
  el.classList.add('pressed');
  window.setTimeout(() => el.classList.remove('pressed'), 150);
}

/** A key that reads or writes the clipboard, which needs a completed tap —
 * a click, not a pointer-down (iOS then shows its own Paste bubble). No
 * default on the press: the terminal keeps focus, and with it the on-screen
 * keyboard. That also means iOS fires no click for a touch, so a finger acts
 * on lifting — still the tap the clipboard wants — and a mouse on the click. */
function ClipboardKey({
  label,
  testId,
  off = false,
  onTap,
  onRefocus,
  onDiag,
  children,
}: {
  label: string;
  testId: string;
  /** Shown, but does nothing. */
  off?: boolean;
  onTap(): void;
  onRefocus(): void;
  onDiag?(message: string): void;
  children: React.ReactNode;
}) {
  /** A touch already acted on lifting; the click after it, if any, mustn't. */
  const byTouch = useRef(false);
  return (
    <button
      type="button"
      className="key"
      aria-label={label}
      title={label}
      data-testid={testId}
      aria-disabled={off}
      onPointerDown={(e) => {
        e.preventDefault();
        if (off) return;
        flash(e.currentTarget);
        onDiag?.(`${testId}: pointerdown (${e.pointerType})`);
      }}
      onPointerUp={(e) => {
        if (e.pointerType === 'mouse' || off) return;
        byTouch.current = true;
        onDiag?.(`${testId}: pointerup (${e.pointerType})`);
        onTap();
      }}
      onClick={() => {
        if (byTouch.current) {
          byTouch.current = false;
          return;
        }
        if (off) return;
        onRefocus();
        onTap();
      }}
    >
      {children}
    </button>
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
