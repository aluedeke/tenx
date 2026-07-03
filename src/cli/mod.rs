pub mod hooks;
pub mod init;
pub mod repo;
pub mod standup;
pub mod tab;
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
    /// Generate a daily standup from recent activity and task files
    Standup {
        /// Collect activity since this ISO timestamp (default: last standup, or start of yesterday)
        #[arg(long)]
        since: Option<String>,
    },
    /// Manage Claude Code hooks for the active workspace
    Hooks {
        #[command(subcommand)]
        command: HooksCommands,
    },
    /// Tab operations invoked by Claude Code hooks
    #[command(hide = true)]
    Tab {
        #[command(subcommand)]
        command: TabCommands,
    },
}

#[derive(Subcommand)]
pub enum HooksCommands {
    /// (Re)install Claude Code hooks, overwriting any existing versions
    Install,
}

#[derive(Subcommand)]
pub enum TabCommands {
    /// Add the 💬 indicator to the task's zellij tab (runs from task cwd)
    Notify,
    /// Remove the 💬 indicator from the task's zellij tab (runs from task cwd)
    NotifyClear,
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
    /// Close a task's zellij tab and stop its processes, keeping the worktree
    Close {
        name: String,
    },
    /// List all tasks
    List,
    /// Reap leftover processes from closed tasks (leaked MCP servers, etc.)
    Gc,
    /// Delete a task and its worktrees
    Rm {
        name: String,
        /// Skip confirmation prompt
        #[arg(long)]
        force: bool,
    },
}
