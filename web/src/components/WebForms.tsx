'use client';

// The column's forms as real web forms: text inputs, a workspace select, an
// agent radio group, checkboxes — native Tab order, Enter submits, Escape
// cancels. The server's form state (view.rs `ModeView`) stays the one truth:
// every edit goes over as a `form` message (`FormOp`) and comes back in the
// next view; submit and cancel run the form's own ⏎ / esc there, so a web form
// creates, adds and renames exactly as the terminal's does.
//
// Text inputs are uncontrolled while they have the focus — the page sends the
// whole value on every input and never writes the server's echo back into a
// field being typed in, so the caret doesn't jump. Checkboxes, the select and
// the radios follow the server, showing a change at once until its echo.

import { useEffect, useRef, useState, type ReactNode } from 'react';
import type { FormOp, ModeView } from '@/protocol';

type Send = (op: FormOp) => void;

interface Props {
  mode: ModeView;
  /** `view.status`: shown in the form as its error once it changes. */
  status: string | null;
  send: Send;
  /** A key as the column would take it (the edit-repos confirm step). */
  onKey(key: string): void;
  /** A touch screen: no autofocus (it can't raise the keyboard outside a tap). */
  touch: boolean;
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

interface TextProps {
  label: string;
  field: string;
  value: string;
  send: Send;
  autoFocus?: boolean;
  placeholder?: string;
}

function TextField({ label, field, value, send, autoFocus, placeholder }: TextProps) {
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
    <label className="wf-row">
      <span className="wf-label">{label}</span>
      <input
        ref={ref}
        className="wf-input"
        data-field={field}
        defaultValue={value}
        placeholder={placeholder}
        autoComplete="off"
        autoCapitalize="off"
        autoCorrect="off"
        spellCheck={false}
        onInput={(e) => send({ op: 'set', field, value: e.currentTarget.value })}
      />
    </label>
  );
}

function CheckField({ label, note, checked, onChange }: { label: string; note?: string; checked: boolean; onChange(on: boolean): void }) {
  const [on, setOn] = useEcho(checked);
  return (
    <label className={on ? 'wf-check on' : 'wf-check'}>
      <input
        type="checkbox"
        checked={on}
        onChange={(e) => {
          setOn(e.currentTarget.checked);
          onChange(e.currentTarget.checked);
        }}
      />
      <span>{label}</span>
      {note && <span className="wf-note">{note}</span>}
    </label>
  );
}

interface FormProps {
  title: string;
  primary: string;
  send: Send;
  error: string | null;
  /** Buttons left of cancel (edit repos: all / none). */
  extra?: ReactNode;
  children: ReactNode;
  inline?: boolean;
}

/** A form's frame (title on the border, as the TUI's box), its error, and
 * cancel / primary inside at the bottom-right. */
function WebForm({ title, primary, send, error, extra, children, inline }: FormProps) {
  return (
    <form
      className={inline ? 'search rename webform' : 'form webform'}
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
      <span className={inline ? 'search-title' : 'form-title'}>{title}</span>
      {inline ? children : <div className="wf-body">{children}</div>}
      {error && (
        <div className="wf-error" role="alert" data-testid="form-error">
          {error}
        </div>
      )}
      <div className="fbtns" data-testid="form-buttons">
        {extra}
        <button type="button" className="pill ghost" onClick={() => send({ op: 'cancel' })}>
          cancel <span className="k">esc</span>
        </button>
        <button type="submit" className={inline ? 'pill pri' : 'pill pri big'}>
          {primary} <span className="k">⏎</span>
        </button>
      </div>
    </form>
  );
}

