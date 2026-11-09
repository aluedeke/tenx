'use client';

// The sidebar beside the terminal: the current task's todos, its PRs (a dot
// for the state, a square per check, the review, the comments), Linear
// issues with their status, what was made with Claude, Slack messages, the
// repos' git state and other links — `view.sidebar`, built by the server
// (tenx_core::sidebar). A panel right of the terminal on a wide page; where
// there is no room, a sheet over it from the right, the task list's mirror.
// A tap on an entry opens its link.
//
// Its left edge is a handle (desktop and iPad): dragging it sets the width.
// A panel shows a line where the edge will land and resizes once, on
// release, so the terminal beside it is refitted once; a drawer, over the
// terminal, follows the drag. A double-click or double-tap goes back to
// the default.

import { useRef, useState, type PointerEvent as ReactPointerEvent } from 'react';
import type { LinkView, SidebarEntry, SidebarView } from '@/protocol';
import { linkHref } from '@/lib/links';
import { DRAWER_MARGIN, SIDEBAR_PX, type Drag } from '@/lib/sidebarfit';

const KIND_COLOR: Partial<Record<LinkView['kind'], string>> = {
  PR: 'var(--success)',
  artifact: 'var(--accent)',
  design: 'var(--accent)',
  linear: 'var(--current)',
  slack: 'var(--warn)',
  port: 'var(--info)',
  web: 'var(--muted)',
};

interface Props {
  view: SidebarView | null;
  /** Drawn over the terminal: a header with a ×, and a tap on a link puts
   * it away. */
  overlay: boolean;
  onClose(): void;
  /** The dragged width, null for the default (globals.css). */
  width: number | null;
  /** Whether the edge is a handle (not on a phone). */
  resizable: boolean;
  /** A touch screen: the handle shows a grip a finger can hold. */
  touch: boolean;
  /** Where a drag to `want` px lands (lib/sidebarfit `dragTo`). */
  dragTo(want: number): Drag;
  /** A drag ended at `px`; null: back to the default. */
  onWidth(px: number | null): void;
}

/** Two taps on the handle this close together are a double-tap. */
const DOUBLE_TAP_MS = 350;

