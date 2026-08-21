use anyhow::{bail, Result};
use std::env;
use std::path::Path;

fn infer_name(url: &str) -> String {
    url.rsplit('/')
        .next()
        .unwrap_or(url)
        .trim_end_matches(".git")
        .to_string()
}

/// `ws_dir` selects the workspace directly (the zellij overlay plugin shells
/// out from an arbitrary cwd and has no other way to name a workspace);
/// without it, the workspace is found by walking up from cwd, same as every
/// other bare `tenx` invocation.
pub fn add(url: &str, name: Option<&str>, ws_dir: Option<&str>) -> Result<()> {
    let mut ws = match ws_dir {
        Some(dir) => crate::workspace::load(Path::new(dir))?,
        None => crate::workspace::find(&env::current_dir()?)?,
    };
    add_in(&mut ws, url, name)
}

/// Add a repo to an explicit workspace (bare clone + config). Used by `add` and
/// the overlay's Repos tab, which targets the selected repo's workspace.
pub fn add_in(ws: &mut crate::workspace::Workspace, url: &str, name: Option<&str>) -> Result<()> {
    let global = crate::workspace::load_global()?;

    let repo_name = name.map(|s| s.to_string()).unwrap_or_else(|| infer_name(url));
    let bare_dir = ws.bare_dir(&global);
    let bare_path = crate::git::bare_repo_path(&bare_dir, &repo_name);

    if bare_path.exists() {
        bail!("bare repo already exists at {}", bare_path.display());
    }

    crate::git::bare_clone(url, &bare_dir, &repo_name)?;

    ws.add_repo(crate::workspace::RepoConfig {
        name: repo_name.clone(),
        url: url.to_string(),
    })?;

    Ok(())
}

pub fn list() -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = crate::workspace::find(&cwd)?;
    let global = crate::workspace::load_global()?;
    let bare_dir = ws.bare_dir(&global);
    let cloned = crate::workspace::cloned_repos(&bare_dir, &ws.config.repos);

    println!("{:<20} {:<50} {}", "NAME", "URL", "BARE");
    println!("{}", "-".repeat(75));
    for repo in &ws.config.repos {
        let mark = if cloned.contains(&repo.name) { "✓" } else { "✗" };
        println!("{:<20} {:<50} {}", repo.name, repo.url, mark);
    }
    Ok(())
}

pub fn fetch(name: Option<&str>) -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = crate::workspace::find(&cwd)?;
    let global = crate::workspace::load_global()?;
    let bare_dir = ws.bare_dir(&global);

    let repos: Vec<_> = match name {
        Some(n) => {
            let r = ws.find_repo(n).ok_or_else(|| {
                crate::workspace::WorkspaceError::RepoNotFound(n.to_string())
            })?;
            vec![r.clone()]
        }
        None => ws.config.repos.clone(),
    };

    for repo in &repos {
        let bare_path = crate::git::bare_repo_path(&bare_dir, &repo.name);
        if !bare_path.exists() {
            eprintln!("! repo '{}' not cloned yet — run: tenx repo add {}", repo.name, repo.url);
            continue;
        }
        eprint!("  fetching {} ... ", repo.name);
        match crate::git::fetch(&bare_path) {
            Ok(true) => eprintln!("✓ updated"),
            Ok(false) => eprintln!("✓ up to date"),
            Err(e) => eprintln!("✗ {}", e),
        }
    }
    Ok(())
}
