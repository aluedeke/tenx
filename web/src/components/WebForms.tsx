'use client';

// The column's forms as real web forms (design turn 3: 3c's sections, chips
// and sticky action bar, 3a's boxed inputs and per-field errors, 3e's other
// forms, 3f on the phone, 3g's progress). The server's form state (view.rs
// `ModeView`) stays the one truth: every edit goes over as a `form` message
// (`FormOp`) and comes back in the next view; submit and cancel run the
// form's own ⏎ / esc there, so a web form creates, adds and renames exactly
// as the terminal's does.
//
// Text inputs are uncontrolled while they have the focus — the page sends the
// whole value on every input and never writes the server's echo back into a
// field being typed in, so the caret doesn't jump. Chips (radios and
// checkboxes under the hood, so Tab, arrows and Space work) follow the
// server, showing a change at once until its echo.

import { useEffect, useRef, useState, type ReactNode } from 'react';
import type { FormOp, JobProgress, ModeView } from '@/protocol';

type Send = (op: FormOp) => void;
type FormMode = Extract<ModeView, { kind: 'create' | 'add_repo' | 'new_workspace' | 'edit_repos' }>;

/** A submitted form, frozen while the job it started runs (3g). */
export interface FormProgress {
  job: JobProgress;
  /** esc / ✕: stop watching; the job keeps going on the Work tab. */
  onLeave(): void;
}

interface Props {
  mode: FormMode;
  /** `view.status`: shown in the form as its error once it changes. */
  status: string | null;
  send: Send;
  /** A key as the column would take it (the edit-repos confirm step). */
  onKey(key: string): void;
  /** A touch screen: no autofocus (it can't raise the keyboard outside a tap). */
  touch: boolean;
  progress?: FormProgress;
}

/** A value from the server, shown changed at once when the user changes it. */
function useEcho<T>(server: T): [T, (v: T) => void] {
  const [local, setLocal] = useState(server);
  useEffect(() => setLocal(server), [server]);
  return [local, setLocal];
}

/** The status as an error for this form: only a message that arrived while
 * it was open, not whatever the column said before. */
function useFormError(status: string | null): string | null {
  const opened = useRef(status);
  return status && status !== opened.current ? status : null;
}

/** Which field an error is about, so it shows under that field (3a); `null`
 * shows it above the action bar. */
function errorField(kind: FormMode['kind'] | 'rename', error: string): string | null {
  const e = error.toLowerCase();
  switch (kind) {
    case 'create':
      return /name|title|exists|slug/.test(e) ? 'name' : null;
    case 'add_repo':
      return /url|clone/.test(e) ? 'url' : /name/.test(e) ? 'name' : null;
    case 'new_workspace':
      return /path|folder|director/.test(e) ? 'path' : /repo|url|clone/.test(e) ? 'repo_url' : /name/.test(e) ? 'name' : null;
    case 'rename':
      return 'title';
    default:
      return null;
  }
}

function Section({ caption, aside, children }: { caption: string; aside?: ReactNode; children: ReactNode }) {
  return (
    <section className="wf-sec">
      <div className="wf-cap">
        <span>{caption}</span>
        {aside && <span className="wf-aside">{aside}</span>}
      </div>
      {children}
    </section>
  );
}

interface TextProps {
  caption: string;
  field: string;
  value: string;
  send: Send;
  autoFocus?: boolean;
  placeholder?: string;
  aside?: ReactNode;
  hint?: ReactNode;
  error?: string | null;
}

