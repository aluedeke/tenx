//! Task-scoped secret unlock (`ARCHITECTURE.md` § Secrets has the overview;
//! this doc comment is the authoritative detail). This module shells out to the system
//! `age`/`age-keygen`/`sops` binaries rather than linking a crypto crate,
//! matching `git/mod.rs`'s reasoning for shelling to `git`: it's what the user
//! already has installed, at whatever version, with no reimplementation risk.
//! `sops` is the one encrypt/decrypt tool used everywhere now — our own
//! sealed bundle, agent-added secrets, and adopted secrets (see *Sops
//! adoption* below) all go
//! through it uniformly — but it has no passphrase prompt of its own: it
//! always needs an already-decrypted identity file via `SOPS_AGE_KEY_FILE`,
//! so raw `age -d` is still what actually unwraps a passphrase-protected
//! identity (see `sops_decrypt`), and `age -p`/`age-keygen` are still what
//! generates/protects one (see `generate_identity`). `age` never
//! encrypts/decrypts a *secret value* directly anymore — only the identity
//! that guards them.
//!
//! Command names deliberately track `sops`'s own vocabulary now — `encrypt`
//! (was `seal`), `decrypt` (was `unlock`), `set` (was `add`) — and, `set`
//! especially, their actual semantics too, not just the names: `set` is
//! literally `sops set` under the hood, editing the *existing* sealed bundle
//! in place. That's a real behavior change from the `add` this replaced, not
//! just a rename — see `set`'s own doc comment for what that costs.
//!
//! Hard rule, enforced throughout this file, not just documented: **no
//! function here ever writes a decrypted secret value to stdout.** `decrypt`
//! only ever writes to its fixed task-scoped file (`--output`, `Stdio::null()`
//! on every child process that could theoretically emit plaintext). Stdout an
//! agent's Bash tool captures becomes part of its own conversation transcript
//! — a durable artifact outside the task folder that `task rm`'s cleanup never
//! reaches, so this is a stricter guarantee than "the agent may read the
//! plaintext file afterward".
//!
//! `decrypt` is safe for an agent to call, not just a human: before touching
//! anything, it tries to open `/dev/tty` — the exact file `age`'s own
//! passphrase prompt reads from (not stdin; confirmed by `age`'s own error
//! when it's missing: "standard input is not a terminal, and /dev/tty is not
//! available"). A Bash-tool child process normally has no controlling
//! terminal, so this fails there, and `decrypt` falls back to enqueue-then-
//! wait behavior instead of letting a raw `age`/`sops` tty error surface:
//! append the given name to a durable per-task marker file, never touching
//! the identity or an encrypted bundle, then **block** until a human acts on
//! it (see *Waiting* below). Idempotent — re-requesting an already-pending
//! name is a no-op, so a chatty agent can't spam repeat notifications. When
//! `/dev/tty` *is* reachable (a human's real shell, or the overlay's spawned
//! pane), `decrypt` proceeds straight to the real decrypt, same passphrase
//! prompt as always. Which behavior a caller gets is decided entirely by
//! whether a real terminal is actually there, not by which subcommand name
//! was typed.
//!
//! `set` mirrors that shape with its own queue (`enqueue_pending_set`): with
//! no terminal it records "someone needs to type in a value for `name`" and
//! waits; with one it prompts for the value (masked) and the passphrase and
//! performs the edit. What it never does is queue a *value* — `sops set`
//! edits an already-encrypted document in place, which needs the identity's
//! passphrase, so an agent can't add a secret without a human, and a queued
//! plaintext value sitting on disk before any human confirmed anything would
//! be a strictly worse exposure than anything else in this design. Only the
//! *name* is queued; the value is typed by the human, on the real terminal,
//! when they fulfil it.
//!
//! **Waiting.** The point of an agent asking for a credential is usually
//! that it can't continue without it, so on the no-terminal path both
//! `decrypt` and `set` block after enqueueing (`wait_for_human`): poll the
//! queue once a second until the name is gone, then decide what that meant
//! — `tenx_core::secrets::wait_outcome` — from the disk alone: an output
//! file (the released plaintext, or the re-encrypted bundle for `set`)
//! modified at or after the request means *fulfilled*, nothing modified
//! means *withdrawn* (`cancel`). No receipt or tombstone is recorded — the
//! queue removal is already the commit point, because every fulfilment path
//! writes its output *before* clearing the name. The wait is bounded
//! (`--timeout`, default `DEFAULT_WAIT`) because an agent's shell tool kills
//! long-running commands: on timeout the request stays queued and the exit
//! message says to re-run the same command, which resumes waiting thanks to
//! the idempotent enqueue. `--no-wait` restores the fire-and-forget behavior.
//!
//! `cancel` withdraws a request — removes the name from either queue (or
//! `--all`) and nothing else. It never touches key material, so it is safe
//! from anywhere, and a waiter blocked on that name sees the removal and
//! exits reporting the withdrawal rather than silently succeeding.
//!
//! Nothing tenx seals needs a `.gitignore`: `.secrets.enc.env`/`.secrets.env`/
//! `.secrets-pending` all live directly under a task's own directory
//! (`tasks/<slug>/`), which is never itself a git repo (only the `<repo>/`
//! worktree subdirectories under it are) — so they're structurally outside
//! git's reach, and `task rm`'s existing `fs::remove_dir_all(&task.path)`
//! already shreds them for free on teardown. This does *not* extend to
//! adopted secrets (see §4.2 below and `find_sops_covered_files`) — those
//! decrypt to a sibling of their ciphertext *inside* the repo worktree,
//! matching the project's own convention, so it's that project's own
//! `.gitignore` (not ours) doing the work there.

