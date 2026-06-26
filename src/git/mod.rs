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

/// Create a git worktree from the bare repo, branching off the default remote branch.
pub fn add_worktree(bare_repo_path: &Path, worktree_path: &Path, branch_name: &str) -> Result<()> {
    let check = Command::new("git")
        .args(["-C", &bare_repo_path.to_string_lossy(), "branch", "--list", branch_name])
        .output()
        .context("check branch")?;

    let bare = bare_repo_path.to_string_lossy();
    let wt = worktree_path.to_string_lossy();
    let out = if check.stdout.is_empty() {
        let base = default_remote_branch(bare_repo_path)?;
        Command::new("git")
            .args(["-C", &bare, "worktree", "add", "-b", branch_name, &wt, &base])
            .output()
    } else {
        Command::new("git")
            .args(["-C", &bare, "worktree", "add", &wt, branch_name])
            .output()
    }
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
pub fn remove_worktree(bare_repo_path: &Path, worktree_path: &Path) -> Result<()> {
    let out = Command::new("git")
        .args(["-C", &bare_repo_path.to_string_lossy(), "worktree", "remove", "--force"])
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
