'use client';

// The page: the column beside the terminal, and the focus model the TUI
// client (src/tui/client.rs) has — ^w cycles terminal → column → hidden →
// column; every other key goes to whichever side has the keyboard. The
// column's own keys are the server's business: they go over as `key`
// messages and come back as a new view.

import { useCallback, useEffect, useRef, useState } from 'react';
import { Column } from './Column';
import { KeyBar } from './KeyBar';
import { Terminal, type TerminalHandle } from './Terminal';
import { Connection, type Status } from '@/lib/connection';
import { TERMINAL_BYTES, ctrlChar, isFocusCycle, isMac, isModifierOnly, isNewTaskAlias, keyMessage } from '@/lib/keys';
import { bell } from '@/lib/bell';
import type { Click, ColumnView, KeyMessage, ServerMessage } from '@/protocol';

type Focus = 'column' | 'terminal';

/** Under this many cells the column is an overlay (tmux SMALL_CLIENT_COLS),
 * until the server says otherwise in `layout`. */
const NARROW_COLS = 100;

function measureCell(fontSize: number): number {
  const probe = document.createElement('span');
  probe.style.cssText = `position:absolute;visibility:hidden;white-space:pre;font:${fontSize}px "JetBrains Mono", ui-monospace, Menlo, monospace`;
  probe.textContent = 'M'.repeat(100);
  document.body.appendChild(probe);
  const w = probe.getBoundingClientRect().width / 100;
  probe.remove();
  return w || fontSize * 0.6;
}

