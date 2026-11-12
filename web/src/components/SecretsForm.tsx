'use client';

// A task's pending secrets requests, answered in a web form instead of the
// terminal's `tenx secrets fulfill` sheet (which the TUI client still opens):
// a real form takes iOS's Paste and offers the password manager, where a
// terminal can't. The same questions — grant or deny each request, a note
// for the denied, a value for each new one, the passphrase once — sent in
// one go (`POST /secrets/answer`, `src/web/secrets.rs`). A wrong passphrase
// changes nothing, so the form stays as typed and can be sent again. Each
// value field has a Paste button (`lib/secretclip`: this device's clipboard,
// else the Mac's), and what it took is shown as its length and tail — a
// value the agent asked for with `need --clipboard` says to copy it first.
// Design: the "tenx secrets clipboard" file of the tenx web Claude Design
// project.

import { useEffect, useRef, useState } from 'react';
import { apiUrl } from '@/lib/api';
import { describeValue, readSecretClip, type ClipSource } from '@/lib/secretclip';

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
  // What Paste put in a field (shown while the field still holds it), and
  // why it put nothing.
  const [pasted, setPasted] = useState<Record<string, { text: string; source: ClipSource }>>({});
  const [clipError, setClipError] = useState<Record<string, string>>({});
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
  // The passphrase field is always there when the identity has one, and
  // waits until every granted value is filled in.
  const missing = grants.filter((r) => r.ask === 'value' && !values[r.name]);
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

  const paste = async (name: string) => {
    try {
      const clip = await readSecretClip();
      setValues((vs) => ({ ...vs, [name]: clip.text }));
      setPasted((p) => ({ ...p, [name]: clip }));
      setClipError((c) => ({ ...c, [name]: '' }));
    } catch (e) {
      setClipError((c) => ({ ...c, [name]: e instanceof Error ? e.message : String(e) }));
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
                  {r.ask === 'release' ? `release · ${r.sealed_in}` : 'new value'}
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
                  {r.clipboard && !values[r.name] && <div className="wf-ask">Copy the value, then paste it here.</div>}
                  <div className="wf-pasterow">
                    <input
                      className={clipError[r.name] ? 'wf-in err' : pasted[r.name]?.text === values[r.name] && values[r.name] ? 'wf-in ok' : 'wf-in'}
                      type="password"
                      aria-label={`value for ${r.name}`}
                      placeholder="value"
                      autoComplete="off"
                      autoCapitalize="off"
                      autoCorrect="off"
                      spellCheck={false}
                      autoFocus={!touch && i === 0}
                      value={values[r.name] ?? ''}
                      onChange={(e) => {
                        const v = e.currentTarget.value;
                        setValues((vs) => ({ ...vs, [r.name]: v }));
                        setClipError((c) => ({ ...c, [r.name]: '' }));
                      }}
                    />
                    <button
                      type="button"
                      className={values[r.name] ? 'wf-paste quiet' : 'wf-paste'}
                      aria-label={`paste ${r.name}`}
                      onClick={() => void paste(r.name)}
                    >
                      ⎘ Paste
                    </button>
                  </div>
                  {clipError[r.name] ? (
                    <div className="wf-err" role="alert">
                      {clipError[r.name]}
                    </div>
                  ) : values[r.name] ? (
                    <div className={pasted[r.name]?.text === values[r.name] ? 'wf-hint wf-ok' : 'wf-hint'}>
                      {pasted[r.name]?.text === values[r.name]
                        ? `✓ from ${pasted[r.name].source === 'device' ? 'this device' : 'the Mac'} · ${describeValue(values[r.name])}`
                        : describeValue(values[r.name])}
                    </div>
                  ) : (
                    !r.clipboard && <div className="wf-hint">empty: it stays pending</div>
                  )}
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
        {grants.length > 0 && sitting?.passphrase && !sitting.problem && (
          <section className="wf-sec">
            <div className="wf-cap">
              <span>PASSPHRASE</span>
              <span className="wf-aside">
                {missing.length ? `fill in ${missing.map((r) => r.name).join(', ')} first` : 'unlocks your age identity once'}
              </span>
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
              disabled={missing.length > 0}
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
