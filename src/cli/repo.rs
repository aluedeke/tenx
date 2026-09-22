use anyhow::{bail, Result};
use std::env;
use std::path::Path;

pub(crate) fn infer_name(url: &str) -> String {
    url.rsplit('/')
        .next()
        .unwrap_or(url)
        .trim_end_matches(".git")
        .to_string()
}

/// `ws_dir` selects the workspace directly (a front end shells
/// out from an arbitrary cwd and has no other way to name a workspace);
/// without it, the workspace is found by walking up from cwd, same as every
/// other bare `tenx` invocation.
pub fn add(url: &str, name: Option<&str>, ws_dir: Option<&str>) -> Result<()> {
    let mut ws = match ws_dir {
        Some(dir) => crate::workspace::load(Path::new(dir))?,
        None => crate::workspace::find(&env::current_dir()?)?,
    };
    add_in(&mut ws, url, name, crate::progress::for_cli().as_ref())
}

/// Add a repo to an explicit workspace (bare clone + config). Used by `add` and
/// the column's Repos tab, which targets the selected repo's workspace.
pub fn add_in(
    ws: &mut crate::workspace::Workspace,
    url: &str,
    name: Option<&str>,
    rep: &dyn crate::progress::Reporter,
) -> Result<()> {
    use crate::progress::Event;
    let global = crate::workspace::load_global()?;

    let repo_name = name.map(|s| s.to_string()).unwrap_or_else(|| infer_name(url));
    let bare_dir = ws.bare_dir(&global);
    let bare_path = crate::git::bare_repo_path(&bare_dir, &repo_name);

    if bare_path.exists() {
        bail!("bare repo already exists at {}", bare_path.display());
    }

    rep.emit(Event::Start { step: 0, label: repo_name.clone(), verb: "cloning" });
    let _lock = crate::git::lock_repo(&bare_dir, &repo_name)?;
    let mut on = |snap| rep.emit(Event::Update { step: 0, snap });
    if let Err(e) = crate::git::bare_clone(url, &bare_dir, &repo_name, &mut on) {
        rep.emit(Event::Failed { step: 0, err: e.to_string() });
        return Err(e);
    }
    rep.emit(Event::Done { step: 0, note: "cloned".into() });

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

    println!("{:<20} {:<50} BARE", "NAME", "URL");
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

    // One reported step per repo, like every other multi-repo operation, so
    // a fetch of a large repo shows what it is doing instead of a bare "...".
    let rep = crate::progress::for_cli();
    for (step, repo) in repos.iter().enumerate() {
        let bare_path = crate::git::bare_repo_path(&bare_dir, &repo.name);
        if !bare_path.exists() {
            eprintln!("! repo '{}' not cloned yet — run: tenx repo add {}", repo.name, repo.url);
            continue;
        }
        rep.emit(crate::progress::Event::Start { step, label: repo.name.clone(), verb: "fetching" });
        let mut on = |snap| rep.emit(crate::progress::Event::Update { step, snap });
        match crate::git::fetch(&bare_path, &mut on) {
            Ok(updated) => {
                let note = if updated { "updated" } else { "up to date" };
                rep.emit(crate::progress::Event::Done { step, note: note.into() });
            }
            Err(e) => rep.emit(crate::progress::Event::Failed { step, err: e.to_string() }),
        }
    }
    Ok(())
}
