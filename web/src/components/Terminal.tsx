'use client';

// The tmux client: xterm.js on the bytes of `tmux attach -t tenx-web-<id>`.
// Everything inside it — panes, borders, the status line — is tmux's own.

import { forwardRef, useEffect, useImperativeHandle, useRef } from 'react';
import type { Terminal as XTerm } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';
import { palette } from '@/palette';
import { arrow, planTap, type Cell } from '@/lib/tapcursor';
import { imagesIn } from '@/lib/paste';
import { Accumulator, FLING_STOP, LINES_PER_NOTCH, SCROLL_SLOP_PX, decay, wheelNotch } from '@/lib/touchscroll';

export interface TerminalHandle {
  write(bytes: Uint8Array): void;
  reset(): void;
  focus(): void;
  blur(): void;
  fit(): void;
  size(): { cols: number; rows: number } | null;
  /** Paste text as the terminal would (bracketed when the program asked). */
  paste(text: string): void;
}

interface Props {
  fontSize: number;
  /** Keyboard input, as the string xterm produced for it. */
  onData(data: string): void;
  onResize(cols: number, rows: number): void;
  onBell(): void;
  /** A key the page keeps (^w) — xterm must not see it. */
  intercept(ev: KeyboardEvent): boolean;
  onFocus(): void;
  /** Images pasted or dropped on the terminal. */
  onImages(files: File[]): void;
}

// The ANSI colours agg renders the demo with (Makefile `demo-gif`): the
// palette's hues on its ground.
const theme = {
  background: palette.GROUND,
  foreground: palette.TEXT,
  cursor: palette.TEXT,
  cursorAccent: palette.GROUND,
  selectionBackground: '#3a4152',
  black: palette.GROUND,
  red: palette.DANGER,
  green: palette.SUCCESS,
  yellow: palette.WARN,
  blue: palette.INFO,
  magenta: palette.ACCENT,
  cyan: palette.INFO,
  white: palette.TEXT,
  brightBlack: palette.IDLE,
  brightRed: palette.DANGER,
  brightGreen: palette.SUCCESS,
  brightYellow: palette.WARN,
  brightBlue: palette.CURRENT,
  brightMagenta: palette.ACCENT,
  brightCyan: palette.CURRENT,
  brightWhite: palette.BRIGHT,
};

export const Terminal = forwardRef<TerminalHandle, Props>(function Terminal(props, ref) {
  const host = useRef<HTMLDivElement>(null);
  const term = useRef<XTerm | null>(null);
  const fitRef = useRef<{ fit(): void } | null>(null);
  // Bytes that arrive before xterm has loaded (it is imported lazily).
  const pending = useRef<Uint8Array[]>([]);
  const propsRef = useRef(props);
  propsRef.current = props;

  useImperativeHandle(ref, () => ({
    write(bytes) {
      if (term.current) term.current.write(bytes);
      else pending.current.push(bytes);
    },
    reset() {
      pending.current = [];
      term.current?.reset();
    },
    focus() {
      term.current?.focus();
    },
    blur() {
      term.current?.blur();
    },
    fit() {
      try {
        fitRef.current?.fit();
      } catch {
        // Not laid out yet.
      }
    },
    size() {
      return term.current ? { cols: term.current.cols, rows: term.current.rows } : null;
    },
    paste(text) {
      term.current?.paste(text);
    },
  }));

  useEffect(() => {
    let disposed = false;
    let observer: ResizeObserver | null = null;
    let xterm: XTerm | null = null;
    (async () => {
      const [{ Terminal: XTermCtor }, { FitAddon }, { WebLinksAddon }, { ClipboardAddon }] = await Promise.all([
        import('@xterm/xterm'),
        import('@xterm/addon-fit'),
        import('@xterm/addon-web-links'),
        import('@xterm/addon-clipboard'),
      ]);
      try {
        await document.fonts.load(`${propsRef.current.fontSize}px "JetBrains Mono"`);
      } catch {
        // Falls back to the next monospace in the stack.
      }
      if (disposed || !host.current) return;
      xterm = new XTermCtor({
        fontFamily: '"JetBrains Mono", ui-monospace, Menlo, monospace',
        fontSize: propsRef.current.fontSize,
        lineHeight: 1.1,
        theme,
        cursorBlink: true,
        allowProposedApi: true,
        scrollback: 0, // tmux keeps the history; copy-mode scrolls it.
        macOptionIsMeta: true,
      });
      const fit = new FitAddon();
      xterm.loadAddon(fit);
      xterm.loadAddon(new WebLinksAddon());
      xterm.loadAddon(new ClipboardAddon());
      xterm.open(host.current);
      // WebGL on desktops only: a phone's grid is small enough for the DOM
      // renderer, and mobile GPUs are where WebGL draws nothing. `?renderer=`
      // `dom` / `webgl` overrides either way.
      const pick = new URLSearchParams(location.search).get('renderer');
      const webglWanted = pick ? pick === 'webgl' : !window.matchMedia('(pointer: coarse)').matches;
      if (webglWanted) try {
        const { WebglAddon } = await import('@xterm/addon-webgl');
        const webgl = new WebglAddon();
        webgl.onContextLoss(() => {
          // xterm falls back to its DOM renderer, which starts blank.
          console.warn('tenx web: WebGL context lost, using the DOM renderer');
          webgl.dispose();
          xterm?.refresh(0, xterm.rows - 1);
        });
        xterm.loadAddon(webgl);
      } catch {
        // No WebGL: xterm's DOM renderer draws instead.
      }
      xterm.attachCustomKeyEventHandler((ev) => !propsRef.current.intercept(ev));
      xterm.onData((d) => propsRef.current.onData(d));
      xterm.onBinary((d) => propsRef.current.onData(d));
      xterm.onResize(({ cols, rows }) => propsRef.current.onResize(cols, rows));
      xterm.onBell(() => propsRef.current.onBell());
      xterm.textarea?.addEventListener('focus', () => propsRef.current.onFocus());
      const gesture = { scrolled: false };
      touchScroll(xterm, host.current, (d) => propsRef.current.onData(d), gesture);
      tapToPlaceCursor(xterm, host.current, (d) => propsRef.current.onData(d), gesture);
      catchImages(host.current, (files) => propsRef.current.onImages(files));
      term.current = xterm;
      fitRef.current = fit;
      for (const b of pending.current) xterm.write(b);
      pending.current = [];
      fit.fit();
      propsRef.current.onResize(xterm.cols, xterm.rows);
      observer = new ResizeObserver(() => {
        try {
          fit.fit();
        } catch {
          // Hidden (display: none) while the column overlays it.
        }
      });
      observer.observe(host.current);
    })();
    return () => {
      disposed = true;
      observer?.disconnect();
      xterm?.dispose();
      term.current = null;
    };
  }, []);

  return <div className="xterm-host" ref={host} data-testid="terminal" />;
});