use anyhow::{bail, Context, Result};
use std::env;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use tenx_core::secrets::{wait_outcome, WaitOutcome};

use crate::workspace::{self, Task, Workspace};

/// Resolve or create the age identity for the active workspace.
pub fn init() -> Result<()> {
    let cwd = env::current_dir()?;
    let mut ws = workspace::find(&cwd)?;

    if let Ok(path) = resolve_identity_path(&ws) {
        eprintln!("using existing age identity at {}", path.display());
        eprintln!("(nothing to do — tenx secrets encrypt/set/decrypt will use it)");
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

/// Set one secret in the current task's (resolved from cwd) sealed bundle —
/// literally `sops set`: decrypts the bundle's existing data key and
/// re-encrypts with `name` added/updated, leaving every other key as it was.
/// Same tty-detection shape as `decrypt`, mirrored: no real terminal →
/// enqueue "someone needs to supply a value for `name`" (own queue, see
/// `enqueue_pending_set`) and return, never touching the identity or the
/// bundle. Real terminal → prompt for the value first (masked — tenx's own
/// prompt, `read_masked_line`, since `age`'s passphrase masking doesn't cover
/// this), *then* the passphrase, then perform the edit. The value is never a
/// CLI argument or read from stdin (ps-visible to `ps`/`/proc`, and stdin
/// specifically would collide with piping a value in non-interactively,
/// which this command no longer supports on purpose) — always typed directly
/// into `/dev/tty`, same channel the passphrase itself uses. `wait` is how
/// long the no-terminal path blocks for a human (`None`: enqueue and return).
pub fn set(name: &str, wait: Option<Duration>) -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = workspace::find(&cwd)?;
    let task = current_task(&ws, &cwd)?;
    set_in(&ws, &task, name, wait)
}

/// Same as [`set`], for an explicit workspace/task rather than cwd — used by
/// the overlays, same reasoning as [`decrypt_in`]: they pick a task from a
/// list spanning every workspace rather than being invoked from inside one.
pub fn set_in(ws: &Workspace, task: &Task, name: &str, wait: Option<Duration>) -> Result<()> {
    if name.is_empty() || name.contains(['/', '\\', '"']) || name == "." || name == ".." {
        bail!("invalid secret name: {name:?}");
    }

    if !tty_available() {
        // Mirrors `decrypt`'s tty fallback, but in the opposite direction:
        // this isn't "release something already sealed", it's "someone needs
        // to type in a value for something that doesn't exist yet" — a
        // genuinely different queue (see module docs and
        // `workspace::SECRETS_PENDING_SET_FILE`), fulfilled by a human simply
        // re-running `set` from a real terminal, same command either way.
        let requested_at = request_instant();
        enqueue_pending_set(task, name)?;
        if let Some(timeout) = wait {
            wait_for_human(task, Queue::Set, name, requested_at, timeout)?;
        }
        return Ok(());
    }

    let identity = resolve_identity_path(ws)?;
    let bundle = bundle_path(task);
    ensure_bundle_exists(&identity, &bundle)?;

    let value = read_masked_line(&format!("value for '{name}'"))?;
    if value.is_empty() {
        bail!("no value given — aborted, nothing was set");
    }

    sops_set(&identity, &bundle, name, &value)?;
    clear_pending_set(task, name)?;
    eprintln!("✓ set '{name}' for task '{}' — released at the next decrypt", task.name);
    Ok(())
}

/// Append `name` to the current task's durable pending-request marker file.
/// This is the enqueue-only half of `decrypt`'s tty-detection fallback (see
/// module docs) — the only thing that runs when `/dev/tty` isn't reachable.
/// Idempotent: re-requesting an already-pending name is a no-op, so a chatty
/// agent re-running `decrypt` can't spam repeat notifications.
fn enqueue_pending(task: &Task, name: &str) -> Result<()> {
    let pending_path = pending_path(task);
    let mut names = read_pending(task);
    if names.iter().any(|n| n == name) {
        eprintln!("'{name}' already pending for task '{}'", task.name);
        return Ok(());
    }
    names.push(name.to_string());
    std::fs::write(&pending_path, names.join("\n") + "\n")
        .with_context(|| format!("write {}", pending_path.display()))?;
    eprintln!("requested '{name}' for task '{}' — see: tenx secrets status", task.name);
    Ok(())
}

/// Append `name` to the current task's pending-*set* marker file — the
/// enqueue-only half of `set`'s tty-detection fallback. Separate file from
/// `enqueue_pending` above: this queue means "a human needs to type in a
/// value for `name`", not "release something already sealed" — different
/// fulfillment action, so it can't share `decrypt`'s queue (see module
/// docs and `workspace::SECRETS_PENDING_SET_FILE`). Idempotent, same
/// reasoning as `enqueue_pending`.
fn enqueue_pending_set(task: &Task, name: &str) -> Result<()> {
    let path = pending_set_path(task);
    let mut names = read_pending_set(task);
    if names.iter().any(|n| n == name) {
        eprintln!("value for '{name}' already requested for task '{}'", task.name);
        return Ok(());
    }
    names.push(name.to_string());
    std::fs::write(&path, names.join("\n") + "\n").with_context(|| format!("write {}", path.display()))?;
    eprintln!("requested a value for '{name}' for task '{}' — see: tenx secrets status", task.name);
    Ok(())
}

/// Interactive convenience: do whatever's pending for the current task
/// (resolved from cwd) in one sitting — `decrypt` once if anything is
/// pending release (satisfies every pending release-name at once, same as
/// `decrypt` itself), then `set` once per pending value-name (each is an
/// independent edit, so each gets its own value-then-passphrase round; see
/// `set_in`'s doc comment for why those can't be batched into one
/// passphrase entry). Exists specifically for spawn-a-real-pane callers
/// (`tenx-zellij`, which can only shell out to a subprocess, not link
/// against `decrypt_in`/`set_in` directly) so they don't have to reimplement
/// this sequencing themselves — the native overlay's `run_unlock` uses
/// [`fulfill_in`] for the same reason, even though it *could* call
/// `decrypt_in`/`set_in` directly, just to keep the two overlays from
/// drifting on what "handle everything pending for this task" means.
/// Errors from one step are printed but don't stop the rest; returns `Err`
/// at the end if anything failed, so a non-interactive caller's exit code
/// still reflects it.
pub fn fulfill() -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = workspace::find(&cwd)?;
    let task = current_task(&ws, &cwd)?;
    fulfill_in(&ws, &task)
}