function TextField({ caption, field, value, send, autoFocus, placeholder, aside, hint, error }: TextProps) {
  const ref = useRef<HTMLInputElement>(null);
  // The server's value, but never into a field being typed in.
  useEffect(() => {
    const el = ref.current;
    if (el && document.activeElement !== el && el.value !== value) el.value = value;
  }, [value]);
  useEffect(() => {
    if (autoFocus) ref.current?.focus({ preventScroll: true });
  }, [autoFocus]);
  return (
    <Section caption={caption} aside={aside}>
      <input
        ref={ref}
        className={error ? 'wf-in err' : 'wf-in'}
        data-field={field}
        aria-label={caption.toLowerCase()}
        aria-invalid={!!error}
        defaultValue={value}
        placeholder={placeholder}
        autoComplete="off"
        autoCapitalize="off"
        autoCorrect="off"
        spellCheck={false}
        onInput={(e) => send({ op: 'set', field, value: e.currentTarget.value })}
      />
      {error ? (
        <div className="wf-err" role="alert" data-testid="form-error">
          {error}
        </div>
      ) : (
        hint && <div className="wf-hint">{hint}</div>
      )}
    </Section>
  );
}

/** A chip that is a radio underneath: arrows move within its group. */
function ChipRadio({ name, checked, onPick, children }: { name: string; checked: boolean; onPick(): void; children: ReactNode }) {
  return (
    <label className={checked ? 'wf-chip on' : 'wf-chip'}>
      <input type="radio" className="wf-hidden" name={name} checked={checked} onChange={onPick} />
      {children}
    </label>
  );
}

/** A chip that is a checkbox underneath: Space toggles it. */
function ChipCheck({ label, checked, onChange }: { label: string; checked: boolean; onChange(on: boolean): void }) {
  const [on, setOn] = useEcho(checked);
  return (
    <label className={on ? 'wf-chip on' : 'wf-chip'}>
      <input
        type="checkbox"
        className="wf-hidden"
        checked={on}
        onChange={(e) => {
          setOn(e.currentTarget.checked);
          onChange(e.currentTarget.checked);
        }}
      />
      {on && <span className="wf-cm">✓</span>}
      {label}
    </label>
  );
}

/** "N of M · all · none" beside a checklist's caption. */
function AllNone({ n, of, onAll }: { n?: number; of?: number; onAll(on: boolean): void }) {
  return (
    <>
      {n != null && of != null && <>{`${n} of ${of} · `}</>}
      <button type="button" className="wf-link" onClick={() => onAll(true)}>
        all
      </button>
      {' · '}
      <button type="button" className="wf-link" onClick={() => onAll(false)}>
        none
      </button>
    </>
  );
}

interface ShellProps {
  title: string;
  primary: string;
  send: Send;
  /** An error no field claims, shown above the bar. */
  error?: string | null;
  progress?: FormProgress;
  /** Replaces the action bar (the edit-repos confirm step). */
  bar?: ReactNode | false;
  children: ReactNode;
}

/** A form that owns the column: a title row with ✕, sections that scroll,
 * and an action bar pinned to the bottom (3c) — or, once submitted, the
 * frozen form with the job's progress in the bar (3g). */
