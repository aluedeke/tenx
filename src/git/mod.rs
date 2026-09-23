use anyhow::{bail, Context, Result};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

use tenx_core::progress::Snapshot;

/// What a streaming git call reports as it goes: every progress redraw git
/// writes, already parsed. Called on the thread that runs the command, so an
/// implementation must not block — the TUI's sends on a channel.
pub type OnProgress<'a> = &'a mut dyn FnMut(Snapshot);

/// Run a `git` command with `--progress`, feeding each progress redraw to
/// `on` and returning its stderr for the error path.
///
/// Git writes progress to stderr as `\r`-separated redraws of one line, and
/// interleaves its real diagnostics on the same stream. So the stream is read
/// in raw chunks (never by line: a progress redraw has no newline and would
/// block until the phase ended), split on both separators, and each piece
/// offered to the parser — which ignores everything that isn't a phase it
/// knows. The whole text is kept regardless, because only the exit status
/// decides whether this failed, and the message then has to come from here.
fn run_streaming(cmd: &mut Command, on: OnProgress) -> Result<(std::process::ExitStatus, String)> {
    let mut child = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn git")?;
    let mut stderr = child.stderr.take().context("git stderr")?;

    let mut captured = String::new();
    // `pending` holds the tail of a chunk that ended mid-redraw, so a line
    // split across two reads is parsed once, whole, rather than twice, broken.
    let mut pending = String::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stderr.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        let text = String::from_utf8_lossy(&buf[..n]);
        captured.push_str(&text);
        pending.push_str(&text);
        // Everything up to the last separator is complete; the rest waits.
        let cut = pending.rfind(['\r', '\n']).map(|i| i + 1);
        if let Some(cut) = cut {
            let complete: String = pending.drain(..cut).collect();
            for snap in tenx_core::progress::split_progress(&complete).filter_map(tenx_core::progress::parse_git_line) {
                on(snap);
            }
        }
    }
    // A last redraw with no trailing separator (git's final 100%).
    for snap in tenx_core::progress::split_progress(&pending).filter_map(tenx_core::progress::parse_git_line) {
        on(snap);
    }

    let status = child.wait().context("wait for git")?;
    Ok((status, captured))
}

pub fn bare_repo_path(bare_dir: &Path, name: &str) -> std::path::PathBuf {
    bare_dir.join(format!("{}.git", name))
}

// ── Cross-process locking ────────────────────────────────────────────────────

/// An exclusive claim on one bare repo, held until the guard is dropped.
///
/// Every route that writes a bare repo — task creation, `repo add`, add/detach
/// repo, `init` — can run in more than one process at once: clients are per
/// terminal (a desktop and a phone over SSH each have their own column), and
/// nothing stops `tenx task new` being typed into a pane at the same time.
/// Without this, two `git clone`s race into the same `.bare/<name>.git`.
///
/// The column's own "one job at a time" rule is per client, so it cannot
/// stand in for this — it makes a single client orderly, not the machine.
///
/// `flock(2)` rather than a lockfile with a pid in it: the kernel drops the
/// lock when the holder dies, however it dies, so there is no such thing as a
/// stale lock to detect and break. The file is only a handle for the lock and
/// is never read; it lives outside the repo so git never sees it.
pub struct RepoLock {
    _file: std::fs::File,
}

fn lock_path(bare_dir: &Path, name: &str) -> std::path::PathBuf {
    bare_dir.join(".locks").join(format!("{name}.lock"))
}

fn open_lock_file(bare_dir: &Path, name: &str) -> Result<std::fs::File> {
    let path = lock_path(bare_dir, name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create lock dir")?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("open lock file {}", path.display()))
}

fn flock(file: &std::fs::File, exclusive_nonblocking: bool) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let op = libc::LOCK_EX | if exclusive_nonblocking { libc::LOCK_NB } else { 0 };
    // SAFETY: `file` owns the fd for the duration of the call.
    if unsafe { libc::flock(file.as_raw_fd(), op) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Take the lock if it is free right now, else `Ok(None)`.
///
/// Callers try this first so they can *say* they are waiting before they
/// block on [`lock_repo`] — a clone that sits still because another tenx has
/// the repo should not look like a clone that has hung.
pub fn try_lock_repo(bare_dir: &Path, name: &str) -> Result<Option<RepoLock>> {
    let file = open_lock_file(bare_dir, name)?;
    match flock(&file, true) {
        Ok(()) => Ok(Some(RepoLock { _file: file })),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(e) => Err(e).context("lock bare repo"),
    }
}

/// Wait for the lock and take it.
pub fn lock_repo(bare_dir: &Path, name: &str) -> Result<RepoLock> {
    let file = open_lock_file(bare_dir, name)?;
    flock(&file, false).context("lock bare repo")?;
    Ok(RepoLock { _file: file })
}

/// Return the short hash + subject of the latest commit in a bare repo, e.g. `"a1b2c3d feat: …"`.
pub fn last_commit(bare_repo_path: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["-C", &bare_repo_path.to_string_lossy(), "log", "-1", "--format=%h %s"])
        .output()
        .ok()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !s.is_empty() { Some(s) } else { None }
    } else {
        None
    }
}

