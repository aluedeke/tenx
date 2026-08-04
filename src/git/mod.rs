use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

pub fn bare_repo_path(bare_dir: &Path, name: &str) -> std::path::PathBuf {
    bare_dir.join(format!("{}.git", name))
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

/// Clone url as a bare repo into `bare_dir/<name>.git`.
/// Uses the system `git` so SSH agent and credential helpers work correctly.
/// Output is captured (not forwarded) so it doesn't bleed into the TUI.
pub fn bare_clone(url: &str, bare_dir: &Path, name: &str) -> Result<()> {
    let dest = bare_repo_path(bare_dir, name);
    std::fs::create_dir_all(bare_dir).context("create bare dir")?;

    let out = Command::new("git")
        .args(["clone", "--bare", "--quiet", url, &dest.to_string_lossy()])
        .output()
        .context("run git clone --bare")?;

    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&dest);
        bail!(
            "git clone --bare failed for {url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Clone if the bare repo doesn't exist yet, or fetch if it does.
pub fn ensure_synced(url: &str, bare_dir: &Path, name: &str) -> Result<()> {
    let path = bare_repo_path(bare_dir, name);
    if path.exists() {
        fetch(&path).ok(); // best-effort; errors are non-fatal here
    } else {
        bare_clone(url, bare_dir, name)?;
    }
    Ok(())
}

/// Fetch all remotes of a bare repo. Returns true if any refs were updated.
/// Uses the system `git` so SSH agent and credential helpers work correctly.
/// Output is captured (not forwarded) so it doesn't bleed into the TUI.
pub fn fetch(bare_repo_path: &Path) -> Result<bool> {
    let out = Command::new("git")
        .args(["-C", &bare_repo_path.to_string_lossy(), "fetch", "--all", "--prune", "--quiet"])
        .output()
        .context("run git fetch")?;

    if !out.status.success() {
        bail!(
            "git fetch failed in {}: {}",
            bare_repo_path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    // git fetch writes ref-update lines to stderr; empty = already up-to-date.
    Ok(!out.stderr.is_empty())
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
pub fn add_worktree(bare_repo_path: &Path, worktree_path: &Path, branch_name: &str) -> Result<()> {
    let bare = bare_repo_path.to_string_lossy();
    let wt = worktree_path.to_string_lossy();
    let base = base_ref(bare_repo_path)?;
    let out = Command::new("git")
        .args(["-C", &bare, "worktree", "add", "-B", branch_name, &wt, &base])
        .output()
        .context("run git worktree add")?;

    if !out.status.success() {
        bail!(
            "git worktree add failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
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
