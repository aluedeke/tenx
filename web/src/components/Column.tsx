'use client';

// The column, drawn from the ColumnView the server sends: nothing here
// decides what to show — which rows, which chips, which hint — only how it
// looks. Clicks go back by id; keys go through the page (App).

import { Fragment, useEffect, useRef, type ReactNode } from 'react';
import type { Click, ColumnView, Footer, Item, JobItem, ModeView, SubItem, TaskItem } from '@/protocol';
import type { Status } from '@/lib/connection';

interface Props {
  view: ColumnView | null;
  /** The column has the keyboard: show the selection and the caret. */
  focused: boolean;
  status: Status;
  failures: number;
  host: string;
  onClick(click: Click): void;
  onFocus(): void;
}

export function Column({ view, focused, status, failures, host, onClick, onFocus }: Props) {
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
  const form = formFor(mode);

  return (
    <aside className="column" data-testid="column" onMouseDown={onFocus}>
      <Brand status={status} failures={failures} host={host} />
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
          <SearchBox view={view} focused={focused} onClick={onClick} />
          {form ?? (
            <div className="list" ref={listRef} data-testid="list">
              {view.items.map((item, i) => (
                <ListItem key={itemKey(item, i)} item={item} first={i === 0} focused={focused} onClick={onClick} />
              ))}
            </div>
          )}
          <FooterLine footer={view.footer} focused={focused} />
          {mode.kind === 'help' && <Help view={view} scroll={mode.scroll} />}
        </>
      )}
      {!view && <div className="list" />}
    </aside>
  );
}

function Brand({ status, failures, host }: { status: Status; failures: number; host: string }) {
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
      <span className={`conn ${status}`} data-testid="conn">
        <span className="dot" />
        {label}
      </span>
    </div>
  );
}

function SearchBox({ view, focused, onClick }: { view: ColumnView; focused: boolean; onClick(c: Click): void }) {
  if (view.mode.kind === 'rename') {
    return (
      <div className="search rename">
        <span className="search-title">rename task</span>
        <span className="accent">✎ </span>
        <span>{view.mode.buffer}</span>
        <span className="caret" />
      </div>
    );
  }
  const typing = focused && view.focus === 'search' && view.mode.kind === 'list';
  return (
    <div className={typing ? 'search typing' : 'search'} onClick={() => onClick({ kind: 'search' })}>
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

function ListItem({ item, first, focused, onClick }: { item: Item; first: boolean; focused: boolean; onClick(c: Click): void }) {
  switch (item.kind) {
    case 'header':
      return (
        <div className={first ? 'group' : 'group gap'} style={{ color: item.color }}>
          <span>{item.label}</span>
          {item.count != null && <span className="count">{item.count}</span>}
        </div>
      );
    case 'task':
      return <TaskRow task={item} focused={focused} onClick={onClick} />;
    case 'sub':
      return <SubRow sub={item} focused={focused} onClick={onClick} />;
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

function TaskRow({ task, focused, onClick }: { task: TaskItem; focused: boolean; onClick(c: Click): void }) {
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

function SubRow({ sub, focused, onClick }: { sub: SubItem; focused: boolean; onClick(c: Click): void }) {
  const sel = sub.selected && focused;
  return (
    <div
      className={sel ? 'row sub sel' : 'row sub'}
      onClick={(e) => {
        e.stopPropagation();
        onClick({ kind: 'task', id: sub.task, sub: sub.id });
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

function Field({ focused, label, children }: { focused: boolean; label: string; children: ReactNode }) {
  return (
    <div className="ln field">
      <span className={focused ? 'flabel on' : 'flabel'}>
        {focused ? '▸ ' : '  '}
        {label.padEnd(11)}
      </span>
      <span className={focused ? 'fvalue on' : 'fvalue'}>{children}</span>
    </div>
  );
}

function Checkbox({ focused, checked, label, note }: { focused: boolean; checked: boolean; label: string; note?: string }) {
  const cls = focused ? 'ln check on' : checked ? 'ln check checked' : 'ln check';
  return (
    <div className={cls}>
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

function formFor(mode: ModeView): ReactNode | null {
  switch (mode.kind) {
    case 'create':
      return (
        <FormBox title="new task">
          <Field focused={mode.focus === 'workspace'} label="workspace">
            {mode.workspaces > 1 ? (
              <>
                ‹ {mode.workspace} › <span className="muted"> ({mode.workspace_index} of {mode.workspaces})</span>
              </>
            ) : (
              mode.workspace
            )}
          </Field>
          <div className="ln" />
          <Field focused={mode.focus === 'name'} label="name">
            {mode.name}
            {mode.focus === 'name' && <span className="caret thin" />}
          </Field>
          <div className="ln" />
          <div className="ln muted">{'  repos'}</div>
          {mode.repos.map((r, i) => (
            <Checkbox key={r.name} focused={mode.focus === 'repo' && mode.focus_repo === i} checked={r.checked} label={r.name} />
          ))}
          <div className="ln" />
          <Field focused={mode.focus === 'agent'} label="agent">
            ‹ {mode.agent}
            {mode.agent_inherits ? '  (inherits default)' : ''} ›
          </Field>
        </FormBox>
      );
    case 'add_repo':
      return (
        <FormBox title={`add repo to ${mode.workspace}`}>
          <div className="ln" />
          <Field focused={mode.focus === 'url'} label="url">
            {mode.url}
            {mode.focus === 'url' && <span className="caret thin" />}
          </Field>
          <div className="ln" />
          <Field focused={mode.focus === 'name'} label="name">
            {mode.name}
            {mode.focus === 'name' && <span className="caret thin" />}
          </Field>
        </FormBox>
      );
    case 'new_workspace':
      return (
        <FormBox title="new workspace">
          <div className="ln" />
          <Field focused={mode.focus === 'path'} label="path">
            {mode.path}
            {mode.focus === 'path' && <span className="caret thin" />}
          </Field>
          <div className="ln" />
          <Field focused={mode.focus === 'name'} label="name">
            {mode.name}
            {mode.focus === 'name' && <span className="caret thin" />}
          </Field>
          <div className="ln" />
          <Field focused={mode.focus === 'repo_url'} label="repo url">
            {mode.repo_url}
            {mode.focus === 'repo_url' && <span className="caret thin" />}
          </Field>
          <div className="ln" />
          <Field focused={mode.focus === 'skills'} label="skills">
            {mode.skills ? '[x]' : '[ ]'}
          </Field>
        </FormBox>
      );
    case 'edit_repos':
      return (
        <FormBox title={`repos of ${mode.task}`}>
          <div className="ln" />
          {mode.picks.map((p, i) => (
            <Checkbox
              key={p.name}
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

function Help({ view, scroll }: { view: ColumnView; scroll: number }) {
  return (
    <div className="help" data-testid="help">
      <div className="bright bold">keys</div>
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
