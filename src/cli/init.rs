use anyhow::{Context, Result};
use std::env;
use std::io::{self, BufRead, Write};

pub fn run(name: Option<&str>) -> Result<()> {
    let cwd = env::current_dir()?;
    let (ws_dir, ws_name) = match name {
        None => {
            // No name: work in cwd, use cwd folder name as workspace name
            let n = cwd
                .file_name()
                .context("current directory has no name")?
                .to_string_lossy()
                .into_owned();
            (cwd.clone(), n)
        }
        Some(n) => {
            // Name given: create a new subdirectory
            (cwd.join(n), n.to_string())
        }
    };

    eprintln!("Initializing workspace '{ws_name}'");
    eprintln!();

    // Prompt for repos
    let repos = prompt_repos()?;

    // Prompt for layout file
    let layout = prompt_layout()?;

    // Create workspace and fill in config
    let mut ws = crate::workspace::init(&ws_dir, &ws_name)?;
    ws.config.layout = layout;
    for repo in repos {
        ws.config.repos.push(repo);
    }
    ws.save_config()?;

    eprintln!();
    eprintln!("✓ workspace '{}' created at {}", ws.config.name, ws.dir.display());
    if ws.dir != cwd {
        eprintln!("  cd {}", ws.dir.display());
    }
    if !ws.config.repos.is_empty() {
        eprintln!("  {} repo(s) configured — run: tenx repo fetch", ws.config.repos.len());
    }
    Ok(())
}

fn prompt_repos() -> Result<Vec<crate::workspace::RepoConfig>> {
    let mut repos = Vec::new();
    eprintln!("Repos (enter a git URL per line, empty line to finish):");
    loop {
        let url = prompt("  URL")?;
        if url.is_empty() {
            break;
        }
        let default_name = infer_name(&url);
        let input = prompt(&format!("  Name [{default_name}]"))?;
        let name = if input.is_empty() { default_name } else { input };
        repos.push(crate::workspace::RepoConfig { name, url });
    }
    Ok(repos)
}

fn prompt_layout() -> Result<String> {
    eprintln!("Zellij layout file for task tabs (optional, enter to use built-in default):");
    let input = prompt("  Layout KDL path")?;
    Ok(input)
}

fn prompt(label: &str) -> Result<String> {
    let mut stdout = io::stdout();
    write!(stdout, "{label}: ")?;
    stdout.flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

fn infer_name(url: &str) -> String {
    url.rsplit('/')
        .next()
        .unwrap_or(url)
        .trim_end_matches(".git")
        .to_string()
}
