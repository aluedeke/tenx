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

export function KeyBar({ column, ctrlSticky, onCtrl, onKey }: Props) {
  // Pointer-down, not click, and no default: a tap mustn't take focus from
  // the terminal (and with it the on-screen keyboard).
  const press = (fn: () => void) => (ev: React.PointerEvent) => {
    ev.preventDefault();
    fn();
  };
  return (
    <div className="keybar" data-testid="keybar">
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
    </div>
  );
}