/// Same as [`fulfill`], for an explicit workspace/task rather than cwd —
/// same reasoning as [`decrypt_in`]/[`set_in`].
pub fn fulfill_in(ws: &Workspace, task: &Task) -> Result<()> {
    let mut failed = false;
    if !workspace::secrets_pending(&task.path).is_empty()
        && let Err(e) = decrypt_in(ws, task, None, None)
    {
        eprintln!("tenx: {e}");
        failed = true;
    }
    for name in workspace::secrets_pending_set(&task.path) {
        if let Err(e) = set_in(ws, task, &name, None) {
            eprintln!("tenx: {e}");
            failed = true;
        }
    }
    if failed {
        bail!("one or more secrets actions failed for task '{}' — see above", task.name);
    }
    Ok(())
}

/// How long the no-terminal path waits by default. Deliberately under the
/// two minutes Claude Code's Bash tool allows a command before killing it
/// (which would be a noisy failure instead of this clean "still pending,
/// re-run" exit); the `/tenx` skill tells the agent to raise both when it
/// really can't continue without the secret.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(100);

/// Poll interval while waiting — the same order as the watcher's 2 s tick;
/// a human typing a passphrase is the slow part, not this.
const WAIT_POLL: Duration = Duration::from_secs(1);

/// Which queue a wait or cancel is about. The two queues have different
/// fulfilment actions and therefore different outputs to watch for.
#[derive(Clone, Copy)]
enum Queue {
    /// `decrypt`'s queue: fulfilled by releasing plaintext.
    Release,
    /// `set`'s queue: fulfilled by `sops set` rewriting the sealed bundle.
    Set,
}

