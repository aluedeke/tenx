//! Task-scoped secrets (`ARCHITECTURE.md` § Secrets has the overview; this
//! doc comment is the authoritative detail). This module shells out to the
//! system `age`/`age-keygen`/`sops` binaries rather than linking a crypto
//! crate, matching `git/mod.rs`'s reasoning for shelling to `git`: it's what
//! the user already has installed, at whatever version, with no
//! reimplementation risk. `sops` does every encrypt/decrypt — the task's own
//! bundle, values a human types in, and adopted secrets (see *Sops adoption*
//! below) — but it has no passphrase prompt of its own: it needs an
//! already-decrypted identity file via `SOPS_AGE_KEY_FILE`, so raw `age -d`
//! is still what unwraps a passphrase-protected identity (`with_plain_identity`),
//! and `age -p`/`age-keygen` are still what generate and protect one
//! (`generate_identity`).
//!
//! Hard rule, enforced throughout this file, not just documented: **no
//! function here ever writes a decrypted secret value to stdout.** Released
//! values only ever go to their fixed task-scoped files; a child process that
//! could emit plaintext has its stdout either nulled or captured into memory,
//! never inherited. Stdout an agent's Bash tool captures becomes part of its
//! conversation transcript — a durable artifact outside the task folder that
//! `task rm`'s cleanup never reaches.
//!
//! **Asking.** An agent asks with one command, `need NAME... [--why ..]`, and
//! never has to know what kind of request it is making. `sops` encrypts
//! values only, so the key names of every sealed file are readable without
//! the identity (`tenx_core::secrets::sealed_keys`), and `need` routes each
//! name by looking: already released → nothing to do; sealed somewhere (a
//! key of the task's bundle, or a key or filename fragment of an adopted
//! file) → the *release* queue; nowhere → the *value* queue, meaning a human
//! must type one. Only the name is ever queued, never a value — a queued
//! plaintext sitting on disk before any human confirmed anything would be a
//! strictly worse exposure than anything else in this design. `--why` is
//! kept beside the queues (`workspace::SECRETS_WHY_FILE`) and shown to
//! whoever answers. `decrypt NAME` and `set NAME` from an agent are the old
//! spellings: `decrypt` is `need`, `set` forces the value queue (rotating a
//! value that already exists).
//!
//! **Who acts.** Every entry point first tries to open `/dev/tty` — the exact
//! file `age`'s own passphrase prompt reads (not stdin; `age`'s error when
//! it's missing: "standard input is not a terminal, and /dev/tty is not
//! available"). A Bash-tool child process has no controlling terminal, so
//! from an agent the commands only enqueue and wait, never touching the
//! identity or a sealed file. With a terminal (a human's shell, or the
//! column's unlock) they act.
//!
//! **Answering.** `fulfill` is the one sitting a human answers everything in:
//! it lists what's pending with each reason, asks once whether to grant all,
//! deny all, or pick, reads a value (masked) for each granted value request,
//! and then unwraps the identity **once** — one passphrase — to seal the new
//! values into the bundle and release every granted name. A value typed in is
//! released in the same sitting, so the agent that asked for it gets it
//! without asking again. Release is per name: the task's `.secrets.env`
//! holds exactly the names granted so far (`tenx_core::secrets::merge_dotenv`),
//! not the whole bundle; adopted files are released whole, one file per
//! matching name. A denial is written down with the human's note
//! (`workspace::SECRETS_DENIED_FILE`) *before* the name leaves its queue, and
//! a fulfilment writes its output before the name leaves — the queue removal
//! is always the commit point.
//!
//! **Waiting.** The point of an agent asking is usually that it can't
//! continue without the answer, so `need` blocks after enqueueing
//! (`wait_for_human`): poll once a second until each name has left both
//! queues, then decide from the disk alone what that meant —
//! `tenx_core::secrets::wait_outcome`: a denial recorded → *denied*; a
//! released output modified at or after the request → *granted*; neither →
//! *withdrawn* (`cancel`). The wait is bounded (`--timeout`, default
//! `DEFAULT_WAIT`) because an agent's shell tool kills long-running commands:
//! on timeout the request stays queued and re-running resumes it, thanks to
//! the idempotent enqueue. Each outcome has its own exit code (`Exit`,
//! `tenx_core::secrets::exit_code`) so an agent can branch without parsing.
//!
//! Nothing tenx seals needs a `.gitignore`: the bundle, `.secrets.env`, the
//! queues and `.secrets-adopted/` all live directly under the task's own
//! directory (`tasks/<slug>/`), which is never itself a git repo (only the
//! `<repo>/` worktree subdirectories under it are) — so they're structurally
//! outside git's reach, and `task rm`'s `fs::remove_dir_all(&task.path)`
//! shreds them on teardown. Adopted secrets are decrypted there too and only
//! *symlinked* into their worktree (see *Sops adoption*).

use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::env;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use tenx_core::secrets::{self as rules, WaitOutcome};

use crate::palette;
use crate::workspace::{self, Task, Workspace};

/// An error carrying its own process exit code — how a wait reports denied /
/// withdrawn / still pending apart (`tenx_core::secrets::exit_code`).
/// `main` maps it; every other error exits 1.
#[derive(Debug)]
pub struct Exit {
    pub code: i32,
    pub message: String,
}

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Exit {}

/// Resolve or create the age identity for the active workspace.
pub fn init() -> Result<()> {
    let cwd = env::current_dir()?;
    let mut ws = workspace::find(&cwd)?;

    if let Ok(path) = resolve_identity_path(&ws) {
        eprintln!("using existing age identity at {}", path.display());
        eprintln!("(nothing to do — tenx secrets need/set will use it)");
        return Ok(());
    }

    eprintln!("no age identity found in the usual places:");
    eprintln!("  $SOPS_AGE_KEY_FILE, ~/.config/sops/age/keys.txt, ~/.config/age/keys.txt");
    eprintln!();
    let input = prompt("path to an existing identity (empty to generate a new one)")?;

    if input.is_empty() {
        let target = workspace::home_dir()?.join(".config").join("age").join("keys.txt");
        generate_identity(&target)?;
        eprintln!("✓ generated new passphrase-protected identity at {}", target.display());
    } else {
        let path = PathBuf::from(workspace::expand_home(&input));
        if !path.exists() {
            bail!("no such file: {}", path.display());
        }
        // Not at one of the default lookup paths — record it as this
        // workspace's explicit override rather than copying key material
        // around (copies drift; a pointer doesn't).
        ws.config.age_identity = Some(path.to_string_lossy().into_owned());
        ws.save_config()?;
        eprintln!("✓ workspace will use identity at {}", path.display());
    }
    Ok(())
}

/// Encrypt `file` as the sealed secrets bundle for `task_slug` — `sops
/// --encrypt`, matching that command's own name.
pub fn encrypt(task_slug: &str, file: &str) -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = workspace::find(&cwd)?;
    let task = ws.find_task(task_slug)?;
    let identity = resolve_identity_path(&ws)?;
    let recipient = resolve_recipient(&identity)?;

    let src = Path::new(file);
    if !src.exists() {
        bail!("no such file: {}", src.display());
    }
    let bundle_path = bundle_path(&task);

    sops_encrypt(&recipient, src, &bundle_path)?;
    eprintln!("✓ encrypted {} → {}", src.display(), bundle_path.display());
    Ok(())
}


// ── Asking ──────────────────────────────────────────────────────────────────

/// Ask for `names` for the current task (resolved from cwd): route each to
/// the queue that can satisfy it and, from an agent, wait up to `wait` for a
/// human to answer (`None`: enqueue and return). See module docs, *Asking*.
pub fn need(names: &[String], why: Option<&str>, wait: Option<Duration>) -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = workspace::find(&cwd)?;
    let task = current_task(&ws, &cwd)?;
    need_in(&ws, &task, names, why, wait)
}

