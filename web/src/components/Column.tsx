'use client';

// The column, drawn from the ColumnView the server sends: nothing here
// decides what to show — which rows, which chips, which hint — only how it
// looks. Clicks go back by id; keys go through the page (App).
//
// Everything a key does can also be done with a pointer, where the thing it
// acts on is: a click selects a row and a double click opens it; a blocked
// row's chip answers it (allow / deny); ⋯, a right-click or a long-press opens
// the row's menu (a sheet on touch); a swipe reveals allow / deny or delete;
// the header (Header.tsx) holds what doesn't depend on a row; forms carry
// their own submit / cancel, and a delete confirms on the row it deletes.
// All of it sends the same keys and clicks the keyboard would.

import { Fragment, useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { PopupLayer, Slide, jobEntries, subEntries, taskEntries, useRowGestures, type Popup, type Reveal } from './RowActions';
import type { Action, Click, ColumnView, Footer, Item, JobItem, ModeView, SubItem, TaskItem } from '@/protocol';

interface Props {
  view: ColumnView | null;
  /** The column has the keyboard: show the selection and the caret. */
  focused: boolean;
  onClick(click: Click): void;
  onFocus(): void;
  /** An action-bar button (view.rs `Action`). */
  onAction(action: Action): void;
  /** A key, as if typed with the column focused (form and prompt buttons). */
  onKey(key: string, shift?: boolean): void;
  /** Something that takes typing was tapped: raise the on-screen keyboard. */
  onWantKeyboard(): void;
  /** A touch screen: long-press opens a sheet, rows swipe. */
  touch: boolean;
}

/** Keys and clicks, for the pieces below. */
interface Ctl {
  onClick(click: Click): void;
  onKey(key: string, shift?: boolean): void;
  onWantKeyboard(): void;
}

export function Column(props: Props) {
  const { view, focused, onClick, onFocus, onAction, onKey, onWantKeyboard } = props;
  const listRef = useRef<HTMLDivElement>(null);

  // Keep the selected row in view as the cursor moves.
  useEffect(() => {
    const list = listRef.current;
    const sel = list?.querySelector<HTMLElement>('.sel');
    if (!list || !sel) return;
    const top = sel.offsetTop - list.offsetTop;
    if (top < list.scrollTop) list.scrollTop = top;
    else if (top + sel.offsetHeight > list.scrollTop + list.clientHeight) {
      list.scrollTop = top + sel.offsetHeight - list.clientHeight;
    }
  }, [view]);

  const mode = view?.mode ?? { kind: 'list' };
  const ctl: Ctl = { onClick, onKey, onWantKeyboard };
  const form = formFor(mode, ctl);
  const [popup, setPopup] = useState<Popup | null>(null);
  const [reveal, setReveal] = useState<{ id: string; side: Reveal } | null>(null);
  const closePopup = useCallback(() => setPopup(null), []);

  // Leaving list mode (a form, a confirm) or a new list: nothing stays open.
  useEffect(() => {
    if (mode.kind !== 'list') {
      setPopup(null);
      setReveal(null);
    }
  }, [mode.kind]);

  const rows: RowCtl = {
    focused,
    touch: props.touch,
    confirming: mode.kind === 'confirm',
    onClick,
    onKey,
    onOpen: () => onAction('open'),
    act: (click, action) => {
      onClick(click);
      onAction(action);
    },
    openPopup: setPopup,
    reveal,
    setReveal: (id, side) => setReveal(side ? { id, side } : null),
  };

  return (
    <aside className="column" data-testid="column" onMouseDown={onFocus}>
      {view && (
        <>
          <div className="tabs">
            {view.tabs.map((t, i) => (
              <button
                key={t.label}
                type="button"
                className={t.active ? 'tab on' : 'tab'}
                onClick={() => onClick({ kind: 'tab', index: i })}
              >
                {t.label}
                {t.running > 0 && <span className="jobs"> [{t.running}]</span>}
              </button>
            ))}
          </div>
          <SearchBox view={view} focused={focused} ctl={ctl} />
          {form ?? (
            <div className="list" ref={listRef} data-testid="list">
              {view.items.map((item, i) => (
                <ListItem key={itemKey(item, i)} item={item} first={i === 0} ctl={rows} />
              ))}
            </div>
          )}
          <FooterLine footer={view.footer} focused={focused} onKey={onKey} />
          {mode.kind === 'help' && <Help view={view} scroll={mode.scroll} onKey={onKey} />}
        </>
      )}
      {!view && <div className="list" />}
      <PopupLayer popup={popup} onClose={closePopup} />
    </aside>
  );
}

/** What a row needs to act: select-then-press, the popup, the swipe state. */
interface RowCtl {
  focused: boolean;
  touch: boolean;
  /** A delete is waiting for y / n: the selected task row asks. */
  confirming: boolean;
  onClick(c: Click): void;
  onKey(key: string, shift?: boolean): void;
  onOpen(): void;
  /** Select with `click`, then press `action`'s key. */
  act(click: Click, action: Action): void;
  openPopup(p: Popup): void;
  reveal: { id: string; side: Reveal } | null;
  setReveal(id: string, side: Reveal): void;
}

/** The ⋯ button: a menu under it, right-aligned to it. */
function More({ on, onOpen }: { on: boolean; onOpen(at: { left: number; top: number }): void }) {
  return (
    <button
      type="button"
      className={on ? 'more on' : 'more'}
      aria-label="more actions"
      data-testid="more"
      onClick={(e) => {
        e.stopPropagation();
        const r = e.currentTarget.getBoundingClientRect();
        onOpen({ left: r.right - 218, top: r.bottom + 4 });
      }}
      onDoubleClick={(e) => e.stopPropagation()}
    >
      ⋯
    </button>
  );
}

function SearchBox({ view, focused, ctl }: { view: ColumnView; focused: boolean; ctl: Ctl }) {
  if (view.mode.kind === 'rename') {
    return (
      <div className="search rename" onClick={ctl.onWantKeyboard}>
        <span className="search-title">rename task</span>
        <span className="accent">✎ </span>
        <span className="rename-text">{view.mode.buffer}</span>
        <span className="caret" />
        <span className="grow" />
        <KeyPill label="cancel" k="esc" onKey={ctl.onKey} keyName="Escape" tone="ghost" />
        <KeyPill label="save" k="⏎" onKey={ctl.onKey} keyName="Enter" tone="pri" />
      </div>
    );
  }
  const typing = focused && view.focus === 'search' && view.mode.kind === 'list';
  return (
    <div
      className={typing ? 'search typing' : 'search'}
      onClick={() => {
        ctl.onClick({ kind: 'search' });
        ctl.onWantKeyboard();
      }}
    >
      <span className="slash">/ </span>
      {view.filter ? <span>{view.filter}</span> : !typing && <span className="muted">filter</span>}
      {typing && <span className="caret" />}
    </div>
  );
}

function itemKey(item: Item, i: number): string {
  switch (item.kind) {
    case 'task':
      return `t:${item.id}`;
    case 'sub':
      return `s:${item.task}:${item.id}`;
    case 'header':
      return `h:${item.label}:${i}`;
    default:
      return `${item.kind}:${i}`;
  }
}

function ListItem({ item, first, ctl }: { item: Item; first: boolean; ctl: RowCtl }) {
  const { focused, onClick } = ctl;
  switch (item.kind) {
    case 'header':
      return (
        <div className={first ? 'group' : 'group gap'} style={{ color: item.color }}>
          <span>{item.label}</span>
          {item.count != null && <span className="count">{item.count}</span>}
        </div>
      );
    case 'task':
      return <TaskRow task={item} ctl={ctl} />;
    case 'sub':
      return <SubRow sub={item} ctl={ctl} />;
    case 'repo':
      return (
        <div
          className={item.selected && focused ? 'row repo sel' : 'row repo'}
          onClick={() => onClick({ kind: 'item', pos: item.pos })}
        >
          <div className="line1">
            <span className="glyph" style={{ color: item.cloned ? 'var(--success)' : 'var(--idle)' }}>
              {item.cloned ? '✔' : '·'}
            </span>
            <span className="title">{item.name}</span>
          </div>
          <div className="line2 muted">{item.detail}</div>
        </div>
      );
    case 'job':
      return <JobRow job={item} ctl={ctl} />;
    case 'empty':
      return (
        <div className="empty">
          {item.lines.map((l, i) => (
            <div key={i} className="ln">
              {l}
            </div>
          ))}
        </div>
      );
  }
}

function TaskRow({ task, ctl }: { task: TaskItem; ctl: RowCtl }) {
  const select: Click = { kind: 'task', id: task.id };
  const act = (a: Action) => ctl.act(select, a);
  const reveal = ctl.reveal?.id === task.id ? ctl.reveal.side : null;
  const gesture = useRowGestures({
    left: ctl.touch && task.answerable,
    right: ctl.touch && !task.pending,
    reveal,
    setReveal: (side) => ctl.setReveal(task.id, side),
    onFullSwipe: () => act('approve'),
    onLongPress: () => sheet(),
  });

  // Touch: the sheet, answers first; its header says what the task wants.
  function sheet() {
    ctl.onClick(select);
    const waiting = task.status === 'blocked' || task.status === 'signaled';
    ctl.openPopup({
      entries: taskEntries(task, act, true),
      head: {
        glyph: task.glyph,
        color: task.glyph_color,
        title: task.title,
        detail: [task.ws, task.age && (waiting ? `waiting ${task.age}` : task.age), task.reason?.label].filter(Boolean).join(' · '),
      },
    });
  }

  // Second line, in the TUI's priority order: what it wants from you, the
  // workspace, a non-default agent, the age, PRs, ports. A permission it can
  // answer is a split chip: the answer sits on what it answers.
  const pieces: ReactNode[] = [];
  if (task.reason && task.answerable) {
    pieces.push(
      <span key="reason" className="split" data-testid="answer">
        <span className="s0" style={{ color: task.reason.fg, background: task.reason.bg }}>
          {task.reason.label}
        </span>
        <button
          type="button"
          className="s1"
          title="approve (A)"
          onClick={(e) => {
            e.stopPropagation();
            act('approve');
          }}
          onDoubleClick={(e) => e.stopPropagation()}
        >
          ✓ allow
        </button>
        <button
          type="button"
          className="s2"
          title="deny (D)"
          onClick={(e) => {
            e.stopPropagation();
            act('deny');
          }}
          onDoubleClick={(e) => e.stopPropagation()}
        >
          ✕ deny
        </button>
      </span>,
    );
  } else if (task.reason) {
    pieces.push(
      <span key="reason" className="chip" style={{ color: task.reason.fg, background: task.reason.bg }}>
        {task.reason.label}
      </span>,
    );
  }
  pieces.push(
    <span key="ws" style={{ color: task.ws_color }}>
      {task.ws}
    </span>,
  );
  if (task.agent) pieces.push(<span key="agent" style={{ color: 'var(--info)' }}>{task.agent}</span>);
  if (task.age) pieces.push(<span key="age">{task.age}</span>);
  task.prs.forEach((pr, i) =>
    pieces.push(
      <span key={`pr${i}`} style={{ color: pr.fg }}>
        {pr.label}
      </span>,
    ),
  );
  if (task.ports.length) pieces.push(<span key="ports">{task.ports.map((p) => `:${p}`).join(' ')}</span>);

  const sel = task.selected && ctl.focused;
  // The delete prompt sits on the row it deletes (the selection).
  const confirming = ctl.confirming && task.selected;
  const menu = (at: { left: number; top: number }) =>
    ctl.touch ? sheet() : ctl.openPopup({ at, entries: taskEntries(task, act, false) });
  const cls = ['row', 'task', (sel || confirming) && 'sel', confirming && 'conf'].filter(Boolean).join(' ');

  return (
    <Slide
      offset={gesture.offset}
      dragging={gesture.dragging}
      left={
        <>
          <button type="button" className="ua ok" onClick={() => (ctl.setReveal(task.id, null), act('approve'))}>
            ✓ allow<span className="k">A</span>
          </button>
          <button type="button" className="ua no" onClick={() => (ctl.setReveal(task.id, null), act('deny'))}>
            ✕ deny<span className="k">D</span>
          </button>
        </>
      }
      right={
        <button type="button" className="ua del" onClick={() => (ctl.setReveal(task.id, null), act('delete'))}>
          delete…<span className="k">dd</span>
        </button>
      }
    >
      <div
        className={cls}
        data-testid="task"
        data-id={task.id}
        onClick={() => ctl.onClick(select)}
        onDoubleClick={ctl.onOpen}
        onContextMenu={(e) => {
          if (ctl.touch || task.pending) return; // touch: the long-press sheet
          e.preventDefault();
          ctl.onClick(select);
          menu({ left: e.clientX, top: e.clientY });
        }}
        {...gesture.handlers}
      >
        <div className="line1">
          <span className={task.pending ? 'glyph spin' : 'glyph'} style={{ color: confirming ? 'var(--danger)' : task.glyph_color }}>
            {task.glyph}
          </span>
          <span className="title" style={{ color: sel || confirming ? 'var(--sel-text)' : task.title_color }}>
            {task.title}
          </span>
          {sel && !task.pending && !ctl.confirming && <More on={false} onOpen={menu} />}
        </div>
        <div className="line2">
          {pieces.map((p, i) => (
            <Fragment key={i}>
              {i > 0 && <span className="sep"> · </span>}
              {p}
            </Fragment>
          ))}
        </div>
        {confirming && (
          <>
            <div className="ctext">delete it and its worktrees? uncommitted work in them is lost.</div>
            <div className="inl" data-testid="confirm">
              <KeyPill label="delete" k="y" keyName="y" onKey={ctl.onKey} tone="no" big />
              <KeyPill label="keep" k="n / esc" keyName="n" onKey={ctl.onKey} tone="ghost" />
            </div>
          </>
        )}
      </div>
    </Slide>
  );
}

/** A button that presses a key, showing it: `delete y`, `save ⏎`. */
function KeyPill({
  label,
  k,
  keyName,
  onKey,
  tone,
  big,
}: {
  label: string;
  k: string;
  keyName: string;
  onKey(key: string): void;
  tone?: 'pri' | 'ok' | 'no' | 'ghost';
  big?: boolean;
}) {
  return (
    <button
      type="button"
      className={['pill', tone, big && 'big'].filter(Boolean).join(' ')}
      onClick={(e) => {
        e.stopPropagation();
        onKey(keyName);
      }}
      onDoubleClick={(e) => e.stopPropagation()}
    >
      {label} <span className="k">{k}</span>
    </button>
  );
}

function SubRow({ sub, ctl }: { sub: SubItem; ctl: RowCtl }) {
  const select: Click = { kind: 'task', id: sub.task, sub: sub.id };
  const act = (a: Action) => ctl.act(select, a);
  const sel = sub.selected && ctl.focused;
  const menu = (at: { left: number; top: number }) => ctl.openPopup({ at, entries: subEntries(sub, act) });
  const gesture = useRowGestures({
    left: false,
    right: false,
    reveal: null,
    setReveal: () => {},
    onFullSwipe: () => {},
    onLongPress: () => {
      ctl.onClick(select);
      ctl.openPopup({ entries: subEntries(sub, act), head: { glyph: sub.glyph, color: sub.glyph_color, title: sub.label, detail: sub.extras.join(' · ') } });
    },
  });
  return (
    <div
      className={sel ? 'row sub sel' : 'row sub'}
      onClick={(e) => {
        e.stopPropagation();
        ctl.onClick(select);
      }}
      onDoubleClick={(e) => {
        e.stopPropagation();
        ctl.onOpen();
      }}
      onContextMenu={(e) => {
        if (ctl.touch) return;
        e.preventDefault();
        ctl.onClick(select);
        menu({ left: e.clientX, top: e.clientY });
      }}
      {...gesture.handlers}
    >
      <span className="glyph" style={{ color: sub.glyph_color }}>
        {sub.glyph}
      </span>
      <span className="sub-label" style={{ color: sel ? 'var(--sel-text)' : sub.finished ? 'var(--muted)' : 'var(--text)' }}>
        {sub.label}
        {sub.extras.length > 0 && <span className="muted"> · {sub.extras.join(' · ')}</span>}
      </span>
      {sel && <More on={false} onOpen={menu} />}
    </div>
  );
}

const STEP_GLYPH = { pending: '·', running: '◐', done: '✔', failed: '✗' } as const;
const STEP_COLOR = { pending: 'var(--idle)', running: 'var(--info)', done: 'var(--success)', failed: 'var(--danger)' } as const;

function JobRow({ job, ctl }: { job: JobItem; ctl: RowCtl }) {
  const select: Click = { kind: 'item', pos: job.pos };
  const entries = jobEntries(job, (a) => ctl.act(select, a));
  const sel = job.selected && ctl.focused;
  return (
    <div
      className={sel ? 'row job sel' : 'row job'}
      onClick={() => ctl.onClick(select)}
      onContextMenu={(e) => {
        if (entries.length === 0) return;
        e.preventDefault();
        ctl.onClick(select);
        ctl.openPopup({ at: { left: e.clientX, top: e.clientY }, entries });
      }}
    >
      <div className="line1">
        <span className="glyph" style={{ color: STEP_COLOR[job.state] }}>
          {STEP_GLYPH[job.state]}
        </span>
        <span className="title">{job.title}</span>
        {job.counter && <span className="muted"> {job.counter}</span>}
        {sel && entries.length > 0 && <More on={false} onOpen={(at) => ctl.openPopup({ at, entries })} />}
      </div>
      {job.state === 'running' && (
        <div className="progress">
          <div
            className={job.fraction == null ? 'bar marquee' : 'bar'}
            style={job.fraction == null ? undefined : { width: `${Math.round(job.fraction * 100)}%` }}
          />
        </div>
      )}
      {job.steps.map((s, i) => (
        <div key={i} className="step">
          <span style={{ color: STEP_COLOR[s.state] }}>{STEP_GLYPH[s.state]} </span>
          <span className={s.state === 'pending' ? 'muted' : undefined}>{s.label}</span>
          {s.note && <span className="muted"> {s.note}</span>}
        </div>
      ))}
      {job.transfer && <div className="step muted">{job.transfer}</div>}
      {job.outcome && (
        <div className="step" style={{ color: job.state === 'failed' ? 'var(--danger)' : 'var(--success)' }}>
          {job.outcome}
        </div>
      )}
    </div>
  );
}

// ── Forms ─────────────────────────────────────────────────────────────────
//
// Each field is clickable (`field` click: it takes the focus), a checkbox
// toggles on click (focus, then space), ‹ › step a picker (focus, then ← / →),
// and a text field raises the on-screen keyboard.

interface FieldProps {
  ctl: Ctl;
  index: number;
  focused: boolean;
  label: string;
  /** Takes typing: a tap also raises the keyboard. */
  text?: boolean;
  children: ReactNode;
}

function Field({ ctl, index, focused, label, text, children }: FieldProps) {
  return (
    <div
      className="ln field"
      onClick={() => {
        ctl.onClick({ kind: 'field', index });
        if (text) ctl.onWantKeyboard();
      }}
    >
      <span className={focused ? 'flabel on' : 'flabel'}>
        {focused ? '▸ ' : '  '}
        {label.padEnd(11)}
      </span>
      <span className={focused ? 'fvalue on' : 'fvalue'}>{children}</span>
    </div>
  );
}

/** ‹ value › with the arrows as buttons. */
function Picker({ ctl, index, children }: { ctl: Ctl; index: number; children: ReactNode }) {
  const step = (key: string) => (e: React.MouseEvent) => {
    e.stopPropagation();
    ctl.onClick({ kind: 'field', index });
    ctl.onKey(key);
  };
  return (
    <>
      <button type="button" className="step-btn" onClick={step('ArrowLeft')} aria-label="previous">
        ‹
      </button>{' '}
      {children}{' '}
      <button type="button" className="step-btn" onClick={step('ArrowRight')} aria-label="next">
        ›
      </button>
    </>
  );
}

interface CheckboxProps {
  ctl: Ctl;
  index: number;
  focused: boolean;
  checked: boolean;
  label: string;
  note?: string;
}

function Checkbox({ ctl, index, focused, checked, label, note }: CheckboxProps) {
  const cls = focused ? 'ln check on' : checked ? 'ln check checked' : 'ln check';
  return (
    <div
      className={cls}
      onClick={() => {
        ctl.onClick({ kind: 'field', index });
        ctl.onKey(' ');
      }}
    >
      {focused ? '▸ ' : '  '}
      {checked ? '[x]' : '[ ]'} {label}
      {note && <span className="muted">  {note}</span>}
    </div>
  );
}

/** A form's frame, title on the border, its buttons inside at the
 * bottom-right (primary last). */
function FormBox({ title, buttons, children }: { title: string; buttons?: ReactNode; children: ReactNode }) {
  return (
    <div className="form">
      <span className="form-title">{title}</span>
      {children}
      {buttons && (
        <div className="fbtns" data-testid="form-buttons">
          {buttons}
        </div>
      )}
    </div>
  );
}

/** cancel esc · <primary> ⏎ */
function submitCancel(ctl: Ctl, primary: string): ReactNode {
  return (
    <>
      <KeyPill label="cancel" k="esc" keyName="Escape" onKey={ctl.onKey} tone="ghost" />
      <KeyPill label={primary} k="⏎" keyName="Enter" onKey={ctl.onKey} tone="pri" big />
    </>
  );
}

function formFor(mode: ModeView, ctl: Ctl): ReactNode | null {
  switch (mode.kind) {
    case 'create': {
      const agentIndex = 2 + mode.repos.length;
      return (
        <FormBox title="new task" buttons={submitCancel(ctl, 'create')}>
          <Field ctl={ctl} index={0} focused={mode.focus === 'workspace'} label="workspace">
            {mode.workspaces > 1 ? (
              <>
                <Picker ctl={ctl} index={0}>
                  {mode.workspace}
                </Picker>
                <span className="muted"> ({mode.workspace_index} of {mode.workspaces})</span>
              </>
            ) : (
              mode.workspace
            )}
          </Field>
          <div className="ln" />
          <Field ctl={ctl} index={1} focused={mode.focus === 'name'} label="name" text>
            {mode.name}
            {mode.focus === 'name' && <span className="caret thin" />}
          </Field>
          <div className="ln" />
          <div className="ln muted">{'  repos'}</div>
          {mode.repos.map((r, i) => (
            <Checkbox
              key={r.name}
              ctl={ctl}
              index={2 + i}
              focused={mode.focus === 'repo' && mode.focus_repo === i}
              checked={r.checked}
              label={r.name}
            />
          ))}
          <div className="ln" />
          <Field ctl={ctl} index={agentIndex} focused={mode.focus === 'agent'} label="agent">
            <Picker ctl={ctl} index={agentIndex}>
              {mode.agent}
              {mode.agent_inherits ? '  (inherits default)' : ''}
            </Picker>
          </Field>
        </FormBox>
      );
    }
    case 'add_repo':
      return (
        <FormBox title={`add repo to ${mode.workspace}`} buttons={submitCancel(ctl, 'add')}>
          <div className="ln" />
          <Field ctl={ctl} index={0} focused={mode.focus === 'url'} label="url" text>
            {mode.url}
            {mode.focus === 'url' && <span className="caret thin" />}
          </Field>
          <div className="ln" />
          <Field ctl={ctl} index={1} focused={mode.focus === 'name'} label="name" text>
            {mode.name}
            {mode.focus === 'name' && <span className="caret thin" />}
          </Field>
        </FormBox>
      );
    case 'new_workspace':
      return (
        <FormBox title="new workspace" buttons={submitCancel(ctl, 'create')}>
          <div className="ln" />
          <Field ctl={ctl} index={0} focused={mode.focus === 'path'} label="path" text>
            {mode.path}
            {mode.focus === 'path' && <span className="caret thin" />}
          </Field>
          <div className="ln" />
          <Field ctl={ctl} index={1} focused={mode.focus === 'name'} label="name" text>
            {mode.name}
            {mode.focus === 'name' && <span className="caret thin" />}
          </Field>
          <div className="ln" />
          <Field ctl={ctl} index={2} focused={mode.focus === 'repo_url'} label="repo url" text>
            {mode.repo_url}
            {mode.focus === 'repo_url' && <span className="caret thin" />}
          </Field>
          <div className="ln" />
          <Checkbox ctl={ctl} index={3} focused={mode.focus === 'skills'} checked={mode.skills} label="skills" />
        </FormBox>
      );
    case 'edit_repos':
      return (
        <FormBox
          title={`repos of ${mode.task}`}
          buttons={
            mode.confirm ? (
              <>
                <KeyPill label="back" k="esc" keyName="Escape" onKey={ctl.onKey} tone="ghost" />
                <KeyPill label="detach" k="y" keyName="y" onKey={ctl.onKey} tone="no" big />
              </>
            ) : (
              <>
                <KeyPill label="all" k="a" keyName="a" onKey={ctl.onKey} tone="ghost" />
                <KeyPill label="none" k="n" keyName="n" onKey={ctl.onKey} tone="ghost" />
                <span className="grow" />
                {submitCancel(ctl, 'apply')}
              </>
            )
          }
        >
          <div className="ln" />
          {mode.picks.map((p, i) => (
            <Checkbox
              key={p.name}
              ctl={ctl}
              index={i}
              focused={mode.focus === i}
              checked={p.checked}
              label={p.name}
              note={p.checked === p.present ? undefined : p.checked ? 'add' : 'detach'}
            />
          ))}
        </FormBox>
      );
    default:
      return null;
  }
}

// ── Footer and help ───────────────────────────────────────────────────────

function FooterLine({ footer, focused, onKey }: { footer: Footer; focused: boolean; onKey(key: string): void }) {
  switch (footer.kind) {
    case 'command':
      return (
        <div className="footer" data-testid="footer">
          <span className="accent bold">:</span>
          <span>{footer.text}</span>
          <span className="caret thin" />
          {footer.hint && <span className="muted">{'  ' + footer.hint}</span>}
          {footer.warn && <span className="warn">{'  ' + footer.warn}</span>}
          <span className="grow" />
          <KeyPill label="cancel" k="esc" keyName="Escape" onKey={onKey} tone="ghost" />
          <KeyPill label="run" k="⏎" keyName="Enter" onKey={onKey} tone="pri" />
        </div>
      );
    case 'confirm':
      // The question and its buttons are on the row being deleted; the
      // footer only echoes the keys.
      return (
        <div className="footer" data-testid="footer">
          <span className="muted">y delete · n keep</span>
        </div>
      );
    case 'message':
      return (
        <div className="footer success" data-testid="footer">
          {footer.text}
        </div>
      );
    case 'error':
      return (
        <div className="footer wrap danger" data-testid="footer">
          {footer.text}
        </div>
      );
    case 'hint':
      return (
        <div className="footer" data-testid="footer">
          {footer.tag && focused && <span className={footer.tag === 'INSERT' ? 'tag insert' : 'tag'}>{footer.tag}</span>}
          <span className="muted">{(footer.tag && focused ? ' ' : '') + footer.text}</span>
        </div>
      );
  }
}

function Help({ view, scroll, onKey }: { view: ColumnView; scroll: number; onKey(key: string): void }) {
  // The server keeps the scroll (j/k); a wheel or a swipe presses them. A tap
  // closes, as any other key does.
  const touchY = useRef<number | null>(null);
  const moved = useRef(false);
  const lines = (dy: number) => {
    const n = Math.trunc(dy / 18);
    for (let i = 0; i < Math.abs(n); i++) onKey(n > 0 ? 'j' : 'k');
    return n;
  };
  return (
    <div
      className="help"
      data-testid="help"
      onClick={() => {
        if (!moved.current) onKey('Escape');
        moved.current = false;
      }}
      onWheel={(e) => lines(e.deltaY)}
      onTouchStart={(e) => {
        touchY.current = e.touches[0].clientY;
        moved.current = false;
      }}
      onTouchMove={(e) => {
        if (touchY.current == null) return;
        const dy = touchY.current - e.touches[0].clientY;
        if (lines(dy) !== 0) {
          touchY.current = e.touches[0].clientY;
          moved.current = true;
        }
      }}
    >
      <div className="help-head">
        <span className="bright bold">keys</span>
        <button type="button" className="icon-btn" aria-label="close">
          ✕
        </button>
      </div>
      <div className="help-body">
        <div className="keys" style={{ transform: `translateY(calc(${-scroll} * var(--line)))` }}>
          {view.help.map((s) => (
            <Fragment key={s.section}>
              <div className="ksec">{s.section}</div>
              {s.keys.map(([k, a]) => (
                <Fragment key={k}>
                  <span className="kk">{k}</span>
                  <span>{a}</span>
                </Fragment>
              ))}
            </Fragment>
          ))}
        </div>
      </div>
      <div className="muted">{view.footer.text}</div>
    </div>
  );
}
