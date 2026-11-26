'use client';

// A tabbed task window's tabs, drawn by the page above the terminal: the
// agent, a shell and nvim as one switch. tmux keeps drawing its own strip on
// the window's top line for the terminal client and SSH; the page hides that
// line under this bar (Terminal `stripHidden`). A click asks `tenx web` to
// show the tab (a `tab` message); tmux stays the one that knows which tab is
// shown, and the next view says so (`view.window_tab`).
//
// After the switch, the task's pages (lib/pages) as chips, and `+` to open
// one by its address. A page shown (in the app) lights its chip and none of
// the switch.

import { useEffect, useRef, useState } from 'react';
import { WINDOW_TABS, type PageView, type WindowTab } from '@/protocol';
import { GIVE_UP_MS, litTab, type Asked } from '@/lib/tasktabs';
import { chipLabel, typedUrl } from '@/lib/pages';

interface Props {
  /** The tab the window shows (`view.window_tab`). */
  shown: WindowTab;
  onSelect(tab: WindowTab): void;
  pages: PageView[];
  /** The page this device shows over the terminal, by id. */
  pageShown: string | null;
  onPage(page: PageView): void;
  onClosePage(page: PageView): void;
  onOpenPage(url: string): void;
}

export function TaskTabs({ shown, onSelect, pages, pageShown, onPage, onClosePage, onOpenPage }: Props) {
  // The last tab clicked, lit until the view shows it (lib/tasktabs).
  const [asked, setAsked] = useState<Asked | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const lit = pageShown ? null : litTab(shown, asked, now);
  // The `+` field, open while it holds text or the keyboard.
  const [typing, setTyping] = useState<string | null>(null);
  const field = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (typing !== null) field.current?.focus();
  }, [typing !== null]); // eslint-disable-line react-hooks/exhaustive-deps
  // Confirmed: from now on the view alone says which tab is shown (an
  // Alt key typed in the terminal moves it too).
  useEffect(() => {
    if (asked && shown === asked.tab) setAsked(null);
  }, [asked, shown]);
  // Lit for a click the view never confirmed only until it gives up: then
  // a render, so the bar falls back to what the window shows.
  useEffect(() => {
    if (!asked || lit !== asked.tab) return;
    const t = setTimeout(() => setNow(Date.now()), Math.max(0, asked.at + GIVE_UP_MS - Date.now()) + 10);
    return () => clearTimeout(t);
  }, [asked, lit]);
  return (
    <div className="task-tabs" data-testid="task-tabs">
      <div className="task-tabs-switch" role="tablist" aria-label="task window">
        {WINDOW_TABS.map((tab, i) => (
          <button
            key={tab}
            type="button"
            role="tab"
            aria-selected={tab === lit}
            className={tab === lit ? 'on' : undefined}
            title={`${tab} (Alt+${i + 1})`}
            // No focus on the button: the click hands the keyboard to the
            // terminal, as a click on it does — after a switch to the shell
            // you type there.
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => {
              // Lit but not shown yet: asking again is allowed. Under a page
              // nothing is lit, so the tab shown is a click away too: the
              // page goes (App sends no switch for it).
              if (tab === lit && tab === shown) return;
              const at = Date.now();
              setAsked({ tab, at });
              setNow(at);
              onSelect(tab);
            }}
          >
            {tab}
          </button>
        ))}
      </div>
      <div className="task-tabs-pages" role="tablist" aria-label="pages">
        {pages.map((page) => (
          <span key={page.id} className={page.id === pageShown ? 'page-chip on' : 'page-chip'}>
            <button
              type="button"
              role="tab"
              aria-selected={page.id === pageShown}
              title={page.url}
              data-testid={`page-${page.id}`}
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => onPage(page)}
            >
              {chipLabel(page)}
            </button>
            <button
              type="button"
              className="page-close"
              aria-label={`close ${chipLabel(page)}`}
              title="close this page"
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => onClosePage(page)}
            >
              ×
            </button>
          </span>
        ))}
        {typing === null ? (
          <button
            type="button"
            className="page-add"
            title="open a page in this task"
            aria-label="open a page"
            data-testid="page-add"
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => setTyping('')}
          >
            +
          </button>
        ) : (
          <form
            className="page-field"
            onSubmit={(e) => {
              e.preventDefault();
              const url = typedUrl(typing);
              if (url) onOpenPage(url);
              setTyping(null);
            }}
          >
            <input
              ref={field}
              value={typing}
              data-testid="page-url"
              placeholder="address"
              inputMode="url"
              autoCapitalize="off"
              autoCorrect="off"
              spellCheck={false}
              enterKeyHint="go"
              onChange={(e) => setTyping(e.target.value)}
              onBlur={() => setTyping(null)}
              onKeyDown={(e) => {
                e.stopPropagation();
                if (e.key === 'Escape') setTyping(null);
              }}
            />
          </form>
        )}
      </div>
    </div>
  );
}
