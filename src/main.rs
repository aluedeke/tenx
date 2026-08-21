mod cli;
mod git;
mod palette;
mod progress;
mod tui;
mod workspace;
mod zellij;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands, HooksCommands, RepoCommands, TaskCommands};
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
        None => open()?,

        Some(Commands::Overlay { home, json }) => {
            if json {
                tui::dump_json()?;
            } else {
                tui::run_overlay(home)?;
            }
        }

        Some(Commands::Init { name }) => {
            cli::init::run(name.as_deref())?;
        }

        Some(Commands::Standup { since }) => {
            cli::standup::run(since.as_deref())?;
        }

        Some(Commands::Repo { command }) => match command {
            RepoCommands::Add { url, name, ws_dir } => {
                cli::repo::add(&url, name.as_deref(), ws_dir.as_deref())?;
            }
            RepoCommands::List => {
                cli::repo::list()?;
            }
            RepoCommands::Fetch { name } => {
                cli::repo::fetch(name.as_deref())?;
            }
        },

        Some(Commands::Hooks { command }) => match command {
            HooksCommands::Install => {
                cli::hooks::install()?;
            }
        },

        Some(Commands::Watch) => cli::watch::run()?,


        Some(Commands::Task { command }) => match command {
            TaskCommands::New { name, repos, no_open, ws_dir } => match ws_dir {
                Some(dir) => cli::task::new_by_dir(&dir, &name, repos.as_deref())?,
                None => cli::task::new(&name, repos.as_deref(), no_open)?,
            },
            TaskCommands::AddRepo { name, repos, ws_dir } => {
                cli::task::add_repo(ws_dir.as_deref(), &name, &repos)?;
            }
            TaskCommands::RmRepo { name, repos, force, ws_dir } => {
                cli::task::rm_repo(ws_dir.as_deref(), &name, &repos, force)?;
            }
            TaskCommands::SetRepos { name, repos, force, ws_dir } => {
                cli::task::set_repos(ws_dir.as_deref(), &name, &repos, force)?;
            }
            TaskCommands::Open { name, ws_dir } => match ws_dir {
                Some(dir) => cli::task::open_by_dir(&dir, &name)?,
                None => cli::task::open(&name)?,
            },
            TaskCommands::Rename { name, title, ws_dir } => {
                cli::task::rename(ws_dir.as_deref(), &name, &title)?;
            }
            TaskCommands::List => {
                cli::task::list()?;
            }
            TaskCommands::Rm { name, force, ws_dir } => match ws_dir {
                Some(dir) => cli::task::rm_by_dir(&dir, &name)?,
                None => cli::task::rm(&name, force)?,
            },
        },
    }

    Ok(())
}

/// Connect to the single global tenx session, regardless of cwd: attach (or
/// create) it from a plain terminal, switch to it in place from a foreign
/// zellij session, or run the overlay directly when already inside it. If cwd
/// is inside a workspace, self-heal the registry first so it shows up in the
/// overlay.
fn open() -> Result<()> {
    let cwd = env::current_dir()?;
    if let Some(ws) = workspace::find_opt(&cwd)? {
        let _ = workspace::register_workspace(&ws.dir);
    }

    let bin = env::current_exe()?;
    let bin_str = bin.to_string_lossy().into_owned();

    // Every route into the session lands here, so this is the one place that
    // guarantees a watcher exists. No-op when one is already running.
    cli::watch::ensure_running(&bin);

    match zellij::current_session().as_deref() {
        // Already inside the tenx session → run the overlay in this pane.
        Some(zellij::SESSION) => tui::run_overlay(false)?,
        // Inside a different zellij session → switch the client in place
        // (creating the tenx session from the home layout if needed).
        Some(_) => zellij::switch_to_tenx_session(&bin_str)?,
        // Outside zellij entirely → attach, creating the session if missing.
        None => {
            if zellij::session_exists(zellij::SESSION)? {
                zellij::attach_session(zellij::SESSION)?;
            } else {
                eprintln!("  creating session '{}'", zellij::SESSION);
                zellij::create_and_attach_session(&bin_str)?;
            }
        }
    }

    Ok(())
}