/// Same as [`need`], for an explicit workspace/task rather than cwd.
pub fn need_in(ws: &Workspace, task: &Task, names: &[String], why: Option<&str>, wait: Option<Duration>) -> Result<()> {
    for name in names {
        check_name(name)?;
    }
    let requested_at = request_instant();
    let mut asked: Vec<String> = Vec::new();
    for name in names {
        if asked.contains(name) {
            continue;
        }
        if let Some(at) = released_at(task, name) {
            eprintln!("✓ '{name}' is already released → {}", at.display());
            continue;
        }
        let queue = if is_sealed(task, name) { Queue::Release } else { Queue::Value };
        enqueue(task, queue, name, why)?;
        asked.push(name.clone());
    }
    if asked.is_empty() {
        return Ok(());
    }
    if tty_available() {
        // A human typed it: answer on the spot, in the same sitting the
        // column's unlock opens.
        return fulfill_in(ws, task);
    }
    match wait {
        Some(timeout) => wait_for_human(task, &asked, requested_at, timeout),
        None => Ok(()),
    }
}

/// Put `name` on `queue`, clear any earlier denial of it, and record `why`.
/// Idempotent: a name already on either queue stays where it is, so a
/// chatty agent re-running the same request can't spam notifications.
fn enqueue(task: &Task, queue: Queue, name: &str, why: Option<&str>) -> Result<()> {
    set_note(task, workspace::SECRETS_DENIED_FILE, name, None)?;
    if let Some(why) = why.map(str::trim).filter(|w| !w.is_empty()) {
        set_note(task, workspace::SECRETS_WHY_FILE, name, Some(why))?;
    }
    if let Some(on) = queued_on(task, name) {
        eprintln!("'{name}' is already requested for task '{}' — {}", task.name, on.what());
        return Ok(());
    }
    queue.push(task, name)?;
    eprintln!("requested '{name}' for task '{}' — {}", task.name, queue.what());
    Ok(())
}

/// Supply a value for `name` in the current task's (resolved from cwd)
/// sealed bundle — literally `sops set`, editing the bundle in place and
/// leaving every other key as it was. From an agent it can only ask: the
/// name goes on the value queue even when something is already sealed under
/// it (that's how an agent asks for a rotation). From a terminal it prompts
/// for the value (masked — tenx's own prompt, `read_masked_line`), then the
/// passphrase, and seals it; if someone had asked for `name`, or it was
/// released before, it's released in the same unlock. The value is never a
/// CLI argument or read from stdin (visible to `ps`, and stdin would invite
/// piping one in) — always typed into `/dev/tty`, the channel the passphrase
/// itself uses.
pub fn set(name: &str, wait: Option<Duration>) -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = workspace::find(&cwd)?;
    let task = current_task(&ws, &cwd)?;
    set_in(&ws, &task, name, wait)
}

/// Same as [`set`], for an explicit workspace/task rather than cwd.
pub fn set_in(ws: &Workspace, task: &Task, name: &str, wait: Option<Duration>) -> Result<()> {
    check_name(name)?;
    if !tty_available() {
        let requested_at = request_instant();
        enqueue(task, Queue::Value, name, None)?;
        return match wait {
            Some(timeout) => wait_for_human(task, &[name.to_string()], requested_at, timeout),
            None => Ok(()),
        };
    }

    let value = read_masked_line(&format!("value for '{name}'"))?;
    if value.is_empty() {
        bail!("no value given — aborted, nothing was set");
    }
    // Asked for, or granted before (a rotation must not leave the old
    // plaintext in place): release it in the same unlock.
    let release = queued_on(task, name).is_some() || released_at(task, name).is_some();
    let values = [(name.to_string(), value)];
    with_unlocked(ws, true, |id| if release { seal_and_release(id, task, &values, &[]) } else { seal(id, task, &values) })?;
    if !release {
        eprintln!("  released to the task when someone asks for it: tenx secrets need {name}");
    }
    Ok(())
}

/// Release the current task's (resolved from cwd) secrets. From a terminal:
/// `name` if given, else whatever is pending release, else everything sealed.
/// From an agent this is the old spelling of [`need`] for one name.
pub fn decrypt(name: Option<&str>, wait: Option<Duration>) -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = workspace::find(&cwd)?;
    let task = current_task(&ws, &cwd)?;
    decrypt_in(&ws, &task, name, wait)
}

/// Same as [`decrypt`], for an explicit workspace/task rather than cwd.
pub fn decrypt_in(ws: &Workspace, task: &Task, name: Option<&str>, wait: Option<Duration>) -> Result<()> {
    if !tty_available() {
        let Some(name) = name else {
            bail!(
                "no real terminal available (this looks like an agent's Bash tool) — \
                 say what you need, e.g.: tenx secrets need STRIPE_KEY --why \"...\""
            );
        };
        return need_in(ws, task, &[name.to_string()], None, wait);
    }
    if let Some(n) = name {
        check_name(n)?;
    }
    if !bundle_path(task).exists() && find_sops_covered_files(task).is_empty() {
        bail!(
            "no sealed secrets for task '{}' — run: tenx secrets set <NAME>, or tenx secrets encrypt {} <file>",
            task.name,
            task.name
        );
    }
    reroute(task)?;
    let pending = Queue::Release.names(task);
    let want: Option<Vec<String>> = match name {
        Some(n) if !is_sealed(task, n) => {
            eprintln!("nothing sealed under '{n}' — supply a value with: tenx secrets set {n}");
            if pending.is_empty() {
                return Ok(());
            }
            Some(pending)
        }
        Some(n) => Some(pending.into_iter().filter(|p| p != n).chain([n.to_string()]).collect()),
        None if !pending.is_empty() => Some(pending),
        None => None,
    };
    let released = with_unlocked(ws, false, |id| release(id, task, want.as_deref()))?;
    Queue::Release.remove(task, &released)
}

// ── Answering ───────────────────────────────────────────────────────────────

/// Answer everything pending for the current task (resolved from cwd) in one
/// sitting — see module docs, *Answering*. With `hold`, wait for Enter
/// before returning: the column runs this in a popup that closes when it
/// exits, and the outcome should be readable first.
pub fn fulfill(hold: bool) -> Result<()> {
    let result = (|| {
        let cwd = env::current_dir()?;
        let ws = workspace::find(&cwd)?;
        let task = current_task(&ws, &cwd)?;
        fulfill_in(&ws, &task)
    })();
    if hold {
        if let Err(e) = &result {
            eprintln!("\ntenx: {e}");
        }
        let _ = read_tty_line("\npress Enter to close");
    }
    result
}

