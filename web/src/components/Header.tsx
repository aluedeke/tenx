'use client';

// The header, across the whole page above the column and the terminal: on
// the left the task list's button and the wordmark (its dot is the
// connection, words only when it's lost), on the right what doesn't depend
// on a row — next that needs you, read the current task's last reply aloud,
// speak, new — and the sidebar's button, the task list's mirror, last. The
// task in front of you is on tmux's status line, not here. What you set once
// (notifications, the keys, the connection) is behind ⋯, which the task
// list carries (`MoreMenu`).

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
  push: PushState;
  mic: MicState;
  /** Seconds recorded so far, while recording. */
  micSeconds: number;
  onMic(): void;
  speaker: SpeakerState;
  /** Read the task's last reply aloud, pause or go on (`taskId` empty when
   * no task is current). */
  onSpeak(taskId: string): void;
  /** Stop reading. */
  onSpeakStop(): void;
  /** The sidebar is shown; its button shows or hides it. */
  sidebarShown: boolean;
  onSidebar(): void;
}

/** The speaker button: off, waiting for the audio, reading aloud, or
 * paused. */
export type SpeakerState = 'idle' | 'loading' | 'playing' | 'paused';

const SPEAKER_TITLE: Record<SpeakerState, string> = {
  idle: 'read the agent’s last reply aloud (⌃.)',
  loading: 'making the audio… — tap to cancel',
  playing: 'reading aloud — tap (or ⌃.) to pause',
  paused: 'paused — tap (or ⌃.) to go on',
};

/** The speech button: off, listening, or turning what it heard into text. */
export type MicState = 'idle' | 'recording' | 'transcribing';

const MIC_TITLE: Record<MicState, string> = {
  idle: 'speak instead of typing — tap (or ⌃M), talk, tap again; ⏎ stops and sends',
  recording: 'listening — tap (or ⌃M) to stop and type it, ⏎ to stop and send it',
  transcribing: 'turning speech into text…',
};

const PUSH_TITLE: Record<PushState, string> = {
  unsupported: 'notifications need HTTPS (tailscale serve) and a browser with Web Push',
  'needs-install': 'on iPhone and iPad: Share → Add to Home Screen, then enable here',
  app: '',
  off: 'notify me when a task needs me',
  on: 'notifications on — click to turn off',
  denied: 'notifications are blocked in this browser’s settings',
};

