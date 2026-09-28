'use client';

// What a row can do, for the pointer: the ⋯ / right-click menu (desktop), the
// long-press sheet (touch), and the swipe that reveals allow / deny or delete.
// Every entry is a list key the server already has (`action` messages); the
// row is selected first with a `task` click, so the key acts on it.

import { useEffect, useRef, useState, type PointerEvent as RPointerEvent, type ReactNode } from 'react';
import type { Action, JobItem, SubItem, TaskItem } from '@/protocol';

export interface Entry {
  label: string;
  /** The key the entry stands for, shown beside it. */
  key: string;
  run(): void;
  /** Destructive: red, after a separator. */
  danger?: boolean;
  /** Sheet only: the answer buttons and the wide primary one. */
  tone?: 'ok' | 'no' | 'pri';
  wide?: boolean;
}

/** `act(a)` selects the row and presses `a`'s key. */
export function taskEntries(task: TaskItem, act: (a: Action) => void, answers: boolean): Entry[] {
  if (task.pending) return [];
  const out: Entry[] = [];
  if (answers && task.answerable) {
    out.push({ label: '✓ approve', key: 'A', tone: 'ok', run: () => act('approve') });
    out.push({ label: '✕ deny', key: 'D', tone: 'no', run: () => act('deny') });
  }
  out.push({ label: 'go to task', key: '⏎', tone: 'pri', wide: true, run: () => act('open') });
  if (task.locked) out.push({ label: 'unlock secrets', key: 'u', wide: true, run: () => act('unlock') });
  out.push({ label: 'rename', key: 'r', run: () => act('rename') });
  out.push({ label: 'edit repos', key: 'e', run: () => act('edit_repos') });
  if (!task.closed) out.push({ label: 'close window', key: 'x', run: () => act('close') });
  out.push({ label: 'delete…', key: 'dd', danger: true, run: () => act('delete') });
  return out;
}

export function subEntries(_sub: SubItem, act: (a: Action) => void): Entry[] {
  return [
    { label: 'open agent', key: '⏎', tone: 'pri', wide: true, run: () => act('open') },
    { label: 'transcript', key: 't', wide: true, run: () => act('transcript') },
  ];
}

export function jobEntries(job: JobItem, act: (a: Action) => void): Entry[] {
  return job.state === 'running' ? [] : [{ label: 'dismiss', key: 'dd', run: () => act('delete') }];
}

// ── Popover and sheet ─────────────────────────────────────────────────────

export interface Popup {
  entries: Entry[];
  /** Menu: where its top-left goes (viewport px). */
  at?: { left: number; top: number };
  /** Sheet: its header. */
  head?: { glyph: string; color: string; title: string; detail: string };
}

const MENU_W = 218;

/** Open while shown; closes on an outside press, on Escape, and after an
 * entry runs. `data-popup` tells the page to leave keys alone meanwhile. */
export function PopupLayer({ popup, onClose }: { popup: Popup | null; onClose(): void }) {
  useEffect(() => {
    if (!popup) return;
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key === 'Escape') {
        ev.preventDefault();
        onClose();
      }
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [popup, onClose]);
  if (!popup) return null;
  const run = (e: Entry) => () => {
    onClose();
    e.run();
  };
  if (popup.head) {
    const { head } = popup;
    return (
      <div className="popup-layer" data-popup="sheet">
        <div className="dim" onPointerDown={onClose} />
        <div className="sheet" data-testid="sheet" role="menu">
          <div className="handle" />
          <div className="shead">
            <span className="bright bold">
              <span style={{ color: head.color }}>{head.glyph}</span> {head.title}
            </span>
            {head.detail && <span className="muted">{head.detail}</span>}
          </div>
          <div className="sgrid">
            {popup.entries.map((e) => (
              <button
                key={e.label}
                type="button"
                role="menuitem"
                className={['sbtn', e.tone, e.danger && 'dan', e.wide && 'wide'].filter(Boolean).join(' ')}
                onClick={run(e)}
              >
                {e.label}
              </button>
            ))}
          </div>
        </div>
      </div>
    );
  }
  const at = popup.at ?? { left: 8, top: 8 };
  const height = popup.entries.length * 27 + 22;
  const left = Math.max(8, Math.min(at.left, window.innerWidth - MENU_W - 8));
  const top = Math.max(8, Math.min(at.top, window.innerHeight - height - 8));
  const plain = popup.entries.filter((e) => !e.danger);
  const danger = popup.entries.filter((e) => e.danger);
  const item = (e: Entry) => (
    <button key={e.label} type="button" role="menuitem" className={e.danger ? 'mi dan' : 'mi'} onClick={run(e)}>
      <span>{e.label}</span>
      <span className="k">{e.key}</span>
    </button>
  );
  return (
    <div className="popup-layer" data-popup="menu">
      <div className="backdrop" onPointerDown={onClose} onContextMenu={(e) => e.preventDefault()} />
      <div className="menu" data-testid="menu" role="menu" style={{ left, top, width: MENU_W }}>
        {plain.map(item)}
        {plain.length > 0 && danger.length > 0 && <div className="msep" />}
        {danger.map(item)}
      </div>
    </div>
  );
}