/// Same as [`fulfill`], for an explicit workspace/task — what the column's
/// unlock (`tui::column::run_unlock`) runs. Needs a real terminal: every
/// prompt reads `/dev/tty`.
pub fn fulfill_in(ws: &Workspace, task: &Task) -> Result<()> {
    reroute(task)?;
    let rows: Vec<(String, Queue)> = Queue::Release
        .names(task)
        .into_iter()
        .map(|n| (n, Queue::Release))
        .chain(Queue::Value.names(task).into_iter().map(|n| (n, Queue::Value)))
        .collect();
    if rows.is_empty() {
        eprintln!("nothing pending for task '{}'", task.name);
        return Ok(());
    }

    let why = workspace::secrets_why(&task.path);
    let width = rows.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    eprintln!("{}\n", paint(&palette::BRIGHT, &format!("secrets requested for '{}'", task.display_name)));
    for (name, queue) in &rows {
        let label = paint(&palette::ACCENT, &format!("{name:<width$}"));
        match queue {
            Queue::Release => eprintln!("  {label}  release    {}", paint(&palette::MUTED, &sealed_in(task, name))),
            Queue::Value => eprintln!("  {label}  new value"),
        }
        if let Some((_, w)) = why.iter().find(|(n, w)| n == name && !w.is_empty()) {
            eprintln!("  {:<width$}  {}", "", paint(&palette::MUTED, &format!("why: {w}")));
        }
    }
    eprintln!();

    let answer = read_tty_line("grant all? [Y]es · [n]o, deny all · [p]ick")?.trim().to_lowercase();
    let granted: Vec<(String, Queue)> = match answer.as_str() {
        "" | "y" | "yes" => rows.clone(),
        "n" | "no" => Vec::new(),
        "p" | "pick" => {
            let mut picked = Vec::new();
            for row in &rows {
                let a = read_tty_line(&format!("  grant '{}'? [Y/n]", row.0))?.trim().to_lowercase();
                if matches!(a.as_str(), "" | "y" | "yes") {
                    picked.push(row.clone());
                }
            }
            picked
        }
        other => bail!("didn't understand {other:?} — nothing was changed"),
    };

    let denied: Vec<String> = rows.iter().filter(|r| !granted.contains(r)).map(|(n, _)| n.clone()).collect();
    if !denied.is_empty() {
        let note = read_tty_line("note for the agent (optional)")?;
        deny_in(task, &denied, Some(note.trim()))?;
    }

    let mut values = Vec::new();
    for (name, _) in granted.iter().filter(|(_, q)| *q == Queue::Value) {
        let value = read_masked_line(&format!("value for '{name}' (empty to skip)"))?;
        if value.is_empty() {
            eprintln!("  skipped '{name}' — still pending");
        } else {
            values.push((name.clone(), value));
        }
    }
    let to_release: Vec<String> =
        granted.iter().filter(|(_, q)| *q == Queue::Release).map(|(n, _)| n.clone()).collect();
    if values.is_empty() && to_release.is_empty() {
        return Ok(());
    }
    with_unlocked(ws, !values.is_empty(), |id| seal_and_release(id, task, &values, &to_release))
}

/// Refuse pending requests for the current task (resolved from cwd), with
/// an optional note the waiting agent is shown.
pub fn deny(names: &[String], note: Option<&str>) -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = workspace::find(&cwd)?;
    let task = current_task(&ws, &cwd)?;
    deny_in(&task, names, note)
}

/// Same as [`deny`], for an explicit task. The denial is written before the
/// name leaves its queue — the commit point — so a waiter can never mistake
/// it for a withdrawal.
pub fn deny_in(task: &Task, names: &[String], note: Option<&str>) -> Result<()> {
    let pending: Vec<String> = names.iter().filter(|n| queued_on(task, n).is_some()).cloned().collect();
    for n in names.iter().filter(|n| !pending.contains(n)) {
        eprintln!("nothing pending named '{n}' for task '{}'", task.name);
    }
    if pending.is_empty() {
        return Ok(());
    }
    for n in &pending {
        set_note(task, workspace::SECRETS_DENIED_FILE, n, Some(note.unwrap_or("")))?;
    }
    let set: HashSet<String> = pending.iter().cloned().collect();
    Queue::Release.remove(task, &set)?;
    Queue::Value.remove(task, &set)?;
    eprintln!("denied {} for task '{}'", pending.join(", "), task.name);
    Ok(())
}

/// Withdraw pending requests for the current task (resolved from cwd): one
/// `name` from whichever queue holds it, or everything when `name` is
/// `None`. Touches nothing but the queue files — no identity, no bundle, no
/// plaintext — so it is safe to run from anywhere, an agent's Bash tool
/// included. A waiter blocked on a withdrawn name exits saying so.
pub fn cancel(name: Option<&str>) -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = workspace::find(&cwd)?;
    let task = current_task(&ws, &cwd)?;
    cancel_in(&task, name)
}

/// Same as [`cancel`], for an explicit task — used by the column's `:cancel`.
pub fn cancel_in(task: &Task, name: Option<&str>) -> Result<()> {
    let pick = |q: Queue| -> Vec<String> { q.names(task).into_iter().filter(|x| name.is_none_or(|n| x == n)).collect() };
    let (drop_release, drop_value) = (pick(Queue::Release), pick(Queue::Value));
    if drop_release.is_empty() && drop_value.is_empty() {
        match name {
            Some(n) => eprintln!("nothing pending named '{n}' for task '{}'", task.name),
            None => eprintln!("nothing pending for task '{}'", task.name),
        }
        return Ok(());
    }
    Queue::Release.remove(task, &drop_release.iter().cloned().collect())?;
    Queue::Value.remove(task, &drop_value.iter().cloned().collect())?;
    let withdrawn: Vec<String> =
        drop_release.into_iter().chain(drop_value.into_iter().map(|n| format!("{n} (needs value)"))).collect();
    eprintln!("withdrew {} for task '{}'", withdrawn.join(", "), task.name);
    Ok(())
}

/// With an unlocked identity: seal `values` into the bundle, then release
/// them together with `release_names` — the whole sitting on one unwrap. A
/// sealed value's name joins the release queue *before* it leaves the value
/// queue, so a waiter never sees it in neither queue before its plaintext is
/// written.
fn seal_and_release(id: &Path, task: &Task, values: &[(String, String)], release_names: &[String]) -> Result<()> {
    seal(id, task, values)?;
    for (name, _) in values {
        Queue::Release.push(task, name)?;
        Queue::Value.remove(task, &HashSet::from([name.clone()]))?;
    }
    let names: Vec<String> = release_names.iter().cloned().chain(values.iter().map(|(n, _)| n.clone())).collect();
    let released = release(id, task, Some(&names))?;
    Queue::Release.remove(task, &released)?;
    let missed: Vec<&str> = names.iter().filter(|n| !released.contains(*n)).map(String::as_str).collect();
    if !missed.is_empty() {
        bail!("couldn't release {} — still pending", missed.join(", "));
    }
    Ok(())
}

/// With an unlocked identity: `sops set` each of `values` into the task's
/// bundle, creating the bundle first if this is its first value.
fn seal(id: &Path, task: &Task, values: &[(String, String)]) -> Result<()> {
    if values.is_empty() {
        return Ok(());
    }
    let bundle = bundle_path(task);
    ensure_bundle_exists(id, &bundle)?;
    for (name, value) in values {
        run_sops_set(id, &bundle, name, value)?;
        eprintln!("✓ sealed '{name}' into {}", bundle.display());
    }
    Ok(())
}

/// With an unlocked identity: release `want` (every sealed name when `None`).
/// Bundle keys are merged into `.secrets.env` — only the ones asked for, next
/// to whatever earlier releases granted; an adopted file is released whole
/// when any wanted name matches it (`file_matches_request`). Returns the
/// names satisfied.
fn release(id: &Path, task: &Task, want: Option<&[String]>) -> Result<HashSet<String>> {
    let mut satisfied = HashSet::new();
    let bundle = bundle_path(task);
    if bundle.exists() {
        let sealed = rules::sealed_keys(&std::fs::read_to_string(&bundle).unwrap_or_default());
        let pick: Vec<String> = match want {
            Some(w) => w.iter().filter(|n| sealed.contains(n)).cloned().collect(),
            None => sealed,
        };
        if !pick.is_empty() {
            let plaintext = run_sops_decrypt_to_memory(id, &bundle)?;
            let out = released_path(task);
            let existing = std::fs::read_to_string(&out).unwrap_or_default();
            write_private(&out, &rules::merge_dotenv(&existing, &plaintext, Some(&pick)))?;
            eprintln!("✓ released {} → {}", pick.join(", "), out.display());
            satisfied.extend(pick);
        }
    }
    for ciphertext in find_sops_covered_files(task) {
        let matched: Vec<String> =
            want.unwrap_or_default().iter().filter(|n| file_matches_request(&ciphertext, n)).cloned().collect();
        if want.is_some() && matched.is_empty() {
            continue;
        }
        release_adopted(id, task, &ciphertext)?;
        satisfied.extend(matched);
    }
    Ok(satisfied)
}