const MENU_W = 218;
/** The ⋯ menu's width (`.hmenu`). */
const MORE_W = 270;

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
    const r = e.currentTarget.getBoundingClientRect();
    setPopup({
      at: { left: Math.max(8, r.right - MENU_W), top: r.bottom + 4 },
      entries:
        tab === 'Repos'
          ? [
              { label: 'add repo', key: 'a', run: () => onAction('add_repo') },
              { label: 'new workspace', key: 'W', run: () => onAction('new_workspace') },
            ]
          : [
              // The prompt first, a session in adhoc, titled by Claude.
              { label: 'quick task', key: '^⇧n', run: () => onAction('quick') },
              { label: 'new task', key: '^n', run: () => onAction('new') },
            ],
    });
  };

  const label = connLabel(status, failures, host);

  return (
    <>
      <header className="header" data-testid="header">
        <button
          type="button"
          className={columnShown ? 'hbtn toggle on' : 'hbtn toggle'}
          data-testid="toggle"
          title={columnShown ? 'hide the tasks (^w)' : 'show the tasks (^w)'}
          aria-label={columnShown ? 'hide the tasks' : 'show the tasks'}
          aria-expanded={columnShown}
          onClick={onToggle}
        >
          <Icon d={PANEL_LEFT} />
        </button>
        <span className="wordmark" title={label}>
          <span className="wtext">
            ten<span className="accent">x</span>
          </span>
          <span className={`cdot ${status}`} data-testid="conn-dot" />
        </span>
        <span className="hgrow" />
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
            className={`hbtn speaker ${props.speaker}`}
            data-testid="speaker"
            data-state={props.speaker}
            title={SPEAKER_TITLE[props.speaker]}
            aria-label={SPEAKER_TITLE[props.speaker]}
            aria-pressed={props.speaker === 'playing' || props.speaker === 'loading'}
            // Not over a dictation: the microphone would hear the voice.
            disabled={props.speaker === 'idle' && (!current || props.mic !== 'idle')}
            // Not on pointer-down: iOS starts audio only on a completed tap.
            onClick={() => props.onSpeak(current?.id ?? '')}
          >
            {props.speaker === 'loading' ? '…' : props.speaker === 'playing' ? <PauseIcon /> : <SpeakerIcon />}
          </button>
          {/* While it reads, ■ takes the microphone's place: the header must
              not grow on a phone, and nobody dictates over the voice. */}
          {props.speaker === 'playing' || props.speaker === 'paused' ? (
            <button
              type="button"
              className="hbtn speakstop"
              data-testid="speaker-stop"
              title="stop reading"
              aria-label="stop reading"
              onClick={props.onSpeakStop}
            >
              <svg width="14" height="14" viewBox="0 0 24 24" aria-hidden="true">
                <rect x="6" y="6" width="12" height="12" rx="1.5" fill="currentColor" />
              </svg>
            </button>
          ) : (
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
          )}
          <button
            type="button"
            className="hbtn primary"
            data-testid="add"
            title={tab === 'Repos' ? 'add a repo or a workspace' : 'quick task (^⇧n) or new task (^n)'}
            aria-label={tab === 'Repos' ? 'add a repo or a workspace' : 'quick task or new task'}
            onClick={plus}
          >
            <Icon d={['M12 5v14', 'M5 12h14']} />
          </button>
          <button
            type="button"
            className={props.sidebarShown ? 'hbtn hsidebar on' : 'hbtn hsidebar'}
            data-testid="sidebar-toggle"
            title="sidebar: the task's todos, PRs, links, git (s in the column)"
            aria-label={props.sidebarShown ? 'hide the sidebar' : 'show the sidebar'}
            aria-pressed={props.sidebarShown}
            onClick={props.onSidebar}
          >
            <Icon d={PANEL_RIGHT} />
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

function connLabel(status: Status, failures: number, host: string): string {
  return status === 'open'
    ? host || (typeof location !== 'undefined' ? location.host : '')
    : status === 'connecting'
      ? 'connecting…'
      : failures > 5
        ? 'offline — is tenx web running?'
        : 'reconnecting…';
}

interface MoreProps {
  status: Status;
  failures: number;
  host: string;
  push: PushState;
  onPushToggle(): void;
  onPushTest(): void;
  onAction(action: Action): void;
}

/** ⋯ at the task list's top right: what you set once, out of the way of the
 * daily actions — notifications, the keys, where this page is connected. */
export function MoreMenu(props: MoreProps) {
  const { status, push, onAction } = props;
  const [menu, setMenu] = useState(false);
  /** The menu's place: under its button, inside the window. */
  const [menuAt, setMenuAt] = useState({ left: 8, top: 40 });
  const label = connLabel(status, props.failures, props.host);
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
  return (
    <>
      <button
        type="button"
        className={menu ? 'hbtn tmore on' : 'hbtn tmore'}
        data-testid="hmore"
        title="notifications, keys, connection"
        aria-label="more: notifications, keys, connection"
        aria-haspopup="menu"
        aria-expanded={menu}
        onClick={(e) => {
          const r = e.currentTarget.getBoundingClientRect();
          setMenuAt({ left: Math.max(8, Math.min(r.right - MORE_W, window.innerWidth - MORE_W - 8)), top: r.bottom + 4 });
          setMenu((m) => !m);
        }}
      >
        <Icon d={[]} dots />
      </button>
      {menu && (
        <div className="popup-layer" data-popup="menu">
          <div className="backdrop" onPointerDown={() => setMenu(false)} />
          <div className="hmenu" data-testid="hmenu" role="menu" style={menuAt}>
            {push !== 'app' && ( // the tenx iOS app: no Web Push to offer
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
            )}
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

function SpeakerIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M4 9.5h3.5L12 5.5v13l-4.5-4H4z" />
      <path d="M15.5 9a4 4 0 0 1 0 6M18.5 6.5a7.5 7.5 0 0 1 0 11" />
    </svg>
  );
}

function PauseIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" aria-hidden="true">
      <path d="M9 6v12M15 6v12" />
    </svg>
  );
}

const BELL = ['M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9', 'M10.3 21a1.94 1.94 0 0 0 3.4 0'];

/** A line icon in the header's one style; `dots` draws ⋯. */
/** The task list's button: a window with a panel on its left; the
 * sidebar's is its mirror. */
const PANEL_LEFT = ['M4 5h16v14H4z', 'M9 5v14'];
const PANEL_RIGHT = ['M4 5h16v14H4z', 'M15 5v14'];

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
