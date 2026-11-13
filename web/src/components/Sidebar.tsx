'use client';

// The sidebar beside the terminal: the current task's todos, its PRs (a dot
// for the state, a square per check, the review, the comments), Linear
// issues with their status, what was made with Claude, Slack messages, the
// repos' git state and other links — `view.sidebar`, built by the server
// (tenx_core::sidebar). A panel right of the terminal on a wide page; where
// there is no room, a sheet over it from the right, the task list's mirror.
// A tap on an entry opens its link.
//
// Its left edge is a handle (desktop and iPad, `ResizeEdge`): dragging it
// sets the width.

import { useRef, useState } from 'react';
import type { LinkView, SidebarEntry, SidebarView } from '@/protocol';
import { linkHref } from '@/lib/links';
import { DRAWER_MARGIN, SIDEBAR_PX, type Drag } from '@/lib/sidebarfit';
import { ResizeEdge } from './ResizeEdge';

const KIND_COLOR: Partial<Record<LinkView['kind'], string>> = {
  PR: 'var(--success)',
  artifact: 'var(--accent)',
  design: 'var(--accent)',
  linear: 'var(--current)',
  slack: 'var(--warn)',
  obsidian: 'var(--accent)',
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

export function Sidebar({ view, overlay, onClose, width, resizable, touch, dragTo, onWidth }: Props) {
  const aside = useRef<HTMLElement>(null);
  const [live, setLive] = useState<Drag | null>(null);

  // A drawer follows the drag; a panel keeps its width until the release.
  const shown = live ? live.width : width;
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
        // An app link (Obsidian) leaves the page where it is; a new tab for
        // it would stay empty.
        target={e.kind === 'obsidian' ? undefined : '_blank'}
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
        <ResizeEdge
          edge="left"
          panel={aside}
          drawer={overlay}
          touch={touch}
          title={`drag to resize · double-click: back to ${SIDEBAR_PX} px`}
          testid="sidebar"
          dragTo={dragTo}
          label={(d) => (
            <>
              <b>{d.width} px</b>
              {d.stop ? (
                <span className="tone-warn">the task needs {d.cols} columns</span>
              ) : (
                <span>the task keeps {d.cols} columns</span>
              )}
            </>
          )}
          onLive={setLive}
          onDone={(d) => onWidth(d && d.width)}
        />
      )}
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