export function App() {
  const [view, setView] = useState<ColumnView | null>(null);
  const [status, setStatus] = useState<Status>('connecting');
  const [failures, setFailures] = useState(0);
  const [host, setHost] = useState('');
  const [columnCols, setColumnCols] = useState(36);
  const [narrow, setNarrow] = useState(false);
  const [visible, setVisible] = useState(true);
  const [focus, setFocus] = useState<Focus>('column');
  const [touch, setTouch] = useState(false);
  const [ctrlSticky, setCtrlSticky] = useState(false);
  const [cell, setCell] = useState(7.8);

  const conn = useRef<Connection | null>(null);
  const term = useRef<TerminalHandle>(null);
  const mac = useRef(false);
  const fontSize = touch && narrow ? 12 : 13;

  // Latest state for the listeners registered once.
  const state = useRef({ focus, visible, narrow, ctrlSticky });
  state.current = { focus, visible, narrow, ctrlSticky };

  const send = useCallback((msg: Parameters<Connection['send']>[0]) => conn.current?.send(msg), []);
  const sendKey = useCallback((k: Omit<KeyMessage, 'type'>) => send({ type: 'key', ...k }), [send]);

  const columnHasKeys = focus === 'column' && visible;

  const sendViewport = useCallback(() => {
    const cellW = measureCell(13);
    setCell(cellW);
    const cols = Math.floor(window.innerWidth / cellW);
    send({ type: 'viewport', cols });
    return cols;
  }, [send]);

  // ── The connection ──────────────────────────────────────────────────────
  useEffect(() => {
    mac.current = isMac();
    const coarse = window.matchMedia('(pointer: coarse)');
    setTouch(coarse.matches);
    const onCoarse = () => setTouch(coarse.matches);
    coarse.addEventListener('change', onCoarse);

    // Before the server's first `layout`: guess from the width, and start a
    // narrow page on the terminal, as the TUI does.
    const cols = Math.floor(window.innerWidth / measureCell(13));
    if (cols < NARROW_COLS) {
      setNarrow(true);
      setVisible(false);
      setFocus('terminal');
    }

    const c = new Connection({
      status(s, n) {
        setStatus(s);
        setFailures(n);
      },
      output(bytes) {
        term.current?.write(bytes);
      },
      message(msg: ServerMessage) {
        switch (msg.type) {
          case 'hello': {
            setHost(msg.host);
            // A new attach redraws the whole screen.
            term.current?.reset();
            const size = term.current?.size();
            if (size) c.send({ type: 'resize', ...size });
            c.send({ type: 'viewport', cols: Math.floor(window.innerWidth / measureCell(13)) });
            const { focus, visible } = state.current;
            c.send({ type: 'focus', column: focus === 'column' && visible });
            break;
          }
          case 'view':
            setView(msg.view);
            break;
          case 'layout':
            setColumnCols(msg.column_cols);
            setNarrow((was) => {
              if (msg.narrow && !was) {
                setVisible(false);
                setFocus('terminal');
              }
              return msg.narrow;
            });
            break;
          case 'request':
            if (msg.request === 'focus_terminal') {
              setFocus('terminal');
              if (state.current.narrow) setVisible(false);
            } else if (msg.request === 'focus_column') {
              // An unlock popup closed: the column comes back, as in the TUI.
              setVisible(true);
              setFocus('column');
            } else {
              // `hide`, and `quit` — the tab stays; closing it is how you quit.
              setVisible(false);
              setFocus('terminal');
            }
            break;
          case 'error':
            console.warn('tenx web:', msg.message);
            break;
        }
      },
    });
    conn.current = c;
    c.start();
    return () => {
      c.stop();
      coarse.removeEventListener('change', onCoarse);
    };
  }, []);

  // ── Tell the server where the keyboard is; put xterm's focus with it ────
  useEffect(() => {
    send({ type: 'focus', column: columnHasKeys });
    if (columnHasKeys) term.current?.blur();
    else term.current?.focus();
  }, [columnHasKeys, send]);

  // ── Width, visibility ───────────────────────────────────────────────────
  useEffect(() => {
    const onResize = () => sendViewport();
    const onVisible = () => {
      if (document.visibilityState === 'visible') send({ type: 'visible' });
    };
    const onFocus = () => send({ type: 'visible' });
    window.addEventListener('resize', onResize);
    document.addEventListener('visibilitychange', onVisible);
    window.addEventListener('focus', onFocus);
    return () => {
      window.removeEventListener('resize', onResize);
      document.removeEventListener('visibilitychange', onVisible);
      window.removeEventListener('focus', onFocus);
    };
  }, [send, sendViewport]);

  // The terminal's size follows the column's.
  useEffect(() => {
    requestAnimationFrame(() => term.current?.fit());
  }, [visible, narrow, columnCols, fontSize]);

  const cycle = useCallback(() => {
    const { focus, visible } = state.current;
    if (!visible) {
      setVisible(true);
      setFocus('column');
    } else if (focus === 'terminal') {
      setFocus('column');
    } else {
      setVisible(false);
      setFocus('terminal');
    }
  }, []);

  // ── Keys ────────────────────────────────────────────────────────────────
  useEffect(() => {
    const onKey = (ev: KeyboardEvent) => {
      if (isModifierOnly(ev)) return;
      if (isFocusCycle(ev, mac.current)) {
        ev.preventDefault();
        ev.stopPropagation();
        cycle();
        return;
      }
      const { focus, visible, ctrlSticky } = state.current;
      if (focus !== 'column' || !visible) return; // xterm has it
      if (ev.metaKey) return; // the browser's own: Cmd+R, Cmd+C, …
      ev.preventDefault();
      ev.stopPropagation();
      if (isNewTaskAlias(ev, mac.current)) {
        sendKey({ key: 'n', ctrl: true, alt: false, shift: false });
        return;
      }
      const msg = keyMessage(ev);
      if (ctrlSticky) {
        msg.ctrl = true;
        setCtrlSticky(false);
      }
      send(msg);
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [cycle, send, sendKey]);

  const onTerminalData = useCallback((data: string) => {
    if (state.current.ctrlSticky && data.length === 1) {
      setCtrlSticky(false);
      conn.current?.input(ctrlChar(data));
      return;
    }
    conn.current?.input(data);
  }, []);

  const onClick = useCallback(
    (click: Click) => {
      setFocus('column');
      send({ type: 'click', ...click });
    },
    [send],
  );

  const waiting = view?.items.filter((i) => i.kind === 'task' && (i.status === 'blocked' || i.status === 'signaled')).length ?? 0;

  const overlay = narrow;
  const columnStyle = overlay ? undefined : { width: `${Math.ceil(columnCols * cell + 20)}px` };
  const showFab = (touch || narrow) && !(visible && overlay);

  return (
    <div className={`app${overlay ? ' narrow' : ''}${touch ? ' touch' : ''}`}>
      <div className="main">
        {visible && (
          <div className={overlay ? 'column-wrap overlay' : 'column-wrap'} style={columnStyle}>
            <Column
              view={view}
              focused={columnHasKeys}
              status={status}
              failures={failures}
              host={host}
              onClick={onClick}
              onFocus={() => setFocus('column')}
            />
          </div>
        )}
        <div className="terminal-wrap" onMouseDown={() => setFocus('terminal')}>
          <Terminal
            ref={term}
            fontSize={fontSize}
            onData={onTerminalData}
            onResize={(cols, rows) => send({ type: 'resize', cols, rows })}
            onBell={bell}
            intercept={(ev) => isFocusCycle(ev, mac.current)}
            onFocus={() => {
              if (!state.current.narrow || !state.current.visible) setFocus('terminal');
            }}
          />
          {showFab && (
            <button type="button" className="fab" data-testid="fab" onClick={cycle}>
              {waiting > 0 && <span className="warn">●</span>}
              {waiting > 0 && <span>{waiting}</span>}
              <span className="muted">tasks</span>
            </button>
          )}
        </div>
      </div>
      {touch && (
        <KeyBar
          column={columnHasKeys}
          ctrlSticky={ctrlSticky}
          onCtrl={() => setCtrlSticky((s) => !s)}
          onKey={(key, shift) => {
            if (columnHasKeys) {
              sendKey({ key, ctrl: state.current.ctrlSticky, alt: false, shift });
              setCtrlSticky(false);
            } else if (key === 'A' || key === 'D') {
              // Answer the task in front of you: the column's A/D act on its
              // selection, and focusing the column selects the current task.
              send({ type: 'focus', column: true });
              sendKey({ key, ctrl: false, alt: false, shift: true });
              send({ type: 'focus', column: false });
            } else {
              onTerminalData(TERMINAL_BYTES[key] ?? key);
            }
          }}
        />
      )}
    </div>
  );
}
