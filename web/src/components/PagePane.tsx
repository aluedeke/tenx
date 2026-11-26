'use client';

// One of a task's pages, shown by the iOS app over the terminal (lib/pages):
// the page draws the bar (back, forward, reload, the address, Safari) and
// leaves a hole under it, whose place it hands the app; the app lays a web
// view of its own there, with the workspace's cookie store. The app reports
// where the page went (`PAGE_EVENT`), and the page tells tenx (`onMoved`), so
// every device lists it where it is now.
//
// A `localhost` page is the Mac's own, which the phone can't reach: a note
// instead.

import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import type { PageView } from '@/protocol';
import { PAGE_EVENT, nativePageDo, nativePageHide, nativePageShow, type NativePageState } from '@/lib/native';
import { isLocalUrl, workspaceOf } from '@/lib/pages';

interface Props {
  task: string;
  /** `pageKey(task, page.id)`. */
  pageKey: string;
  page: PageView;
  store: string;
  /** The app couldn't show it (too old): the page leaves it to Safari. */
  onRefused(): void;
  onMoved(url: string, title: string): void;
  onSafari(url: string): void;
}

export function PagePane({ task, pageKey, page, store, onRefused, onMoved, onSafari }: Props) {
  const hole = useRef<HTMLDivElement>(null);
  const [state, setState] = useState<NativePageState | null>(null);
  const local = isLocalUrl(page.url);
  // Shown where the hole is, again whenever the hole moves (a resize, the
  // keyboard, the sidebar); hidden when the pane goes.
  const refused = useRef(onRefused);
  refused.current = onRefused;
  useLayoutEffect(() => {
    // The terminal lets go of the keyboard: iOS's bar over it would cover
    // the page, and a key typed now is for the page.
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
    const el = hole.current;
    if (!el || local) return;
    let gaveUp = false;
    const show = () => {
      const r = el.getBoundingClientRect();
      if (gaveUp || r.width < 1 || r.height < 1) return;
      // Where the hole is on the screen: the layout's place less how far iOS
      // panned the visible part (the keyboard).
      const vv = window.visualViewport;
      const x = r.left - (vv?.offsetLeft ?? 0);
      const y = r.top - (vv?.offsetTop ?? 0);
      nativePageShow({
        key: pageKey,
        url: page.url,
        store,
        storeName: workspaceOf(task),
        rect: { x, y, width: r.width, height: r.height },
      }).catch(() => {
        // Once: the observer's first call would ask again.
        if (gaveUp) return;
        gaveUp = true;
        refused.current();
      });
    };
    show();
    const watch = new ResizeObserver(show);
    watch.observe(el);
    window.addEventListener('resize', show);
    window.visualViewport?.addEventListener('resize', show);
    window.visualViewport?.addEventListener('scroll', show);
    return () => {
      watch.disconnect();
      window.removeEventListener('resize', show);
      window.visualViewport?.removeEventListener('resize', show);
      window.visualViewport?.removeEventListener('scroll', show);
      nativePageHide();
    };
    // The address only matters for a web view the app makes now: a page it
    // already has stays where it went.
  }, [pageKey, store, task, local]); // eslint-disable-line react-hooks/exhaustive-deps
  const moved = useRef(onMoved);
  moved.current = onMoved;
  useEffect(() => {
    const on = (ev: Event) => {
      const s = (ev as CustomEvent<NativePageState>).detail;
      if (s?.key !== pageKey) return;
      setState(s);
      moved.current(s.url, s.title);
    };
    window.addEventListener(PAGE_EVENT, on);
    return () => window.removeEventListener(PAGE_EVENT, on);
  }, [pageKey]);
  const url = state?.url || page.url;
  return (
    <div className="page-pane" data-testid="page-pane" onMouseDown={(e) => e.stopPropagation()}>
      <div className="page-bar">
        <button type="button" aria-label="back" disabled={!state?.canGoBack} onClick={() => nativePageDo(pageKey, 'back')}>
          ‹
        </button>
        <button type="button" aria-label="forward" disabled={!state?.canGoForward} onClick={() => nativePageDo(pageKey, 'forward')}>
          ›
        </button>
        <button type="button" aria-label="reload" disabled={local} onClick={() => nativePageDo(pageKey, 'reload')}>
          {state?.loading ? '×' : '↻'}
        </button>
        <span className="page-url" title={url}>
          {url.replace(/^https?:\/\//, '')}
        </span>
        <button type="button" aria-label="open in Safari" title="open in Safari" onClick={() => onSafari(url)}>
          ↗
        </button>
      </div>
      {local ? (
        <div className="page-note" data-testid="page-local">
          <strong>{url.replace(/^https?:\/\//, '')} is on the Mac</strong>
          <span>
            A localhost address is the Mac&apos;s own, and this device can&apos;t reach it. tenx can&apos;t show it here yet.
          </span>
        </div>
      ) : (
        <div className="page-hole" ref={hole} />
      )}
    </div>
  );
}
