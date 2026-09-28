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
import { asTyped, upload } from '@/lib/paste';
import * as push from '@/lib/push';
import type { Action, Click, ColumnView, KeyMessage, ServerMessage } from '@/protocol';

type Focus = 'column' | 'terminal';

/** Under this many cells the column is an overlay (tmux SMALL_CLIENT_COLS),
 * until the server says otherwise in `layout`. */
const NARROW_COLS = 100;

const HW_KEY = 'tenx-hardware-keyboard';
const FONT_KEY = 'tenx-terminal-font';

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
  /** The on-screen keyboard is up: the key bar sits on it, not above the
   * home indicator's inset. */
  const [kbOpen, setKbOpen] = useState(false);
  /** A paste on its way up, or why it failed. */
  const [pasting, setPasting] = useState<string | null>(null);
  /** The notice is a button that brings the keyboard back (after 📎). */
  const [pastingTap, setPastingTap] = useState(false);
  /** Keys arrive while no on-screen keyboard is up: a hardware keyboard
   * (an iPad's). Remembered per device; the on-screen keyboard appearing
   * undoes it. */
  const [hwKeyboard, setHwKeyboard] = useState(false);
  const [pushState, setPushState] = useState<push.PushState>('unsupported');
  /** A task to open once the column has rows: from `?task=` (a notification
   * opened the page) or the service worker (it focused this page). */
  const wantTask = useRef<string | null>(null);
  const lastView = useRef<ColumnView | null>(null);

  const conn = useRef<Connection | null>(null);
  /** Off-screen: focusing it is what raises a phone's keyboard for the column. */
  const kbd = useRef<HTMLInputElement>(null);
  const term = useRef<TerminalHandle>(null);
  const mac = useRef(false);
  const [fontPref, setFontPref] = useState<{ size?: number; weight?: number }>({});
  const fontSize = fontPref.size ?? (touch && narrow ? 12 : 13);
  const fontWeight = fontPref.weight ?? 400;

  // The terminal's font, overridable per device from the address bar —
  // `?font=12&weight=300` — and remembered (`?font=&weight=` forgets it).
  useEffect(() => {
    const q = new URLSearchParams(location.search);
    let pref: { size?: number; weight?: number } = {};
    try {
      pref = JSON.parse(localStorage.getItem(FONT_KEY) ?? '{}');
    } catch {
      // None stored, or storage blocked.
    }
    const num = (v: string | null, lo: number, hi: number) => {
      const n = Number(v);
      return v && Number.isFinite(n) && n >= lo && n <= hi ? n : undefined;
    };
    if (q.has('font')) pref.size = num(q.get('font'), 8, 32);
    if (q.has('weight')) pref.weight = num(q.get('weight'), 300, 600);
    if (q.has('font') || q.has('weight')) {
      try {
        localStorage.setItem(FONT_KEY, JSON.stringify(pref));
      } catch {
        // Applies to this visit only.
      }
    }
    setFontPref(pref);
  }, []);

  // Latest state for the listeners registered once.
  const state = useRef({ focus, visible, narrow, ctrlSticky, touch, kbOpen });
  state.current = { focus, visible, narrow, ctrlSticky, touch, kbOpen };

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
            lastView.current = msg.view;
            openWanted(msg.view);
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
    else {
      kbd.current?.blur();
      term.current?.focus();
    }
  }, [columnHasKeys, send]);

  // ── Notifications, the badge, and opening a task from one ─────────────
  // `openWanted` runs from the connection's handler, registered once: it
  // reads only refs and the connection.
  const openWanted = (v: ColumnView) => {
    const id = wantTask.current;
    if (!id || !v.items.some((i) => i.kind === 'task' && i.id === id)) return;
    wantTask.current = null;
    conn.current?.send({ type: 'click', kind: 'task', id });
    conn.current?.send({ type: 'action', name: 'open' });
    setFocus('terminal');
    if (state.current.narrow) setVisible(false);
  };

  useEffect(() => {
    const url = new URL(location.href);
    const task = url.searchParams.get('task');
    if (task) {
      wantTask.current = task;
      url.searchParams.delete('task');
      history.replaceState(null, '', url.pathname + (url.search || '') + url.hash);
    }
    push.register().then(() => push.state()).then(setPushState);
    const onMessage = (ev: MessageEvent) => {
      if (ev.data?.type === 'open-task' && typeof ev.data.task === 'string') {
        wantTask.current = ev.data.task;
        if (lastView.current) openWanted(lastView.current);
      }
    };
    navigator.serviceWorker?.addEventListener('message', onMessage);
    return () => navigator.serviceWorker?.removeEventListener('message', onMessage);
  }, []);

  const needsYou = view?.items.filter((i) => i.kind === 'task' && (i.status === 'blocked' || i.status === 'signaled')).length ?? 0;
  useEffect(() => push.badge(needsYou), [needsYou]);

  // ── A hardware keyboard ─────────────────────────────────────────────────
  // No browser says whether one is attached; what gives it away is a key
  // press while the viewport shows no on-screen keyboard (iOS doesn't raise
  // it when a keyboard is connected). The special-key bar is then only in
  // the way. The on-screen keyboard coming up turns it back on.
  const rememberHw = useCallback((on: boolean) => {
    setHwKeyboard(on);
    try {
      if (on) localStorage.setItem(HW_KEY, '1');
      else localStorage.removeItem(HW_KEY);
    } catch {
      // Storage blocked: it's detected again on the next key.
    }
  }, []);
  useEffect(() => {
    try {
      setHwKeyboard(localStorage.getItem(HW_KEY) === '1');
    } catch {
      // As above.
    }
  }, []);

  // ── The visible area ────────────────────────────────────────────────────
  // A phone's keyboard covers the page rather than shrinking it (iOS ignores
  // `interactive-widget`), and Safari scrolls the page to keep the focused
  // input in view. Pin the page to the visual viewport instead, so the
  // terminal ends — and the key bar sits — right on top of the keyboard.
  useEffect(() => {
    const vv = window.visualViewport;
    if (!vv) return;
    // The tallest the viewport has been at this width: the height with no
    // keyboard. Measured rather than taken from innerHeight, which Android
    // shrinks along with the viewport (`interactive-widget`) and iOS doesn't.
    let full = { width: vv.width, height: vv.height };
    const apply = () => {
      const root = document.documentElement.style;
      root.setProperty('--vv-h', `${vv.height}px`);
      root.setProperty('--vv-top', `${vv.offsetTop}px`);
      if (Math.abs(vv.width - full.width) > 1) full = { width: vv.width, height: vv.height };
      else full.height = Math.max(full.height, vv.height, window.innerHeight);
      const open = full.height - vv.height > 120;
      setKbOpen(open);
      if (open) rememberHw(false);
    };
    apply();
    vv.addEventListener('resize', apply);
    vv.addEventListener('scroll', apply);
    return () => {
      vv.removeEventListener('resize', apply);
      vv.removeEventListener('scroll', apply);
    };
  }, []);

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
      const { touch, kbOpen } = state.current;
      if (touch && !kbOpen && !ev.isComposing && ev.key !== 'Unidentified' && ev.keyCode !== 229) rememberHw(true);
      if (isModifierOnly(ev)) return;
      // A row's menu or sheet is open: it takes Escape itself, and nothing
      // else should reach the column or the terminal behind it.
      if (document.querySelector('[data-popup]')) return;
      if (isFocusCycle(ev, mac.current)) {
        ev.preventDefault();
        ev.stopPropagation();
        cycle();
        return;
      }
      const { focus, visible, ctrlSticky } = state.current;
      if (focus !== 'column' || !visible) return; // xterm has it
      if (ev.metaKey) return; // the browser's own: Cmd+R, Cmd+C, …
      // A soft keyboard that doesn't name its keys (Android: 229 /
      // "Unidentified", or mid-composition): let the text reach the hidden
      // input, whose `input` event sends it.
      if (ev.isComposing || ev.key === 'Unidentified' || ev.keyCode === 229) return;
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
  }, [cycle, send, sendKey, rememberHw]);

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

  const onAction = useCallback(
    (name: Action) => {
      setFocus('column');
      send({ type: 'action', name });
    },
    [send],
  );

  const onColumnKey = useCallback(
    (key: string, shift = false) => {
      setFocus('column');
      sendKey({ key, ctrl: false, alt: false, shift });
    },
    [sendKey],
  );

  /** Images for the agent: uploaded, then their paths pasted where the
   * cursor is. */
  const pasteImages = useCallback(async (files: File[]) => {
    setFocus('terminal');
    setPasting(files.length > 1 ? `uploading ${files.length} images…` : 'uploading image…');
    try {
      const paths: string[] = [];
      for (const f of files) paths.push(asTyped(await upload(f)));
      term.current?.paste(paths.join(' ') + ' ');
      term.current?.focus();
      setPasting(null);
      // The photo picker took the keyboard away, and iOS only brings it back
      // for a focus inside a tap — so offer the tap.
      setTimeout(() => {
        if (state.current.touch && !state.current.kbOpen) {
          setPastingTap(true);
          setPasting('pasted · tap here to keep typing');
          setTimeout(() => {
            setPasting(null);
            setPastingTap(false);
          }, 6000);
        }
      }, 400);
    } catch (e) {
      setPasting(`paste failed: ${e instanceof Error ? e.message : String(e)}`);
      setTimeout(() => setPasting(null), 4000);
    }
  }, []);

  const notice = useCallback((text: string) => {
    setPasting(text);
    setTimeout(() => setPasting(null), 4000);
  }, []);

  /** The 📋 key: whatever is on the phone's clipboard — an image goes up
   * like a picked one, text is pasted as typed. The Clipboard API only
   * exists on HTTPS (or localhost), so over plain http this can only say so. */
  const pasteClipboard = useCallback(async () => {
    if (!navigator.clipboard?.read) {
      notice('reading the clipboard needs HTTPS — open tenx web through `tailscale serve`');
      return;
    }
    try {
      const images: File[] = [];
      let text = '';
      for (const item of await navigator.clipboard.read()) {
        const image = item.types.find((t) => t.startsWith('image/'));
        if (image) {
          const blob = await item.getType(image);
          images.push(new File([blob], 'clipboard', { type: image }));
        } else if (item.types.includes('text/plain')) {
          text += await (await item.getType('text/plain')).text();
        }
      }
      if (images.length) await pasteImages(images);
      else if (text) {
        setFocus('terminal');
        term.current?.paste(text);
      } else notice('the clipboard is empty');
    } catch (e) {
      notice(`couldn't read the clipboard: ${e instanceof Error ? e.message : String(e)}`);
    }
  }, [notice, pasteImages]);

  const togglePush = useCallback(() => {
    // Straight from the click: the permission prompt needs the gesture.
    const next = pushState === 'on' ? push.disable() : push.enable();
    next.then(setPushState).catch((e) => notice(`notifications: ${e instanceof Error ? e.message : String(e)}`));
  }, [pushState, notice]);

  const testPush = useCallback(() => {
    push
      .test()
      .then(({ sent, subscriptions }) => notice(`test sent to ${sent} of ${subscriptions} device${subscriptions === 1 ? '' : 's'}`))
      .catch((e) => notice(`test failed: ${e instanceof Error ? e.message : String(e)}`));
  }, [notice]);

  const hide = useCallback(() => {
    setVisible(false);
    setFocus('terminal');
  }, []);

  // Called from the tap itself: a phone only raises its keyboard for a
  // focus() made inside a user gesture.
  const wantKeyboard = useCallback(() => {
    if (state.current.touch) kbd.current?.focus({ preventScroll: true });
  }, []);

  /** What the soft keyboard typed into the hidden input, as column keys. */
  const onKbdInput = useCallback(
    (ev: React.FormEvent<HTMLInputElement>) => {
      const input = ev.nativeEvent as InputEvent;
      const el = ev.currentTarget;
      if (input.inputType === 'deleteContentBackward') {
        sendKey({ key: 'Backspace', ctrl: false, alt: false, shift: false });
      } else if (input.inputType === 'insertLineBreak') {
        sendKey({ key: 'Enter', ctrl: false, alt: false, shift: false });
      } else {
        for (const ch of input.data ?? el.value) sendKey({ key: ch, ctrl: false, alt: false, shift: false });
      }
      el.value = '';
    },
    [sendKey],
  );

  const waiting = view?.items.filter((i) => i.kind === 'task' && (i.status === 'blocked' || i.status === 'signaled')).length ?? 0;

  const overlay = narrow;
  const columnStyle = overlay ? undefined : { width: `${Math.ceil(columnCols * cell + 20)}px` };
  // Whenever the column is hidden, and on touch screens beside it too.
  const showFab = !visible || (touch && !overlay);

  return (
    <div className={`app${overlay ? ' narrow' : ''}${touch ? ' touch' : ''}${kbOpen ? ' kb-open' : ''}`}>
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
              onAction={onAction}
              onKey={onColumnKey}
              onWantKeyboard={wantKeyboard}
              onHide={hide}
              push={pushState}
              onPushToggle={togglePush}
              onPushTest={testPush}
              touch={touch}
            />
          </div>
        )}
        <div className="terminal-wrap" onMouseDown={() => setFocus('terminal')}>
          <Terminal
            ref={term}
            fontSize={fontSize}
            fontWeight={fontWeight}
            onData={onTerminalData}
            onResize={(cols, rows) => send({ type: 'resize', cols, rows })}
            onBell={bell}
            intercept={(ev) => isFocusCycle(ev, mac.current)}
            onFocus={() => {
              if (!state.current.narrow || !state.current.visible) setFocus('terminal');
            }}
            onImages={pasteImages}
          />
          {pasting && (
            <div
              className={pastingTap ? 'toast tap' : 'toast'}
              role="status"
              onClick={() => {
                if (!pastingTap) return;
                term.current?.focus();
                setPasting(null);
                setPastingTap(false);
              }}
            >
              {pasting}
            </div>
          )}
          {showFab && (
            <button type="button" className="fab" data-testid="fab" onClick={cycle}>
              {waiting > 0 && <span className="warn">●</span>}
              {waiting > 0 && <span>{waiting}</span>}
              <span className="muted">tasks</span>
            </button>
          )}
        </div>
      </div>
      <input
        ref={kbd}
        className="kbd-sink"
        aria-hidden="true"
        tabIndex={-1}
        autoCapitalize="off"
        autoCorrect="off"
        autoComplete="off"
        spellCheck={false}
        onInput={onKbdInput}
      />
      {touch && (
        <KeyBar
          compact={hwKeyboard}
          column={columnHasKeys}
          ctrlSticky={ctrlSticky}
          onCtrl={() => setCtrlSticky((s) => !s)}
          onImages={pasteImages}
          onPaste={pasteClipboard}
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
