pub mod agentlog;
pub mod agentview;
pub mod doctor;
pub mod hooks;
pub mod adhoc;
pub mod drive;
pub mod init;
pub mod notify;
pub mod repo;
pub mod secrets;
pub mod session_event;
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
    /// Ask an agent a question in a session of its own
    ///
    /// Creates a session in the adhoc workspace (no repos; it can read
    /// every registered workspace) titled after the question, with the
    /// question as its first message, and prints its slug. `--ws-dir` asks in
    /// a workspace instead, as a task without worktrees.
    Ask {
        /// The question; several words are joined with spaces
        #[arg(required = true, num_args = 1..)]
        prompt: Vec<String>,
        /// Coding agent (`claude`, `codex`, `pi`); default as for any task
        #[arg(long)]
        agent: Option<String>,
        /// Ask in this workspace (directory or registered name)
        #[arg(long)]
        ws_dir: Option<String>,
        /// Open the window without switching to it
        #[arg(long)]
        no_focus: bool,
    },
    /// Watch tasks and notify when one starts waiting on you
    ///
    /// Started automatically when tenx opens the session and runs until the
    /// tmux server exits. Also pushes each task's status into the tmux status
    /// bar and opens a log pane for background agents.
    Watch,
    /// Serve the task column and the session to a browser
    ///
    /// The column as a web page beside a terminal attached to the tenx
    /// session — each browser tab switches tasks on its own, with the same
    /// keys as the terminal client. Prints the address to open, with the
    /// token that logs the browser in. Runs in the foreground.
    Web {
        #[command(subcommand)]
        command: Option<WebCommand>,
        /// Address to listen on. Anything but loopback serves a shell on this
        /// machine over plain HTTP — prefer `tailscale serve` in front
        #[arg(long, default_value = "127.0.0.1")]
        listen: String,
        #[arg(long, default_value_t = 7070)]
        port: u16,
        /// Open the page in the default browser
        #[arg(long)]
        open: bool,
        /// Replace the login token, logging out every browser
        #[arg(long)]
        rotate_token: bool,
        /// Also accept the page from this origin (`next dev`), e.g.
        /// http://localhost:3000 — repeatable
        #[arg(long = "dev-origin", value_name = "URL")]
        dev_origin: Vec<String>,
    },
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
    /// Set up and inspect coding-agent integrations (Claude Code, Codex, pi)
    Agent {
        #[command(subcommand)]
        command: AgentCommands,
    },
    /// Report agent integration health: binaries, versions, hooks, tmux
    /// options, and whether each workspace's installed skills are current
    Doctor {
        /// Replace installed skill files you edited with tenx's current
        /// version, keeping your copy beside each as <file>.orig
        #[arg(long)]
        reset_skills: bool,
    },
    /// Manage per-task encrypted secrets (age + sops)
    ///
    /// Commands are named and behave like their `sops` equivalents:
    /// `encrypt`, `set`, `decrypt`, plus `fulfill` (do whatever is pending),
    /// `cancel` (withdraw a request) and `status`. `decrypt` and `set` are
    /// agent-safe: without a real terminal each can only enqueue its own
    /// kind of request (release a bundle, or supply a value) and then wait
    /// for a human to act on it — never touching key material or a secret
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
    /// Apply a coding agent's hook/extension event to tenx's session registry.
    /// Reads the hook JSON payload on stdin; prints nothing; always exits 0.
    /// Invoked by the agents' own hooks, not by users.
    SessionEvent {
        /// Which agent's payload this is (`claude`, `codex`, `pi`).
        #[arg(long)]
        agent: String,
        /// The agent's pid, when the caller knows it (the pi extension does).
        /// Omitted by hooks, which climb from their parent instead.
        #[arg(long)]
        pid: Option<u32>,
    },
    /// Follow a background agent's transcript in a pane; exits when the agent
    /// does. Opened by `tenx watch` when a `--bg` session appears under a task.
    AgentLog {
        /// The agent's working directory (its `cwd` in Claude Code's registry).
        cwd: String,
        /// The agent's pid — the pane closes when it's gone.
        pid: u32,
        /// The agent's session id: follow exactly that session's transcript
        /// rather than whichever in the directory was written last.
        #[arg(long)]
        session: Option<String>,
        /// Which agent's transcript format to follow (`claude`, `codex`, `pi`).
        #[arg(long, default_value = "claude")]
        agent: String,
        /// Follow exactly this transcript file (a subagent's), instead of
        /// finding the session's under `cwd`.
        #[arg(long)]
        transcript: Option<String>,
        /// Header line to show instead of the directory name.
        #[arg(long)]
        title: Option<String>,
        /// Run as an interactive viewer: close on q/Esc, stay open when the
        /// process exits.
        #[arg(long)]
        viewer: bool,
    },
    /// Open a Claude Code subagent in Claude's own agent view, in its
    /// session's pane (what ⏎ on a subagent line in the column does).
    OpenAgent {
        /// The pid of the Claude Code session that spawned it.
        pid: u32,
        /// What its row in Claude's agent panel shows (its description);
        /// omitted, the session's main view.
        label: Option<String>,
        /// Its type, to find its row once Claude shows a live summary there.
        #[arg(long = "type")]
        agent_type: Option<String>,
        /// Its place (0-based) among the session's running subagents of that
        /// type, in launch order...
        #[arg(long)]
        nth: Option<usize>,
        /// ...and how many there are.
        #[arg(long)]
        peers: Option<usize>,
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
    /// Ask for secrets for the current task (task resolved from cwd)
    ///
    /// The one command an agent needs. Each name is routed by looking at
    /// what's sealed (key names are readable without the passphrase):
    /// already released → nothing to do; sealed in the task's bundle or an
    /// adopted sops file → a release request; nowhere → a request for a
    /// human to type a value. From an agent's shell tool (no terminal) it
    /// then waits for a human to answer; from a real terminal it answers
    /// straight away. Exit codes: 0 granted, 3 still pending (re-run to keep
    /// waiting), 4 denied, 5 withdrawn.
    Need {
        /// Names to ask for — dotenv keys (STRIPE_KEY), or a fragment of an
        /// adopted sops file's name (staging)
        #[arg(required = true)]
        names: Vec<String>,
        /// Why you need them — shown to the human who answers
        #[arg(long)]
        why: Option<String>,
        /// Enqueue and return immediately instead of waiting (no-terminal path only)
        #[arg(long)]
        no_wait: bool,
        /// How long to wait for a human before giving up — "<N><unit>", e.g.
        /// "90s", "30m". The request stays queued on timeout; re-running
        /// resumes waiting. Default: 100s, under a shell tool's usual kill
        /// limit.
        #[arg(long, conflicts_with = "no_wait")]
        timeout: Option<String>,
    },
    /// Set one secret in the current task's sealed bundle (task resolved from cwd)
    ///
    /// Literally `sops set`: edits the existing document in place. From a
    /// real terminal it prompts for the value (masked), then the
    /// passphrase, and seals it — releasing it too if someone had asked for
    /// it. From an agent's shell tool it can only ask a human to supply a
    /// value (use `need` unless you mean to replace one that exists). The
    /// value is never a CLI argument or read from stdin.
    Set {
        /// Secret name (becomes its key in the decrypted .secrets.env)
        name: String,
        /// Enqueue and return immediately instead of waiting (no-terminal path only)
        #[arg(long)]
        no_wait: bool,
        /// How long to wait for a human before giving up — "<N><unit>", e.g.
        /// "90s", "9m". The request stays queued on timeout; re-running
        /// resumes waiting. Default: 100s, under a shell tool's usual kill
        /// limit.
        #[arg(long, conflicts_with = "no_wait")]
        timeout: Option<String>,
    },
    /// Release the current task's secrets (task resolved from cwd)
    ///
    /// From a real terminal: prompts for the passphrase and releases NAME,
    /// else whatever is pending release, else everything sealed — the
    /// task's bundle into tasks/<slug>/.secrets.env, adopted sops files to
    /// their plaintext sibling (stored outside the worktree, symlinked in).
    /// From an agent's shell tool it's the older spelling of `need NAME`.
    /// Never prints a decrypted value to stdout.
    Decrypt {
        /// Secret name — a key of the bundle, or a fragment of an adopted
        /// sops file's name ("staging"). Required when no real terminal is
        /// reachable.
        name: Option<String>,
        /// Enqueue and return immediately instead of waiting (no-terminal path only)
        #[arg(long)]
        no_wait: bool,
        /// How long to wait for a human before giving up — "<N><unit>", e.g.
        /// "90s", "9m". The request stays queued on timeout; re-running
        /// resumes waiting. Default: 100s, under a shell tool's usual kill
        /// limit.
        #[arg(long, conflicts_with = "no_wait")]
        timeout: Option<String>,
    },
    /// Answer everything pending for the current task in one sitting (task resolved from cwd)
    ///
    /// Lists each request with the agent's reason, asks once whether to
    /// grant all, deny all or pick, reads a value for each granted value
    /// request, then takes the passphrase once to seal the new values and
    /// release every granted name. What `u` in the column runs. Needs a real
    /// terminal.
    Fulfill {
        /// Wait for Enter before exiting, so the outcome can be read — for
        /// the column's unlock popup, which closes when this exits
        #[arg(long, hide = true)]
        hold: bool,
    },
    /// Refuse pending requests for the current task (task resolved from cwd)
    ///
    /// The waiting agent is told, with your note. `fulfill` asks the same
    /// question interactively.
    Deny {
        /// Names to refuse
        #[arg(required = true)]
        names: Vec<String>,
        /// Why — shown to the agent that asked
        #[arg(long)]
        note: Option<String>,
    },
    /// Withdraw a pending request for the current task (task resolved from cwd)
    ///
    /// Removes the name from whichever queue holds it (release or value)
    /// and nothing else — never touches the identity, a bundle or a
    /// plaintext, so it is safe from an agent's Bash tool too. A `decrypt`
    /// or `set` still waiting on that name exits reporting the withdrawal.
    Cancel {
        /// Name to withdraw (required unless --all)
        #[arg(required_unless_present = "all")]
        name: Option<String>,
        /// Withdraw every pending request for the task, both queues
        #[arg(long, conflicts_with = "name")]
        all: bool,
    },
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
pub enum AgentCommands {
    /// Install (or, with --check, report) the integration that feeds tenx's
    /// session registry for an agent. Claude Code: merges hooks into
    /// `~/.claude/settings.json` (no trust step). Codex/pi: coming in later phases.
    Setup {
        /// Agent to set up: `claude`, `codex`, or `pi`.
        kind: String,
        /// Only report whether the integration is installed; change nothing.
        #[arg(long)]
        check: bool,
    },
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
        /// Open the window without switching to it — what an agent creating
        /// tasks for others should pass, so your screen stays put
        #[arg(long)]
        no_focus: bool,
        /// A task without worktrees: the agent runs in the task directory and
        /// may read the whole workspace
        #[arg(long, conflicts_with = "repos")]
        no_repos: bool,
        /// Create it in the adhoc workspace (no repos; for questions and
        /// for orchestrating other workspaces' tasks)
        #[arg(long, alias = "detached", conflicts_with_all = ["repos", "ws_dir"])]
        adhoc: bool,
        /// The agent's first message; it starts working on it when the
        /// window opens
        #[arg(long)]
        prompt: Option<String>,
        /// Coding agent for this task (`claude`, `codex`, `pi`); default is the
        /// workspace's `agent`. Writes the task's `.tenx-agent`.
        #[arg(long)]
        agent: Option<String>,
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
    /// List the workspace's tasks
    List {
        /// Every task across every registered workspace, with the workspaces
        /// and their repos, as JSON sorted by activity — for scripts and
        /// other front ends.
        #[arg(long)]
        json: bool,
    },
    /// Send a message to a task's agent, as if typed into its pane
    ///
    /// Opens the task's window first if it's closed. Refuses while the agent
    /// sits on a dialog (a permission prompt, a question) unless `--force`.
    /// Returns once the agent has started the turn — follow with `task wait`.
    Send {
        /// Exact task slug
        name: String,
        /// The message; several words are joined with spaces; `-` reads stdin
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
        /// Paste even though the agent is waiting on a dialog
        #[arg(long)]
        force: bool,
        /// The task's workspace (directory or registered name) instead of cwd's
        #[arg(long)]
        ws_dir: Option<String>,
    },
    /// Wait until a task's agent has finished its turn
    ///
    /// Exits 0 when the turn is over, 2 when the agent stopped on something
    /// that needs an answer (the reason is printed), 3 on timeout.
    Wait {
        /// Exact task slug
        name: String,
        /// Give up after this long: "90s", "10m", "2h"
        #[arg(long, default_value = "10m")]
        timeout: String,
        /// The task's workspace (directory or registered name) instead of cwd's
        #[arg(long)]
        ws_dir: Option<String>,
    },
    /// Print what a task's agent said since the last prompt
    Output {
        /// Exact task slug
        name: String,
        /// The prompt, the replies and the task's status as JSON
        #[arg(long)]
        json: bool,
        /// The task's workspace (directory or registered name) instead of cwd's
        #[arg(long)]
        ws_dir: Option<String>,
    },
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
    /// The column's repo checklist applies its changes through this.
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
    /// Show or set a task's coding agent (writes/clears its `.tenx-agent`).
    Agent {
        /// Exact task slug
        name: String,
        /// New agent (`claude`, `codex`, `pi`); omit to show the current one,
        /// or pass `default` to clear the override and use the workspace default.
        kind: Option<String>,
        /// Resolve the task in this workspace directory instead of cwd.
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
    /// deletes anything: `task open` (or the column) picks a swept task's
    /// conversation back up exactly where it left off.
    Sweep {
        /// How long a finished ("done, waiting on you") task sits unanswered
        /// before its window is swept. "<N><unit>", e.g. "30m", "4h", "2d".
        /// Default: 8h.
        #[arg(long)]
        after: Option<String>,
        /// How long a genuinely idle task (no live agent session at all) must
        /// have been quiet before its window is swept. The grace exists
        /// because "idle" is also what a misread looks like. Same format as
        /// `--after`. Default: 15m.
        #[arg(long)]
        idle_after: Option<String>,
        /// Report what would be closed without closing anything.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
pub enum WebCommand {
    /// Run `tenx web` at login and keep it running (a LaunchAgent on macOS,
    /// a systemd user unit on Linux)
    Service {
        #[command(subcommand)]
        action: WebServiceAction,
    },
}

#[derive(Subcommand)]
pub enum WebServiceAction {
    /// Install (or replace) the service and start it now
    Install {
        /// Address to listen on (see `tenx web --listen`)
        #[arg(long, default_value = "127.0.0.1")]
        listen: String,
        #[arg(long, default_value_t = 7070)]
        port: u16,
        /// Also accept the page from this origin — repeatable
        #[arg(long = "dev-origin", value_name = "URL")]
        dev_origin: Vec<String>,
    },
    /// Restart it, e.g. onto a new binary after an upgrade (no-op when not installed)
    Restart,
    /// Stop the service and remove it
    Uninstall,
    /// Whether it is installed and running
    Status,
}
