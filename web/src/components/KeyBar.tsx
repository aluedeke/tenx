'use client';

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
  /** A hardware keyboard is attached: no special keys, only 📋 and 📎,
   * floating over the terminal instead of a bar under it. */
  compact?: boolean;
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

export function KeyBar({ column, ctrlSticky, onCtrl, onKey, onImages, onPaste, compact }: Props) {
  // Pointer-down, not click, and no default: a tap mustn't take focus from
  // the terminal (and with it the on-screen keyboard).
  const press = (fn: () => void) => (ev: React.PointerEvent) => {
    ev.preventDefault();
    fn();
  };
  if (compact && column) return null;
  return (
    <div className={compact ? 'keybar compact' : 'keybar'} data-testid="keybar">
      {!compact && (
        <>
          <button type="button" className="key" onPointerDown={press(() => onKey('Escape', false))}>
            esc
          </button>
          {!column && (
            <button type="button" className={ctrlSticky ? 'key on' : 'key'} onPointerDown={press(onCtrl)}>
              ctrl
            </button>
          )}
          {KEYS.slice(1).map((k) => (
            <button
              key={k.key}
              type="button"
              className={k.cls ? `key ${k.cls}` : 'key'}
              onPointerDown={press(() => onKey(k.key, !!k.shift))}
            >
              {k.label}
            </button>
          ))}
          {column && (
            <button type="button" className="key" onPointerDown={press(() => onKey('?', true))}>
              ?
            </button>
          )}
        </>
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
          // the on-screen keyboard. The paste itself runs on the click.
          onPointerDown={(e) => e.preventDefault()}
          onClick={onPaste}
        >
          📋
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
