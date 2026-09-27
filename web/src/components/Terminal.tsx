'use client';

// The tmux client: xterm.js on the bytes of `tmux attach -t tenx-web-<id>`.
// Everything inside it — panes, borders, the status line — is tmux's own.

import { forwardRef, useEffect, useImperativeHandle, useRef } from 'react';
import type { Terminal as XTerm } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';
import { palette } from '@/palette';

export interface TerminalHandle {
  write(bytes: Uint8Array): void;
  reset(): void;
  focus(): void;
  blur(): void;
  fit(): void;
  size(): { cols: number; rows: number } | null;
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
