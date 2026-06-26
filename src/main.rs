mod cli;
mod git;
mod tui;
mod workspace;
mod zellij;

use anyhow::{bail, Result};
use clap::Parser;
use cli::{Cli, Commands, RepoCommands, TaskCommands};
use std::env;

fn main() {
    if let Err(e) = run() {
        eprintln!("tenx: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None | Some(Commands::Tui) | Some(Commands::Tasks) => open_workspace()?,

        Some(Commands::Repos) => {
            let cwd = env::current_dir()?;
            let ws = workspace::find(&cwd)?;
            tui::run_repos(ws)?;
        }

        Some(Commands::Init { name }) => {
            cli::init::run(name.as_deref())?;
        }

        Some(Commands::Repo { command }) => match command {
            RepoCommands::Add { url, name } => {
                cli::repo::add(&url, name.as_deref())?;
            }
            RepoCommands::List => {
                cli::repo::list()?;
            }
            RepoCommands::Fetch { name } => {
                cli::repo::fetch(name.as_deref())?;
            }
        },

        Some(Commands::Task { command }) => match command {
            TaskCommands::New { name, repos, no_open } => {
                cli::task::new(&name, repos.as_deref(), no_open)?;
            }
            TaskCommands::Open { name } => {
                cli::task::open(&name)?;
            }
            TaskCommands::List => {
                cli::task::list()?;
            }
            TaskCommands::Rm { name, force } => {
                cli::task::rm(&name, force)?;
            }
        },
    }

    Ok(())
}

/// Open the workspace: run the TUI if already in the right session, otherwise
/// create or attach to the workspace's zellij session first.
fn open_workspace() -> Result<()> {
    let cwd = env::current_dir()?;
    let ws = workspace::find(&cwd)?;
    let session = zellij::session_name(&ws.config.name);

    match zellij::current_session().as_deref() {
        // Inside this workspace's session → run TUI directly in the current pane
        Some(name) if name == session => {
            tui::run(ws)?;
        }
        // Inside a different zellij session → refuse
        Some(other) => {
            bail!(
                "already inside zellij session '{other}' — detach first (Ctrl+o d)"
            );
        }
        // Outside zellij entirely → create or attach to the workspace session
        None => {
            let bin = env::current_exe()?;
            let bin_str = bin.to_string_lossy().into_owned();
            let ws_dir = ws.dir.to_string_lossy().into_owned();
            if zellij::session_exists(&session)? {
                zellij::attach_session(&session)?;
            } else {
                eprintln!("  creating session '{session}'");
                zellij::create_and_attach_session(&session, &bin_str, &ws_dir)?;
            }
        }
    }

    Ok(())
}
