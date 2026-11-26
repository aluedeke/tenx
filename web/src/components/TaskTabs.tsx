'use client';

// A tabbed task window's tabs, drawn by the page above the terminal: the
// agent, a shell and nvim as one switch. tmux keeps drawing its own strip on
// the window's top line for the terminal client and SSH; the page hides that
// line under this bar (Terminal `stripHidden`). A click sends the tab's key,
// Alt+1/2/3 — the same switch as the key, so tmux stays the one that knows
// which tab is shown (`view.window_tab`).

import { useEffect, useState } from 'react';
import { WINDOW_TABS, type WindowTab } from '@/protocol';

interface Props {
  /** The tab the window shows (`view.window_tab`). */
  shown: WindowTab;
  /** Terminal input: the tab's key. */
  onKey(data: string): void;
}

/** How long a clicked tab stays lit before the view confirms it. */
const PENDING_MS = 1500;

export function TaskTabs({ shown, onKey }: Props) {
  // The clicked tab, lit at once: the view says so only on its next tick.
  const [pending, setPending] = useState<WindowTab | null>(null);
  useEffect(() => setPending(null), [shown]);
  useEffect(() => {
    if (!pending) return;
    const t = setTimeout(() => setPending(null), PENDING_MS);
    return () => clearTimeout(t);
  }, [pending]);
  const lit = pending ?? shown;
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
            // The keyboard stays where it was (the terminal, usually).
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => {
              if (tab === lit) return;
              setPending(tab);
              onKey(`\x1b${i + 1}`);
            }}
          >
            {tab}
          </button>
        ))}
      </div>
    </div>
  );
}