function CreateForm({ mode, send, error, touch }: { mode: Extract<ModeView, { kind: 'create' }>; send: Send; error: string | null; touch: boolean }) {
  const [ws, setWs] = useEcho(mode.workspace_index - 1);
  const [agent, setAgent] = useEcho(mode.agent_index);
  const color = mode.workspace_options[ws]?.color;
  return (
    <WebForm title="new task" primary="create" send={send} error={error}>
      <label className="wf-row">
        <span className="wf-label">workspace</span>
        {mode.workspace_options.length > 1 ? (
          <span className="wf-select">
            <span className="wf-dot" style={{ background: color }} />
            <select
              className="wf-input"
              data-field="workspace"
              value={ws}
              onChange={(e) => {
                const i = Number(e.currentTarget.value);
                setWs(i);
                send({ op: 'pick', field: 'workspace', index: i });
              }}
            >
              {mode.workspace_options.map((w, i) => (
                <option key={w.name} value={i}>
                  {w.name}
                </option>
              ))}
            </select>
          </span>
        ) : (
          <span className="wf-static" style={{ color }}>
            {mode.workspace}
          </span>
        )}
      </label>
      <TextField label="name" field="name" value={mode.name} send={send} autoFocus={!touch} placeholder="what the task is about" />
      <fieldset className="wf-group">
        <legend className="wf-label">repos</legend>
        {mode.repos.length === 0 && <span className="wf-note">this workspace has no repos yet</span>}
        {mode.repos.map((r, i) => (
          <CheckField key={`${mode.workspace}/${r.name}`} label={r.name} checked={r.checked} onChange={(on) => send({ op: 'check', field: 'repo', index: i, on })} />
        ))}
      </fieldset>
      <fieldset className="wf-group">
        <legend className="wf-label">agent</legend>
        <div className="wf-seg" role="radiogroup">
          {mode.agent_options.map((a, i) => (
            <label key={a} className={i === agent ? 'on' : undefined}>
              <input
                type="radio"
                name="agent"
                value={i}
                checked={i === agent}
                onChange={() => {
                  setAgent(i);
                  send({ op: 'pick', field: 'agent', index: i });
                }}
              />
              {a}
            </label>
          ))}
        </div>
        {agent === 0 && <span className="wf-note">inherits the workspace's agent</span>}
      </fieldset>
    </WebForm>
  );
}

export function WebForms({ mode, status, send, onKey, touch }: Props) {
  const error = useFormError(status);
  switch (mode.kind) {
    case 'create':
      return <CreateForm mode={mode} send={send} error={error} touch={touch} />;
    case 'add_repo':
      return (
        <WebForm title={`add repo to ${mode.workspace}`} primary="add" send={send} error={error}>
          <TextField label="url" field="url" value={mode.url} send={send} autoFocus={!touch} placeholder="git@github.com:org/repo.git" />
          <TextField label="name" field="name" value={mode.name} send={send} placeholder="from the url" />
        </WebForm>
      );
    case 'new_workspace':
      return (
        <WebForm title="new workspace" primary="create" send={send} error={error}>
          <TextField label="path" field="path" value={mode.path} send={send} autoFocus={!touch} placeholder="~/work/acme" />
          <TextField label="name" field="name" value={mode.name} send={send} placeholder="the path's last part" />
          <TextField label="repo url" field="repo_url" value={mode.repo_url} send={send} placeholder="optional — add repos later" />
          <CheckField
            label="skills"
            note="/tenx, /standup and AGENTS.md"
            checked={mode.skills}
            onChange={(on) => send({ op: 'check', field: 'skills', on })}
          />
        </WebForm>
      );
    case 'edit_repos':
      if (mode.confirm) {
        const detach = mode.picks.filter((p) => p.present && !p.checked).map((p) => p.name);
        return (
          <div className="form webform" data-testid="webform">
            <span className="form-title">{`repos of ${mode.task}`}</span>
            <div className="wf-error" role="alert">
              detach {detach.join(', ')}? Each worktree and its branch are removed.
            </div>
            <div className="fbtns" data-testid="form-buttons">
              <button type="button" className="pill ghost" onClick={() => onKey('Escape')}>
                back <span className="k">esc</span>
              </button>
              <button type="button" className="pill no big" onClick={() => onKey('y')} autoFocus>
                detach <span className="k">y</span>
              </button>
            </div>
          </div>
        );
      }
      return (
        <WebForm
          title={`repos of ${mode.task}`}
          primary="apply"
          send={send}
          error={error}
          extra={
            <>
              <button type="button" className="pill ghost" onClick={() => mode.picks.forEach((_, i) => send({ op: 'check', field: 'repo', index: i, on: true }))}>
                all
              </button>
              <button type="button" className="pill ghost" onClick={() => mode.picks.forEach((_, i) => send({ op: 'check', field: 'repo', index: i, on: false }))}>
                none
              </button>
              <span className="grow" />
            </>
          }
        >
          <fieldset className="wf-group">
            {mode.picks.map((p, i) => (
              <CheckField
                key={p.name}
                label={p.name}
                checked={p.checked}
                note={p.checked === p.present ? undefined : p.checked ? 'add' : 'detach'}
                onChange={(on) => send({ op: 'check', field: 'repo', index: i, on })}
              />
            ))}
          </fieldset>
        </WebForm>
      );
    default:
      return null;
  }
}

/** Rename, in the search box's place: one field, cancel and save. */
export function RenameForm({ value, status, send, touch }: { value: string; status: string | null; send: Send; touch: boolean }) {
  const error = useFormError(status);
  return (
    <WebForm title="rename task" primary="save" send={send} error={error} inline>
      <span className="accent">✎ </span>
      <RenameInput value={value} send={send} autoFocus={!touch} />
    </WebForm>
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
      className="wf-input wf-bare"
      data-field="title"
      aria-label="task title"
      defaultValue={value}
      autoComplete="off"
      spellCheck={false}
      onInput={(e) => send({ op: 'set', field: 'title', value: e.currentTarget.value })}
    />
  );
}