export function Sidebar({ view, overlay, onClose, width, resizable, touch, dragTo, onWidth }: Props) {
  const aside = useRef<HTMLElement>(null);
  const start = useRef<{ x: number; width: number; moved: boolean } | null>(null);
  const lastTap = useRef(0);
  const [drag, setDrag] = useState<Drag | null>(null);

  const onDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (e.button !== 0 || !aside.current) return;
    e.preventDefault();
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      // Not an active pointer (a synthetic event): moves still reach the handle.
    }
    start.current = { x: e.clientX, width: aside.current.getBoundingClientRect().width, moved: false };
  };
  const onMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    const s = start.current;
    if (!s) return;
    if (Math.abs(e.clientX - s.x) < 3 && !s.moved) return;
    s.moved = true;
    setDrag(dragTo(s.width + s.x - e.clientX));
  };
  const onUp = () => {
    const s = start.current;
    start.current = null;
    if (!s) return;
    if (s.moved) {
      lastTap.current = 0;
      if (drag) onWidth(drag.width);
      setDrag(null);
      return;
    }
    // Two taps without a move: back to the default.
    const now = Date.now();
    if (now - lastTap.current < DOUBLE_TAP_MS) {
      lastTap.current = 0;
      onWidth(null);
    } else {
      lastTap.current = now;
    }
  };
  const onCancel = () => {
    start.current = null;
    setDrag(null);
  };

  // A drawer follows the drag; a panel keeps its width until the release.
  const shown = drag && overlay ? drag.width : width;
  const current = aside.current?.getBoundingClientRect().width ?? 0;
  const style = shown === null ? undefined : { width: overlay ? `min(${shown}px, calc(100vw - ${DRAWER_MARGIN}px))` : `${shown}px` };
  const entry = (e: SidebarEntry, i: number) => {
    const color = e.icon_tone ? `var(--tone-${e.icon_tone})` : (e.kind && KIND_COLOR[e.kind]) || 'var(--muted)';
    const state = (e.checks?.length || e.parts?.length) && (
      <span className="sb-detail">
        {!!e.checks?.length && (
          <span className="sb-checks" aria-label={`${e.checks.length} checks`}>
            {e.checks.map((t, j) => (
              <i key={j} className={`sq tone-bg-${t}`} />
            ))}
          </span>
        )}
        {e.parts?.map((p, j) => (
          <span key={j} className={p.pill ? `sb-pill pill-${p.tone}` : `tone-${p.tone}`}>
            {p.text}
          </span>
        ))}
      </span>
    );
    const body = (
      <>
        <span className="sb-icon" style={{ color }}>
          {e.icon}
        </span>
        <span className="sb-text">
          <span className="sb-label">{e.label}</span>
          {state}
        </span>
      </>
    );
    if (!e.url) {
      return (
        <div key={i} className="sb-entry">
          {body}
        </div>
      );
    }
    const href = linkHref({ url: e.url, kind: e.kind ?? 'web' } as LinkView, location.hostname);
    return (
      <a
        key={i}
        className="sb-entry sb-link"
        href={href}
        target="_blank"
        rel="noopener noreferrer"
        title={e.url}
        onClick={() => overlay && onClose()}
      >
        {body}
      </a>
    );
  };

  return (
    <aside ref={aside} className={overlay ? 'sidebar drawer' : 'sidebar'} style={style} data-testid="sidebar">
      {resizable && (
        <div
          className={['sb-resize', touch && 'touch', drag && 'hot'].filter(Boolean).join(' ')}
          data-testid="sidebar-resize"
          title={`drag to resize · double-click: back to ${SIDEBAR_PX} px`}
          onPointerDown={onDown}
          onPointerMove={onMove}
          onPointerUp={onUp}
          onPointerCancel={onCancel}
        >
          {touch && (
            <span className="sb-grip">
              <i />
              <i />
            </span>
          )}
        </div>
      )}
      {drag && !overlay && (
        <div className={drag.stop ? 'sb-ghost stop' : 'sb-ghost'} style={{ left: current - drag.width - 1 }} data-testid="sidebar-ghost">
          <div className="sb-ghost-tip">
            <b>{drag.width} px</b>
            {drag.stop ? (
              <span className="tone-warn">the task needs {drag.cols} columns</span>
            ) : (
              <span>the task keeps {drag.cols} columns</span>
            )}
          </div>
        </div>
      )}
      {drag && overlay && <div className="sb-ghost-tip drawer">{drag.width} px</div>}
      {overlay && (
        <div className="sb-head">
          <span className="sb-title">{view?.title ?? 'no task open'}</span>
          <button type="button" className="hbtn sb-close" data-testid="sidebar-close" title="hide the sidebar (s in the column)" aria-label="hide the sidebar" onClick={onClose}>
            ×
          </button>
        </div>
      )}
      <div className="sb-body">
        {!view && <div className="sb-empty">no task open</div>}
        {view && view.sections.length === 0 && (
          <div className="sb-empty">
            <div>nothing linked yet</div>
            <div className="dim">links the agent names and TASK.md&apos;s ## Links show here</div>
          </div>
        )}
        {view?.sections.map((s) => (
          <section key={s.title} className="sb-section">
            <div className="sb-section-title">
              {s.title}
              {s.count && <span className="sb-count">{s.count}</span>}
            </div>
            {s.progress !== undefined && (
              <div className="sb-progress">
                <i style={{ width: `${Math.round(s.progress * 100)}%` }} />
              </div>
            )}
            {s.entries.map(entry)}
          </section>
        ))}
      </div>
    </aside>
  );
}
