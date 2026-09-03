mod cli;
mod git;
mod live;
mod palette;
mod progress;
mod tmux;
mod tui;
mod workspace;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands, HooksCommands, InternalCommands, RepoCommands, SecretsCommands, TaskCommands};
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

        Some(Commands::Internal { command }) => match command {
            InternalCommands::TmuxConf => {
                let bin = env::current_exe()?;
                print!("{}", tmux::render_config(&bin.to_string_lossy()));
            }
            InternalCommands::Ports => {
                println!("{}", serde_json::to_string(&live::ports_by_window())?);
            }
            InternalCommands::AgentLog { cwd, pid } => cli::agentlog::run(&cwd, pid)?,
        },

        Some(Commands::Secrets { command }) => match command {
            SecretsCommands::Init => cli::secrets::init()?,
            SecretsCommands::Encrypt { task, file } => cli::secrets::encrypt(&task, &file)?,
            SecretsCommands::Set { name } => cli::secrets::set(&name)?,
            SecretsCommands::Decrypt { name } => cli::secrets::decrypt(name.as_deref())?,
            SecretsCommands::Fulfill => cli::secrets::fulfill()?,
            SecretsCommands::Status => cli::secrets::status()?,
        },


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
            TaskCommands::Pin { name, ws_dir } => {
                cli::task::pin(ws_dir.as_deref(), &name)?;
            }
            TaskCommands::Unpin { name, ws_dir } => {
                cli::task::unpin(ws_dir.as_deref(), &name)?;
            }
            TaskCommands::Sweep { after, dry_run } => {
                let after = after.as_deref().map(cli::task::parse_duration).transpose()?;
                cli::task::sweep(after, dry_run)?;
            }
        },
    }

    Ok(())
}

/// Connect to the single global tenx session, regardless of cwd: attach (or
/// create) it from a plain terminal, or run the overlay directly when already
/// inside it. If cwd is inside a workspace, self-heal the registry first so it
/// shows up in the overlay.
fn open() -> Result<()> {
    let cwd = env::current_dir()?;
    if let Some(ws) = workspace::find_opt(&cwd)? {
        let _ = workspace::register_workspace(&ws.dir);
    }

    let bin = env::current_exe()?;
    let bin_str = bin.to_string_lossy().into_owned();

    tmux::check_version()?;

    // Every route into the session lands here, so this is the one place that
    // guarantees a watcher exists. No-op when one is already running.
    cli::watch::ensure_running(&bin);

    if tmux::inside_tenx_session() {
        // Already inside the tenx session → run the overlay in this pane.
        tui::run_overlay(false)?;
    } else if tmux::inside_any_tmux() {
        // A client of some other tmux server: no in-place switch exists.
        anyhow::bail!("{}", tmux::foreign_client_hint());
    } else {
        // Outside tmux entirely → attach, creating the server/session if
        // missing (exec; does not return on success).
        if !tmux::server_running() {
            eprintln!("  creating session '{}'", tmux::SESSION);
        }
        tmux::attach_or_create(&bin_str)?;
    }

    Ok(())
}
