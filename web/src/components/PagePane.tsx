'use client';

// One of a task's pages, shown by the iOS app over the terminal (lib/pages):
// the page draws the bar (back, forward, reload, the address, Safari) and
// leaves a hole under it, whose place it hands the app; the app lays a web
// view of its own there, with the workspace's cookie store. The app reports
// where the page went (`PAGE_EVENT`), and the page tells tenx (`onMoved`), so
// every device lists it where it is now.
//
// A `localhost` page is the Mac's own: the app reaches it through `tenx web`'s
// page tunnel (`tunnel`, tenx_core::pagetunnel). An app without it: a note.

import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import type { PageView } from '@/protocol';
import { PAGE_EVENT, nativePageDo, nativePageHide, nativePageShow, type NativePageState } from '@/lib/native';
import { isLocalUrl, viaTunnel, workspaceOf } from '@/lib/pages';

interface Props {
  task: string;
  /** `pageKey(task, page.id)`. */
  pageKey: string;
  page: PageView;
  store: string;
  /** The app reaches the Mac's `localhost` (`nativePageTunnelSupported`). */
  tunnel: boolean;
  /** The app couldn't show it (too old): the page leaves it to Safari. */
  onRefused(): void;
  onMoved(url: string, title: string): void;
  onSafari(url: string): void;
}

/** Whether something the page drew lies over the hole: a point inside it
 * (the middle and near each corner) whose topmost element isn't the hole. */
function covered(hole: HTMLElement, r: DOMRect): boolean {
  // Past a panel's resize handle, which reaches 11px over its edge.
  const inset = 24;
  const points: [number, number][] = [
    [r.left + r.width / 2, r.top + r.height / 2],
    [r.left + inset, r.top + inset],
    [r.right - inset, r.top + inset],
    [r.left + inset, r.bottom - inset],
    [r.right - inset, r.bottom - inset],
  ];
  return points.some(([x, y]) => {
    const top = document.elementFromPoint(x, y);
    return top !== null && top !== hole && !hole.contains(top);
  });
}

export function PagePane({ task, pageKey, page, store, tunnel, onRefused, onMoved, onSafari }: Props) {
  const hole = useRef<HTMLDivElement>(null);
  const [state, setState] = useState<NativePageState | null>(null);
  // The Mac's own address with no way to it.
  const local = isLocalUrl(page.url) && (!tunnel || !viaTunnel(page.url));
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
    let hidden = false;
    // What the app was last told: only a change is sent again.
    let last = '';
    const show = () => {
      const r = el.getBoundingClientRect();
      if (gaveUp || r.width < 1 || r.height < 1) return;
      // The app's web view sits over the whole page: under a drawer, a
      // menu or a form the page drew over the hole, it steps aside.
      if (covered(el, r)) {
        if (!hidden) nativePageHide();
        hidden = true;
        last = '';
        return;
      }
      hidden = false;
      // The terminal under the page never keeps the keyboard (a drawer
      // closing hands it back): iOS's bar over it would cover the page.
      const typing = document.activeElement;
      if (typing instanceof HTMLElement && typing.closest('.xterm-host')) typing.blur();
      // Where the hole is on the screen: the layout's place less how far iOS
      // panned the visible part (the keyboard).
      const vv = window.visualViewport;
      const x = r.left - (vv?.offsetLeft ?? 0);
      const y = r.top - (vv?.offsetTop ?? 0);
      const at = `${x},${y},${r.width},${r.height}`;
      if (at === last) return;
      last = at;
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
    // Overlays come and go without the hole moving.
    let queued = 0;
    const later = () => {
      cancelAnimationFrame(queued);
      queued = requestAnimationFrame(show);
    };
    const overlays = new MutationObserver(later);
    overlays.observe(document.body, { childList: true, subtree: true, attributes: true, attributeFilter: ['class', 'style', 'hidden'] });
    window.addEventListener('resize', show);
    window.visualViewport?.addEventListener('resize', show);
    window.visualViewport?.addEventListener('scroll', show);
    return () => {
      watch.disconnect();
      overlays.disconnect();
      cancelAnimationFrame(queued);
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
            A localhost address is the Mac&apos;s own, and this device can&apos;t reach it: {!tunnel
              ? 'this app is too old for it. Update the app.'
              : 'only localhost goes through to the Mac: open it as localhost instead.'}
          </span>
        </div>
      ) : (
        <div className="page-hole" ref={hole} />
      )}
    </div>
  );
}
