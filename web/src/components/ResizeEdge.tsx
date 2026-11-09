'use client';

// The edge of a panel beside the terminal as a handle: the sidebar's left
// edge, the task list's right edge (desktop and iPad). Dragging it sets the
// panel's width. A panel shows a line where the edge will land, with a label,
// and is resized once, on release, so the terminal beside it is refitted
// once; a drawer, over the terminal, follows the drag (`onLive`). A
// double-click or double-tap goes back to the default. On touch the edge
// carries a grip a finger can hold. Lives inside the panel, which is
// `position: relative`.

import { useRef, useState, type PointerEvent as ReactPointerEvent, type ReactNode, type RefObject } from 'react';
import type { Drag } from '@/lib/sidebarfit';

interface Props {
  /** Which edge of the panel: the sidebar's left, the task list's right. */
  edge: 'left' | 'right';
  panel: RefObject<HTMLElement | null>;
  /** Drawn over the terminal: no line, the panel itself follows. */
  drawer: boolean;
  touch: boolean;
  /** Tooltip, and the test ids' prefix (`<testid>-resize`, `-ghost`). */
  title: string;
  testid: string;
  /** Where a drag to `want` px lands. */
  dragTo(want: number): Drag;
  /** The line's label for a drag. */
  label(d: Drag): ReactNode;
  /** A drawer's width while dragged; null when the drag ends. */
  onLive?(d: Drag | null): void;
  /** A drag ended here; null (a double-click or double-tap): the default. */
  onDone(d: Drag | null): void;
}

/** Two taps on the handle this close together are a double-tap. */
const DOUBLE_TAP_MS = 350;

export function ResizeEdge({ edge, panel, drawer, touch, title, testid, dragTo, label, onLive, onDone }: Props) {
  const start = useRef<{ x: number; width: number; moved: boolean } | null>(null);
  const lastTap = useRef(0);
  const [drag, setDrag] = useState<Drag | null>(null);

  const track = (d: Drag | null) => {
    setDrag(d);
    if (drawer) onLive?.(d);
  };
  const onDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (e.button !== 0 || !panel.current) return;
    e.preventDefault();
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      // Not an active pointer (a synthetic event): moves still reach the handle.
    }
    start.current = { x: e.clientX, width: panel.current.getBoundingClientRect().width, moved: false };
  };
  const onMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    const s = start.current;
    if (!s) return;
    if (Math.abs(e.clientX - s.x) < 3 && !s.moved) return;
    s.moved = true;
    const by = edge === 'left' ? s.x - e.clientX : e.clientX - s.x;
    track(dragTo(s.width + by));
  };
  const onUp = () => {
    const s = start.current;
    start.current = null;
    if (!s) return;
    if (s.moved) {
      lastTap.current = 0;
      if (drag) onDone(drag);
      track(null);
      return;
    }
    // Two taps without a move: back to the default.
    const now = Date.now();
    if (now - lastTap.current < DOUBLE_TAP_MS) {
      lastTap.current = 0;
      onDone(null);
    } else {
      lastTap.current = now;
    }
  };
  const onCancel = () => {
    start.current = null;
    track(null);
  };

  const current = panel.current?.getBoundingClientRect().width ?? 0;
  // The line, in the panel's own coordinates.
  const at = drag && (edge === 'left' ? current - drag.width - 1 : drag.width - 1);
  return (
    <>
      <div
        className={['edge-resize', `at-${edge}`, touch && 'touch', drag && 'hot'].filter(Boolean).join(' ')}
        data-testid={`${testid}-resize`}
        title={title}
        onPointerDown={onDown}
        onPointerMove={onMove}
        onPointerUp={onUp}
        onPointerCancel={onCancel}
      >
        {touch && (
          <span className="edge-grip">
            <i />
            <i />
          </span>
        )}
      </div>
      {drag && !drawer && (
        <div className={`edge-ghost at-${edge}${drag.stop ? ' stop' : ''}`} style={{ left: at ?? 0 }} data-testid={`${testid}-ghost`}>
          <div className="edge-tip">{label(drag)}</div>
        </div>
      )}
      {drag && drawer && <div className={`edge-tip drawer at-${edge}`}>{drag.width} px</div>}
    </>
  );
}