impl Queue {
    fn names(self, task: &Task) -> Vec<String> {
        match self {
            Queue::Release => read_pending(task),
            Queue::Set => read_pending_set(task),
        }
    }

    /// Modification times of every file a fulfilment of this queue would
    /// have written — `tenx_core::secrets::wait_outcome`'s evidence. Files
    /// that don't exist contribute nothing.
    fn output_mtimes(self, task: &Task) -> Vec<SystemTime> {
        let mut paths = Vec::new();
        match self {
            Queue::Release => {
                paths.push(task.path.join(".secrets.env"));
                if let Ok(entries) = std::fs::read_dir(task.path.join(".secrets-adopted")) {
                    paths.extend(entries.flatten().map(|e| e.path()));
                }
            }
            Queue::Set => paths.push(bundle_path(task)),
        }
        paths.iter().filter_map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok()).collect()
    }

    fn verb(self) -> &'static str {
        match self {
            Queue::Release => "release",
            Queue::Set => "supply a value for",
        }
    }
}

/// The instant a request is considered made, for `wait_outcome`'s "written at
/// or after the request" test. Padded back by a couple of seconds so a
/// filesystem with coarse (whole-second) mtimes can't round a fulfilment
/// that landed in the same second to *before* the request and make it look
/// like a cancellation; a genuine cancellation can't be confused by this,
/// because it writes no output at all.
fn request_instant() -> SystemTime {
    SystemTime::now() - Duration::from_secs(2)
}

/// Block until `name` leaves `queue` — or `timeout` passes — and say which.
/// See the module docs (*Waiting*) for the contract; this is the I/O half,
/// the decision is `tenx_core::secrets::wait_outcome`. Errors on both
/// withdrawal and timeout so an agent's exit code reflects that it did *not*
/// get what it asked for; the message tells the two apart.
fn wait_for_human(task: &Task, queue: Queue, name: &str, requested_at: SystemTime, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    eprintln!(
        "waiting for someone to {} '{name}' (up to {}) — withdraw with: tenx secrets cancel {name}",
        queue.verb(),
        fmt_wait(timeout)
    );
    loop {
        let still_pending = queue.names(task).iter().any(|n| n == name);
        match wait_outcome(still_pending, &queue.output_mtimes(task), requested_at) {
            WaitOutcome::Fulfilled => {
                match queue {
                    Queue::Release => eprintln!(
                        "✓ '{name}' released for task '{}' — see {}",
                        task.name,
                        task.path.join(".secrets.env").display()
                    ),
                    Queue::Set => eprintln!(
                        "✓ a value for '{name}' was set for task '{}' — run: tenx secrets decrypt {name}",
                        task.name
                    ),
                }
                return Ok(());
            }
            WaitOutcome::Cancelled => {
                bail!("the request for '{name}' was withdrawn before it was fulfilled")
            }
            WaitOutcome::Pending => {}
        }
        if Instant::now() >= deadline {
            bail!(
                "'{name}' is still pending for task '{}' after {} — it stays queued; re-run the same \
                 command to keep waiting, or withdraw it with: tenx secrets cancel {name}",
                task.name,
                fmt_wait(timeout)
            );
        }
        std::thread::sleep(WAIT_POLL);
    }
}