function Shell({ title, primary, send, error, progress, bar, children }: ShellProps) {
  const cancel = () => (progress ? progress.onLeave() : send({ op: 'cancel' }));
  // Frozen, nothing in it has the focus: esc anywhere leaves it (the page's
  // own key handling steps aside for a `data-popup`).
  const leave = progress?.onLeave;
  useEffect(() => {
    if (!leave) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      e.preventDefault();
      e.stopPropagation();
      leave();
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [leave]);
  return (
    <form
      className={progress ? 'wf webform frozen' : 'wf webform'}
      data-popup={progress ? '' : undefined}
      data-testid="webform"
      onSubmit={(e) => {
        e.preventDefault();
        if (!progress) send({ op: 'submit' });
      }}
      onKeyDown={(e) => {
        if (e.key === 'Escape') {
          e.preventDefault();
          e.stopPropagation();
          cancel();
        }
      }}
    >
      <div className="wf-top">
        <span className="wf-title">{title}</span>
        <span className="grow" />
        <button type="button" className="wf-x" aria-label={progress ? 'leave it running' : 'cancel'} onClick={cancel}>
          ✕
        </button>
      </div>
      <fieldset className="wf-scroll" disabled={!!progress}>
        {children}
      </fieldset>
      {error && !progress && (
        <div className="wf-err wf-err-bar" role="alert" data-testid="form-error">
          {error}
        </div>
      )}
      {progress ? (
        <div className="wf-bar" data-testid="form-progress">
          <div className="wf-prog">
            <div className="wf-prog-row">
              <span>{progress.job.title}</span>
              <span className="muted">
                {progress.job.counter}
                {progress.job.fraction != null && ` ${Math.round(progress.job.fraction * 100)}%`}
              </span>
            </div>
            <div className="wf-track">
              <i
                className={progress.job.fraction == null ? 'marquee' : undefined}
                style={progress.job.fraction == null ? undefined : { width: `${Math.round(progress.job.fraction * 100)}%` }}
              />
            </div>
            <div className="wf-hint">keeps going on the Work tab — esc to leave it running</div>
          </div>
        </div>
      ) : bar !== undefined ? (
        bar
      ) : (
        <div className="wf-bar" data-testid="form-buttons">
          <button type="button" className="wf-btn ghost" onClick={cancel}>
            Cancel <span className="k">esc</span>
          </button>
          <button type="submit" className="wf-btn pri">
            {primary} <span className="k">⏎</span>
          </button>
        </div>
      )}
    </form>
  );
}

function CreateForm({ mode, send, error, touch, progress }: { mode: Extract<FormMode, { kind: 'create' }>; send: Send; error: string | null; touch: boolean; progress?: FormProgress }) {
  const [ws, setWs] = useEcho(mode.workspace_index - 1);
  const [agent, setAgent] = useEcho(mode.agent_index);
  const pickWs = (i: number) => {
    setWs(i);
    send({ op: 'pick', field: 'workspace', index: i });
  };
  const nameError = error && errorField('create', error) === 'name' ? error : null;
  const checked = mode.repos.filter((r) => r.checked).length;
  return (
    <Shell title="New task" primary="Create task" send={send} error={nameError ? null : error} progress={progress}>
      <TextField
        caption="NAME"
        field="name"
        value={mode.name}
        send={send}
        autoFocus={!touch && !progress}
        placeholder="what the task is about"
        error={nameError}
        hint={mode.slug ? <span data-testid="slug">→ {mode.slug}</span> : null}
      />
      <Section caption="WORKSPACE">
        {mode.workspace_options.length > 6 ? (
          // Many workspaces: a native list reads better than a wall of chips.
          <span className="wf-select">
            <span className="wf-dot" style={{ background: mode.workspace_options[ws]?.color }} />
            <select className="wf-in" data-field="workspace" aria-label="workspace" value={ws} onChange={(e) => pickWs(Number(e.currentTarget.value))}>
              {mode.workspace_options.map((w, i) => (
                <option key={w.name} value={i}>
                  {w.name}
                </option>
              ))}
            </select>
          </span>
        ) : (
          <div className="wf-chips" role="radiogroup" aria-label="workspace">
            {mode.workspace_options.map((w, i) => (
              <ChipRadio key={w.name} name="workspace" checked={i === ws} onPick={() => pickWs(i)}>
                <span className="wf-dot" style={{ background: w.color }} />
                {w.name}
              </ChipRadio>
            ))}
          </div>
        )}
      </Section>
      <Section
        caption="REPOS"
        aside={
          mode.repos.length > 0 && (
            <AllNone n={checked} of={mode.repos.length} onAll={(on) => mode.repos.forEach((_, i) => send({ op: 'check', field: 'repo', index: i, on }))} />
          )
        }
      >
        {mode.repos.length === 0 ? (
          <span className="wf-hint">this workspace has no repos yet</span>
        ) : (
          <div className="wf-chips" aria-label="repos">
            {mode.repos.map((r, i) => (
              <ChipCheck key={`${mode.workspace}/${r.name}`} label={r.name} checked={r.checked} onChange={(on) => send({ op: 'check', field: 'repo', index: i, on })} />
            ))}
          </div>
        )}
      </Section>
      <Section caption="AGENT">
        <div className="wf-chips" role="radiogroup" aria-label="agent">
          {mode.agent_options.map((a, i) => (
            <ChipRadio
              key={a}
              name="agent"
              checked={i === agent}
              onPick={() => {
                setAgent(i);
                send({ op: 'pick', field: 'agent', index: i });
              }}
            >
              {a}
              {i === 0 && <span className="wf-sub">{mode.agent_default}</span>}
            </ChipRadio>
          ))}
        </div>
      </Section>
    </Shell>
  );
}

function SkillsSwitch({ checked, send }: { checked: boolean; send: Send }) {
  const [on, setOn] = useEcho(checked);
  return (
    <section className="wf-sec">
      <label className="wf-switch-row">
        <input
          type="checkbox"
          role="switch"
          className="wf-hidden"
          checked={on}
          onChange={(e) => {
            setOn(e.currentTarget.checked);
            send({ op: 'check', field: 'skills', on: e.currentTarget.checked });
          }}
        />
        <span className={on ? 'wf-sw on' : 'wf-sw'} aria-hidden="true" />
        <span className="bright">Install the tenx skills</span>
      </label>
      <div className="wf-hint">/tenx and /standup for Claude, Codex and pi, plus AGENTS.md</div>
    </section>
  );
}

function EditRepos({ mode, send, onKey, error, progress }: { mode: Extract<FormMode, { kind: 'edit_repos' }>; send: Send; onKey(key: string): void; error: string | null; progress?: FormProgress }) {
  const detach = mode.picks.filter((p) => p.present && !p.checked).map((p) => p.name);
  const confirm = mode.confirm && !progress;
  return (
    <Shell
      title={`Repos of ${mode.task}`}
      primary="Apply"
      send={send}
      error={error}
      progress={progress}
      bar={confirm ? false : undefined}
    >
      <div className="wf-cap wf-cap-pad">
        <span>IN THIS TASK</span>
        {!confirm && (
          <span className="wf-aside">
            <AllNone onAll={(on) => mode.picks.forEach((_, i) => send({ op: 'check', field: 'repo', index: i, on }))} />
          </span>
        )}
      </div>
      {mode.picks.map((p, i) => (
        <RepoRow key={p.name} name={p.name} checked={p.checked} present={p.present} disabled={confirm} onChange={(on) => send({ op: 'check', field: 'repo', index: i, on })} />
      ))}
      {confirm && (
        <div className="wf-confirm" role="alert" data-testid="detach-confirm">
          <div>
            <b>Detach {detach.join(', ')}?</b> {detach.length > 1 ? 'Their worktrees and branches are' : 'Its worktree and branch are'} removed;
            uncommitted work in {detach.length > 1 ? 'them' : 'it'} is lost.
          </div>
          <div className="wf-confirm-row">
            <button type="button" className="wf-btn ghost on-danger" onClick={() => onKey('Escape')}>
              Back <span className="k">esc</span>
            </button>
            <button type="button" className="wf-btn danger" onClick={() => onKey('y')} autoFocus>
              Detach <span className="k">y</span>
            </button>
          </div>
        </div>
      )}
    </Shell>
  );
}

function RepoRow({ name, checked, present, disabled, onChange }: { name: string; checked: boolean; present: boolean; disabled: boolean; onChange(on: boolean): void }) {
  const [on, setOn] = useEcho(checked);
  const badge = on === present ? (present ? ['in', 'in task'] : null) : on ? ['add', '+ add'] : ['det', '− detach'];
  return (
    <label className="wf-row">
      <input
        type="checkbox"
        className="wf-hidden"
        checked={on}
        disabled={disabled}
        onChange={(e) => {
          setOn(e.currentTarget.checked);
          onChange(e.currentTarget.checked);
        }}
      />
      <span className={on ? 'wf-cbx on' : 'wf-cbx'} aria-hidden="true">
        {on ? '✓' : ''}
      </span>
      <span className={on ? 'bright' : 'muted'}>{name}</span>
      <span className="grow" />
      {badge && <span className={`wf-badge ${badge[0]}`}>{badge[1]}</span>}
    </label>
  );
}

export function WebForms({ mode, status, send, onKey, touch, progress }: Props) {
  const error = useFormError(status);
  const errFor = (kind: FormMode['kind'], field: string) => (error && errorField(kind, error) === field ? error : null);
  const rest = (kind: FormMode['kind']) => (error && errorField(kind, error) === null ? error : null);
  switch (mode.kind) {
    case 'create':
      return <CreateForm mode={mode} send={send} error={error} touch={touch} progress={progress} />;
    case 'add_repo':
      return (
        <Shell title={`Add a repo to ${mode.workspace}`} primary="Add repo" send={send} error={rest('add_repo')} progress={progress}>
          <TextField
            caption="URL"
            field="url"
            value={mode.url}
            send={send}
            autoFocus={!touch && !progress}
            placeholder="git@github.com:org/repo.git"
            error={errFor('add_repo', 'url')}
          />
          <TextField
            caption="NAME"
            field="name"
            value={mode.name}
            send={send}
            aside="optional"
            placeholder="from the url"
            error={errFor('add_repo', 'name')}
          />
        </Shell>
      );
    case 'new_workspace':
      return (
        <Shell title="New workspace" primary="Create workspace" send={send} error={rest('new_workspace')} progress={progress}>
          <TextField
            caption="FOLDER"
            field="path"
            value={mode.path}
            send={send}
            autoFocus={!touch && !progress}
            placeholder="~/work/acme"
            hint="created if it doesn't exist"
            error={errFor('new_workspace', 'path')}
          />
          <TextField caption="NAME" field="name" value={mode.name} send={send} placeholder="the folder's last part" error={errFor('new_workspace', 'name')} />
          <TextField
            caption="FIRST REPO"
            field="repo_url"
            value={mode.repo_url}
            send={send}
            aside="optional"
            placeholder="git@github.com:org/app.git"
            error={errFor('new_workspace', 'repo_url')}
          />
          <SkillsSwitch checked={mode.skills} send={send} />
        </Shell>
      );
    case 'edit_repos':
      return <EditRepos mode={mode} send={send} onKey={onKey} error={error} progress={progress} />;
  }
}

/** Rename, in the search box's place (3e): the title, esc and Save, and a
 * note that only the title changes. */
export function RenameForm({ value, status, send, touch }: { value: string; status: string | null; send: Send; touch: boolean }) {
  const error = useFormError(status);
  return (
    <form
      className="wf-rename webform"
      data-testid="webform"
      onSubmit={(e) => {
        e.preventDefault();
        send({ op: 'submit' });
      }}
      onKeyDown={(e) => {
        if (e.key === 'Escape') {
          e.preventDefault();
          e.stopPropagation();
          send({ op: 'cancel' });
        }
      }}
    >
      <div className={error ? 'wf-ren err' : 'wf-ren'}>
        <span className="accent">✎</span>
        <RenameInput value={value} send={send} autoFocus={!touch} />
        <button type="button" className="wf-btn ghost small" aria-label="cancel" onClick={() => send({ op: 'cancel' })}>
          <span className="k">esc</span>
        </button>
        <button type="submit" className="wf-btn pri small">
          Save <span className="k">⏎</span>
        </button>
      </div>
      {error ? (
        <div className="wf-err wf-ren-note" role="alert" data-testid="form-error">
          {error}
        </div>
      ) : (
        <div className="wf-hint wf-ren-note">renames the title only — the branch and folder stay the same</div>
      )}
    </form>
  );
}

function RenameInput({ value, send, autoFocus }: { value: string; send: Send; autoFocus: boolean }) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    if (autoFocus) {
      el.focus({ preventScroll: true });
      el.select();
    }
  }, [autoFocus]);
  useEffect(() => {
    const el = ref.current;
    if (el && document.activeElement !== el && el.value !== value) el.value = value;
  }, [value]);
  return (
    <input
      ref={ref}
      className="wf-ren-in"
      data-field="title"
      aria-label="task title"
      defaultValue={value}
      autoComplete="off"
      spellCheck={false}
      onInput={(e) => send({ op: 'set', field: 'title', value: e.currentTarget.value })}
    />
  );
}
