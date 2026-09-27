'use client';

// The column, drawn from the ColumnView the server sends: nothing here
// decides what to show — which rows, which chips, which hint — only how it
// looks. Clicks go back by id; keys go through the page (App).
//
// Everything a key does can also be done with a pointer: a click selects a
// row, a double click (or double tap) opens it, the action bar under the list
// presses the list's keys for the selection, and forms and prompts get
// buttons. Those send the same keys and clicks the keyboard would.

import { Fragment, useEffect, useRef, type ReactNode } from 'react';
import type { Action, Click, ColumnView, Footer, Item, JobItem, ModeView, SubItem, TaskItem } from '@/protocol';
import type { Status } from '@/lib/connection';
import type { PushState } from '@/lib/push';

interface Props {
  view: ColumnView | null;
  /** The column has the keyboard: show the selection and the caret. */
  focused: boolean;
  status: Status;
  failures: number;
  host: string;
  onClick(click: Click): void;
  onFocus(): void;
  /** An action-bar button (view.rs `Action`). */
  onAction(action: Action): void;
  /** A key, as if typed with the column focused (form and prompt buttons). */
  onKey(key: string, shift?: boolean): void;
  /** Something that takes typing was tapped: raise the on-screen keyboard. */
  onWantKeyboard(): void;
  onHide(): void;
  push: PushState;
  onPushToggle(): void;
  onPushTest(): void;
}

/** Keys and clicks, for the pieces below. */
interface Ctl {
  onClick(click: Click): void;
  onKey(key: string, shift?: boolean): void;
  onWantKeyboard(): void;
}

export function Column(props: Props) {
  const { view, focused, status, failures, host, onClick, onFocus, onAction, onKey, onWantKeyboard, onHide } = props;
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

  return (
    <aside className="column" data-testid="column" onMouseDown={onFocus}>
      <Brand status={status} failures={failures} host={host} onHide={onHide} push={props.push} onPushToggle={props.onPushToggle} onPushTest={props.onPushTest} />
      {props.push === 'needs-install' && (
        <div className="hint" data-testid="push-hint">
          Add to Home Screen, then enable notifications
        </div>
      )}
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
                <ListItem
                  key={itemKey(item, i)}
                  item={item}
                  first={i === 0}
                  focused={focused}
                  onClick={onClick}
                  onOpen={() => onAction('open')}
                />
              ))}
            </div>
          )}
          <Buttons view={view} onAction={onAction} onKey={onKey} />
          <FooterLine footer={view.footer} focused={focused} />
          {mode.kind === 'help' && <Help view={view} scroll={mode.scroll} onKey={onKey} />}
        </>
      )}
      {!view && <div className="list" />}
    </aside>
  );
}