/// "100s" / "9m" — exact, unlike `tenx_core::time::format_duration`, which
/// buckets to the nearest unit for the overlay's age column and would call
/// the default wait "1m".
fn fmt_wait(d: Duration) -> String {
    let secs = d.as_secs();
    if secs > 0 && secs.is_multiple_of(60) { format!("{}m", secs / 60) } else { format!("{secs}s") }
}

/// Withdraw pending requests for the current task (resolved from cwd): one
/// `name` from whichever queue holds it, or everything when `name` is
/// `None`. Touches nothing but the two queue files — no identity, no bundle,
/// no plaintext — so it is safe to run from anywhere, agent's Bash tool
/// included. A waiter blocked on a withdrawn name exits with an error saying
/// so (see `wait_for_human`).
pub fn cancel(name: Option<&str>) -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = workspace::find(&cwd)?;
    let task = current_task(&ws, &cwd)?;
    cancel_in(&task, name)
}

/// Same as [`cancel`], for an explicit task — used by the overlay's `:cancel`,
/// same reasoning as [`decrypt_in`].
pub fn cancel_in(task: &Task, name: Option<&str>) -> Result<()> {
    let release = read_pending(task);
    let set = read_pending_set(task);
    let (drop_release, drop_set): (Vec<String>, Vec<String>) = match name {
        None => (release, set),
        Some(n) => (
            release.into_iter().filter(|x| x == n).collect(),
            set.into_iter().filter(|x| x == n).collect(),
        ),
    };
    if drop_release.is_empty() && drop_set.is_empty() {
        match name {
            Some(n) => eprintln!("nothing pending named '{n}' for task '{}'", task.name),
            None => eprintln!("nothing pending for task '{}'", task.name),
        }
        return Ok(());
    }
    clear_pending_names(task, &drop_release.iter().cloned().collect())?;
    for n in &drop_set {
        clear_pending_set(task, n)?;
    }
    let withdrawn: Vec<String> = drop_release
        .into_iter()
        .chain(drop_set.into_iter().map(|n| format!("{n} (needs value)")))
        .collect();
    eprintln!("withdrew {} for task '{}'", withdrawn.join(", "), task.name);
    Ok(())
}

/// Whether a real controlling terminal is reachable right now — the same
/// thing `age`'s own passphrase prompt checks (it reads `/dev/tty` directly,
/// not stdin, specifically so it still works when stdin/stdout are
/// redirected). An agent's Bash tool child process normally has none; a
/// human's real shell, or a pane the overlay just spawned, always does.
fn tty_available() -> bool {
    std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty").is_ok()
}

/// Decrypt the current task's (resolved from cwd) secrets — or, when no real
/// terminal is reachable, enqueue `name` for a human to release and wait up
/// to `wait` for that to happen (`None`: enqueue and return). See module
/// docs for the tty-detection fallback this implements.
pub fn decrypt(name: Option<&str>, wait: Option<Duration>) -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = workspace::find(&cwd)?;
    let task = current_task(&ws, &cwd)?;
    decrypt_in(&ws, &task, name, wait)
}

