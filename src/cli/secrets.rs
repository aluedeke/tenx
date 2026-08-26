//! Task-scoped secret unlock (Phase 0 CLI baseline — see `PRD.md` at the task
//! root for the full design). This module intentionally shells out to the
//! system `age`/`age-keygen` binaries rather than linking a crypto crate,
//! matching `git/mod.rs`'s reasoning for shelling to `git`: it's what the user
//! already has installed, at whatever version, with no reimplementation risk.
//!
//! Hard rule, enforced throughout this file, not just documented: **no
//! function here ever writes a decrypted secret value to stdout.** `unlock`
//! only ever writes to its fixed task-scoped file (`-o`, `Stdio::null()` on
//! every child process that could theoretically emit plaintext). Stdout an
//! agent's Bash tool captures becomes part of its own conversation transcript
//! — a durable artifact outside the task folder that `task rm`'s cleanup never
//! reaches, so this is a stricter guarantee than "the agent may read the
//! plaintext file afterward".
//!
//! `request` is the one command in this module safe for an agent to call: it
//! only ever appends to a durable, per-task marker file. It has no code path
//! that touches an identity or an encrypted bundle.
//!
//! Nothing tenx seals needs a `.gitignore`: `.secrets.age`/`.secrets.env`/
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

use crate::workspace::{self, Task, Workspace};

