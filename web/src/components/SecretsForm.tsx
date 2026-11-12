'use client';

// A task's pending secrets requests, answered in a web form instead of the
// terminal's `tenx secrets fulfill` sheet (which the TUI client still opens):
// a real form takes iOS's Paste and offers the password manager, where a
// terminal can't. The same questions — grant or deny each request, a note
// for the denied, a value for each new one, the passphrase once — sent in
// one go (`POST /secrets/answer`, `src/web/secrets.rs`). A wrong passphrase
// changes nothing, so the form stays as typed and can be sent again. Each
// value field can take the Mac's clipboard (`lib/macclip` `fetchFromMac`),
// shown as its length and tail — what a value the agent asked for with
// `need --clipboard` is meant to be filled with, one copy per field.

import { useEffect, useRef, useState } from 'react';
import { apiUrl } from '@/lib/api';
import { fetchFromMac } from '@/lib/macclip';

interface Request {
  name: string;
  /** `release`: sealed, the passphrase opens it; `value`: someone types one. */
  ask: 'release' | 'value';
  why: string;
  sealed_in: string;
  /** Asked for from the clipboard (`need --clipboard`). */
  clipboard: boolean;
}

interface Sitting {
  title: string;
  requests: Request[];
  passphrase: boolean;
  /** Why granting can't work from here (no identity yet). */
  problem: string | null;
}

interface Answered {
  denied: string[];
  released: string[];
  skipped: string[];
}

interface Props {
  /** The task, as a column row's id. */
  task: string;
  /** A touch screen: no autofocus (it can't raise the keyboard outside a tap). */
  touch: boolean;
  /** Closed: with what happened, when something did. */
  onClose(outcome?: string): void;
}

async function post<T>(path: string, task: string, body?: unknown): Promise<T> {
  const res = await fetch(apiUrl(`${path}?task=${encodeURIComponent(task)}`), {
    method: 'POST',
    credentials: 'include',
    ...(body === undefined ? {} : { body: JSON.stringify(body), headers: { 'content-type': 'application/json' } }),
  });
  if (!res.ok) throw new Error((await res.text()) || `failed: ${res.status}`);
  return (await res.json()) as T;
}

/** A value as the terminal shows one it can't print (`describe_value` in
 *  tenx-core): its length, and the last four characters of a long one. */
export function describeValue(value: string): string {
  const chars = [...value];
  const n = chars.length;
  return n >= 16 ? `${n} characters, ending …${chars.slice(n - 4).join('')}` : `${n} characters`;
}

/** What the column's footer says after an answer. */
export function outcomeText(a: Answered): string {
  const parts = [
    a.released.length && `released ${a.released.join(', ')}`,
    a.denied.length && `denied ${a.denied.join(', ')}`,
    a.skipped.length && `${a.skipped.join(', ')} still pending (no value)`,
  ].filter(Boolean);
  return parts.length ? parts.join(' · ') : 'nothing was pending any more';
}

