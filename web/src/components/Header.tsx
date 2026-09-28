'use client';

// The header, across the whole page above the column and the terminal: the
// column's toggle, the wordmark, the task in front of you (the view's
// `current`, i.e. this tab's tmux window) with what it wants from you, and
// what doesn't depend on a row — next that needs you, new, notifications,
// the connection, the keys. On a phone it is how the column opens.

import { useRef, useState } from 'react';
import { PopupLayer, type Popup } from './RowActions';
import type { Action, ColumnView, TaskItem } from '@/protocol';
import type { Status } from '@/lib/connection';
import type { PushState } from '@/lib/push';

interface Props {
  view: ColumnView | null;
  status: Status;
  failures: number;
  host: string;
  /** The column is on screen. */
  columnShown: boolean;
  onToggle(): void;
  onAction(action: Action): void;
  /** Show the column with the current task selected. */
  onShowCurrent(id: string): void;
  push: PushState;
  onPushToggle(): void;
  onPushTest(): void;
}

const PUSH_TITLE: Record<PushState, string> = {
  unsupported: 'notifications need HTTPS (tailscale serve) and a browser with Web Push',
  'needs-install': 'on iPhone and iPad: Share → Add to Home Screen, then enable here',
  off: 'notify me when a task needs me',
  on: 'notifications on — click to turn off',
  denied: 'notifications are blocked in this browser’s settings',
};

const MENU_W = 218;

export function Header(props: Props) {
  const { view, status, failures, host, columnShown, onToggle, onAction, push } = props;
  const [popup, setPopup] = useState<Popup | null>(null);

  // The current task, as the Tasks tab last listed it: on the Repos or Work
  // tab the view's items are repos or jobs, and the header keeps showing the
  // task you're in rather than going blank.
  const known = useRef<TaskItem | null>(null);
  const tab = view?.tabs.find((t) => t.active)?.label ?? 'Tasks';
  if (view) {
    const found = view.items.find((i): i is Extract<typeof i, { kind: 'task' }> => i.kind === 'task' && i.id === view.current);
    if (found) known.current = found;
    else if (!view.current || (tab === 'Tasks' && !view.filter)) known.current = null;
  }
  const current = known.current && known.current.id === view?.current ? known.current : null;

  // Another task needs you: not the one you're already in.
  const another =
    tab === 'Tasks' &&
    !!view?.items.some((i) => i.kind === 'task' && i.id !== view.current && (i.status === 'blocked' || i.status === 'signaled'));

  const plus = (e: React.MouseEvent<HTMLButtonElement>) => {
    if (tab !== 'Repos') return onAction('new');
    const r = e.currentTarget.getBoundingClientRect();
    setPopup({
      at: { left: Math.max(8, r.right - MENU_W), top: r.bottom + 4 },
      entries: [
        { label: 'add repo', key: 'a', run: () => onAction('add_repo') },
        { label: 'new workspace', key: 'W', run: () => onAction('new_workspace') },
      ],
    });
  };

  const label =
    status === 'open'
      ? host || (typeof location !== 'undefined' ? location.host : '')
      : status === 'connecting'
        ? 'connecting…'
        : failures > 5
          ? 'offline — is tenx web running?'
          : 'reconnecting…';

  return (
    <>
      <header className="header" data-testid="header">
        <button
          type="button"
          className="hbtn toggle"
          data-testid="toggle"
          title={columnShown ? 'hide the tasks (^w)' : 'show the tasks (^w)'}
          aria-label={columnShown ? 'hide the tasks' : 'show the tasks'}
          aria-expanded={columnShown}
          onClick={onToggle}
        >
          {columnShown ? '⟨' : '☰'}
        </button>
        <span className="wordmark">
          ten<span className="accent">x</span>
        </span>
        <button
          type="button"
          className="current"
          data-testid="current"
          disabled={!current}
          title={current ? `${current.title} — show it in the tasks` : undefined}
          onClick={() => current && props.onShowCurrent(current.id)}
        >
          {current && (
            <>
              <span className="glyph" style={{ color: current.glyph_color }}>
                {current.glyph}
              </span>
              <span className="ctitle">{current.title}</span>
              <span className="cws" style={{ color: current.ws_color }}>
                {current.ws}
              </span>
              {current.reason && (
                <span className="chip" style={{ color: current.reason.fg, background: current.reason.bg }}>
                  {current.reason.label}
                </span>
              )}
            </>
          )}
        </button>
        <span className="hright">
          {another && (
            <button type="button" className="hbtn hot" data-testid="next" title="next task that needs you (n)" onClick={() => onAction('next')}>
              ● next
            </button>
          )}
          <button
            type="button"
            className="hbtn box"
            data-testid="add"
            title={tab === 'Repos' ? 'add a repo or a workspace' : 'new task (^n)'}
            onClick={plus}
          >
            +
          </button>
          {push === 'on' && (
            <button type="button" className="hbtn small" data-testid="push-test" title="send a test notification" onClick={props.onPushTest}>
              test
            </button>
          )}
          <button
            type="button"
            className={`hbtn bell ${push}`}
            data-testid="push"
            data-state={push}
            title={PUSH_TITLE[push]}
            aria-label={PUSH_TITLE[push]}
            disabled={push === 'unsupported' || push === 'denied' || push === 'needs-install'}
            onClick={props.onPushToggle}
          >
            {push === 'on' ? '🔔' : '🔕'}
          </button>
          <span className={`conn ${status}`} data-testid="conn" title={label}>
            <span className="dot" />
            <span className="clabel">{label}</span>
          </span>
          <button type="button" className="hbtn box" data-testid="keys" title="keys (?)" onClick={() => onAction('help')}>
            ?
          </button>
        </span>
      </header>
      {push === 'needs-install' && (
        <div className="hint" data-testid="push-hint">
          Add to Home Screen, then enable notifications
        </div>
      )}
      <PopupLayer popup={popup} onClose={() => setPopup(null)} />
    </>
  );
}