/** A tap — or an Alt/Option-click — in the text being edited walks the
 * cursor there with arrow keys (lib/tapcursor). The click still goes on to
 * tmux, which only selects the pane it is already in. */
/** What the touch handlers share: a drag that scrolled is not a tap. */
interface Gesture {
  scrolled: boolean;
}

/** The cell under a point on the terminal, or null outside it. */
function cellAt(xterm: XTerm, el: HTMLElement, clientX: number, clientY: number): Cell | null {
  const screen = el.querySelector('.xterm-screen');
  if (!screen) return null;
  const r = screen.getBoundingClientRect();
  const x = Math.floor(((clientX - r.left) / r.width) * xterm.cols);
  const y = Math.floor(((clientY - r.top) / r.height) * xterm.rows);
  return x < 0 || y < 0 || x >= xterm.cols || y >= xterm.rows ? null : { x, y };
}

/** A vertical finger drag scrolls the pane under the finger through tmux's
 * history (lib/touchscroll), and a flick keeps going for a moment. */
function touchScroll(xterm: XTerm, el: HTMLElement, send: (data: string) => void, gesture: Gesture) {
  let start: { x: number; y: number } | null = null;
  let lastY = 0;
  let anchor: Cell | null = null;
  let scrolling = false;
  let acc = new Accumulator(1);
  let samples: { y: number; t: number }[] = [];
  let fling = 0;

  const notches = (n: number) => {
    if (!anchor || n === 0) return;
    send(wheelNotch(n > 0, anchor.x, anchor.y).repeat(Math.abs(n)));
  };
  const stopFling = () => {
    if (fling) cancelAnimationFrame(fling);
    fling = 0;
  };

  el.addEventListener(
    'pointerdown',
    (ev) => {
      if (ev.pointerType !== 'touch') return;
      stopFling();
      start = { x: ev.clientX, y: ev.clientY };
      lastY = ev.clientY;
      scrolling = false;
      gesture.scrolled = false;
      const screen = el.querySelector('.xterm-screen');
      const lineH = screen ? screen.getBoundingClientRect().height / xterm.rows : 16;
      acc = new Accumulator(lineH * LINES_PER_NOTCH);
      samples = [{ y: ev.clientY, t: performance.now() }];
    },
    true,
  );
  el.addEventListener(
    'pointermove',
    (ev) => {
      if (!start || ev.pointerType !== 'touch') return;
      if (!scrolling) {
        const dx = ev.clientX - start.x;
        const dy = ev.clientY - start.y;
        if (Math.abs(dy) < SCROLL_SLOP_PX || Math.abs(dy) < Math.abs(dx)) return;
        anchor = cellAt(xterm, el, start.x, start.y);
        if (!anchor) return;
        scrolling = true;
        gesture.scrolled = true;
        lastY = start.y;
      }
      ev.preventDefault();
      ev.stopPropagation();
      notches(acc.add(ev.clientY - lastY));
      lastY = ev.clientY;
      const now = performance.now();
      samples.push({ y: ev.clientY, t: now });
      samples = samples.filter((s) => now - s.t < 100);
    },
    true,
  );
  const end = (ev: PointerEvent) => {
    if (ev.pointerType !== 'touch' || !start) return;
    start = null;
    if (!scrolling) return;
    // The flick: the finger's speed over its last 100 ms, then decaying.
    const first = samples[0];
    const last = samples[samples.length - 1];
    let v = first && last && last.t > first.t ? (last.y - first.y) / (last.t - first.t) : 0;
    let t = performance.now();
    const step = (now: number) => {
      const dt = now - t;
      t = now;
      notches(acc.add(v * dt));
      v = decay(v, dt);
      fling = Math.abs(v) > FLING_STOP ? requestAnimationFrame(step) : 0;
    };
    if (Math.abs(v) > FLING_STOP * 4) fling = requestAnimationFrame(step);
  };
  el.addEventListener('pointerup', end, true);
  el.addEventListener('pointercancel', end, true);
}