/// Clone url as a bare repo into `bare_dir/<name>.git`, reporting git's own
/// progress to `on` as it runs.
///
/// Uses the system `git` so SSH agent and credential helpers work correctly.
/// `--progress` is explicit because git only volunteers progress to a tty and
/// this stderr is a pipe. Nothing is forwarded to the real terminal: the
/// caller decides how to show it, which is what keeps it out of the TUI's
/// screen. Pass `&mut |_| {}` to ignore it.
pub fn bare_clone(url: &str, bare_dir: &Path, name: &str, on: OnProgress) -> Result<()> {
    let dest = bare_repo_path(bare_dir, name);
    std::fs::create_dir_all(bare_dir).context("create bare dir")?;

    let mut cmd = Command::new("git");
    cmd.args(["clone", "--bare", "--progress", url, &dest.to_string_lossy()]);
    let (status, stderr) = run_streaming(&mut cmd, on).context("run git clone --bare")?;

    if !status.success() {
        let _ = std::fs::remove_dir_all(&dest);
        bail!("git clone --bare failed for {url}: {}", last_error(&stderr));
    }
    Ok(())
}

/// The part of git's stderr worth putting in an error message: its last
/// non-progress line.
///
/// The stream is mostly progress redraws, and the failure itself is one line
/// near the end ("fatal: ..."). Showing the whole capture would bury it under
/// percentages, and showing the literal last line would often show a redraw.
fn last_error(stderr: &str) -> String {
    tenx_core::progress::split_progress(stderr)
        .filter(|l| tenx_core::progress::parse_git_line(l).is_none())
        .rfind(|l| !l.is_empty() && *l != "remote:")
        .unwrap_or("no output")
        .to_string()
}

/// Clone if the bare repo doesn't exist yet, or fetch if it does, reporting
/// progress to `on`. Says which it did, so the caller can label the finished
/// step ("cloned" vs "fetched").
pub fn ensure_synced(url: &str, bare_dir: &Path, name: &str, on: OnProgress) -> Result<Synced> {
    let path = bare_repo_path(bare_dir, name);
    // A clone that was interrupted — Ctrl-C on the CLI, the client quitting
    // while a background job ran, a laptop lid — leaves a directory that
    // exists but is not a repository. Taking it at face value turns into a
    // silent failed fetch and then a baffling "worktree add failed"; treat it
    // as absent instead, so the next attempt simply clones again.
    if path.exists() && !is_repo(&path) {
        std::fs::remove_dir_all(&path).context("clear a half-written bare repo")?;
    }
    if path.exists() {
        // Best-effort, as before: a fetch that fails (offline, a dead remote)
        // still leaves a usable bare repo to branch from.
        let updated = fetch(&path, on).unwrap_or(false);
        Ok(if updated { Synced::Fetched } else { Synced::UpToDate })
    } else {
        bare_clone(url, bare_dir, name, on)?;
        Ok(Synced::Cloned)
    }
}

/// What [`ensure_synced_with`] ended up doing — the one word a finished step
/// shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Synced {
    Cloned,
    Fetched,
    UpToDate,
}

impl Synced {
    pub fn note(self) -> &'static str {
        match self {
            Synced::Cloned => "cloned",
            Synced::Fetched => "fetched",
            Synced::UpToDate => "up to date",
        }
    }

    /// The present-tense word for the step while it runs. A repo that is
    /// already on disk is being fetched; one that isn't is being cloned.
    pub fn verb(exists: bool) -> &'static str {
        if exists { "fetching" } else { "cloning" }
    }
}

/// Fetch all remotes of a bare repo, reporting git's own progress to `on`.
/// Returns true if any refs were updated.
/// Uses the system `git` so SSH agent and credential helpers work correctly.
pub fn fetch(bare_repo_path: &Path, on: OnProgress) -> Result<bool> {
    let mut cmd = Command::new("git");
    cmd.args(["-C", &bare_repo_path.to_string_lossy(), "fetch", "--all", "--prune", "--progress"]);
    let (status, stderr) = run_streaming(&mut cmd, on).context("run git fetch")?;

    if !status.success() {
        bail!("git fetch failed in {}: {}", bare_repo_path.display(), last_error(&stderr));
    }

    // git fetch writes ref-update lines to stderr; nothing but progress means
    // there was nothing to update. (`--quiet` used to make "empty stderr" the
    // test; with `--progress` the redraws have to be discounted first.)
    Ok(tenx_core::progress::split_progress(&stderr)
        .any(|l| tenx_core::progress::parse_git_line(l).is_none() && !l.is_empty() && l != "remote:"))
}