/// Move release requests nothing sealed can satisfy to the value queue — a
/// name queued without routing (an older tenx), or whose sealed file has
/// since gone. Pushed before removed, same commit-point rule as
/// `seal_and_release`.
fn reroute(task: &Task) -> Result<()> {
    let stale: HashSet<String> = Queue::Release.names(task).into_iter().filter(|n| !is_sealed(task, n)).collect();
    for n in &stale {
        Queue::Value.push(task, n)?;
    }
    Queue::Release.remove(task, &stale)
}

/// Resolve the identity and unwrap it once for everything `f` does — the
/// single passphrase prompt of a sitting. With `may_create` (the sitting
/// seals a new value, which any identity can do) and nothing to resolve, a
/// human is offered one on the spot rather than sent off to `tenx secrets
/// init` with what they just typed thrown away. Not for a release alone: a
/// fresh identity can't open anything sealed before it existed.
fn with_unlocked<T>(ws: &Workspace, may_create: bool, f: impl FnOnce(&Path) -> Result<T>) -> Result<T> {
    let identity = match resolve_identity_path(ws) {
        Ok(path) => path,
        Err(e) if may_create && ws.config.age_identity.is_none() && tty_available() => {
            let target = workspace::home_dir()?.join(".config").join("age").join("keys.txt");
            eprintln!("\nno age identity yet — tenx seals secrets to one, kept at {}", target.display());
            let answer = read_tty_line("create it now, protected by a passphrase you choose? [Y/n]")?;
            if !matches!(answer.trim().to_lowercase().as_str(), "" | "y" | "yes") {
                return Err(e);
            }
            generate_identity(&target)?;
            eprintln!("✓ created {} — now unlock it once more to seal:", target.display());
            target
        }
        Err(e) => return Err(e),
    };
    with_plain_identity(&identity, f)
}

// ── Waiting ─────────────────────────────────────────────────────────────────

/// How long the no-terminal path waits by default. Deliberately under the
/// two minutes Claude Code's Bash tool allows a command before killing it
/// (which would be a noisy failure instead of this clean "still pending,
/// re-run" exit); the `/tenx` skill tells the agent to run it in the
/// background with a longer `--timeout` instead.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(100);

/// Poll interval while waiting — the same order as the watcher's 2 s tick;
/// a human typing a passphrase is the slow part, not this.
const WAIT_POLL: Duration = Duration::from_secs(1);

/// The instant a request is considered made, for `wait_outcome`'s "written at
/// or after the request" test. Padded back by a couple of seconds so a
/// filesystem with coarse (whole-second) mtimes can't round a fulfilment
/// that landed in the same second to *before* the request and make it look
/// like a cancellation; a genuine cancellation can't be confused by this,
/// because it writes no output at all.
fn request_instant() -> SystemTime {
    SystemTime::now() - Duration::from_secs(2)
}

/// Block until every one of `names` has left both queues — or `timeout`
/// passes — reporting each outcome as it lands. See the module docs
/// (*Waiting*); the decision per name is `tenx_core::secrets::wait_outcome`,
/// the exit code `tenx_core::secrets::exit_code`.
fn wait_for_human(task: &Task, names: &[String], requested_at: SystemTime, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    eprintln!(
        "waiting up to {} for someone to answer — withdraw with: tenx secrets cancel <NAME>",
        fmt_wait(timeout)
    );
    let mut outcomes = vec![WaitOutcome::Pending; names.len()];
    loop {
        let denied = notes(task, workspace::SECRETS_DENIED_FILE);
        let outputs = release_output_mtimes(task);
        for (name, outcome) in names.iter().zip(outcomes.iter_mut()) {
            if *outcome != WaitOutcome::Pending {
                continue;
            }
            let note = denied.iter().find(|(n, _)| n == name).map(|(_, t)| t.as_str());
            *outcome = rules::wait_outcome(queued_on(task, name).is_some(), note.is_some(), &outputs, requested_at);
            report(task, name, outcome, note);
        }
        if !outcomes.contains(&WaitOutcome::Pending) || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(WAIT_POLL);
    }
    let still: Vec<&str> = names
        .iter()
        .zip(&outcomes)
        .filter(|(_, o)| **o == WaitOutcome::Pending)
        .map(|(n, _)| n.as_str())
        .collect();
    let code = rules::exit_code(&outcomes);
    let message = match code {
        0 => return Ok(()),
        rules::EXIT_DENIED => "denied — don't ask for it again without saying why you need it".to_string(),
        rules::EXIT_WITHDRAWN => "the request was withdrawn before anyone answered".to_string(),
        _ => format!(
            "{} still pending after {} — it stays queued; re-run the same command to keep waiting, \
             or withdraw it with: tenx secrets cancel <NAME>",
            still.join(", "),
            fmt_wait(timeout)
        ),
    };
    Err(Exit { code, message }.into())
}

/// One line per settled name, as it settles.
fn report(task: &Task, name: &str, outcome: &WaitOutcome, note: Option<&str>) {
    match outcome {
        WaitOutcome::Pending => {}
        WaitOutcome::Fulfilled => match released_at(task, name) {
            Some(at) => eprintln!("✓ '{name}' granted → {}", at.display()),
            None => eprintln!("✓ '{name}' granted"),
        },
        WaitOutcome::Denied => match note.filter(|n| !n.is_empty()) {
            Some(note) => eprintln!("✗ '{name}' denied: {note}"),
            None => eprintln!("✗ '{name}' denied"),
        },
        WaitOutcome::Cancelled => eprintln!("✗ '{name}' withdrawn before anyone answered"),
    }
}

/// Modification times of every file a release writes — `wait_outcome`'s
/// evidence. Files that don't exist contribute nothing.
fn release_output_mtimes(task: &Task) -> Vec<SystemTime> {
    let mut paths = vec![released_path(task)];
    if let Ok(entries) = std::fs::read_dir(task.path.join(".secrets-adopted")) {
        paths.extend(entries.flatten().map(|e| e.path()));
    }
    paths.iter().filter_map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok()).collect()
}

/// "100s" / "9m" — exact, unlike `tenx_core::time::format_duration`, which
/// buckets to the nearest unit for the column's age column and would call
/// the default wait "1m".
fn fmt_wait(d: Duration) -> String {
    let secs = d.as_secs();
    if secs > 0 && secs.is_multiple_of(60) { format!("{}m", secs / 60) } else { format!("{secs}s") }
}

/// Whether a real controlling terminal is reachable right now — the same
/// thing `age`'s own passphrase prompt checks (it reads `/dev/tty` directly,
/// not stdin, specifically so it still works when stdin/stdout are
/// redirected). An agent's Bash tool child process normally has none; a
/// human's real shell, or a pane the column just spawned, always does.
fn tty_available() -> bool {
    std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty").is_ok()
}

// ── What is where (no identity needed) ──────────────────────────────────────

/// Where `name` has already been released to, if anywhere: a key of
/// `.secrets.env`, or an adopted file it matches whose plaintext is in place.
fn released_at(task: &Task, name: &str) -> Option<PathBuf> {
    let env_file = released_path(task);
    let released = std::fs::read_to_string(&env_file).unwrap_or_default();
    if rules::dotenv_keys(&released).iter().any(|k| k == name) {
        return Some(env_file);
    }
    find_sops_covered_files(task)
        .into_iter()
        .filter(|f| file_matches_request(f, name))
        .map(|f| strip_enc_suffix(&f))
        .find(|p| p.exists())
}

/// Whether something sealed can satisfy `name` — a key of the task's
/// bundle, or an adopted file it matches.
fn is_sealed(task: &Task, name: &str) -> bool {
    bundle_keys(task).iter().any(|k| k == name) || find_sops_covered_files(task).iter().any(|f| file_matches_request(f, name))
}

/// Human-readable "where would this come from", for the answering sheet.
fn sealed_in(task: &Task, name: &str) -> String {
    let mut from: Vec<String> = Vec::new();
    if bundle_keys(task).iter().any(|k| k == name) {
        from.push("task bundle".into());
    }
    for f in find_sops_covered_files(task).iter().filter(|f| file_matches_request(f, name)) {
        from.push(f.strip_prefix(&task.path).unwrap_or(f).display().to_string());
    }
    from.join(", ")
}

