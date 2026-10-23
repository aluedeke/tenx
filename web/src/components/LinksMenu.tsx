'use client';

// A task's links (tenx_core::links): a menu under the header, a bottom
// sheet on a phone — on an iPad a sheet across the screen is far from the ↗
// it was opened with, so it gets the menu, with rows sized for a finger. Opened two ways: the header's
// ↗ button (the current task — the page's own state, so it takes the keys
// while open, like the row menu) and the column's `L` (the server's mode —
// keys go to the server, which moves the selection; App opens what a key
// names, `linkKey`). Each link is a real anchor, so the browser opens it the
// way it opens any link; copy sits beside it, since on an iPad the link is
// often for pasting somewhere else.

import { useEffect } from 'react';
import type { LinkView } from '@/protocol';
import { copyText, linkHref, openExternal } from '@/lib/links';

interface Props {
  title: string;
  links: LinkView[];
  /** The server's cursor, in `L` mode. */
  selected?: number;
  touch: boolean;
  /** Opened by the page (the ↗ button): it takes Escape and keeps keys from
   * the column while open. The server's `L` mode leaves keys to the server. */
  own: boolean;
  onClose(): void;
  /** What happened, for the page to show. */
  onDone(message: string): void;
}

/** Narrower than this, a touch screen gets the bottom sheet. */
const SHEET_MAX_WIDTH = 600;

const GROUPS:  { label: string; kinds: LinkView['kind'][] }[] = [
  { label: 'pull requests', kinds: ['PR'] },
  { label: 'made with claude', kinds: ['artifact', 'design'] },
  { label: 'other', kinds: ['linear', 'port', 'web'] },
];

export function LinksMenu(props: Props) {
  const { links, selected, touch, own, onClose, onDone } = props;
  const sheet = touch && window.innerWidth < SHEET_MAX_WIDTH;
  useEffect(() => {
    if (!own) return;
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key === 'Escape') {
        ev.preventDefault();
        onClose();
      } else if (/^[1-9]$/.test(ev.key) && links[Number(ev.key) - 1]) {
        ev.preventDefault();
        onClose();
        openExternal(linkHref(links[Number(ev.key) - 1], location.hostname));
      }
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [own, onClose, links]);

  const copy = (link: LinkView) => {
    const href = linkHref(link, location.hostname);
    onClose();
    void copyText(href).then((ok) => onDone(ok ? `copied ${link.label}` : 'the browser refused the clipboard'));
  };
  const row = (link: LinkView, i: number) => (
    <div key={link.url} className={i === selected ? 'lrow on' : 'lrow'} role="menuitem">
      <a
        className="lopen"
        href={linkHref(link, location.hostname)}
        target="_blank"
        rel="noopener noreferrer"
        title={link.url}
        onClick={onClose}
      >
        <span className="lkey">{i < 9 ? i + 1 : ''}</span>
        <span className="licon" style={{ color: link.color }}>
          {link.icon}
        </span>
        <span className="llabel">{link.label}</span>
        <span className="lkind">{link.kind}</span>
      </a>
      <button type="button" className="lcopy" title="copy the link" onClick={() => copy(link)}>
        copy
      </button>
    </div>
  );
  // Grouped as in the picker, numbered in list order (the server's order).
  const body = GROUPS.map((g) => {
    const inGroup = links.map((l, i) => [l, i] as const).filter(([l]) => g.kinds.includes(l.kind));
    if (!inGroup.length) return null;
    return (
      <div key={g.label}>
        <div className="lgrp">{g.label}</div>
        {inGroup.map(([l, i]) => row(l, i))}
      </div>
    );
  });

  return (
    <div className="popup-layer" {...(own ? { 'data-popup': sheet ? 'sheet' : 'menu' } : {})}>
      <div className={sheet ? 'dim' : 'backdrop'} onPointerDown={onClose} />
      {sheet ? (
        <div className="sheet links" data-testid="links-sheet" role="menu">
          <div className="handle" />
          <div className="shead">
            <span className="bright bold">links · {props.title}</span>
          </div>
          {body}
        </div>
      ) : (
        <div className="menu links" data-testid="links-menu" role="menu">
          <div className="lhead">links · {props.title}</div>
          {body}
          <div className="lfoot">{own ? '1-9 open · esc' : '⏎ 1-9 open · y copy · esc'}</div>
        </div>
      )}
    </div>
  );
}
