'use client';

// The header, across the whole page above the column and the terminal: the
// column's toggle, the wordmark, the task in front of you (the view's
// `current`, i.e. this tab's tmux window) with what it wants from you, and
// what doesn't depend on a row: the daily actions (next that needs you,
// speak, new) on the right, and behind ⋯ what you set once (notifications,
// the keys, the connection — a dot on the wordmark, words only when it's
// lost). On a phone it is how the column opens.

import { useEffect, useRef, useState } from 'react';
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
  mic: MicState;
  /** Seconds recorded so far, while recording. */
  micSeconds: number;
  onMic(): void;
}

/** The speech button: off, listening, or turning what it heard into text. */
export type MicState = 'idle' | 'recording' | 'transcribing';

const MIC_TITLE: Record<MicState, string> = {
  idle: 'speak instead of typing — tap (or ⌥M), talk, tap again',
  recording: 'listening — tap (or ⌥M) to stop and type it',
  transcribing: 'turning speech into text…',
};

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
  const [menu, setMenu] = useState(false);
  /** The menu's right edge, from the window's (under its button). */
  const [menuAt, setMenuAt] = useState(8);
  const run = (fn: () => void) => () => {
    setMenu(false);
    fn();
  };
  useEffect(() => {
    if (!menu) return;
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key === 'Escape') {
        ev.preventDefault();
        setMenu(false);
      }
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [menu]);

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
        <span className="wordmark" title={label}>
          <span className="wtext">
            ten<span className="accent">x</span>
          </span>
          <span className={`cdot ${status}`} data-testid="conn-dot" />
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
          {status !== 'open' && (
            <span className={`hstatus ${status}`} data-testid="conn-alert">
              {label}
            </span>
          )}
          {another && (
            <button type="button" className="hbtn hot" data-testid="next" title="next task that needs you (n)" onClick={() => onAction('next')}>
              <span className="hotdot" />
              <span className="nlabel">next</span>
            </button>
          )}
          <button
            type="button"
            className={`hbtn mic ${props.mic}`}
            data-testid="mic"
            data-state={props.mic}
            title={MIC_TITLE[props.mic]}
            aria-label={MIC_TITLE[props.mic]}
            aria-pressed={props.mic === 'recording'}
            disabled={props.mic === 'transcribing'}
            // Not on pointer-down: getUserMedia wants a completed tap on iOS.
            onClick={props.onMic}
          >
            {props.mic === 'recording' ? (
              <>
                <span className="micdot" />
                {formatSeconds(props.micSeconds)}
              </>
            ) : props.mic === 'transcribing' ? (
              '…'
            ) : (
              <MicIcon />
            )}
          </button>
          <button
            type="button"
            className="hbtn primary"
            data-testid="add"
            title={tab === 'Repos' ? 'add a repo or a workspace' : 'new task (^n)'}
            aria-label={tab === 'Repos' ? 'add a repo or a workspace' : 'new task'}
            onClick={plus}
          >
            <Icon d={['M12 5v14', 'M5 12h14']} />
          </button>
          <span className="hsep" />
          <button
            type="button"
            className={menu ? 'hbtn primary' : 'hbtn'}
            data-testid="hmore"
            title="notifications, keys, connection"
            aria-label="more: notifications, keys, connection"
            aria-haspopup="menu"
            aria-expanded={menu}
            onClick={(e) => {
              const r = e.currentTarget.getBoundingClientRect();
              setMenuAt(window.innerWidth - r.right);
              setMenu((m) => !m);
            }}
          >
            <Icon d={[]} dots />
          </button>
        </span>
      </header>
      {menu && (
        // Things set once, out of the way of the daily ones: notifications,
        // the keys, where this page is connected.
        <div className="popup-layer" data-popup="menu">
          <div className="backdrop" onPointerDown={() => setMenu(false)} />
          <div className="hmenu" data-testid="hmenu" role="menu" style={{ right: menuAt }}>
            <button
              type="button"
              role="menuitemcheckbox"
              aria-checked={push === 'on'}
              className="hmi"
              data-testid="push"
              data-state={push}
              title={PUSH_TITLE[push]}
              disabled={push === 'unsupported' || push === 'denied' || push === 'needs-install'}
              onClick={props.onPushToggle}
            >
              <Icon d={BELL} />
              <span className="hmlabel">
                notifications
                {push !== 'on' && push !== 'off' && <span className="hmwhy">{PUSH_TITLE[push]}</span>}
              </span>
              <span className={push === 'on' ? 'switch on' : 'switch'} />
            </button>
            {push === 'on' && (
              <button type="button" role="menuitem" className="hmi" data-testid="push-test" onClick={run(props.onPushTest)}>
                <Icon d={['M4 12l16-8-6 16-2-7z']} />
                <span className="hmlabel">send a test notification</span>
              </button>
            )}
            <button type="button" role="menuitem" className="hmi" data-testid="keys" onClick={run(() => onAction('help'))}>
              <Icon d={['M4.5 6h15a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2h-15a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2z', 'M6 10h.01M10 10h.01M14 10h.01M18 10h.01', 'M8 14h8']} />
              <span className="hmlabel">keys</span>
              <span className="k">?</span>
            </button>
            <div className="msep" />
            <div className={`hmconn ${status}`} data-testid="conn">
              <span className="dot" />
              {status === 'open' ? `connected · ${label}` : label}
            </div>
          </div>
        </div>
      )}
      {push === 'needs-install' && (
        <div className="hint" data-testid="push-hint">
          Add to Home Screen, then enable notifications
        </div>
      )}
      <PopupLayer popup={popup} onClose={() => setPopup(null)} />
    </>
  );
}

export function formatSeconds(s: number): string {
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

export function MicIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <rect x="9" y="3" width="6" height="11" rx="3" />
      <path d="M5.5 11a6.5 6.5 0 0 0 13 0" />
      <path d="M12 17.5V21" />
    </svg>
  );
}

const BELL = ['M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9', 'M10.3 21a1.94 1.94 0 0 0 3.4 0'];

/** A line icon in the header's one style; `dots` draws ⋯. */
function Icon({ d, dots }: { d: string[]; dots?: boolean }) {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      {d.map((p) => (
        <path key={p} d={p} />
      ))}
      {dots && [5, 12, 19].map((x) => <circle key={x} cx={x} cy="12" r="1.3" fill="currentColor" />)}
    </svg>
  );
}