interface BrandProps {
  status: Status;
  failures: number;
  host: string;
  onHide(): void;
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

function Brand({ status, failures, host, onHide, push, onPushToggle, onPushTest }: BrandProps) {
  const label =
    status === 'open'
      ? host || (typeof location !== 'undefined' ? location.host : '')
      : status === 'connecting'
        ? 'connecting…'
        : failures > 5
          ? 'offline — is tenx web running?'
          : 'reconnecting…';
  return (
    <div className="brand">
      <span className="wordmark">
        ten<span className="accent">x</span>
      </span>
      <span className="brand-right">
        <span className={`conn ${status}`} data-testid="conn">
          <span className="dot" />
          {label}
        </span>
        {push === 'on' && (
          <button type="button" className="icon-btn small" data-testid="push-test" title="send a test notification" onClick={onPushTest}>
            test
          </button>
        )}
        <button
          type="button"
          className={`icon-btn bell ${push}`}
          data-testid="push"
          data-state={push}
          title={PUSH_TITLE[push]}
          aria-label={PUSH_TITLE[push]}
          disabled={push === 'unsupported' || push === 'denied' || push === 'needs-install'}
          onClick={(e) => {
            e.stopPropagation();
            onPushToggle();
          }}
        >
          {push === 'on' ? '🔔' : '🔕'}
        </button>
        <button
          type="button"
          className="icon-btn"
          data-testid="hide"
          title="hide the column (^w)"
          aria-label="hide the column"
          onClick={(e) => {
            e.stopPropagation();
            onHide();
          }}
        >
          ⟨
        </button>
      </span>
    </div>
  );
}

function SearchBox({ view, focused, ctl }: { view: ColumnView; focused: boolean; ctl: Ctl }) {
  if (view.mode.kind === 'rename') {
    return (
      <div className="search rename" onClick={ctl.onWantKeyboard}>
        <span className="search-title">rename task</span>
        <span className="accent">✎ </span>
        <span>{view.mode.buffer}</span>
        <span className="caret" />
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

interface ItemProps {
  item: Item;
  first: boolean;
  focused: boolean;
  onClick(c: Click): void;
  /** A double click or double tap: ⏎ on what the first click selected. */
  onOpen(): void;
}

function ListItem({ item, first, focused, onClick, onOpen }: ItemProps) {
  switch (item.kind) {
    case 'header':
      return (
        <div className={first ? 'group' : 'group gap'} style={{ color: item.color }}>
          <span>{item.label}</span>
          {item.count != null && <span className="count">{item.count}</span>}
        </div>
      );
    case 'task':
      return <TaskRow task={item} focused={focused} onClick={onClick} onOpen={onOpen} />;
    case 'sub':
      return <SubRow sub={item} focused={focused} onClick={onClick} onOpen={onOpen} />;
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
      return <JobRow job={item} focused={focused} onClick={onClick} />;
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

function TaskRow({ task, focused, onClick, onOpen }: { task: TaskItem; focused: boolean; onClick(c: Click): void; onOpen(): void }) {
  // Second line, in the TUI's priority order: what it wants from you, the
  // workspace, a non-default agent, the age, PRs, ports.
  const pieces: ReactNode[] = [];
  if (task.reason) {
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

  const sel = task.selected && focused;
  return (
    <div
      className={sel ? 'row task sel' : 'row task'}
      data-testid="task"
      data-id={task.id}
      onClick={() => onClick({ kind: 'task', id: task.id })}
      onDoubleClick={onOpen}
    >
      <div className="line1">
        <span className={task.pending ? 'glyph spin' : 'glyph'} style={{ color: task.glyph_color }}>
          {task.glyph}
        </span>
        <span className="title" style={{ color: sel ? 'var(--sel-text)' : task.title_color }}>
          {task.title}
        </span>
      </div>
      <div className="line2">
        {pieces.map((p, i) => (
          <Fragment key={i}>
            {i > 0 && <span className="sep"> · </span>}
            {p}
          </Fragment>
        ))}
      </div>
    </div>
  );
}

function SubRow({ sub, focused, onClick, onOpen }: { sub: SubItem; focused: boolean; onClick(c: Click): void; onOpen(): void }) {
  const sel = sub.selected && focused;
  return (
    <div
      className={sel ? 'row sub sel' : 'row sub'}
      onClick={(e) => {
        e.stopPropagation();
        onClick({ kind: 'task', id: sub.task, sub: sub.id });
      }}
      onDoubleClick={(e) => {
        e.stopPropagation();
        onOpen();
      }}
    >
      <span className="glyph" style={{ color: sub.glyph_color }}>
        {sub.glyph}
      </span>
      <span style={{ color: sel ? 'var(--sel-text)' : sub.finished ? 'var(--muted)' : 'var(--text)' }}>{sub.label}</span>
      {sub.extras.length > 0 && <span className="muted"> · {sub.extras.join(' · ')}</span>}
    </div>
  );
}

const STEP_GLYPH = { pending: '·', running: '◐', done: '✔', failed: '✗' } as const;
const STEP_COLOR = { pending: 'var(--idle)', running: 'var(--info)', done: 'var(--success)', failed: 'var(--danger)' } as const;

function JobRow({ job, focused, onClick }: { job: JobItem; focused: boolean; onClick(c: Click): void }) {
  return (
    <div className={job.selected && focused ? 'row job sel' : 'row job'} onClick={() => onClick({ kind: 'item', pos: job.pos })}>
      <div className="line1">
        <span className="glyph" style={{ color: STEP_COLOR[job.state] }}>
          {STEP_GLYPH[job.state]}
        </span>
        <span className="title">{job.title}</span>
        {job.counter && <span className="muted"> {job.counter}</span>}
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

function FormBox({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="form">
      <span className="form-title">{title}</span>
      {children}
    </div>
  );
}

function formFor(mode: ModeView, ctl: Ctl): ReactNode | null {
  switch (mode.kind) {
    case 'create': {
      const agentIndex = 2 + mode.repos.length;
      return (
        <FormBox title="new task">
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
        <FormBox title={`add repo to ${mode.workspace}`}>
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
        <FormBox title="new workspace">
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
        <FormBox title={`repos of ${mode.task}`}>
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

// ── Buttons ───────────────────────────────────────────────────────────────
//
// Under the list: in list mode, what the list keys would do to the selection;
// in a form or prompt, its submit / cancel. Only the buttons that apply now.

interface Btn {
  label: string;
  title: string;
  run(): void;
  cls?: string;
}

function Buttons({ view, onAction, onKey }: { view: ColumnView; onAction(a: Action): void; onKey(key: string, shift?: boolean): void }) {
  const act = (label: string, title: string, a: Action, cls?: string): Btn => ({ label, title, run: () => onAction(a), cls });
  const key = (label: string, title: string, k: string, cls?: string): Btn => ({ label, title, run: () => onKey(k), cls });
  const mode = view.mode;
  let btns: Btn[] = [];

  switch (mode.kind) {
    case 'list': {
      const tab = view.tabs.find((t) => t.active)?.label ?? 'Tasks';
      if (tab === 'Tasks') {
        const task = view.items.find((i): i is Extract<Item, { kind: 'task' }> => i.kind === 'task' && i.selected);
        const sub = view.items.find((i): i is Extract<Item, { kind: 'sub' }> => i.kind === 'sub' && i.selected);
        const waiting = view.items.some((i) => i.kind === 'task' && !i.selected && (i.status === 'blocked' || i.status === 'signaled'));
        btns.push(act('+ new', 'new task (^n)', 'new'));
        if (sub) {
          btns.push(act('open agent', 'open the agent (⏎)', 'open', 'primary'), act('transcript', 'its transcript (t)', 'transcript'));
        } else if (task && !task.pending) {
          if (task.answerable) btns.push(act('approve', 'approve the permission (A)', 'approve', 'ok'), act('deny', 'deny it (D)', 'deny', 'danger'));
          if (task.locked) btns.push(act('unlock', 'unlock its secrets (u)', 'unlock', 'accent'));
          btns.push(act(task.closed ? 'open' : 'go to', 'open the task (⏎)', 'open', 'primary'));
          btns.push(act('rename', 'rename (r)', 'rename'), act('repos', 'edit repos (e)', 'edit_repos'));
          if (!task.closed) btns.push(act('close', 'close its window (x)', 'close'));
          btns.push(act('delete', 'delete task + worktrees (dd)', 'delete', 'danger'));
        }
        if (waiting) btns.push(act('next ●', 'next task that needs you (n)', 'next', 'warn'));
      } else if (tab === 'Repos') {
        btns.push(act('+ repo', 'add a repo (a)', 'add_repo'), act('+ workspace', 'new workspace (W)', 'new_workspace'));
      } else {
        const job = view.items.find((i): i is Extract<Item, { kind: 'job' }> => i.kind === 'job' && i.selected);
        if (job && job.state !== 'running') btns.push({ label: 'dismiss', title: 'dismiss (dd)', run: () => onAction('delete') });
      }
      btns.push(act('?', 'keys (?)', 'help'));
      break;
    }
    case 'command':
      btns = [key('run', 'run (⏎)', 'Enter', 'primary'), key('cancel', 'cancel (esc)', 'Escape')];
      break;
    case 'create':
      btns = [key('create', 'create (⏎)', 'Enter', 'primary'), key('cancel', 'cancel (esc)', 'Escape')];
      break;
    case 'add_repo':
      btns = [key('add', 'add (⏎)', 'Enter', 'primary'), key('cancel', 'cancel (esc)', 'Escape')];
      break;
    case 'new_workspace':
      btns = [key('create', 'create (⏎)', 'Enter', 'primary'), key('cancel', 'cancel (esc)', 'Escape')];
      break;
    case 'edit_repos':
      btns = mode.confirm
        ? [key('detach', 'confirm (y)', 'y', 'danger'), key('back', 'back to the list', 'Escape')]
        : [key('apply', 'apply (⏎)', 'Enter', 'primary'), key('all', 'check all (a)', 'a'), key('none', 'clear all (n)', 'n'), key('cancel', 'cancel (esc)', 'Escape')];
      break;
    case 'confirm':
      btns = [key('delete', 'delete (y)', 'y', 'danger'), key('cancel', 'cancel (n)', 'n')];
      break;
    case 'rename':
      btns = [key('save', 'save (⏎)', 'Enter', 'primary'), key('cancel', 'cancel (esc)', 'Escape')];
      break;
    case 'help':
      return null;
  }
  return (
    <div className="actions" data-testid="actions">
      {btns.map((b) => (
        <button
          key={b.label}
          type="button"
          className={b.cls ? `act ${b.cls}` : 'act'}
          title={b.title}
          onClick={(e) => {
            e.stopPropagation();
            b.run();
          }}
        >
          {b.label}
        </button>
      ))}
    </div>
  );
}

// ── Footer and help ───────────────────────────────────────────────────────

function FooterLine({ footer, focused }: { footer: Footer; focused: boolean }) {
  switch (footer.kind) {
    case 'command':
      return (
        <div className="footer" data-testid="footer">
          <span className="accent bold">:</span>
          <span>{footer.text}</span>
          <span className="caret thin" />
          {footer.hint && <span className="muted">{'  ' + footer.hint}</span>}
          {footer.warn && <span className="warn">{'  ' + footer.warn}</span>}
        </div>
      );
    case 'confirm':
      return (
        <div className="footer wrap danger bold" data-testid="footer">
          {footer.text}
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