/// Resolve or create the age identity for the active workspace.
pub fn init() -> Result<()> {
    let cwd = env::current_dir()?;
    let mut ws = workspace::find(&cwd)?;

    if let Ok(path) = resolve_identity_path(&ws) {
        eprintln!("using existing age identity at {}", path.display());
        eprintln!("(nothing to do — tenx secrets seal/unlock will use it)");
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

/// Encrypt `file` as the sealed secrets bundle for `task_slug`.
pub fn seal(task_slug: &str, file: &str) -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = workspace::find(&cwd)?;
    let task = ws.find_task(task_slug)?;
    let identity = resolve_identity_path(&ws)?;
    let recipient = resolve_recipient(&identity)?;

    let src = Path::new(file);
    if !src.exists() {
        bail!("no such file: {}", src.display());
    }
    let bundle_path = task.path.join(".secrets.age");

    let out = Command::new("age")
        .args(["-r", &recipient, "-o"])
        .arg(&bundle_path)
        .arg(src)
        .output()
        .context("run age -e")?;
    if !out.status.success() {
        bail!("age encrypt failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    eprintln!("✓ sealed {} → {}", src.display(), bundle_path.display());
    Ok(())
}

/// Declare that the current task (resolved from cwd) wants a secret unlocked.
/// Agent-safe: this is the *only* function in this module reachable without
/// touching key material — it appends a name to a durable per-task marker
/// file and returns. Idempotent: re-requesting an already-pending name is a
/// no-op, so a chatty agent can't spam repeat notifications.
pub fn request(name: &str) -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = workspace::find(&cwd)?;
    let task = current_task(&ws, &cwd)?;
    let pending_path = pending_path(&task);

    let mut names = read_pending(&task);
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

/// Decrypt the current task's (resolved from cwd) sealed bundle into
/// `tasks/<slug>/.secrets.env`. Human-only in the real flow — never wired as
/// something an agent's Bash tool would invoke — and prompts for the
/// identity's passphrase interactively on the real terminal.
pub fn unlock() -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = workspace::find(&cwd)?;
    let task = current_task(&ws, &cwd)?;
    unlock_in(&ws, &task)
}

/// Same as [`unlock`], for an explicit workspace/task rather than cwd — used
/// by the overlays (native `tui::overlay` and the `tenx-zellij` wasm plugin,
/// via a spawned pane whose cwd is set to the task directory rather than a
/// direct call), which pick a task from a list spanning every workspace
/// rather than being invoked from inside one. Same terminal-interactive
/// passphrase prompt, same file-output-only guarantee.
pub fn unlock_in(ws: &Workspace, task: &Task) -> Result<()> {
    let identity = resolve_identity_path(ws)?;

    let bundle = task.path.join(".secrets.age");
    let sops_files = find_sops_covered_files(task);

    if !bundle.exists() && sops_files.is_empty() {
        bail!(
            "no sealed secrets for task '{}' — run: tenx secrets seal {} <file>",
            task.name,
            task.name
        );
    }

    if bundle.exists() {
        let out_path = task.path.join(".secrets.env");
        if is_age_encrypted(&identity)? {
            decrypt_via_passphrase_identity(&identity, &bundle, &out_path)?;
        } else {
            decrypt_via_plain_identity(&identity, &bundle, &out_path)?;
        }
        set_permissions_600(&out_path)?;
        eprintln!("✓ unlocked → {}", out_path.display());
    }

    // Adopted secrets (see PRD.md §4.2): a repo the task has a worktree for
    // may already have its own age/sops setup — an existing `.sops.yaml`
    // plus `*.enc.*` files, sealed by that project's own tooling, not by
    // `tenx secrets seal`. Decrypt each to its conventional sibling name
    // (`secrets.staging.enc.env` → `secrets.staging.env`), matching how the
    // project already expects to consume it, rather than forcing everything
    // through our own single-bundle-per-task shape.
    for ciphertext in &sops_files {
        let plaintext_out = strip_enc_suffix(ciphertext);
        sops_decrypt(&identity, ciphertext, &plaintext_out)?;
        set_permissions_600(&plaintext_out)?;
        eprintln!("✓ unlocked (sops) → {}", plaintext_out.display());
    }

    clear_pending(task)?;
    Ok(())
}

/// Two real child processes wired with a real OS pipe between them — the
/// process-level equivalent of `age -d identity | age -d -i - -o out bundle`.
/// The plaintext identity flows stage1 → stage2 entirely inside the pipe; it's
/// never captured into this process's own memory, and neither stage is ever
/// given a stdout that isn't `Stdio::null()`/`-o`, so there is no code path
/// through which a decrypted value could reach this command's own stdout.
fn decrypt_via_passphrase_identity(identity: &Path, bundle: &Path, out_path: &Path) -> Result<()> {
    let mut stage1 = Command::new("age")
        .arg("-d")
        .arg(identity)
        .stdin(Stdio::inherit()) // passphrase prompt reaches the real terminal
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("run age -d (identity)")?;
    let piped = stage1.stdout.take().context("capture stage1 stdout")?;

    let status = Command::new("age")
        .args(["-d", "-i", "-", "-o"])
        .arg(out_path)
        .arg(bundle)
        .stdin(piped)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .context("run age -d (bundle)")?;

    let stage1_status = stage1.wait().context("wait on stage1")?;
    if !stage1_status.success() {
        bail!("failed to decrypt the identity (wrong passphrase?)");
    }
    if !status.success() {
        bail!("failed to decrypt the task's sealed bundle");
    }
    Ok(())
}

fn decrypt_via_plain_identity(identity: &Path, bundle: &Path, out_path: &Path) -> Result<()> {
    let status = Command::new("age")
        .arg("-d")
        .arg("-i")
        .arg(identity)
        .arg("-o")
        .arg(out_path)
        .arg(bundle)
        .stdin(Stdio::inherit())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .context("run age -d")?;
    if !status.success() {
        bail!("failed to decrypt the task's sealed bundle");
    }
    Ok(())
}

// ── Sops adoption (PRD.md §4.2) ─────────────────────────────────────────────

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
/// name every sops project already expects (and, in practice, already has
/// `.gitignore`d — this project's ciphertext-vs-plaintext naming split is
/// what makes that possible in the first place).
fn strip_enc_suffix(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(name.replacen(".enc.", ".", 1))
}

/// Decrypt one sops-covered file to `plaintext_out` using `identity`. `sops`
/// resolves its decryption key via `SOPS_AGE_KEY_FILE`, which — unlike raw
/// `age -i -` — must be a real file path, not something stdin can feed it.
/// For a passphrase-protected identity this means a real intermediate file
/// is unavoidable here (unlike `decrypt_via_passphrase_identity`'s pipe-only
/// approach for our own bundles): decrypted to a mode-700 temp directory,
/// used for exactly this one `sops` invocation, and removed immediately
/// after — never the task folder, never longer-lived than this function call.
fn sops_decrypt(identity: &Path, ciphertext: &Path, plaintext_out: &Path) -> Result<()> {
    if !is_age_encrypted(identity)? {
        return run_sops_decrypt(identity, ciphertext, plaintext_out);
    }

    let tmp_dir = std::env::temp_dir().join(format!("tenx-sops-{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir).with_context(|| format!("create {}", tmp_dir.display()))?;
    set_dir_permissions_700(&tmp_dir)?;
    let tmp_identity = tmp_dir.join("identity");

    let result = (|| -> Result<()> {
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
        run_sops_decrypt(&tmp_identity, ciphertext, plaintext_out)
    })();

    let _ = std::fs::remove_dir_all(&tmp_dir); // shred immediately, success or not
    result
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
        let sealed = task.path.join(".secrets.age").exists() || !sops_files.is_empty();
        let unlocked = task.path.join(".secrets.env").exists()
            || sops_files.iter().any(|f| strip_enc_suffix(f).exists());
        let pending = read_pending(task);
        if !sealed && !unlocked && pending.is_empty() {
            continue;
        }
        any = true;
        println!(
            "{:<20} {:<8} {:<10} {}",
            task.display_name,
            if sealed { "yes" } else { "no" },
            if unlocked { "yes" } else { "no" },
            pending.join(", "),
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
/// tenx-specific setup. See PRD.md §4.1.
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
/// this passphrase is the entire "confirm every time" gate (see PRD.md §5:
/// `age` has no daemon and no cache, so there's nothing to layer `sudo` on top
/// of). The unencrypted intermediate is written to a sibling `.tmp` file only
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
        .context("not inside a task directory (tenx secrets request/unlock take no <task> argument — cd into the task first)")?;
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

fn clear_pending(task: &Task) -> Result<()> {
    let _ = std::fs::remove_file(pending_path(task));
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
