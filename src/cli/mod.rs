pub mod agentlog;
pub mod hooks;
pub mod init;
pub mod notify;
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
        /// Print all tasks and workspaces as JSON (activity-sorted) instead of
        /// running the TUI — for scripts and other front ends.
        #[arg(long)]
        json: bool,
    },
    /// Watch tasks and notify when one starts waiting on you
    ///
    /// Started automatically when tenx opens the session and runs until the
    /// tmux server exits. Also pushes each task's status into the tmux status
    /// bar and opens a log pane for background agents.
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
    /// Manage per-task encrypted secrets (age + sops)
    ///
    /// Commands are named and behave like their `sops` equivalents:
    /// `encrypt`, `set`, `decrypt`, plus `fulfill` (do whatever is pending)
    /// and `status`. `decrypt` and `set` are agent-safe: without a real
    /// terminal each can only enqueue its own kind of request (release a
    /// bundle, or supply a value) and never touches key material or a secret
    /// value. Decrypted values are written to files, never to stdout.
    Secrets {
        #[command(subcommand)]
        command: SecretsCommands,
    },
    /// Plumbing for tmux hooks and debugging — not part of the user-facing CLI.
    #[command(hide = true)]
    Internal {
        #[command(subcommand)]
        command: InternalCommands,
    },
}

#[derive(Subcommand)]
pub enum InternalCommands {
    /// Print the tmux config tenx generates for its server (what
    /// `~/.config/tenx/tmux.conf` will contain on the next session creation).
    TmuxConf,
    /// Print listening ports per open task window as JSON — what `tenx watch`
    /// caches into each task's `.tenx-live.json`.
    Ports,
    /// Follow a background agent's transcript in a pane; exits when the agent
    /// does. Opened by `tenx watch` when a `--bg` session appears under a task.
    AgentLog {
        /// The agent's working directory (its `cwd` in Claude Code's registry).
        cwd: String,
        /// The agent's pid — the pane closes when it's gone.
        pid: u32,
        /// Claude Code's session id: follow exactly `<session>.jsonl` rather
        /// than whichever transcript in the directory was written last.
        #[arg(long)]
        session: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum SecretsCommands {
    /// Resolve an existing age identity ($SOPS_AGE_KEY_FILE, ~/.config/sops/age,
    /// ~/.config/age) or generate a new passphrase-protected one
    Init,
    /// Encrypt a file as the sealed secrets bundle for a task
    ///
    /// Equivalent to `sops --encrypt`, and named after it.
    Encrypt {
        /// Exact task slug
        task: String,
        /// File to encrypt (typically a .env)
        file: String,
    },
    /// Set one secret in the current task's sealed bundle (task resolved from cwd)
    ///
    /// Literally `sops set`: edits the existing document in
    /// place, reusing its data key. Same tty-detection shape as `decrypt`,
    /// mirrored: real terminal reachable → prompts for the value (masked),
    /// then the passphrase, then edits; not reachable → enqueues "someone
    /// needs to supply a value for this" instead, agent-safe, own queue
    /// separate from `decrypt`'s. The value itself is never a CLI argument
    /// or read from stdin — always typed directly into the real terminal.
    Set {
        /// Secret name (becomes its key in the decrypted .secrets.env)
        name: String,
    },
    /// Ask for the current task's secrets (task resolved from cwd)
    ///
    /// The one command both an agent and a human use. Whether it prompts for
    /// a passphrase or just enqueues a durable request depends entirely on
    /// whether a real terminal is reachable (checked via /dev/tty, the same
    /// thing age's own prompt reads from), never on how it's invoked: from a
    /// human's real shell or the overlay's spawned pane it decrypts straight
    /// away; from an agent's Bash tool (no controlling terminal) it falls
    /// back to enqueue-only — never touches the identity or the encrypted
    /// bundle in that case. When it does decrypt: the task's own sealed
    /// bundle into tasks/<slug>/.secrets.env, and/or any sops-covered files
    /// an adopted repo already has (to their plaintext sibling, inside the
    /// worktree) — filtered by pending names when any match a filename,
    /// otherwise every sops file found. Never prints a decrypted value to
    /// stdout — file output only.
    Decrypt {
        /// Secret name to ask for (shown in `status`)
        ///
        /// Used by the non-interactive fallback and to seed the request queue
        /// before an interactive decrypt. For the task's own sealed bundle this is just a label —
        /// decrypt always releases the whole bundle. For an adopted repo with
        /// its own .sops.yaml, it's matched against candidate filenames and
        /// *does* select which file gets decrypted (e.g. "staging" vs
        /// "prod") — name the file, not a field inside it. Required when no
        /// real terminal is reachable (nothing else to do in that case);
        /// optional otherwise.
        name: Option<String>,
    },
    /// Do whatever is pending for the current task in one sitting (task resolved from cwd)
    ///
    /// Decrypts if anything is pending release, then runs `set` once per
    /// pending value-request. Human-only in
    /// practice (each step still needs a real terminal); exists mainly for
    /// the overlay's spawned pane, so it doesn't need to know which of the
    /// two pending kinds a task has before deciding what to run.
    Fulfill,
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
        /// Resolve the workspace from this directory instead of cwd (for
        /// scripts and front ends that don't run inside the workspace).
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
        /// Task name — the display title; its slug is the directory, branch
        /// and window name
        name: String,
        /// Comma-separated repo names to include (default: all workspace repos)
        #[arg(long, value_delimiter = ',')]
        repos: Option<Vec<String>>,
        /// Fill TASK.md's `## Description` (e.g. a ticket's body)
        #[arg(long)]
        description: Option<String>,
        /// Fill a `## Links` row, as "Label: value" — e.g. `--link "Linear:
        /// https://linear.app/…"`. Repeatable. Default rows (Linear Project,
        /// Linear Milestone, Linear, PR) are filled in place; other labels
        /// are appended.
        #[arg(long = "link")]
        links: Vec<String>,
        /// Create worktrees but don't open a window in the tenx session
        #[arg(long)]
        no_open: bool,
        /// Create the task in this workspace directory instead of cwd.
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
    /// Open a task's window in the tenx session (or switch to it if already open)
    Open {
        name: String,
        /// Resolve the task in this workspace directory instead of cwd. `name`
        /// is treated as an exact slug (not slugified).
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
        /// is treated as an exact slug.
        #[arg(long)]
        ws_dir: Option<String>,
    },
    /// Exempt a task from `sweep` — its window is never auto-closed for being idle.
    Pin {
        name: String,
        #[arg(long)]
        ws_dir: Option<String>,
    },
    /// Undo `pin`.
    Unpin {
        name: String,
        #[arg(long)]
        ws_dir: Option<String>,
    },
    /// Close idle task windows across every workspace, freeing the claude process each holds
    ///
    /// Never touches a task waiting on a
    /// prompt or mid-turn, the current window, or a pinned task, and never
    /// deletes anything: `task open` (or the overlay) picks a swept task's
    /// conversation back up exactly where it left off.
    Sweep {
        /// How long a finished ("done, waiting on you") task sits unanswered
        /// before its window is swept. A genuinely idle task (no live claude
        /// session at all) is swept immediately regardless. "<N><unit>",
        /// e.g. "30m", "4h", "2d". Default: 8h.
        #[arg(long)]
        after: Option<String>,
        /// Report what would be closed without closing anything.
        #[arg(long)]
        dry_run: bool,
    },
}