function tapToPlaceCursor(xterm: XTerm, el: HTMLElement, send: (data: string) => void, gesture: Gesture) {
  let down: { x: number; y: number; t: number } | null = null;
  let moving = false;
  // Capture: xterm handles these itself on the way down.
  el.addEventListener(
    'pointerdown',
    (ev) => {
      down = { x: ev.clientX, y: ev.clientY, t: performance.now() };
    },
    true,
  );
  el.addEventListener('pointerup', (ev) => {
    const start = down;
    down = null;
    if (!start || moving || gesture.scrolled) return;
    const isTap = ev.pointerType === 'touch' || ev.pointerType === 'pen' || ev.altKey;
    const still = Math.hypot(ev.clientX - start.x, ev.clientY - start.y) < 10 && performance.now() - start.t < 500;
    if (!isTap || !still) return;
    const screen = el.querySelector('.xterm-screen');
    if (!screen) return;
    const r = screen.getBoundingClientRect();
    const tap: Cell = {
      x: Math.floor(((ev.clientX - r.left) / r.width) * xterm.cols),
      y: Math.floor(((ev.clientY - r.top) / r.height) * xterm.rows),
    };
    if (tap.x < 0 || tap.y < 0 || tap.x >= xterm.cols || tap.y >= xterm.rows) return;
    const plan = planTap(screenRows(xterm), cursorOf(xterm), tap);
    if (!plan) return;
    moving = true;
    walk(xterm, plan.target, plan.vertical, send).finally(() => {
      moving = false;
    });
  }, true);
}

function screenRows(xterm: XTerm): string[] {
  const buf = xterm.buffer.active;
  const rows: string[] = [];
  for (let y = 0; y < xterm.rows; y++) rows.push(buf.getLine(buf.viewportY + y)?.translateToString(false) ?? '');
  return rows;
}

function cursorOf(xterm: XTerm): Cell {
  const buf = xterm.buffer.active;
  return { x: buf.cursorX, y: buf.cursorY + buf.baseY - buf.viewportY };
}

const settle = () => new Promise((r) => setTimeout(r, 70));

/** Step toward `target`, re-reading the cursor after each move: the program
 * decides where an arrow lands (wide characters, wrapped lines, a line too
 * short to reach the tapped column), so this converges rather than computing
 * the whole path up front. Rows move one at a time and stop the moment an
 * arrow doesn't move the cursor — at the top of Claude's input another ↑
 * would recall history. */
async function walk(xterm: XTerm, target: Cell, vertical: boolean, send: (data: string) => void) {
  const app = () => xterm.modes.applicationCursorKeysMode;
  for (let step = 0; step < 40; step++) {
    const at = cursorOf(xterm);
    if (vertical && at.y !== target.y) {
      send(arrow(target.y < at.y ? 'up' : 'down', app()));
      await settle();
      if (cursorOf(xterm).y === at.y) return;
      continue;
    }
    const dx = target.x - at.x;
    if (dx === 0) return;
    send(arrow(dx < 0 ? 'left' : 'right', app()).repeat(Math.abs(dx)));
    await settle();
    const now = cursorOf(xterm);
    // Stuck (end of the line) or overshot back and forth: stop.
    if (now.x === at.x || Math.sign(target.x - now.x) === -Math.sign(dx)) return;
  }
}

/** A paste or a drop that carries an image goes to `onImages` instead of
 * xterm (which would paste nothing, or the file's name). Text pastes are
 * left alone. Capture phase, ahead of xterm's own paste handler. */
function catchImages(el: HTMLElement, onImages: (files: File[]) => void) {
  el.addEventListener(
    'paste',
    (ev) => {
      const files = imagesIn(ev.clipboardData);
      if (files.length === 0) return;
      ev.preventDefault();
      ev.stopPropagation();
      onImages(files);
    },
    true,
  );
  el.addEventListener('dragover', (ev) => {
    if (Array.from(ev.dataTransfer?.items ?? []).some((i) => i.kind === 'file')) ev.preventDefault();
  });
  el.addEventListener('drop', (ev) => {
    const files = imagesIn(ev.dataTransfer);
    if (files.length === 0) return;
    ev.preventDefault();
    onImages(files);
  });
}
