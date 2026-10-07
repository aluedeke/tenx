mod agent;
mod cli;
mod git;
mod live;
mod palette;
mod progress;
mod snapshot;
mod tmux;
mod tui;
mod web;
mod workspace;

use anyhow::Result;
use clap::Parser;
use cli::{AgentCommands, Cli, Commands, HooksCommands, InternalCommands, RepoCommands, SecretsCommands, TaskCommands};
use std::env;

fn main() {
    if let Err(e) = run() {
        eprintln!("tenx: {e}");
        let code = e.downcast_ref::<cli::secrets::Exit>().map_or(1, |x| x.code);
        std::process::exit(code);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    // A workspace command run from inside a workspace re-registers it: one
    // that was moved after `tenx init` has lost its entry (see
    // `register_enclosing`). Hooks (`internal`) run far too often for this.
    if matches!(cli.command, None | Some(Commands::Repo { .. }) | Some(Commands::Task { .. })) {
        if let Ok(cwd) = env::current_dir() {
            workspace::register_enclosing(&cwd);
        }
    }

    match cli.command {
        None => open()?,

        Some(Commands::Ask { prompt, agent, ws_dir, no_focus }) => {
            let agent = agent.as_deref().map(agent::AgentKind::from_token);
            let open = cli::task::OpenMode::for_cli(false, no_focus);
            cli::adhoc::ask(&prompt.join(" "), ws_dir.as_deref(), agent, open)?;
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

        Some(Commands::Web { command: Some(cli::WebCommand::Service { action }), .. }) => match action {
            cli::WebServiceAction::Install { listen, port, dev_origin } => web::service::install(&listen, port, &dev_origin)?,
            cli::WebServiceAction::Restart => web::service::restart()?,
            cli::WebServiceAction::Uninstall => web::service::uninstall()?,
            cli::WebServiceAction::Status => web::service::status()?,
        },
        Some(Commands::Web { command: None, listen, port, open, rotate_token, dev_origin }) => {
            web::run(web::Options { listen, port, open, rotate_token, dev_origins: dev_origin })?
        }

        Some(Commands::Agent { command }) => match command {
            AgentCommands::Setup { kind, check } => {
                cli::session_event::setup(&kind, check)?;
            }
        },

        Some(Commands::Doctor { reset_skills }) => cli::doctor::run(reset_skills)?,

        Some(Commands::Internal { command }) => match command {
            InternalCommands::TmuxConf => print!("{}", tmux::render_config()),
            InternalCommands::Ports => {
                println!("{}", serde_json::to_string(&live::ports_by_window())?);
            }
            InternalCommands::SessionEvent { agent, pid } => cli::session_event::run(&agent, pid),
            InternalCommands::OpenAgent { pid, label, agent_type, nth, peers } => {
                cli::agentview::run(pid, label.as_deref(), agent_type.as_deref(), nth, peers)?
            }
            InternalCommands::AgentLog { cwd, pid, session, agent, transcript, title, viewer } => {
                cli::agentlog::run(cli::agentlog::Follow {
                    cwd: &cwd,
                    pid,
                    session: session.as_deref(),
                    agent: &agent,
                    transcript: transcript.as_deref(),
                    title: title.as_deref(),
                    viewer,
                })?
            }
        },

        Some(Commands::Secrets { command }) => match command {
            SecretsCommands::Init => cli::secrets::init()?,
            SecretsCommands::Encrypt { task, file } => cli::secrets::encrypt(&task, &file)?,
            SecretsCommands::Need { names, why, no_wait, timeout } => {
                cli::secrets::need(&names, why.as_deref(), secrets_wait(no_wait, timeout.as_deref())?)?
            }
            SecretsCommands::Set { name, no_wait, timeout } => {
                cli::secrets::set(&name, secrets_wait(no_wait, timeout.as_deref())?)?
            }
            SecretsCommands::Decrypt { name, no_wait, timeout } => {
                cli::secrets::decrypt(name.as_deref(), secrets_wait(no_wait, timeout.as_deref())?)?
            }
            SecretsCommands::Fulfill { hold } => cli::secrets::fulfill(hold)?,
            SecretsCommands::Deny { names, note } => cli::secrets::deny(&names, note.as_deref())?,
            SecretsCommands::Cancel { name, all: _ } => cli::secrets::cancel(name.as_deref())?,
            SecretsCommands::Status => cli::secrets::status()?,
        },


        Some(Commands::Task { command }) => match command {
            TaskCommands::New { name, repos, description, links, no_open, no_focus, no_repos, adhoc, prompt, agent, ws_dir } => {
                let links = links
                    .iter()
                    .map(|l| tenx_core::taskmd::parse_link(l).ok_or_else(|| anyhow::anyhow!("--link wants \"Label: value\", got {l:?}")))
                    .collect::<Result<Vec<_>>>()?;
                let md = cli::task::TaskMd {
                    description: description.as_deref().unwrap_or(""),
                    links: &links,
                    prompt: prompt.as_deref().unwrap_or(""),
                };
                let agent = agent.as_deref().map(agent::AgentKind::from_token);
                let open = cli::task::OpenMode::for_cli(no_open, no_focus);
                let none: Vec<String> = Vec::new();
                let repos = if no_repos || adhoc { Some(none.as_slice()) } else { repos.as_deref() };
                let ws = match (adhoc, ws_dir.as_deref()) {
                    (true, _) => cli::adhoc::ensure()?,
                    (false, Some(dir)) => cli::task::load_ws_arg(dir)?,
                    (false, None) => workspace::find(&env::current_dir()?)?,
                };
                let slug = cli::task::new_with(&ws, &name, repos, open, &md, agent, progress::for_cli().as_ref())?;
                // The slug is what `send`/`wait`/`output` take; an adhoc
                // one may have been counted up from the title.
                println!("{slug}");
                // Return once the first turn is under way, so a `task wait`
                // right after waits for it rather than for nothing.
                if !md.prompt.is_empty() && open != cli::task::OpenMode::Closed && tmux::server_running() {
                    cli::drive::await_turn(&ws.tasks_dir().join(&slug), cli::drive::TURN_LIMIT);
                }
            }
            TaskCommands::Send { name, text, force, ws_dir } => {
                let text = if text.len() == 1 && text[0] == "-" {
                    let mut buf = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
                    buf
                } else {
                    text.join(" ")
                };
                cli::drive::send(ws_dir.as_deref(), &name, &text, force)?;
            }
            TaskCommands::Wait { name, timeout, ws_dir } => {
                cli::drive::wait(ws_dir.as_deref(), &name, cli::task::parse_duration(&timeout)?)?;
            }
            TaskCommands::Output { name, json, ws_dir } => {
                cli::drive::output(ws_dir.as_deref(), &name, json)?;
            }
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
            TaskCommands::List { json } => {
                if json {
                    tui::dump_json()?;
                } else {
                    cli::task::list()?;
                }
            }
            TaskCommands::Rm { name, force, ws_dir } => match ws_dir {
                Some(dir) => cli::task::rm_by_dir(&dir, &name)?,
                None => cli::task::rm(&name, force)?,
            },
            TaskCommands::Agent { name, kind, ws_dir } => {
                cli::task::agent(ws_dir.as_deref(), &name, kind.as_deref())?;
            }
            TaskCommands::Pin { name, ws_dir } => {
                cli::task::pin(ws_dir.as_deref(), &name)?;
            }
            TaskCommands::Unpin { name, ws_dir } => {
                cli::task::unpin(ws_dir.as_deref(), &name)?;
            }
            TaskCommands::Sweep { after, idle_after, dry_run } => {
                let after = after.as_deref().map(cli::task::parse_duration).transpose()?;
                let idle_after = idle_after.as_deref().map(cli::task::parse_duration).transpose()?;
                cli::task::sweep(after, idle_after, dry_run)?;
            }
        },
    }

    Ok(())
}

/// Connect to the single global tenx session, regardless of cwd: attach (or
/// create) it from a plain terminal, or run the column directly when already
/// inside it. If cwd is inside a workspace, self-heal the registry first so it
/// shows up in the column.
/// `--no-wait`/`--timeout` → how long `secrets need`/`decrypt`/`set` block for a
/// human on the no-terminal path (`None`: enqueue and return).
fn secrets_wait(no_wait: bool, timeout: Option<&str>) -> Result<Option<std::time::Duration>> {
    if no_wait {
        return Ok(None);
    }
    Ok(Some(match timeout {
        Some(t) => cli::task::parse_duration(t)?,
        None => cli::secrets::DEFAULT_WAIT,
    }))
}

fn open() -> Result<()> {
    let bin = tmux::self_bin()?;

    tmux::check_version()?;

    // After an upgrade the server still runs the old binary's config (tmux
    // reads it once, at start), so say so — here, before tmux takes the
    // terminal, and again after an in-session column run below.
    let stale = tmux::server_version().and_then(|v| tmux::stale_server_hint(&v));

    // Every route into the session lands here, so this is the one place that
    // guarantees a watcher exists. No-op when one is already running.
    cli::watch::ensure_running(&bin);

    // One-time, best-effort: wire up the session integrations for whichever
    // agents are installed, so state reporting works without a manual setup
    // step. Sentinel-guarded (see `auto_setup`), so it runs once and never
    // fights a user who later removes an integration.
    cli::session_event::auto_setup();

    // The adhoc workspace (sessions outside any repo) exists and is
    // registered before the column first lists workspaces.
    cli::adhoc::ensure_quiet();

    // Skills `tenx init` installed are refreshed to this binary's version,
    // in every registered workspace — untouched ones only; an edited file is
    // left alone and reported by `tenx doctor`. A few small reads per
    // workspace, silent.
    cli::init::refresh_all_skills();

    if tmux::inside_tenx_session() {
        // Already inside the session (the client's embedded terminal, or a
        // plain attach): the column is on the left — Ctrl+w — or, for a
        // plain attach, `tenx` in another terminal.
        eprintln!("already inside the tenx session — Ctrl+w for the task column, or run tenx from another terminal");
        if let Some(hint) = &stale {
            eprintln!("{hint}");
        }
    } else {
        // Anywhere else — a plain terminal, or a pane of some other tmux —
        // the client: the column beside the session, embedded in this
        // terminal (`tui::client`). It starts the server if needed.
        if !tmux::server_running() {
            eprintln!("  creating session '{}'", tmux::SESSION);
        }
        if let Some(hint) = &stale {
            // The client takes the screen; leave the notice readable first.
            eprintln!("{hint}");
            std::thread::sleep(std::time::Duration::from_millis(2500));
        }
        tui::client::run()?;
    }

    Ok(())
}
