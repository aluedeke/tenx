pub mod init;
pub mod repo;
pub mod task;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "tenx", about = "Workspace & task manager", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize a new workspace in the current directory
    Init {
        /// Workspace name (default: current directory name)
        name: Option<String>,
    },
    /// Manage repos in the active workspace
    Repo {
        #[command(subcommand)]
        command: RepoCommands,
    },
    /// Manage tasks in the active workspace
    Task {
        #[command(subcommand)]
        command: TaskCommands,
    },
    /// Launch the TUI directly (used by the zellij management tab)
    Tui,
    /// Launch the task-list TUI pane (used by the zellij management tab)
    #[command(hide = true)]
    Tasks,
    /// Launch the repo-list TUI pane (used by the zellij management tab)
    #[command(hide = true)]
    Repos,
}

#[derive(Subcommand)]
pub enum RepoCommands {
    /// Add a repo to the workspace (bare clone)
    Add {
        /// Git URL to clone
        url: String,
        /// Override the repo name (default: inferred from URL)
        #[arg(long)]
        name: Option<String>,
    },
    /// List repos in the workspace
    List,
    /// Fetch latest from origin for one or all repos
    Fetch {
        /// Repo name to fetch (default: all)
        name: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum TaskCommands {
    /// Create a new task
    New {
        /// Task name (also used as the git branch name)
        name: String,
        /// Comma-separated repo names to include (default: all workspace repos)
        #[arg(long, value_delimiter = ',')]
        repos: Option<Vec<String>>,
        /// Create worktrees but don't open a zellij tab
        #[arg(long)]
        no_open: bool,
    },
    /// Open a task's zellij tab (or switch to it if already open)
    Open {
        name: String,
    },
    /// List all tasks
    List,
    /// Delete a task and its worktrees
    Rm {
        name: String,
        /// Skip confirmation prompt
        #[arg(long)]
        force: bool,
    },
}
