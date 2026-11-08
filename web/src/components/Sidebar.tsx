'use client';

// The sidebar beside the terminal: the current task's links grouped with
// their state (PRs, Linear, made with Claude, Slack, other), its repos' git
// state and its todos — `view.sidebar`, built by the server
// (tenx_core::sidebar). A panel right of the terminal on a wide page, a sheet
// over it on a phone. A tap on an entry opens its link.

import type { LinkView, SidebarEntry, SidebarView } from '@/protocol';
import { linkHref } from '@/lib/links';

const KIND_COLOR: Partial<Record<LinkView['kind'], string>> = {
  artifact: 'var(--accent)',
  design: 'var(--accent)',
  linear: 'var(--current)',
  slack: 'var(--warn)',
  port: 'var(--info)',
  web: 'var(--muted)',
};

interface Props {
  view: SidebarView | null;
  /** Drawn over the terminal (a phone): a close button, and a tap on a
   * link closes it. */
  overlay: boolean;
  onClose(): void;
}

export function Sidebar({ view, overlay, onClose }: Props) {
  const entry = (e: SidebarEntry, i: number) => {
    const color = (e.kind && KIND_COLOR[e.kind]) || `var(--tone-${e.tone})`;
    const body = (
      <>
        <span className="sb-icon" style={{ color }}>
          {e.icon}
        </span>
        <span className="sb-text">
          <span className="sb-label">{e.label}</span>
          {e.detail && <span className={`sb-detail tone-${e.tone}`}>{e.detail}</span>}
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
    <aside className={overlay ? 'sidebar overlay' : 'sidebar'} data-testid="sidebar">
      <div className="sb-head">
        <div className="sb-title">
          <span className="bright bold">{view?.title ?? 'no task open'}</span>
          {view && <span style={{ color: view.ws_color }}>{view.ws}</span>}
        </div>
        <button type="button" className="sb-close" title="hide the sidebar (s in the column)" aria-label="hide the sidebar" onClick={onClose}>
          ×
        </button>
      </div>
      <div className="sb-body">
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
            {s.entries.map(entry)}
          </section>
        ))}
      </div>
    </aside>
  );
}