// ── Touch gestures on a row ───────────────────────────────────────────────

/** How far a row slides to show its buttons, and how far a swipe must go to
 * allow outright (2d). */
const LEFT_OPEN = -168;
const RIGHT_OPEN = 84;
const FULL_SWIPE = -230;
const LONG_PRESS_MS = 500;

export type Reveal = 'left' | 'right' | null;

interface GestureOpts {
  /** Swipe left shows allow / deny. */
  left: boolean;
  /** Swipe right shows delete. */
  right: boolean;
  reveal: Reveal;
  setReveal(r: Reveal): void;
  onFullSwipe(): void;
  onLongPress(): void;
}

/** Touch-only: horizontal drags slide the row (vertical ones stay the
 * list's scroll — the row is `touch-action: pan-y`), a hold opens the sheet.
 * A click that ends either gesture is swallowed. */
export function useRowGestures(o: GestureOpts) {
  const [drag, setDrag] = useState<number | null>(null);
  const g = useRef<{ x: number; y: number; base: number; sliding: boolean; timer: ReturnType<typeof setTimeout> | null } | null>(null);
  const swallow = useRef(false);
  const opts = useRef(o);
  opts.current = o;

  const base = o.reveal === 'left' ? LEFT_OPEN : o.reveal === 'right' ? RIGHT_OPEN : 0;
  const offset = drag ?? base;

  const end = () => {
    if (g.current?.timer) clearTimeout(g.current.timer);
    g.current = null;
  };

  const handlers = {
    onPointerDown(e: RPointerEvent<HTMLElement>) {
      if (e.pointerType === 'mouse') return;
      const timer = setTimeout(() => {
        if (!g.current || g.current.sliding) return;
        swallow.current = true;
        g.current.timer = null;
        opts.current.onLongPress();
      }, LONG_PRESS_MS);
      g.current = { x: e.clientX, y: e.clientY, base, sliding: false, timer };
    },
    onPointerMove(e: RPointerEvent<HTMLElement>) {
      const s = g.current;
      if (!s) return;
      const dx = e.clientX - s.x;
      const dy = e.clientY - s.y;
      if (!s.sliding) {
        if (Math.abs(dx) > 10 || Math.abs(dy) > 10) {
          if (s.timer) clearTimeout(s.timer);
          s.timer = null;
        }
        const { left, right } = opts.current;
        if (Math.abs(dx) > 12 && Math.abs(dx) > Math.abs(dy) * 1.5 && (left || right || s.base !== 0)) {
          s.sliding = true;
          try {
            e.currentTarget.setPointerCapture(e.pointerId);
          } catch {
            // A synthetic pointer (tests) can't be captured; the drag still works.
          }
        } else return;
      }
      const min = opts.current.left ? -280 : 0;
      const max = opts.current.right ? 120 : 0;
      setDrag(Math.max(min, Math.min(max, s.base + dx)));
    },
    onPointerUp() {
      const s = g.current;
      end();
      if (!s?.sliding) return;
      swallow.current = true;
      const at = drag ?? s.base;
      setDrag(null);
      const { left, right, setReveal, onFullSwipe } = opts.current;
      if (left && at <= FULL_SWIPE) {
        setReveal(null);
        onFullSwipe();
      } else if (left && at <= LEFT_OPEN / 2) setReveal('left');
      else if (right && at >= RIGHT_OPEN * 0.7) setReveal('right');
      else setReveal(null);
    },
    onPointerCancel() {
      end();
      setDrag(null);
    },
    onClickCapture(e: React.MouseEvent) {
      if (swallow.current) {
        swallow.current = false;
        e.stopPropagation();
        e.preventDefault();
        return;
      }
      // A tap on a slid-open row closes it rather than selecting.
      if (opts.current.reveal) {
        e.stopPropagation();
        opts.current.setReveal(null);
      }
    },
  };
  return { offset, dragging: drag !== null, handlers };
}

/** A row that can slide: its content over the buttons it uncovers. */
export function Slide({
  offset,
  dragging,
  left,
  right,
  children,
}: {
  offset: number;
  dragging: boolean;
  left?: ReactNode;
  right?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="swrap">
      {offset < 0 && left && <div className="under">{left}</div>}
      {offset > 0 && right && <div className="under left">{right}</div>}
      <div className="slid" style={{ transform: offset ? `translateX(${offset}px)` : undefined, transition: dragging ? 'none' : undefined }}>
        {children}
      </div>
    </div>
  );
}
