'use client';

// A tabbed task window's tabs, drawn by the page above the terminal: the
// agent, a shell and nvim as one switch. tmux keeps drawing its own strip on
// the window's top line for the terminal client and SSH; the page hides that
// line under this bar (Terminal `stripHidden`). A click asks `tenx web` to
// show the tab (a `tab` message); tmux stays the one that knows which tab is
// shown, and the next view says so (`view.window_tab`).

import { useEffect, useState } from 'react';
import { WINDOW_TABS, type WindowTab } from '@/protocol';
import { GIVE_UP_MS, litTab, type Asked } from '@/lib/tasktabs';

interface Props {
  /** The tab the window shows (`view.window_tab`). */
  shown: WindowTab;
  onSelect(tab: WindowTab): void;
}

export function TaskTabs({ shown, onSelect }: Props) {
  // The last tab clicked, lit until the view shows it (lib/tasktabs).
  const [asked, setAsked] = useState<Asked | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const lit = litTab(shown, asked, now);
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
              // Lit but not shown yet: asking again is allowed.
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
    </div>
  );
}
