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
        None => {
            let cwd = env::current_dir()?;
            let ws = workspace::find(&cwd)?;
            let session = zellij::session_name(&ws.config.name);

            match zellij::current_session().as_deref() {
                // Already inside this workspace's session → open/switch to TUI tab
                Some(name) if name == session => {
                    let bin = env::current_exe()?;
                    zellij::open_tui_tab(&bin.to_string_lossy(), &ws.dir.to_string_lossy())?;
                }
                // Inside a different session → tell the user rather than clobber their session
                Some(other) => {
                    bail!(
                        "currently inside session '{other}'; detach first (Ctrl+o d), \
                         then run tenx to join session '{session}'"
                    );
                }
                // Outside any session → create or attach
                None => {
                    let bin = env::current_exe()?;
                    if zellij::session_exists(&session)? {
                        eprintln!("  attaching to session '{session}'");
                        zellij::attach_session(&session)?;
                    } else {
                        eprintln!("  creating session '{session}'");
                        zellij::create_and_attach_session(
                            &session,
                            &bin.to_string_lossy(),
                            &ws.dir.to_string_lossy(),
                        )?;
                    }
                }
            }
        }

        Some(Commands::Init { name }) => {
            cli::init::run(name.as_deref())?;
        }

        Some(Commands::Tui) => {
            // Invoked by the session layout's pane command — run TUI directly
            let cwd = env::current_dir()?;
            let ws = workspace::find(&cwd)?;
            tui::run(ws)?;
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