export function SecretsForm({ task, touch, onClose }: Props) {
  const [sitting, setSitting] = useState<Sitting | null>(null);
  const [granted, setGranted] = useState<Set<string>>(new Set());
  const [values, setValues] = useState<Record<string, string>>({});
  const [note, setNote] = useState('');
  const [passphrase, setPassphrase] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const pass = useRef<HTMLInputElement>(null);

  useEffect(() => {
    let live = true;
    post<Sitting>('/secrets', task)
      .then((s) => {
        if (!live) return;
        setSitting(s);
        setGranted(new Set(s.requests.map((r) => r.name)));
      })
      .catch((e: unknown) => live && setError(e instanceof Error ? e.message : String(e)));
    return () => {
      live = false;
    };
  }, [task]);

  const requests = sitting?.requests ?? [];
  const grants = requests.filter((r) => granted.has(r.name));
  const denials = requests.filter((r) => !granted.has(r.name));
  const unlocking = grants.some((r) => r.ask === 'release' || values[r.name]);
  const primary = !grants.length ? 'Deny' : !denials.length ? 'Grant' : `Grant ${grants.length} · deny ${denials.length}`;

  const submit = async () => {
    if (!sitting || busy) return;
    setBusy(true);
    setError(null);
    try {
      const answered = await post<Answered>('/secrets/answer', task, {
        shown: requests.map((r) => r.name),
        granted: grants.map((r) => r.name),
        values: grants.filter((r) => r.ask === 'value').map((r) => [r.name, values[r.name] ?? '']),
        note,
        passphrase: unlocking ? passphrase : '',
      });
      onClose(outcomeText(answered));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      if (/passphrase/i.test(String(e))) {
        setPassphrase('');
        pass.current?.focus();
      }
    } finally {
      setBusy(false);
    }
  };

  const fromMac = async (name: string) => {
    setError(null);
    try {
      const text = (await fetchFromMac()).replace(/\r?\n$/, '');
      setValues((vs) => ({ ...vs, [name]: text }));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const grant = (name: string, on: boolean) =>
    setGranted((g) => {
      const next = new Set(g);
      if (on) next.add(name);
      else next.delete(name);
      return next;
    });

  return (
    <form
      className="wf webform"
      data-testid="secrets-form"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
      onKeyDown={(e) => {
        if (e.key === 'Escape') {
          e.preventDefault();
          e.stopPropagation();
          onClose();
        }
      }}
    >
      <div className="wf-top">
        <span className="wf-title">secrets{sitting ? ` · ${sitting.title}` : ''}</span>
        <span className="grow" />
        <button type="button" className="wf-x" aria-label="cancel" onClick={() => onClose()}>
          ✕
        </button>
      </div>
      <fieldset className="wf-scroll" disabled={busy}>
        {!sitting && !error && <div className="wf-sec wf-hint">loading…</div>}
        {sitting && !requests.length && <div className="wf-sec wf-hint">nothing is pending for this task</div>}
        {requests.map((r, i) => {
          const on = granted.has(r.name);
          return (
            <section className="wf-sec" key={r.name} data-testid="secret-request">
              <div className="wf-cap">
                <span className="bright">{r.name}</span>
                <span className="wf-aside">
                  {r.ask === 'release' ? `release · ${r.sealed_in}` : r.clipboard ? 'new value · copy it' : 'new value'}
                </span>
              </div>
              {r.why && <div className="wf-hint">why: {r.why}</div>}
              <div className="wf-chips" role="radiogroup" aria-label={`answer for ${r.name}`}>
                <label className={on ? 'wf-chip on' : 'wf-chip'}>
                  <input type="radio" className="wf-hidden" name={`grant-${r.name}`} checked={on} onChange={() => grant(r.name, true)} />
                  grant
                </label>
                <label className={!on ? 'wf-chip on' : 'wf-chip'}>
                  <input type="radio" className="wf-hidden" name={`grant-${r.name}`} checked={!on} onChange={() => grant(r.name, false)} />
                  deny
                </label>
              </div>
              {on && r.ask === 'value' && (
                <>
                  <input
                    className="wf-in"
                    type="password"
                    aria-label={`value for ${r.name}`}
                    placeholder={r.clipboard ? 'copy it, then take the Mac’s clipboard' : 'paste or type the value'}
                    autoComplete="off"
                    autoCapitalize="off"
                    autoCorrect="off"
                    spellCheck={false}
                    autoFocus={!touch && i === 0}
                    value={values[r.name] ?? ''}
                    onChange={(e) => {
                      const v = e.currentTarget.value;
                      setValues((vs) => ({ ...vs, [r.name]: v }));
                    }}
                  />
                  <div className="wf-hint">
                    {values[r.name] ? describeValue(values[r.name]) : 'empty: it stays pending'} ·{' '}
                    <button type="button" className="wf-link" onClick={() => void fromMac(r.name)}>
                      take the Mac&apos;s clipboard
                    </button>
                  </div>
                </>
              )}
            </section>
          );
        })}
        {denials.length > 0 && (
          <section className="wf-sec">
            <div className="wf-cap">
              <span>NOTE FOR THE AGENT</span>
              <span className="wf-aside">optional</span>
            </div>
            <input
              className="wf-in"
              aria-label="note for the agent"
              value={note}
              autoComplete="off"
              onChange={(e) => setNote(e.currentTarget.value)}
            />
          </section>
        )}
        {unlocking && sitting?.passphrase && !sitting.problem && (
          <section className="wf-sec">
            <div className="wf-cap">
              <span>PASSPHRASE</span>
              <span className="wf-aside">unlocks your age identity once</span>
            </div>
            {/* A username field the password manager can file the
                passphrase under; never sent. */}
            <input type="text" name="username" autoComplete="username" value="tenx age identity" readOnly hidden />
            <input
              ref={pass}
              className="wf-in"
              type="password"
              name="password"
              aria-label="passphrase"
              autoComplete="current-password"
              value={passphrase}
              onChange={(e) => setPassphrase(e.currentTarget.value)}
            />
          </section>
        )}
      </fieldset>
      {(error || (unlocking && sitting?.problem)) && (
        <div className="wf-err wf-err-bar" role="alert" data-testid="form-error">
          {error ?? sitting?.problem}
        </div>
      )}
      <div className="wf-bar">
        <button type="button" className="wf-btn ghost" onClick={() => onClose()}>
          Cancel <span className="k">esc</span>
        </button>
        <button
          type="submit"
          className="wf-btn pri"
          disabled={busy || !requests.length || (unlocking && (!!sitting?.problem || (!!sitting?.passphrase && !passphrase)))}
        >
          {busy ? 'Unlocking…' : primary} <span className="k">⏎</span>
        </button>
      </div>
    </form>
  );
}