/// Whether `path` is a git repository git is willing to work in, as opposed to
/// a directory left behind by an interrupted clone.
fn is_repo(path: &Path) -> bool {
    Command::new("git")
        .args(["-C", &path.to_string_lossy(), "rev-parse", "--git-dir"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Detect the default branch in a bare repo (e.g. "main" or "master").
/// Bare clones store branches as refs/heads/* directly — there is no origin/HEAD.
fn default_remote_branch(bare_repo_path: &Path) -> Result<String> {
    let bare = bare_repo_path.to_string_lossy();
    // In a bare clone HEAD is a symref pointing to refs/heads/<default>.
    let out = Command::new("git")
        .args(["-C", &bare, "symbolic-ref", "HEAD", "--short"])
        .output()
        .context("read HEAD symref")?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !s.is_empty() {
            return Ok(s);
        }
    }
    // Fall back: check common branch names.
    for candidate in &["main", "master"] {
        let check = Command::new("git")
            .args(["-C", &bare, "rev-parse", "--verify", candidate])
            .output()?;
        if check.status.success() {
            return Ok(candidate.to_string());
        }
    }
    bail!("cannot determine default branch in {}", bare_repo_path.display())
}

/// Resolve the ref a new task branch should be based on: the freshly-fetched
/// remote default branch.
///
/// Depending on how the bare repo was created, "freshly fetched" lives in
/// different refs:
///   - `git clone --bare` (heads↔heads): fetch advances `refs/heads/<default>`.
///   - a mirror-style clone (`+refs/heads/*:refs/remotes/origin/*`): fetch only
///     advances `refs/remotes/origin/<default>`, while `refs/heads/<default>`
///     stays frozen at clone time and goes stale.
///
/// So prefer the remote-tracking ref (`origin/<default>`) when it exists, and
/// only fall back to the local head when there is no remote-tracking ref.
fn base_ref(bare_repo_path: &Path) -> Result<String> {
    let bare = bare_repo_path.to_string_lossy();
    let default = default_remote_branch(bare_repo_path)?;
    let remote_ref = format!("refs/remotes/origin/{default}");
    let exists = Command::new("git")
        .args(["-C", &bare, "rev-parse", "--verify", "--quiet", &remote_ref])
        .output()
        .context("check remote-tracking ref")?
        .status
        .success();
    Ok(if exists { format!("origin/{default}") } else { default })
}

/// Create a git worktree for a new task branch, always based on the
/// freshly-fetched default remote branch (e.g. main).
///
/// `-B` (re)points the branch at the default branch even if a stale local
/// branch of the same name was left behind by a previously-deleted task
/// (`task rm` removes the worktree but not the branch). It still refuses to
/// reset a branch that is currently checked out in another live worktree, so
/// active tasks are safe.
///
/// `on` is still passed the stream, but `git worktree add` has no `--progress`
/// (only `--quiet`) and reports nothing through a pipe, so in practice the
/// step stays indeterminate through the checkout. Wired up anyway: it costs
/// nothing, and a future git that does report would light it up for free.
/// Do not add `--progress` here — git rejects it and prints its usage.
pub fn add_worktree(
    bare_repo_path: &Path,
    worktree_path: &Path,
    branch_name: &str,
    on: OnProgress,
) -> Result<()> {
    let bare = bare_repo_path.to_string_lossy();
    let wt = worktree_path.to_string_lossy();
    let base = base_ref(bare_repo_path)?;
    // Drop admin entries for worktrees whose directories are gone: a job
    // killed part-way through an add leaves one behind, as does a task
    // directory deleted from outside tenx, and `worktree add` then refuses
    // the path it thinks is still registered. Only entries with no working
    // tree on disk are removed, so live worktrees are untouched.
    let _ = Command::new("git")
        .args(["-C", &bare, "worktree", "prune"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let mut cmd = Command::new("git");
    cmd.args(["-C", &bare, "worktree", "add", "-B", branch_name, &wt, &base]);
    let (status, stderr) = run_streaming(&mut cmd, on).context("run git worktree add")?;

    if !status.success() {
        bail!("git worktree add failed: {}", last_error(&stderr));
    }
    Ok(())
}

/// Remove a git worktree from the bare repo.
///
/// `force` discards uncommitted changes. Task deletion forces (the whole task is
/// going away and was confirmed), but detaching a single repo from a live task
/// does not — git's refusal to drop a dirty worktree is the safety net there.
pub fn remove_worktree(bare_repo_path: &Path, worktree_path: &Path, force: bool) -> Result<()> {
    let mut cmd = Command::new("git");
    cmd.args(["-C", &bare_repo_path.to_string_lossy(), "worktree", "remove"]);
    if force {
        cmd.arg("--force");
    }
    let out = cmd
        .arg(worktree_path)
        .output()
        .context("run git worktree remove")?;

    if !out.status.success() {
        bail!(
            "git worktree remove failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Force-delete a local branch from the bare repo, if it exists.
///
/// Called after the worktree is removed so leftover task branches don't
/// accumulate in the bare repo. A missing branch is not an error.
pub fn delete_branch(bare_repo_path: &Path, branch_name: &str) -> Result<()> {
    let out = Command::new("git")
        .args(["-C", &bare_repo_path.to_string_lossy(), "branch", "-D", branch_name])
        .output()
        .context("run git branch -D")?;

    if out.status.success() {
        return Ok(());
    }
    // A branch that was never created (e.g. worktree add failed) is fine to skip.
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("not found") {
        return Ok(());
    }
    bail!("git branch -D {branch_name} failed: {}", stderr.trim());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("tenx-git-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Wait up to `ms` for the lock to become free.
    ///
    /// Not impatience: on macOS, under a loaded, multi-threaded test binary,
    /// `close()` can return before the kernel has made the release visible to
    /// the next `flock(LOCK_NB)` — observed failing on the first attempt and
    /// succeeding 10ms later. Production never depends on an instant
    /// same-process re-acquire: `try_lock_repo` is only ever "can I have it
    /// right now, or should I say I'm waiting?", and the answer it leads to is
    /// a *blocking* `lock_repo`, which the kernel wakes on release. A
    /// transient false "busy" there costs a "waiting for" line, nothing more.
    fn lock_within(dir: &Path, name: &str, ms: u64) -> Option<RepoLock> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        loop {
            if let Some(lock) = try_lock_repo(dir, name).unwrap() {
                return Some(lock);
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// `flock` is held per open file description, not per process, so two
    /// separate opens contend even inside one process — which is what makes
    /// the exclusion testable here and what makes it work across clients.
    #[test]
    fn a_repo_lock_excludes_a_second_holder() {
        let dir = tmpdir("excl");
        let first = lock_repo(&dir, "acme").unwrap();
        // Held: a second holder must be refused, and must stay refused. A
        // generous window, because the interesting failure would be the lock
        // quietly becoming available, not it taking a moment.
        assert!(
            lock_within(&dir, "acme", 100).is_none(),
            "a second holder must not get the lock while the first has it"
        );
        // A different repo in the same workspace is a different lock.
        assert!(try_lock_repo(&dir, "other").unwrap().is_some());
        drop(first);
        assert!(
            lock_within(&dir, "acme", 2000).is_some(),
            "dropping the guard releases the lock"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_waiting_holder_gets_the_lock_when_it_is_released() {
        use std::sync::mpsc;
        use std::time::Duration;
        let dir = tmpdir("wait");
        let held = lock_repo(&dir, "acme").unwrap();

        let (tx, rx) = mpsc::channel();
        let d = dir.clone();
        let waiter = std::thread::spawn(move || {
            let _lock = lock_repo(&d, "acme").unwrap();
            tx.send(()).unwrap();
        });

        // It must still be waiting while the first guard lives.
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err(), "waiter got the lock too early");
        drop(held);
        rx.recv_timeout(Duration::from_secs(5)).expect("waiter never got the lock");
        waiter.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The lock file is a handle, not repo content: it must never land inside
    /// the bare repo where git would see it.
    #[test]
    fn the_lock_file_sits_outside_the_repo() {
        let dir = tmpdir("path");
        let lock = lock_path(&dir, "acme");
        let repo = bare_repo_path(&dir, "acme");
        assert!(!lock.starts_with(&repo), "{lock:?} is inside {repo:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_half_written_bare_repo_is_not_mistaken_for_a_repo() {
        let dir = tmpdir("isrepo");
        let fake = dir.join("acme.git");
        std::fs::create_dir_all(fake.join("objects")).unwrap();
        std::fs::write(fake.join("HEAD"), "garbage").unwrap();
        assert!(!is_repo(&fake), "an interrupted clone must not pass as a repository");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