/// Same as [`decrypt`], for an explicit workspace/task rather than cwd — used
/// by the overlays (native `tui::overlay` and the `tenx-zellij` wasm plugin,
/// via a spawned pane whose cwd is set to the task directory rather than a
/// direct call), which pick a task from a list spanning every workspace
/// rather than being invoked from inside one. Both overlays only ever call
/// this from a real interactive pane, so `name` is always `None` there — the
/// tty-detection fallback below exists for the CLI/agent path.
pub fn decrypt_in(ws: &Workspace, task: &Task, name: Option<&str>, wait: Option<Duration>) -> Result<()> {
    let requested_at = request_instant();
    if let Some(name) = name {
        enqueue_pending(task, name)?;
    }
    if !tty_available() {
        let Some(name) = name else {
            bail!(
                "no real terminal available (this looks like an agent's Bash tool) — \
                 pass what you need, e.g.: tenx secrets decrypt STRIPE_KEY"
            );
        };
        // Already enqueued above; that's the whole non-interactive contract
        // — never touch the identity or an encrypted bundle from here. All
        // that's left is to wait for a human to do it.
        if let Some(timeout) = wait {
            wait_for_human(task, Queue::Release, name, requested_at, timeout)?;
        }
        return Ok(());
    }

    let identity = resolve_identity_path(ws)?;

    let bundle = bundle_path(task);
    let sops_files = find_sops_covered_files(task);
    let pending = workspace::secrets_pending(&task.path);

    if !bundle.exists() && sops_files.is_empty() {
        bail!(
            "no sealed secrets for task '{}' — run: tenx secrets encrypt {} <file>, or tenx secrets set <name>",
            task.name,
            task.name
        );
    }

    // Which pending names refer to a specific sops file (see
    // `file_matches_request`) vs. something else entirely (a field inside
    // our own bundle, a typo, free text). A repo adopted from an existing
    // project can have more than one sops-covered file — checkly's
    // local-support has both `secrets.staging.enc.env` and
    // `secrets.prod.enc.env` — and "decrypt every file this repo has,
    // regardless of which one was actually asked for" would violate the
    // same least-privilege principle the rest of this design holds
    // everywhere else. So: if a pending request names a specific file,
    // unlock only that one. With nothing that names a file specifically,
    // fall back to every sops file found — matches how our own single
    // bundle already has no partial-release concept, so "nothing named,
    // unlock what's here" isn't a new behavior, just the existing one.
    let named_sops_files: Vec<&PathBuf> = sops_files
        .iter()
        .filter(|f| pending.iter().any(|n| file_matches_request(f, n)))
        .collect();
    let fell_back_to_all = named_sops_files.is_empty();
    let selected_sops: Vec<&PathBuf> =
        if fell_back_to_all { sops_files.iter().collect() } else { named_sops_files };

    let mut satisfied: std::collections::HashSet<String> = std::collections::HashSet::new();

    if bundle.exists() {
        let out_path = task.path.join(".secrets.env");
        sops_decrypt(&identity, &bundle, &out_path)?;
        set_permissions_600(&out_path)?;
        eprintln!("✓ decrypted → {}", out_path.display());
        // The bundle is all-or-nothing (we control what's sealed/set into
        // it, so least-privilege already happened earlier) — satisfies any
        // pending name that isn't specifically claimed by a sops file match
        // above.
        for n in &pending {
            if !sops_files.iter().any(|f| file_matches_request(f, n)) {
                satisfied.insert(n.clone());
            }
        }
    }

    // Adopted secrets (see *Sops adoption* below): a repo the task has a worktree for
    // may already have its own age/sops setup — an existing `.sops.yaml`
    // plus `*.enc.*` files, sealed by that project's own tooling, not by
    // `tenx secrets encrypt`. The real plaintext never lands inside the
    // worktree at all — it's decrypted to `.secrets-adopted/` directly under
    // the task directory (never a git repo, same structural guarantee our
    // own bundle already has), and a relative symlink is placed at the
    // conventional sibling name (`secrets.staging.enc.env` → the worktree
    // gets `secrets.staging.env`, pointing back at the real file) so the
    // project's own tooling finds it exactly where it already expects to —
    // reading through a symlink is indistinguishable from a real file to
    // anything that isn't specifically inspecting the filesystem entry type.
    // This closes a real gap the old direct-write-into-the-worktree approach
    // had: it trusted that project's own `.gitignore` already covered the
    // plaintext filename, unverified — a wrong or missing pattern meant a
    // plain `git add -A` could stage the actual secret. Now even that
    // mistake only stages a symlink (a relative path, no secret bytes) — the
    // real content structurally can't be committed by any git operation
    // inside the worktree, matching the guarantee `.secrets.enc.env` already
    // has for our own bundle.
    for ciphertext in &selected_sops {
        let plaintext_out = strip_enc_suffix(ciphertext);
        let storage_path = adopted_secret_storage_path(task, ciphertext);
        if let Some(parent) = storage_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        sops_decrypt(&identity, ciphertext, &storage_path)?;
        set_permissions_600(&storage_path)?;

        // Always recreate the symlink fresh — whatever was previously at
        // this path (a stale symlink from an earlier decrypt, or a real
        // plaintext file left over from before this fix existed) is exactly
        // what a fresh decrypt is supposed to replace.
        let _ = std::fs::remove_file(&plaintext_out);
        let target = adopted_symlink_target(task, ciphertext, &storage_path);
        std::os::unix::fs::symlink(&target, &plaintext_out).with_context(|| {
            format!("symlink {} -> {}", plaintext_out.display(), target.display())
        })?;
        eprintln!(
            "✓ decrypted (sops) → {} (stored outside the repo at {}, symlinked in)",
            plaintext_out.display(),
            storage_path.display()
        );
    }
    for n in &pending {
        let claimed_by_selection = selected_sops.iter().any(|f| file_matches_request(f, n));
        // A name that matches nothing at all (not a specific file, and we
        // fell back to "everything") is satisfied by that fallback too.
        let satisfied_by_fallback = fell_back_to_all && !sops_files.is_empty() && !claimed_by_selection;
        if claimed_by_selection || satisfied_by_fallback {
            satisfied.insert(n.clone());
        }
    }

    // Leaves anything not actually resolved this time (e.g. "prod" still
    // pending after only "staging" was requested and unlocked) for a future
    // unlock, rather than wiping the whole queue regardless of what was
    // actually released.
    clear_pending_names(task, &satisfied)?;
    Ok(())
}

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
/// document that doesn't exist yet). So the first-ever `set` for a task
/// bootstraps an empty encrypted document, using the same public-key-only
/// encrypt `seal`'s first bundle uses — after that, every `set` is a genuine
/// in-place edit of the same document.
fn ensure_bundle_exists(identity: &Path, bundle: &Path) -> Result<()> {
    if bundle.exists() {
        return Ok(());
    }
    let recipient = resolve_recipient(identity)?;
    let tmp = std::env::temp_dir().join(format!("tenx-bootstrap-{}.env", std::process::id()));
    std::fs::write(&tmp, "").with_context(|| format!("write {}", tmp.display()))?;
    let result = sops_encrypt(&recipient, &tmp, bundle);
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
/// existing document in place rather than creating anything new — needs
/// `identity`'s decrypt access every call, unwrapped first if it's
/// passphrase-protected (see `with_plain_identity`). The value goes through
/// `sops`'s own stdin channel (`--value-stdin`, "avoids leaking secrets in
/// process listings" per its own `--help`) rather than argv, same reasoning
/// as everywhere else in this module.
fn sops_set(identity: &Path, bundle: &Path, name: &str, value: &str) -> Result<()> {
    with_plain_identity(identity, |plain_identity| run_sops_set(plain_identity, bundle, name, value))
}

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
        if !status.success() {
            bail!("failed to decrypt the identity (wrong passphrase?)");
        }
        f(&tmp_identity)
    })();

    let _ = std::fs::remove_dir_all(&tmp_dir); // shred immediately, success or not
    result
}