fn bundle_keys(task: &Task) -> Vec<String> {
    rules::sealed_keys(&std::fs::read_to_string(bundle_path(task)).unwrap_or_default())
}

/// The task's released dotenv file — exactly the bundle names granted so far.
fn released_path(task: &Task) -> PathBuf {
    task.path.join(".secrets.env")
}

fn check_name(name: &str) -> Result<()> {
    if !rules::valid_name(name) {
        bail!("invalid secret name: {name:?} — a key like STRIPE_KEY, or a file fragment like staging");
    }
    Ok(())
}

// ── Bundle and identity ─────────────────────────────────────────────────────

/// Path of the task's own sealed bundle — a `sops`-encrypted dotenv document.
/// `.env` on the end isn't cosmetic: `sops` auto-detects format from the
/// filename extension when `--input-type`/`--output-type` aren't given, and
/// nothing else in this module passes those explicitly (matching how
/// `run_sops_decrypt` already relies on it for adopted files).
fn bundle_path(task: &Task) -> PathBuf {
    task.path.join(".secrets.enc.env")
}

/// `sops set` needs an existing document to edit — it has no "create if
/// missing" mode of its own (confirmed against the real binary: it rejects
/// `--age` on `set` outright, there's no way to hand it recipients for a
/// document that doesn't exist yet). So the first value ever sealed for a
/// task bootstraps an empty encrypted document to the unlocked identity's
/// own recipients — after that, every `set` is a genuine in-place edit.
fn ensure_bundle_exists(plain_identity: &Path, bundle: &Path) -> Result<()> {
    if bundle.exists() {
        return Ok(());
    }
    let out = Command::new("age-keygen").arg("-y").arg(plain_identity).output().context("run age-keygen -y")?;
    if !out.status.success() {
        bail!("age-keygen -y failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let recipients: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    let tmp = std::env::temp_dir().join(format!("tenx-bootstrap-{}.env", std::process::id()));
    std::fs::write(&tmp, "").with_context(|| format!("write {}", tmp.display()))?;
    let result = sops_encrypt(&recipients.join(","), &tmp, bundle);
    let _ = std::fs::remove_file(&tmp);
    result
}

/// Encrypt `plaintext` fresh to `recipient`'s public key via `sops`, writing
/// to `out`. Needs only the public key — no identity, no passphrase. Used by
/// `encrypt` (a new bundle) and `ensure_bundle_exists` (bootstrapping an
/// empty one for `set`'s first use) — never for an edit to a document that
/// already has content, which is what `set` itself is for.
fn sops_encrypt(recipient: &str, plaintext: &Path, out: &Path) -> Result<()> {
    let output = Command::new("sops")
        .arg("--encrypt")
        .arg("--age")
        .arg(recipient)
        .arg("--output")
        .arg(out)
        .arg(plaintext)
        .output()
        .context("run sops --encrypt")?;
    if !output.status.success() {
        bail!("sops encrypt failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

/// Set `name` = `value` in `bundle` via `sops set --value-stdin`, editing the
/// existing document in place, with an already-unwrapped identity (see
/// `with_plain_identity`). The value goes through `sops`'s own stdin channel
/// (`--value-stdin`, "avoids leaking secrets in process listings" per its
/// own `--help`) rather than argv, same reasoning as everywhere else here.
fn run_sops_set(identity_file: &Path, bundle: &Path, name: &str, value: &str) -> Result<()> {
    // sops's `set` path expression addresses a top-level key as `["key"]`;
    // the value must be JSON-encoded too (confirmed against the real
    // binary — even via --value-stdin, a bare string is rejected as "not
    // valid JSON"). serde_json handles quoting/escaping for both correctly.
    let path_expr = format!("[{}]", serde_json::to_string(name)?);
    let json_value = serde_json::to_string(value)?;

    let mut child = Command::new("sops")
        .env("SOPS_AGE_KEY_FILE", identity_file)
        .arg("set")
        .arg("--value-stdin")
        .arg(bundle)
        .arg(&path_expr)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn sops set")?;
    child
        .stdin
        .take()
        .context("open sops set stdin")?
        .write_all(json_value.as_bytes())
        .context("write value to sops set")?;
    let out = child.wait_with_output().context("wait on sops set")?;
    if !out.status.success() {
        bail!("sops set failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// How many times a sitting asks for the passphrase before giving up.
const PASSPHRASE_ATTEMPTS: u32 = 3;

/// Run `f` with a plain (non-passphrase-protected) identity file `sops` can
/// consume via `SOPS_AGE_KEY_FILE` — `identity` itself if it's already one,
/// or a one-time-use plain copy unwrapped from it (passphrase prompted on
/// the real terminal) if it's passphrase-protected. The temp copy lives in a
/// mode-700 temp directory, used for exactly this one call, and removed
/// immediately after, success or not — never longer-lived than this call,
/// never the task folder. Shared by `sops_decrypt` and `sops_set`, the two
/// operations that actually need decrypt access (`sops_encrypt` never does).
fn with_plain_identity<T>(identity: &Path, f: impl FnOnce(&Path) -> Result<T>) -> Result<T> {
    if !is_age_encrypted(identity)? {
        return f(identity);
    }

    let tmp_dir = std::env::temp_dir().join(format!("tenx-sops-{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir).with_context(|| format!("create {}", tmp_dir.display()))?;
    set_dir_permissions_700(&tmp_dir)?;
    let tmp_identity = tmp_dir.join("identity");

    let result = (|| -> Result<T> {
        // A mistyped passphrase gets another go (as `sudo` gives one) rather
        // than ending the sitting — everything typed before this point, the
        // values included, would be lost with it.
        for attempt in 1..=PASSPHRASE_ATTEMPTS {
            let status = Command::new("age")
                .arg("-d")
                .arg("-o")
                .arg(&tmp_identity)
                .arg(identity)
                .stdin(Stdio::inherit()) // passphrase prompt reaches the real terminal
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .status()
                .context("run age -d (identity)")?;
            if status.success() {
                return f(&tmp_identity);
            }
            if attempt == PASSPHRASE_ATTEMPTS || !tty_available() {
                break;
            }
            eprintln!("try again ({} of {PASSPHRASE_ATTEMPTS}):", attempt + 1);
        }
        bail!("failed to decrypt the identity (wrong passphrase?) — nothing was changed")
    })();

    let _ = std::fs::remove_dir_all(&tmp_dir); // shred immediately, success or not
    result
}

// ── Sops adoption ───────────────────────────────────────────────────────────

/// Whether a request name refers to this sops-covered file: a loose,
/// case-insensitive substring of its filename (`"staging"` matches
/// `secrets.staging.enc.env`; the exact filename always matches itself), or
/// exactly one of the keys sealed in it — readable without the identity,
/// see `tenx_core::secrets::sealed_keys` — so an agent can ask for
/// `DATABASE_URL` without knowing which file holds it.
fn file_matches_request(file: &Path, requested: &str) -> bool {
    let name = file.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
    name.contains(&requested.to_lowercase())
        || rules::sealed_keys(&std::fs::read_to_string(file).unwrap_or_default()).iter().any(|k| k == requested)
}

/// Files inside this task's repo worktrees that an existing `.sops.yaml`
/// covers — detected by the de facto sops naming convention (`name.enc.ext`,
/// e.g. `secrets.staging.enc.env`) in any repo that has a `.sops.yaml` at its
/// root. Deliberately not parsing the config's own `creation_rules` regexes,
/// which would need a YAML parser this project doesn't otherwise pull in —
/// the naming convention is what every sops project actually uses in
/// practice, `.sops.yaml` presence is just the "this repo really uses sops"
/// gate. Scanned shallowly (repo root + one level), which is where these
/// files live in every real project seen so far; a deeply nested one would
/// need a deliberately wider scan, not a silent one.
fn find_sops_covered_files(task: &Task) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for repo in &task.repos {
        let repo_dir = task.path.join(repo);
        if repo_dir.join(".sops.yaml").exists() {
            scan_for_enc_files(&repo_dir, 1, &mut found);
        }
    }
    found
}

fn scan_for_enc_files(dir: &Path, depth: u8, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if depth > 0 && !matches!(name.as_ref(), "node_modules" | ".git" | "dist" | "build" | "target") {
                scan_for_enc_files(&path, depth - 1, out);
            }
            continue;
        }
        if name.contains(".enc.") {
            out.push(path);
        }
    }
}

/// `secrets.staging.enc.env` → `secrets.staging.env` — the plaintext sibling
/// name every sops project already expects. Since the real plaintext moved
/// to `.secrets-adopted/` (see `adopted_secret_storage_path`), this is now
/// where the *symlink* to it goes, not real content — kept at this exact
/// path so the project's own tooling still finds it exactly where it always
/// expected to, unaware anything changed underneath.
fn strip_enc_suffix(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(name.replacen(".enc.", ".", 1))
}

/// Real, git-safe storage location for an adopted secret's plaintext, given
/// its ciphertext path — never inside the repo worktree, so no git operation
/// there can ever stage it, matching the same structural guarantee our own
/// `.secrets.enc.env` bundle already has (`tasks/<slug>/` is never itself a
/// git repo). Named after the ciphertext's path relative to the task
/// directory, with `/` flattened to `__`, so two repos with the same
/// relative sops filename (e.g. both named `secrets.staging.enc.env`) can't
/// collide in this single flat directory.
fn adopted_secret_storage_path(task: &Task, ciphertext: &Path) -> PathBuf {
    let rel = ciphertext.strip_prefix(&task.path).unwrap_or(ciphertext);
    let flat = rel.to_string_lossy().replace('/', "__");
    task.path.join(".secrets-adopted").join(strip_enc_suffix(Path::new(&flat)))
}

/// Relative symlink target from `strip_enc_suffix(ciphertext)`'s location to
/// `storage_path` — relative, not absolute, so an accidentally-committed
/// symlink (the worst case now — see the loop that calls this) leaks a
/// relative path fragment at most, never this machine's home directory
/// layout. Depth is derived from how many directory levels under the task
/// directory the ciphertext (and therefore the symlink, which sits at the
/// same depth) actually is — `<repo>/secrets.enc.env` needs one `../`,
/// `<repo>/config/secrets.enc.env` needs two, and so on.
fn adopted_symlink_target(task: &Task, ciphertext: &Path, storage_path: &Path) -> PathBuf {
    let rel = ciphertext.strip_prefix(&task.path).unwrap_or(ciphertext);
    let depth = rel.components().count().saturating_sub(1);
    let mut target = PathBuf::new();
    for _ in 0..depth {
        target.push("..");
    }
    let storage_rel = storage_path.strip_prefix(&task.path).unwrap_or(storage_path);
    target.push(storage_rel);
    target
}

/// With an unlocked identity: decrypt one adopted file. The plaintext never
/// lands inside the worktree — it goes to `.secrets-adopted/` directly
/// under the task directory (never a git repo, the same structural guarantee
/// the task's own bundle has), and a relative symlink is placed at the
/// conventional sibling name (`secrets.staging.enc.env` → the worktree gets
/// `secrets.staging.env`, pointing back at the real file) so the project's
/// own tooling finds it exactly where it expects to. A wrong or missing
/// `.gitignore` pattern in that project can then only ever stage a symlink
/// (a relative path, no secret bytes), never the secret.
fn release_adopted(id: &Path, task: &Task, ciphertext: &Path) -> Result<()> {
    let plaintext_out = strip_enc_suffix(ciphertext);
    let storage_path = adopted_secret_storage_path(task, ciphertext);
    if let Some(parent) = storage_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    run_sops_decrypt(id, ciphertext, &storage_path)?;
    set_permissions_600(&storage_path)?;

    // Always recreate the symlink fresh — whatever was previously at this
    // path (a stale symlink from an earlier release, or a real plaintext file
    // left over from before adoption stored outside the worktree) is exactly
    // what a fresh release is supposed to replace.
    let _ = std::fs::remove_file(&plaintext_out);
    let target = adopted_symlink_target(task, ciphertext, &storage_path);
    std::os::unix::fs::symlink(&target, &plaintext_out)
        .with_context(|| format!("symlink {} -> {}", plaintext_out.display(), target.display()))?;
    eprintln!(
        "✓ released (sops) → {} (stored outside the repo at {}, symlinked in)",
        plaintext_out.display(),
        storage_path.display()
    );
    Ok(())
}

/// Decrypt one sops-covered file to `plaintext_out` with an already
/// unwrapped identity. `sops` resolves its decryption key via
/// `SOPS_AGE_KEY_FILE`, which must be a real file path — hence
/// `with_plain_identity`'s short-lived copy for a passphrase-protected one.
fn run_sops_decrypt(identity_file: &Path, ciphertext: &Path, plaintext_out: &Path) -> Result<()> {
    let out = Command::new("sops")
        .env("SOPS_AGE_KEY_FILE", identity_file)
        .arg("-d")
        .arg("--output")
        .arg(plaintext_out)
        .arg(ciphertext)
        .output()
        .context("run sops -d")?;
    if !out.status.success() {
        bail!("sops decrypt failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Decrypt `ciphertext` into memory, for a per-name release. `sops`'s stdout
/// is captured by this process, never inherited, so the plaintext reaches
/// neither our stdout nor a file until `release` writes the chosen keys.
fn run_sops_decrypt_to_memory(identity_file: &Path, ciphertext: &Path) -> Result<String> {
    let out = Command::new("sops")
        .env("SOPS_AGE_KEY_FILE", identity_file)
        .arg("-d")
        .arg(ciphertext)
        .stdin(Stdio::null())
        .output()
        .context("run sops -d")?;
    if !out.status.success() {
        bail!("sops decrypt failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    String::from_utf8(out.stdout).context("decrypted bundle isn't UTF-8")
}

fn set_dir_permissions_700(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

/// Metadata-only overview across every task in the workspace: whether it has
/// a sealed bundle, whether it's currently unlocked, and what's pending.
/// Never reads or prints a secret value — only presence/absence of files and
/// the (informational) names collected by `need`.
pub fn status() -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = workspace::find(&cwd)?;
    let tasks = ws.tasks()?;

    println!("{:<20} {:<8} {:<10} PENDING", "TASK", "SEALED", "UNLOCKED");
    println!("{}", "-".repeat(70));
    let mut any = false;
    for task in &tasks {
        let sops_files = find_sops_covered_files(task);
        let sealed =
            bundle_path(task).exists() || !sops_files.is_empty();
        let unlocked = task.path.join(".secrets.env").exists()
            || sops_files.iter().any(|f| strip_enc_suffix(f).exists());
        let pending = Queue::Release.names(task);
        let pending_set = Queue::Value.names(task);
        if !sealed && !unlocked && pending.is_empty() && pending_set.is_empty() {
            continue;
        }
        any = true;
        // Two different kinds of pending, shown together but distinguishable
        // — "release X" (already sealed, waiting on a human to decrypt) vs
        // "X needs value" (doesn't exist yet, waiting on a human to supply
        // one via `set`). Same column rather than a new one, to keep this
        // table from growing sideways for what's still a rare state.
        let combined: Vec<String> = pending
            .iter()
            .cloned()
            .chain(pending_set.iter().map(|n| format!("{n} (needs value)")))
            .collect();
        println!(
            "{:<20} {:<8} {:<10} {}",
            task.display_name,
            if sealed { "yes" } else { "no" },
            if unlocked { "yes" } else { "no" },
            combined.join(", "),
        );
    }
    if !any {
        println!("(no tasks have sealed secrets, unlocked secrets, or pending requests)");
    }
    Ok(())
}

// ── Identity resolution ─────────────────────────────────────────────────────

/// Resolve the age identity to use: an explicit per-workspace override first,
/// then the standard locations `sops`/`age` themselves already look at, so a
/// workspace picks up whatever's already on the machine with zero
/// tenx-specific setup. See `ARCHITECTURE.md` § Secrets, *Identity*.
pub(crate) fn resolve_identity_path(ws: &Workspace) -> Result<PathBuf> {
    if let Some(p) = ws.config.age_identity.as_deref().filter(|p| !p.is_empty()) {
        let path = PathBuf::from(workspace::expand_home(p));
        if path.exists() {
            return Ok(path);
        }
        bail!("workspace's configured age_identity does not exist: {}", path.display());
    }
    if let Ok(p) = env::var("SOPS_AGE_KEY_FILE") {
        let path = PathBuf::from(p);
        if !path.as_os_str().is_empty() && path.exists() {
            return Ok(path);
        }
    }
    let home = workspace::home_dir()?;
    let sops_default = home.join(".config").join("sops").join("age").join("keys.txt");
    if sops_default.exists() {
        return Ok(sops_default);
    }
    let age_default = home.join(".config").join("age").join("keys.txt");
    if age_default.exists() {
        return Ok(age_default);
    }
    bail!("no age identity found — run: tenx secrets init")
}

/// Whether `path` is itself age-ciphertext (i.e. a passphrase-protected
/// identity produced by `age -p`), vs. a plain identity file with a bare
/// `AGE-SECRET-KEY-1...` line. Age ciphertext always starts with this header.
fn is_age_encrypted(path: &Path) -> Result<bool> {
    let mut f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut buf = [0u8; 32];
    let n = f.read(&mut buf)?;
    Ok(buf[..n].starts_with(b"age-encryption.org/v1"))
}

/// The recipient (public key) string for `identity`, cached alongside it as
/// `<identity>.pub` — public keys aren't secret, so caching them in plaintext
/// costs nothing and means `seal` doesn't need the passphrase at all after the
/// first use. For a passphrase-protected identity with no cache yet (e.g. one
/// adopted from an existing project rather than generated by `tenx secrets
/// init`), deriving it requires decrypting once — a one-time cost.
fn resolve_recipient(identity: &Path) -> Result<String> {
    let pub_path = pub_sidecar(identity);
    if let Ok(s) = std::fs::read_to_string(&pub_path) {
        let s = s.trim();
        if !s.is_empty() {
            return Ok(s.to_string());
        }
    }

    let recipient = if is_age_encrypted(identity)? {
        eprintln!("deriving the public key from a passphrase-protected identity (one-time — cached afterward):");
        let mut stage1 = Command::new("age")
            .arg("-d")
            .arg(identity)
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("run age -d")?;
        let mut plaintext = Vec::new();
        stage1
            .stdout
            .take()
            .context("capture stdout")?
            .read_to_end(&mut plaintext)
            .context("read decrypted identity")?;
        if !stage1.wait().context("wait on age -d")?.success() {
            bail!("failed to decrypt the identity (wrong passphrase?)");
        }
        let recipient = pubkey_from_identity_bytes(&plaintext)?;
        // Zero the plaintext identity buffer before it's dropped.
        for b in plaintext.iter_mut() {
            *b = 0;
        }
        recipient
    } else {
        let out = Command::new("age-keygen")
            .arg("-y")
            .arg(identity)
            .output()
            .context("run age-keygen -y")?;
        if !out.status.success() {
            bail!("age-keygen -y failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    let _ = std::fs::write(&pub_path, format!("{recipient}\n"));
    Ok(recipient)
}

fn pub_sidecar(identity: &Path) -> PathBuf {
    let mut s = identity.as_os_str().to_owned();
    s.push(".pub");
    PathBuf::from(s)
}

/// Feed plaintext identity bytes to `age-keygen -y -` to get its public key,
/// without ever writing them to a temp file.
fn pubkey_from_identity_bytes(identity_plaintext: &[u8]) -> Result<String> {
    let mut child = Command::new("age-keygen")
        .arg("-y")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn age-keygen -y")?;
    child
        .stdin
        .take()
        .context("open age-keygen stdin")?
        .write_all(identity_plaintext)
        .context("write identity to age-keygen")?;
    let out = child.wait_with_output().context("wait on age-keygen -y")?;
    if !out.status.success() {
        bail!("age-keygen -y failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Generate a fresh identity at `target`, passphrase-protected via `age -p` —
/// this passphrase is the entire "confirm every time" gate (`age` has no
/// daemon and no cache, so there's nothing to layer `sudo` on top of). The
/// unencrypted intermediate is written to a sibling `.tmp` file only
/// because `age-keygen`/`age -p` are separate processes that need a real file
/// to hand off through, and it's best-effort removed immediately after.
fn generate_identity(target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let tmp = target.with_extension("tmp");

    let out = Command::new("age-keygen").arg("-o").arg(&tmp).output().context("run age-keygen")?;
    if !out.status.success() {
        bail!("age-keygen failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }

    eprintln!("choose a passphrase to protect the identity — you'll type this every time secrets are unlocked:");
    let status = Command::new("age")
        .args(["-p", "-o"])
        .arg(target)
        .arg(&tmp)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    // Cache the public key before removing the plaintext copy, so `seal`
    // never needs the passphrase for an identity `tenx secrets init` created
    // — but only once `age -p` actually succeeded. Doing this unconditionally
    // used to leave an orphaned `.pub` sidecar (a cached recipient with no
    // corresponding identity file) whenever the passphrase step failed.
    let succeeded = status.as_ref().is_ok_and(|s| s.success());
    if succeeded
        && let Ok(recipient) = pubkey_from_identity_bytes(&std::fs::read(&tmp).unwrap_or_default())
    {
        let _ = std::fs::write(pub_sidecar(target), format!("{recipient}\n"));
    }

    let _ = std::fs::remove_file(&tmp); // best-effort shred of the unencrypted copy
    if !succeeded {
        // Surface *why* if the process never ran at all (e.g. `age` missing);
        // a non-zero exit (passphrase mismatch, no tty, ^C) already printed
        // its own reason above via the inherited stderr.
        if let Err(e) = status {
            return Err(e).context("run age -p");
        }
        bail!("failed to passphrase-protect the generated identity");
    }
    // `age -p -o` writes with the umask's default (0644 on a typical Mac) —
    // lock it to owner-only. The passphrase is still the real gate, but
    // there's no reason to leave the ciphertext world-readable too.
    set_permissions_600(target)?;
    Ok(())
}

// ── Task resolution (cwd-based, no <task> argument) ─────────────────────────

/// Resolve which task `cwd` is inside, by walking up to find which direct
/// child of `tasks/` it's under — works from any depth inside a task
/// directory (its own root, a repo worktree, or a subdirectory of one), which
/// is exactly the cwd an agent's Bash tool or a task's shell pane always has.
fn current_task(ws: &Workspace, cwd: &Path) -> Result<Task> {
    let tasks_dir = ws.tasks_dir().canonicalize().unwrap_or_else(|_| ws.tasks_dir());
    let cwd = cwd.canonicalize().context("canonicalize cwd")?;
    let rel = cwd
        .strip_prefix(&tasks_dir)
        .ok()
        .filter(|r| !r.as_os_str().is_empty())
        .context("not inside a task directory (tenx secrets decrypt takes no <task> argument — cd into the task first)")?;
    let slug = rel
        .components()
        .next()
        .context("not inside a task directory")?
        .as_os_str()
        .to_string_lossy()
        .into_owned();
    ws.find_task(&slug)
}

// ── Queues and their side files ─────────────────────────────────────────────

/// The two request queues. Different fulfilment actions — release something
/// sealed vs. have a human type a value — so different files
/// (`workspace::SECRETS_PENDING_FILE` / `SECRETS_PENDING_SET_FILE`), which
/// the column, the watcher and `task_json` read too. Newline-separated names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Queue {
    Release,
    Value,
}

impl Queue {
    fn path(self, task: &Task) -> PathBuf {
        task.path.join(match self {
            Queue::Release => workspace::SECRETS_PENDING_FILE,
            Queue::Value => workspace::SECRETS_PENDING_SET_FILE,
        })
    }

    fn names(self, task: &Task) -> Vec<String> {
        match self {
            Queue::Release => workspace::secrets_pending(&task.path),
            Queue::Value => workspace::secrets_pending_set(&task.path),
        }
    }

    /// Append `name` unless it's already there.
    fn push(self, task: &Task, name: &str) -> Result<()> {
        let mut names = self.names(task);
        if names.iter().any(|n| n == name) {
            return Ok(());
        }
        names.push(name.to_string());
        self.write(task, &names)
    }

    /// Drop `names` from the queue, and the reasons of any name that is now
    /// on neither queue.
    fn remove(self, task: &Task, names: &HashSet<String>) -> Result<()> {
        if names.is_empty() {
            return Ok(());
        }
        let remaining: Vec<String> = self.names(task).into_iter().filter(|n| !names.contains(n)).collect();
        self.write(task, &remaining)?;
        let why = notes(task, workspace::SECRETS_WHY_FILE);
        if why.iter().any(|(n, _)| queued_on(task, n).is_none()) {
            let keep: Vec<(String, String)> = why.into_iter().filter(|(n, _)| queued_on(task, n).is_some()).collect();
            write_notes(task, workspace::SECRETS_WHY_FILE, &keep)?;
        }
        Ok(())
    }

    fn write(self, task: &Task, names: &[String]) -> Result<()> {
        let path = self.path(task);
        if names.is_empty() {
            let _ = std::fs::remove_file(&path);
            return Ok(());
        }
        std::fs::write(&path, names.join("\n") + "\n").with_context(|| format!("write {}", path.display()))
    }

    /// What a request on this queue is waiting for, in the agent's words.
    fn what(self) -> &'static str {
        match self {
            Queue::Release => "it's sealed; waiting for someone to release it",
            Queue::Value => "nothing is sealed under that name; waiting for someone to type a value",
        }
    }
}

/// Which queue `name` is on, if any.
fn queued_on(task: &Task, name: &str) -> Option<Queue> {
    [Queue::Release, Queue::Value].into_iter().find(|q| q.names(task).iter().any(|n| n == name))
}

/// A `NAME<TAB>text` side file of the task — the reasons, the denials.
fn notes(task: &Task, file: &str) -> Vec<(String, String)> {
    rules::parse_notes(&std::fs::read_to_string(task.path.join(file)).unwrap_or_default())
}

/// Set (`Some`) or drop (`None`) `name`'s entry in a side file.
fn set_note(task: &Task, file: &str, name: &str, text: Option<&str>) -> Result<()> {
    let current = notes(task, file);
    let next = match text {
        Some(text) => rules::upsert_note(current, name, text),
        None if current.iter().any(|(n, _)| n == name) => current.into_iter().filter(|(n, _)| n != name).collect(),
        None => return Ok(()),
    };
    write_notes(task, file, &next)
}

fn write_notes(task: &Task, file: &str, notes: &[(String, String)]) -> Result<()> {
    let path = task.path.join(file);
    if notes.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    std::fs::write(&path, rules::render_notes(notes)).with_context(|| format!("write {}", path.display()))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn set_permissions_600(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

fn prompt(label: &str) -> Result<String> {
    let mut stdout = io::stdout();
    write!(stdout, "{label}: ")?;
    stdout.flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

/// Prompt on the real terminal with input echo disabled, for a secret
/// value — the passphrase is masked by `age`'s own prompt, but the value is
/// tenx's prompt, so tenx masks it. Reads `/dev/tty` directly rather than
/// stdin, same reasoning as `tty_available`. Uses `libc` termios directly
/// rather than pulling in a crate for this one call.
///
/// Non-canonical, byte by byte, rather than a line read: canonical mode
/// caps a line at `MAX_CANON` (1024 bytes on macOS) and silently drops the
/// rest — a long JWT or PEM would be sealed truncated. `ISIG` is off too, so
/// Ctrl-C aborts here with the terminal restored instead of killing the
/// process with echo still off. Backspace and Ctrl-U edit as usual. What
/// was read goes through `tenx_core::secrets::clean_typed_value` — a paste
/// arrives wrapped in bracketed-paste markers whenever the terminal has that
/// mode on — and the human is shown its length and tail
/// (`describe_value`), since they can't see what they entered. Falls back
/// to a plain line read if `/dev/tty` isn't a real terminal after all.
fn read_masked_line(label: &str) -> Result<String> {
    use std::os::fd::AsRawFd;

    let tty = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty").context("open /dev/tty")?;
    write!(&tty, "{label}: ")?;
    (&tty).flush()?;

    let fd = tty.as_raw_fd();
    let mut term: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut term) } != 0 {
        let mut line = String::new();
        io::BufReader::new(&tty).read_line(&mut line).context("read from /dev/tty")?;
        return rules::clean_typed_value(&line).map_err(anyhow::Error::msg);
    }
    let original = term;
    term.c_lflag &= !(libc::ECHO | libc::ICANON | libc::ISIG);
    term.c_cc[libc::VMIN] = 1;
    term.c_cc[libc::VTIME] = 0;
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &term) };

    let mut buf: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    let read_result: Result<bool> = loop {
        match (&tty).read(&mut byte) {
            Ok(0) => break Ok(true),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => break Err(e).context("read from /dev/tty"),
        }
        match byte[0] {
            b'\r' | b'\n' => break Ok(true),
            0x03 => break Ok(false), // Ctrl-C
            0x04 if buf.is_empty() => break Ok(true), // Ctrl-D
            0x15 => buf.clear(),     // Ctrl-U
            0x7f | 0x08 => {
                // Backspace: drop one whole UTF-8 character.
                while let Some(b) = buf.pop() {
                    if b & 0xC0 != 0x80 {
                        break;
                    }
                }
            }
            b => buf.push(b),
        }
    };

    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &original) };
    let _ = writeln!(&tty); // the Enter keypress wasn't echoed either

    if !read_result? {
        bail!("aborted — nothing was set");
    }
    let value = rules::clean_typed_value(&String::from_utf8_lossy(&buf)).map_err(anyhow::Error::msg)?;
    if !value.is_empty() {
        let _ = writeln!(&tty, "  {}", paint(&palette::MUTED, &format!("got {}", rules::describe_value(&value))));
    }
    Ok(value)
}

/// `text` in a palette colour, so the answering sheet reads like the column
/// it's opened from — only when stderr is a terminal and `NO_COLOR` is
/// unset, so an agent's captured output stays plain.
fn paint(color: &palette::Rgb, text: &str) -> String {
    use std::io::IsTerminal;
    if !io::stderr().is_terminal() || env::var_os("NO_COLOR").is_some() {
        return text.to_string();
    }
    format!("\x1b[38;2;{};{};{}m{text}\x1b[39m", color.0, color.1, color.2)
}

/// Write `content` to `path`, owner-only from the first byte — a released
/// plaintext never exists with the umask's default permissions, not even
/// between a write and a chmod.
fn write_private(path: &Path, content: &str) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    set_permissions_600(path)?; // it may have existed with wider permissions
    file.write_all(content.as_bytes()).with_context(|| format!("write {}", path.display()))
}

/// Prompt on the real terminal and read one line, echoed — the answering
/// sheet's questions. `/dev/tty`, not stdin, for the same reason as
/// `read_masked_line`.
fn read_tty_line(label: &str) -> Result<String> {
    let tty = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty").context("open /dev/tty")?;
    write!(&tty, "{label}: ")?;
    (&tty).flush()?;
    let mut line = String::new();
    io::BufReader::new(&tty).read_line(&mut line).context("read from /dev/tty")?;
    Ok(rules::strip_paste_markers(&line).trim_end_matches(['\n', '\r']).to_string())
}
