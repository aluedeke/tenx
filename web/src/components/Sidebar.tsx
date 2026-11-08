'use client';

// The sidebar beside the terminal: the current task's todos, its PRs (a dot
// for the state, a square per check, the review, the comments), Linear
// issues with their status, what was made with Claude, Slack messages, the
// repos' git state and other links — `view.sidebar`, built by the server
// (tenx_core::sidebar). A panel right of the terminal on a wide page; where
// there is no room, a sheet over it from the right, the task list's mirror.
// A tap on an entry opens its link.

import type { LinkView, SidebarEntry, SidebarView } from '@/protocol';
import { linkHref } from '@/lib/links';

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
}

export function Sidebar({ view, overlay, onClose }: Props) {
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
    <aside className={overlay ? 'sidebar drawer' : 'sidebar'} data-testid="sidebar">
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
