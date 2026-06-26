use anyhow::{Context, Result};
use git2::{FetchOptions, RemoteCallbacks, Repository};
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;

pub fn bare_repo_path(bare_dir: &Path, name: &str) -> std::path::PathBuf {
    bare_dir.join(format!("{}.git", name))
}

/// Clone url as a bare repo into `bare_dir/<name>.git`.
pub fn bare_clone(url: &str, bare_dir: &Path, name: &str) -> Result<()> {
    let dest = bare_repo_path(bare_dir, name);
    std::fs::create_dir_all(bare_dir).context("create bare dir")?;

    let mut callbacks = RemoteCallbacks::new();
    callbacks.transfer_progress(|stats| {
        if stats.received_objects() == stats.total_objects() {
            eprint!(
                "\rResolving deltas {}/{}",
                stats.indexed_deltas(),
                stats.total_deltas()
            );
        } else if stats.total_objects() > 0 {
            eprint!(
                "\rReceived {}/{} objects ({}) in {} bytes",
                stats.received_objects(),
                stats.total_objects(),
                stats.indexed_objects(),
                stats.received_bytes()
            );
        }
        let _ = io::stderr().flush();
        true
    });

    let mut fo = FetchOptions::new();
    fo.remote_callbacks(callbacks);

    let mut builder = git2::build::RepoBuilder::new();
    builder.bare(true);
    builder.fetch_options(fo);
    builder.clone(url, &dest).with_context(|| format!("clone {url}"))?;

    eprintln!();
    Ok(())
}

/// Fetch all remotes of a bare repo. Returns true if new objects were received.
pub fn fetch(bare_repo_path: &Path) -> Result<bool> {
    let repo = Repository::open(bare_repo_path)
        .with_context(|| format!("open bare repo {}", bare_repo_path.display()))?;

    let remotes = repo.remotes()?;
    let mut updated = false;
    for name in remotes.iter().flatten() {
        let mut remote = repo.find_remote(name)?;
        let tips_updated = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tips_updated_cb = tips_updated.clone();
        let mut callbacks = RemoteCallbacks::new();
        callbacks.transfer_progress(|stats| {
            if stats.received_objects() > 0 {
                eprint!(
                    "\r  {}: {}/{}",
                    name,
                    stats.received_objects(),
                    stats.total_objects()
                );
                let _ = io::stderr().flush();
            }
            true
        });
        callbacks.update_tips(move |_, _, _| {
            tips_updated_cb.store(true, std::sync::atomic::Ordering::Relaxed);
            true
        });
        let mut fo = FetchOptions::new();
        fo.remote_callbacks(callbacks);
        match remote.fetch(&[] as &[&str], Some(&mut fo), None) {
            Ok(_) => {
                eprintln!();
                if tips_updated.load(std::sync::atomic::Ordering::Relaxed) {
                    updated = true;
                }
            }
            Err(e) if e.code() == git2::ErrorCode::NotFound => {}
            Err(e) => return Err(e).context(format!("fetch {name}")),
        }
    }
    Ok(updated)
}

/// Create a git worktree from the bare repo, branching off origin/main.
pub fn add_worktree(bare_repo_path: &Path, worktree_path: &Path, branch_name: &str) -> Result<()> {
    let check = Command::new("git")
        .args(["-C", &bare_repo_path.to_string_lossy(), "branch", "--list", branch_name])
        .output()
        .context("check branch")?;

    let bare = bare_repo_path.to_string_lossy();
    let wt = worktree_path.to_string_lossy();
    let status = if check.stdout.is_empty() {
        Command::new("git")
            .args(["-C", &bare, "worktree", "add", "-b", branch_name, &wt, "origin/main"])
            .status()
    } else {
        Command::new("git")
            .args(["-C", &bare, "worktree", "add", &wt, branch_name])
            .status()
    }
    .context("run git worktree add")?;

    if !status.success() {
        anyhow::bail!("git worktree add failed for {}", worktree_path.display());
    }
    Ok(())
}

/// Remove a git worktree from the bare repo.
pub fn remove_worktree(bare_repo_path: &Path, worktree_path: &Path) -> Result<()> {
    let status = Command::new("git")
        .args(["-C", &bare_repo_path.to_string_lossy(), "worktree", "remove", "--force"])
        .arg(worktree_path)
        .status()
        .context("run git worktree remove")?;

    if !status.success() {
        anyhow::bail!("git worktree remove failed for {}", worktree_path.display());
    }
    Ok(())
}