// ── Sops adoption ───────────────────────────────────────────────────────────

/// Whether a pending request name plausibly refers to this specific
/// sops-covered file — a loose, case-insensitive substring match against the
/// filename. `"staging"` matches `secrets.staging.enc.env`; the exact
/// filename always matches itself. We only ever see filenames without
/// decrypting, so this can't (and doesn't try to) match a field *inside* a
/// file — a name that doesn't match any file here is assumed to be about
/// something else (a field in our own bundle, free text, a typo) and falls
/// through to `unlock_in`'s "nothing named a specific file" fallback.
fn file_matches_request(file: &Path, requested: &str) -> bool {
    let name = file.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
    name.contains(&requested.to_lowercase())
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

/// Decrypt one sops-covered file to `plaintext_out` using `identity`. `sops`
/// resolves its decryption key via `SOPS_AGE_KEY_FILE`, which — unlike raw
/// `age -i -` — must be a real file path, not something stdin can feed it,
/// so a passphrase-protected identity needs a real (temporary, immediately
/// shredded) intermediate file — see `with_plain_identity`.
fn sops_decrypt(identity: &Path, ciphertext: &Path, plaintext_out: &Path) -> Result<()> {
    with_plain_identity(identity, |plain_identity| run_sops_decrypt(plain_identity, ciphertext, plaintext_out))
}

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
/// the (informational) names collected by `request`.
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
        let pending = read_pending(task);
        let pending_set = read_pending_set(task);
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

// ── Pending-request marker ───────────────────────────────────────────────────

fn pending_path(task: &Task) -> PathBuf {
    task.path.join(workspace::SECRETS_PENDING_FILE)
}

/// `workspace::secrets_pending` is the shared reader (also used by
/// `task_json`, so the overlay/status bar see the same data this module
/// writes) — this is just the `Task`-typed convenience wrapper for it.
fn read_pending(task: &Task) -> Vec<String> {
    workspace::secrets_pending(&task.path)
}

/// Remove `resolved` names from the task's pending list, leaving any others
/// (e.g. a second sops file that wasn't actually decrypted this time)
/// pending for a future unlock — partial resolution, not "unlock ran, so the
/// whole queue must be satisfied now."
fn clear_pending_names(task: &Task, resolved: &std::collections::HashSet<String>) -> Result<()> {
    let remaining: Vec<String> =
        read_pending(task).into_iter().filter(|n| !resolved.contains(n)).collect();
    let path = pending_path(task);
    if remaining.is_empty() {
        let _ = std::fs::remove_file(&path);
    } else {
        std::fs::write(&path, remaining.join("\n") + "\n")
            .with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}

fn pending_set_path(task: &Task) -> PathBuf {
    task.path.join(workspace::SECRETS_PENDING_SET_FILE)
}

/// `workspace::secrets_pending_set` is the shared reader (also used by
/// `task_json`) — this is just the `Task`-typed convenience wrapper, same
/// pattern as `read_pending` above for the other queue.
fn read_pending_set(task: &Task) -> Vec<String> {
    workspace::secrets_pending_set(&task.path)
}

/// Remove `name` from the pending-set queue after a successful `set` —
/// single-name, not a `HashSet` like `clear_pending_names`, since one `set`
/// call resolves exactly the one name it was called with, never more.
fn clear_pending_set(task: &Task, name: &str) -> Result<()> {
    let remaining: Vec<String> = read_pending_set(task).into_iter().filter(|n| n != name).collect();
    let path = pending_set_path(task);
    if remaining.is_empty() {
        let _ = std::fs::remove_file(&path);
    } else {
        std::fs::write(&path, remaining.join("\n") + "\n")
            .with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
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

/// Prompt on the real terminal with input echo disabled, for `set`'s value
/// prompt — the passphrase itself is masked for free (`age -p` handles its
/// own prompt), but the secret *value* `set` asks for is tenx's own prompt,
/// so tenx has to do the masking itself. Reads `/dev/tty` directly rather
/// than stdin, same reasoning as `tty_available`: works regardless of
/// whatever stdin happens to be redirected to. Uses `libc` termios directly
/// rather than pulling in a crate for this one call — `libc` is already a
/// dependency. Falls back to unmasked (rather than failing outright) if
/// `tcgetattr`/`tcsetattr` themselves fail, which would only happen on a
/// `/dev/tty` that isn't a real terminal in some unexpected way.
fn read_masked_line(label: &str) -> Result<String> {
    use std::os::fd::AsRawFd;

    let tty = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty").context("open /dev/tty")?;
    write!(&tty, "{label}: ")?;
    (&tty).flush()?;

    let fd = tty.as_raw_fd();
    let mut term: libc::termios = unsafe { std::mem::zeroed() };
    let masked = unsafe { libc::tcgetattr(fd, &mut term) } == 0;
    let original = term;
    if masked {
        term.c_lflag &= !libc::ECHO;
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &term) };
    }

    let mut line = String::new();
    let read_result = io::BufReader::new(&tty).read_line(&mut line);

    if masked {
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &original) };
    }
    let _ = writeln!(&tty); // the Enter keypress wasn't echoed either

    read_result.context("read from /dev/tty")?;
    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}
