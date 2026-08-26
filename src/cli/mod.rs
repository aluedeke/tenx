pub mod hooks;
pub mod init;
pub mod repo;
pub mod secrets;
pub mod standup;
pub mod watch;
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
    /// Run the global task overlay (all tasks across all workspaces)
    Overlay {
        /// Run as the tenx session's long-lived home pane (jump switches tabs
        /// without exiting; quit keys are disabled)
        #[arg(long)]
        home: bool,
        /// Print all tasks as JSON (activity-sorted) instead of running the
        /// TUI. Data source for the tenx-zellij overlay plugin, which runs in
        /// a wasm sandbox whose reachable dirs (`/data`, `/tmp`, `/host`) don't
        /// include the workspace, so it cannot read task state itself.
        #[arg(long)]
        json: bool,
    },
    /// Watch for tasks that start waiting on you and send a notification
    /// (started automatically when tenx opens the session; runs until it ends)
    Watch,
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
    /// Manage per-task encrypted secrets (age-based). See PRD.md for the full
    /// design — this covers the CLI baseline (Phase 0): identity resolution,
    /// sealing, the agent-safe request queue, and unlock.
    Secrets {
        #[command(subcommand)]
        command: SecretsCommands,
    },
}

#[derive(Subcommand)]
pub enum SecretsCommands {
    /// Resolve an existing age identity ($SOPS_AGE_KEY_FILE, ~/.config/sops/age,
    /// ~/.config/age) or generate a new passphrase-protected one
    Init,
    /// Encrypt a file as the sealed secrets bundle for a task
    Seal {
        /// Exact task slug
        task: String,
        /// File to seal (typically a .env)
        file: String,
    },
    /// Declare that the current task wants a secret unlocked. Agent-safe: only
    /// ever enqueues a durable, informational request — never touches the
    /// identity or the encrypted bundle. Task is resolved from cwd.
    Request {
        /// Secret name being requested (shown in `status`; informational only —
        /// unlock always releases the whole task bundle in v1)
        name: String,
    },
    /// Decrypt the current task's sealed bundle into tasks/<slug>/.secrets.env.
    /// Human-only: prompts for the identity's passphrase on the real terminal.
    /// Never prints a decrypted value to stdout — file output only. Task is
    /// resolved from cwd.
    Unlock,
    /// Show sealed/unlocked/pending state across all tasks (metadata only —
    /// never secret values)
    Status,
}

#[derive(Subcommand)]
pub enum HooksCommands {
    /// Remove tenx's Claude Code hooks (tenx no longer installs any — task
    /// state is read live from Claude Code's session registry)
    Install,
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
        /// Resolve the workspace from this directory instead of cwd — used by
        /// the tenx-zellij overlay plugin, which shells out from an arbitrary
        /// cwd and can't link against `workspace::find`.
        #[arg(long)]
        ws_dir: Option<String>,
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
        /// Create the task in this workspace directory instead of cwd. Used by
        /// the overlay plugin, which has no meaningful cwd.
        #[arg(long)]
        ws_dir: Option<String>,
    },
    /// Rename a task's display title (the `# ` heading in TASK.md)
    Rename {
        /// Exact task slug
        name: String,
        /// New display title
        title: String,
        /// Resolve the task in this workspace directory instead of cwd.
        #[arg(long)]
        ws_dir: Option<String>,
    },
    /// Open a task's zellij tab (or switch to it if already open)
    Open {
        name: String,
        /// Resolve the task in this workspace directory instead of cwd. `name`
        /// is treated as an exact slug (not slugified). Used by the overlay
        /// plugin, which jumps across workspaces and has no meaningful cwd.
        #[arg(long)]
        ws_dir: Option<String>,
    },
    /// List all tasks
    List,
    /// Add repos (worktrees on the task's branch) to an existing task
    AddRepo {
        /// Exact task slug
        name: String,
        /// Repo names to add (must already be in the workspace)
        #[arg(required = true)]
        repos: Vec<String>,
        /// Resolve the task in this workspace directory instead of cwd.
        #[arg(long)]
        ws_dir: Option<String>,
    },
    /// Detach repos from a task, removing their worktree and task branch
    RmRepo {
        /// Exact task slug
        name: String,
        /// Repo names to detach
        #[arg(required = true)]
        repos: Vec<String>,
        /// Discard uncommitted changes in the worktree (git refuses otherwise)
        #[arg(long)]
        force: bool,
        /// Resolve the task in this workspace directory instead of cwd.
        #[arg(long)]
        ws_dir: Option<String>,
    },
    /// Reconcile a task's repos to exactly this set (adds and detaches).
    /// The overlay's repo checklist applies its changes through this.
    SetRepos {
        /// Exact task slug
        name: String,
        /// The complete set of repos the task should end up with
        #[arg(required = true)]
        repos: Vec<String>,
        /// Discard uncommitted changes in worktrees being detached
        #[arg(long)]
        force: bool,
        /// Resolve the task in this workspace directory instead of cwd.
        #[arg(long)]
        ws_dir: Option<String>,
    },
    /// Delete a task and its worktrees
    Rm {
        name: String,
        /// Skip confirmation prompt
        #[arg(long)]
        force: bool,
        /// Resolve the task in this workspace directory instead of cwd. `name`
        /// is treated as an exact slug. Used by the overlay plugin.
        #[arg(long)]
        ws_dir: Option<String>,
    },
}
