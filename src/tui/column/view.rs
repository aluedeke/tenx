//! The column as data: everything `render_in` draws — tabs, the search
//! field, the list with its headers, rows, chips and subagent lines, the
//! mode and its form, the footer, the key table — as one serializable value,
//! [`ColumnView`], for a front end that draws it itself (`tenx web`). And
//! the other direction: key presses and clicks that don't come from
//! crossterm ([`WebKey`], [`Click`]), fed through the same handlers the
//! terminal's keys and mouse go through, so both front ends share one
//! interaction model by construction.
//!
//! Colours travel as `#rrggbb` from `palette`, the same values the terminal
//! is painted with; widths are the front end's business, so nothing here is
//! truncated or padded.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};

use super::*;

/// The whole column, as of now.
#[derive(Debug, Serialize)]
pub(crate) struct ColumnView {
    pub(crate) tabs: Vec<TabView>,
    /// Where the cursor is: `search` (Insert) or `list` (Normal).
    pub(crate) focus: &'static str,
    pub(crate) filter: String,
    /// The row id of the task the task area shows (`Column::is_shown`): the
    /// session's current window, or a closed task the cursor rests on.
    pub(crate) current: Option<String>,
    /// The cursor rests on a task with no open window: the task area shows
    /// its empty "⏎ open it here" screen instead of the session, as the TUI
    /// does (`Column::selected_closed`, `client::render_closed`).
    pub(crate) shown_closed: Option<ShownClosed>,
    /// The active tab's list, top to bottom.
    pub(crate) items: Vec<Item>,
    pub(crate) mode: ModeView,
    pub(crate) footer: Footer,
    /// The column's last message (an error or an outcome), even while a mode
    /// footer hides it — so a web form can show why a submit was refused
    /// inside the form.
    pub(crate) status: Option<String>,
    /// The long operations this column started that are still running, on
    /// any tab — so a web form can show the progress of the job its submit
    /// started (the Work tab's items only exist on that tab).
    pub(crate) jobs: Vec<JobProgress>,
    /// The `?` overlay's table: section, then (keys, action) rows.
    pub(crate) help: Vec<HelpSection>,
}

/// A running job, as a form's progress bar shows it.
#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct JobProgress {
    /// Unique in this server, so the page can tell the job its submit
    /// started from the ones already running.
    pub(crate) id: u64,
    pub(crate) title: String,
    /// The step running now, e.g. `2/3 api`.
    pub(crate) counter: String,
    /// 0–1 while the running step reports a percentage.
    pub(crate) fraction: Option<f32>,
}

/// What the empty screen for a closed task names.
#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct ShownClosed {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) ws: String,
    pub(crate) ws_color: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct TabView {
    pub(crate) label: &'static str,
    pub(crate) active: bool,
    /// Running jobs, on the Work tab only; 0 means no `[n]`.
    pub(crate) running: usize,
}

/// A coloured label: a reason or secrets chip (`bg` set), a PR (`fg` only).
#[derive(Debug, Serialize)]
pub(crate) struct Chip {
    pub(crate) label: String,
    pub(crate) fg: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bg: Option<String>,
}

/// One entry of the active tab's list.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Item {
    /// A section header (Tasks: the status group; Repos: the workspace).
    Header { label: String, count: Option<usize>, color: String },
    Task(Box<TaskItem>),
    /// A task's subagent, one line under it.
    Sub(SubItem),
    Repo(RepoItem),
    Job(JobItem),
    /// What an empty list says, line by line.
    Empty { lines: Vec<String> },
}

#[derive(Debug, Serialize)]
pub(crate) struct TaskItem {
    /// `<workspace>/<slug>` — what a [`Click::Task`] names.
    pub(crate) id: String,
    pub(crate) ws: String,
    pub(crate) ws_color: String,
    pub(crate) slug: String,
    pub(crate) title: String,
    pub(crate) title_color: String,
    pub(crate) glyph: String,
    pub(crate) glyph_color: String,
    pub(crate) status: &'static str,
    pub(crate) selected: bool,
    pub(crate) current: bool,
    /// No open window: dimmer, and ⏎ opens it.
    pub(crate) closed: bool,
    /// Still being set up by a job.
    pub(crate) pending: bool,
    /// What it wants from you, first on its second line.
    pub(crate) reason: Option<Chip>,
    /// Waiting on a permission prompt `A`/`D` can answer.
    pub(crate) answerable: bool,
    /// Has secrets waiting to be unlocked (`u`) or rejected (`D`).
    pub(crate) locked: bool,
    /// The pending secrets requests, each with the agent's reason — what
    /// `locked` is about, for a front end to show before you answer.
    pub(crate) wants: Vec<Wanted>,
    /// Its agent, when it isn't the default (Claude).
    pub(crate) agent: Option<String>,
    /// How long it has rested (blocked, signaled and done rows only).
    pub(crate) age: Option<String>,
    pub(crate) prs: Vec<Chip>,
    pub(crate) ports: Vec<u16>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SubItem {
    /// Its task's row id.
    pub(crate) task: String,
    pub(crate) id: String,
    pub(crate) glyph: &'static str,
    pub(crate) glyph_color: String,
    pub(crate) label: String,
    /// `bg` when it runs in the background, then its type.
    pub(crate) extras: Vec<String>,
    pub(crate) finished: bool,
    pub(crate) selected: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct RepoItem {
    /// Its place in the list — what a [`Click::Item`] names.
    pub(crate) pos: usize,
    pub(crate) ws: String,
    pub(crate) name: String,
    pub(crate) cloned: bool,
    /// The last commit, or "not cloned".
    pub(crate) detail: String,
    pub(crate) selected: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct JobItem {
    pub(crate) pos: usize,
    pub(crate) title: String,
    /// `running`, `done` or `failed`.
    pub(crate) state: &'static str,
    /// `2/5` while running.
    pub(crate) counter: Option<String>,
    pub(crate) steps: Vec<StepItem>,
    /// Overall progress 0–1 once something reports a percent; `None` is
    /// indeterminate (draw a marquee).
    pub(crate) fraction: Option<f32>,
    pub(crate) transfer: Option<String>,
    /// The message on success, the error on failure.
    pub(crate) outcome: Option<String>,
    pub(crate) selected: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct StepItem {
    pub(crate) label: String,
    /// `pending`, `running`, `done` or `failed`.
    pub(crate) state: &'static str,
    pub(crate) note: String,
}

/// The column's mode, with the form it shows.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ModeView {
    List,
    Command {
        buffer: String,
    },
    Create {
        workspace: String,
        /// 1-based, of `workspaces`.
        workspace_index: usize,
        workspaces: usize,
        /// Every registered workspace, in picker order (`FormOp::Pick`
        /// `workspace` takes an index into this).
        workspace_options: Vec<WorkspaceOption>,
        /// The agent choices, in picker order: `default` first, then each kind
        /// (`FormOp::Pick` `agent` takes an index into this).
        agent_options: Vec<&'static str>,
        /// 0-based, of `agent_options`.
        agent_index: usize,
        name: String,
        repos: Vec<Check>,
        /// `default` when it inherits.
        agent: String,
        agent_inherits: bool,
        /// What `default` resolves to for the chosen workspace (`claude`, …).
        agent_default: &'static str,
        /// The task's directory and branch name, from `name`
        /// (`tenx_core::slug::slugify`) — empty until the name makes one.
        slug: String,
        /// `workspace`, `name`, `repo` or `agent`.
        focus: &'static str,
        /// The repo under the cursor when `focus` is `repo`.
        focus_repo: Option<usize>,
    },
    AddRepo {
        workspace: String,
        url: String,
        name: String,
        /// `url` or `name`.
        focus: &'static str,
    },
    NewWorkspace {
        path: String,
        name: String,
        repo_url: String,
        skills: bool,
        /// `path`, `name`, `repo_url` or `skills`.
        focus: &'static str,
    },
    EditRepos {
        task: String,
        picks: Vec<Pick>,
        focus: usize,
        /// The second step: detaching needs a yes.
        confirm: bool,
    },
    Confirm {
        title: String,
    },
    Rename {
        buffer: String,
    },
    /// Rejecting a task's pending secrets requests (`D` on such a row,
    /// `:reject`): every pending name is denied, with an optional note the
    /// waiting agent is shown.
    Reject {
        /// The task's title and row id.
        task: String,
        id: String,
        names: Vec<Wanted>,
        note: String,
    },
    Help {
        scroll: u16,
    },
}

/// A pending secrets request: the name, and why the agent asked for it
/// (`need --why`), empty when it gave no reason.
#[derive(Debug, Serialize)]
pub(crate) struct Wanted {
    pub(crate) name: String,
    pub(crate) why: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct WorkspaceOption {
    pub(crate) name: String,
    pub(crate) color: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct Check {
    pub(crate) name: String,
    pub(crate) checked: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct Pick {
    pub(crate) name: String,
    pub(crate) checked: bool,
    /// The task has a worktree for it now; `checked != present` is the change.
    pub(crate) present: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct HelpSection {
    pub(crate) section: &'static str,
    pub(crate) keys: Vec<(&'static str, &'static str)>,
}

/// What the footer says, and in which voice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Footer {
    pub(crate) kind: FooterKind,
    /// The mode tag in front of a hint: `INSERT` or `NORMAL`.
    pub(crate) tag: Option<&'static str>,
    pub(crate) text: String,
    /// After a command buffer: the commands, while it is empty.
    pub(crate) hint: Option<&'static str>,
    /// After a command buffer: `:q` would take a running job with it.
    pub(crate) warn: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FooterKind {
    /// Keys that matter here (muted).
    Hint,
    /// The `:` line; `text` is the buffer.
    Command,
    /// A destructive yes/no (danger, bold).
    Confirm,
    /// What just happened (success).
    Message,
    /// A form's error (danger).
    Error,
}

pub(super) const CREATE_HINT: &str = "⏎ create   esc cancel   ⇥ next   space toggle repo   ←→ workspace / agent";
pub(super) const ADD_REPO_HINT: &str = "⏎ clone & add   esc cancel   ⇥ next field";
pub(super) const NEW_WORKSPACE_HINT: &str = "⏎ create   esc cancel   ⇥ next field   space toggle skills";
pub(super) const EDIT_REPOS_HINT: &str = "⏎ apply   esc cancel   space toggle   a all / n none";
pub(super) const HELP_HINT: &str = "j/k scroll · any key closes";
const COMMANDS_HINT: &str = "new · open · delete · rename · close · help · quit";

impl Footer {
    fn new(kind: FooterKind, text: impl Into<String>) -> Self {
        Footer { kind, tag: None, text: text.into(), hint: None, warn: None }
    }
}

/// The footer for the column's state — one decision, drawn by `render_in`
/// in the terminal and sent as data to the web.
pub(super) fn footer(column: &Column) -> Footer {
    let form_footer = |hint: &str| match &column.status_msg {
        Some(msg) => Footer::new(FooterKind::Error, msg.clone()),
        None => Footer::new(FooterKind::Hint, hint),
    };
    match &column.mode {
        Mode::Command(buf) => Footer {
            kind: FooterKind::Command,
            tag: None,
            text: buf.clone(),
            hint: buf.is_empty().then_some(COMMANDS_HINT),
            warn: (buf == "q" && column.job_running()).then_some("! to quit anyway"),
        },
        Mode::Confirm(c) => {
            Footer::new(FooterKind::Confirm, format!("delete '{}' + worktrees?   y = delete   n/esc = cancel", c.title))
        }
        Mode::Rename(_) => Footer::new(FooterKind::Hint, "⏎ save   esc cancel"),
        // Danger, not bold: a hint for a destructive step, not the yes/no.
        Mode::Reject(form) => Footer::new(FooterKind::Error, format!("⏎ reject {}   esc cancel", form.names.join(", "))),
        Mode::Create(_) => form_footer(CREATE_HINT),
        Mode::AddRepo(_) => form_footer(ADD_REPO_HINT),
        Mode::NewWorkspace(_) => form_footer(NEW_WORKSPACE_HINT),
        Mode::EditRepos(form) if form.confirm => Footer::new(
            FooterKind::Confirm,
            format!(
                "detach {} — removes the worktree AND its '{}' branch.   y = apply   esc = back",
                form.removed().join(", "),
                form.slug
            ),
        ),
        Mode::EditRepos(_) => form_footer(EDIT_REPOS_HINT),
        Mode::Help(_) => Footer::new(FooterKind::Hint, HELP_HINT),
        Mode::List => match &column.status_msg {
            Some(msg) => Footer::new(FooterKind::Message, msg.clone()),
            None => Footer {
                tag: Some(match column.input_mode {
                    InputMode::Insert => "INSERT",
                    InputMode::Normal => "NORMAL",
                }),
                ..Footer::new(FooterKind::Hint, list_hint(column))
            },
        },
    }
}

/// A column of ~36 cells: the two or three keys that matter here; the full
/// set is behind `?`.
fn list_hint(column: &Column) -> &'static str {
    match (column.input_mode, column.tab) {
        (InputMode::Insert, _) => "filter · ↓↑ switch · ⏎ open",
        (InputMode::Normal, Tab::Tasks) if column.selected_sub().is_some() => "⏎ open agent · t transcript",
        (InputMode::Normal, Tab::Tasks) if column.selected_row().is_some_and(|r| r.pending) => "setting up · esc detach",
        (InputMode::Normal, Tab::Tasks) if column.selected_answerable() => "A/D answer · ⏎ open",
        (InputMode::Normal, Tab::Tasks) if column.selected_has_secrets() => "u unlock · D reject · ⏎ open",
        (InputMode::Normal, Tab::Tasks) if column.selected_row().is_some_and(|r| r.window_id.is_none()) => {
            "closed · ⏎ open · ↓↑ move"
        }
        (InputMode::Normal, Tab::Tasks) if column.another_needs_you() => "n needs you · ⏎ open · ^n new",
        (InputMode::Normal, Tab::Tasks) => "↓↑ switch · ⏎ open · ^n new · ? keys",
        (InputMode::Normal, Tab::Repos) => "a add-repo · gt tab · ? keys",
        (InputMode::Normal, Tab::Work) if column.jobs.lock().is_empty() => "gt tab · ? keys",
        (InputMode::Normal, Tab::Work) => "dd dismiss · gt tab",
    }
}

/// A row's id: its workspace and slug (a slug is only unique within a
/// workspace).
pub(crate) fn row_id(row: &Row) -> String {
    format!("{}/{}", row.ws_name, row.slug)
}

impl Column {
    /// The column as data (see the module doc).
    pub(crate) fn view(&self) -> ColumnView {
        let active = self.active_jobs();
        let tabs = Tab::ALL
            .iter()
            .map(|t| TabView {
                label: t.label(),
                active: *t == self.tab,
                running: if *t == Tab::Work { active } else { 0 },
            })
            .collect();
        let items = match self.tab {
            Tab::Tasks => self.task_items(),
            Tab::Repos => self.repo_view_items(),
            Tab::Work => self.job_items(),
        };
        // What the task area shows (`Column::is_shown`): the session's current
        // window, or a closed task's empty screen while the cursor is on it.
        let current = self.rows.iter().find(|r| self.is_shown(r)).map(row_id);
        ColumnView {
            tabs,
            focus: match self.focus {
                Focus::Search => "search",
                Focus::List => "list",
            },
            filter: self.filter.clone(),
            current,
            shown_closed: self.selected_closed_row().map(|r| ShownClosed {
                id: row_id(r),
                title: r.title.clone(),
                ws: r.ws_name.clone(),
                ws_color: palette::workspace_color(&r.ws_name).hex(),
            }),
            items,
            mode: self.mode_view(),
            footer: footer(self),
            status: self.status_msg.clone(),
            jobs: self
                .jobs
                .lock()
                .iter()
                .filter(|j| j.owner == self.id && !j.landed())
                .map(|j| JobProgress {
                    id: j.id,
                    title: j.plan.title.clone(),
                    counter: j.plan.counter(),
                    fraction: j.active_snapshot().and_then(|s| s.percent).map(|_| j.plan.fraction()),
                })
                .collect(),
            help: KEYS.iter().map(|(section, keys)| HelpSection { section, keys: keys.to_vec() }).collect(),
        }
    }

    fn task_items(&self) -> Vec<Item> {
        let mut items = Vec::new();
        let on_sub = self.selected_sub();
        let mut counts: [usize; 4] = [0; 4];
        for &i in &self.filtered {
            counts[self.rows[i].section.rank() as usize] += 1;
        }
        let mut last_group = None;
        for (pos, &i) in self.filtered.iter().enumerate() {
            let row = &self.rows[i];
            if last_group != Some(row.section) {
                items.push(Item::Header {
                    label: row.section.label().to_string(),
                    count: Some(counts[row.section.rank() as usize]),
                    color: group_rgb(row.section).hex(),
                });
                last_group = Some(row.section);
            }
            let on_row = pos == self.selected && self.focus == Focus::List;
            let selected = on_row && on_sub.is_none();
            let is_current = self.is_shown(row);
            let (glyph, glyph_rgb) = row_glyph_rgb(row, self.frame);
            let id = row_id(row);
            let rested = matches!(row.status, TaskStatus::Blocked | TaskStatus::Signaled | TaskStatus::Done);
            items.push(Item::Task(Box::new(TaskItem {
                id: id.clone(),
                ws: row.ws_name.clone(),
                ws_color: palette::workspace_color(&row.ws_name).hex(),
                slug: row.slug.clone(),
                title: row.title.clone(),
                title_color: title_rgb(row, selected, is_current).hex(),
                glyph: glyph.to_string(),
                glyph_color: glyph_rgb.hex(),
                status: row.status.token(),
                selected,
                current: is_current,
                closed: row.window_id.is_none(),
                pending: row.pending,
                reason: row_reason(row).map(|(label, fg, bg)| Chip { label, fg: fg.hex(), bg: Some(bg.hex()) }),
                answerable: row.status == TaskStatus::Blocked
                    && row.waiting_for.as_deref().is_some_and(tenx_core::dialog::is_permission_reason),
                locked: !row.secrets_pending.is_empty() || !row.secrets_pending_set.is_empty(),
                wants: row
                    .secrets_pending
                    .iter()
                    .chain(&row.secrets_pending_set)
                    .map(|name| Wanted {
                        name: name.clone(),
                        why: row.secrets_why.iter().find(|(n, _)| n == name).map(|(_, w)| w.clone()).unwrap_or_default(),
                    })
                    .collect(),
                agent: (row.agent != crate::agent::AgentKind::Claude).then(|| row.agent.as_str().to_string()),
                age: row.changed.filter(|_| rested).map(workspace::format_age),
                prs: row
                    .live
                    .prs
                    .iter()
                    .map(|pr| Chip { label: row.live.chip(pr), fg: pr_rgb(&pr.checks).hex(), bg: None })
                    .chain(row.live.watch_only_chips().into_iter().map(|label| Chip { label, fg: pr_rgb("").hex(), bg: None }))
                    .collect(),
                ports: row.live.ports.clone(),
            })));
            for (k, a) in row.subagents.iter().enumerate() {
                let status = a.status.as_task_status();
                let mut extras = Vec::new();
                if a.background && a.status != SubagentStatus::Finished {
                    extras.push("bg".to_string());
                }
                if a.description.is_some() {
                    extras.push(a.agent_type.clone());
                }
                items.push(Item::Sub(SubItem {
                    task: id.clone(),
                    id: a.id.clone(),
                    glyph: status.glyph(),
                    glyph_color: palette::status_color(status).hex(),
                    label: a.label().to_string(),
                    extras,
                    finished: a.status == SubagentStatus::Finished,
                    selected: on_row && on_sub == Some(k),
                }));
            }
        }
        if items.is_empty() {
            items.push(Item::Empty { lines: vec!["no tasks yet — :n to create one".into()] });
        }
        items
    }

    fn repo_view_items(&self) -> Vec<Item> {
        let mut items = Vec::new();
        let mut last_ws = None;
        for (pos, &i) in self.repo_filtered.iter().enumerate() {
            let r = &self.repo_rows[i];
            if last_ws != Some(r.ws_idx) {
                items.push(Item::Header {
                    label: r.ws_name.clone(),
                    count: None,
                    color: palette::workspace_color(&r.ws_name).hex(),
                });
                last_ws = Some(r.ws_idx);
            }
            items.push(Item::Repo(RepoItem {
                pos,
                ws: r.ws_name.clone(),
                name: r.name.clone(),
                cloned: r.cloned,
                detail: if r.cloned { r.commit.clone().unwrap_or_else(|| "—".into()) } else { "not cloned".into() },
                selected: pos == self.repo_selected && self.focus == Focus::List,
            }));
        }
        if items.is_empty() {
            items.push(Item::Empty { lines: vec!["no repos".into()] });
        }
        items
    }

    fn job_items(&self) -> Vec<Item> {
        use tenx_core::progress::StepState;
        let mut items: Vec<Item> = self
            .jobs
            .lock()
            .iter()
            .enumerate()
            .map(|(pos, job)| {
                let snap = job.active_snapshot();
                Item::Job(JobItem {
                    pos,
                    title: job.plan.title.clone(),
                    state: if job.failed() {
                        "failed"
                    } else if job.landed() {
                        "done"
                    } else {
                        "running"
                    },
                    counter: (!job.landed()).then(|| job.plan.counter()),
                    steps: job
                        .plan
                        .steps
                        .iter()
                        .map(|s| StepItem {
                            label: s.label.clone(),
                            state: match s.state {
                                StepState::Pending => "pending",
                                StepState::Running(_) => "running",
                                StepState::Done(_) => "done",
                                StepState::Failed(_) => "failed",
                            },
                            note: s.note().to_string(),
                        })
                        .collect(),
                    fraction: snap.and_then(|s| s.percent).map(|_| job.plan.fraction()),
                    transfer: snap
                        .as_ref()
                        .map(tenx_core::progress::transfer_line)
                        .filter(|t| !t.is_empty()),
                    outcome: job.outcome_note().map(str::to_string),
                    selected: pos == self.work_selected && self.focus == Focus::List,
                })
            })
            .collect();
        if items.is_empty() {
            items.push(Item::Empty {
                lines: vec![
                    "nothing running".into(),
                    "clones and worktree changes show up here while they run.".into(),
                ],
            });
        }
        items
    }

    fn mode_view(&self) -> ModeView {
        let ws_name = |i: usize| self.workspaces.get(i).map(|w| w.config.name.clone()).unwrap_or_default();
        match &self.mode {
            Mode::List => ModeView::List,
            Mode::Command(buffer) => ModeView::Command { buffer: buffer.clone() },
            Mode::Create(f) => ModeView::Create {
                workspace: ws_name(f.ws_idx),
                workspace_index: f.ws_idx + 1,
                workspaces: self.workspaces.len(),
                workspace_options: self
                    .workspaces
                    .iter()
                    .map(|w| WorkspaceOption {
                        name: w.config.name.clone(),
                        color: palette::workspace_color(&w.config.name).hex(),
                    })
                    .collect(),
                agent_options: CreateForm::AGENTS.iter().map(|a| a.map_or("default", |k| k.as_str())).collect(),
                agent_index: f.agent_index(),
                name: f.name.clone(),
                repos: f.repos.iter().map(|(name, checked)| Check { name: name.clone(), checked: *checked }).collect(),
                agent: f.agent_label(),
                agent_inherits: f.agent.is_none(),
                agent_default: f.inherited.as_str(),
                // The slug creation will use: in the adhoc workspace a taken
                // one counts up (`cli::task::plan_slug`, one `exists` per try),
                // elsewhere it is the name's slug as is.
                slug: match self.workspaces.get(f.ws_idx) {
                    Some(ws) if ws.is_adhoc() && !f.name.trim().is_empty() => {
                        crate::cli::task::plan_slug(ws, &f.name).unwrap_or_else(|_| tenx_core::slug::slugify(&f.name))
                    }
                    _ => tenx_core::slug::slugify(&f.name),
                },
                focus: match f.focus {
                    CreateForm::WORKSPACE => "workspace",
                    CreateForm::NAME => "name",
                    n if n == f.agent_field() => "agent",
                    _ => "repo",
                },
                focus_repo: f.repo_field(),
            },
            Mode::AddRepo(f) => ModeView::AddRepo {
                workspace: ws_name(f.ws_idx),
                url: f.url.clone(),
                name: f.name.clone(),
                focus: if f.focus == 0 { "url" } else { "name" },
            },
            Mode::NewWorkspace(f) => ModeView::NewWorkspace {
                path: f.path.clone(),
                name: f.name.clone(),
                repo_url: f.repo_url.clone(),
                skills: f.skills,
                focus: match f.focus {
                    0 => "path",
                    1 => "name",
                    2 => "repo_url",
                    _ => "skills",
                },
            },
            Mode::EditRepos(f) => ModeView::EditRepos {
                task: f.title.clone(),
                picks: f
                    .picks
                    .iter()
                    .map(|p| Pick { name: p.name.clone(), checked: p.checked, present: p.present })
                    .collect(),
                focus: f.focus,
                confirm: f.confirm,
            },
            Mode::Confirm(c) => ModeView::Confirm { title: c.title.clone() },
            Mode::Rename(f) => ModeView::Rename { buffer: f.buffer.clone() },
            Mode::Reject(f) => {
                let row = self.rows.iter().find(|r| r.ws_idx == f.ws_idx && r.slug == f.slug);
                let why = |name: &str| {
                    row.and_then(|r| r.secrets_why.iter().find(|(n, _)| n == name)).map(|(_, w)| w.clone()).unwrap_or_default()
                };
                ModeView::Reject {
                    task: row.map(|r| r.title.clone()).unwrap_or_else(|| f.slug.clone()),
                    id: row.map(row_id).unwrap_or_else(|| format!("{}/{}", ws_name(f.ws_idx), f.slug)),
                    names: f.names.iter().map(|n| Wanted { name: n.clone(), why: why(n) }).collect(),
                    note: f.buffer.clone(),
                }
            }
            Mode::Help(scroll) => ModeView::Help { scroll: *scroll },
        }
    }

    /// A key from a front end that isn't crossterm, through the same handler
    /// as the terminal's (`handle_key`). Keys it can't name are dropped.
    pub(crate) fn handle_web_key(&mut self, key: &WebKey) -> Result<bool> {
        match key.to_event() {
            Some(ev) => self.handle_key(ev),
            None => Ok(false),
        }
    }

    /// A click on something the view named — the id-based twin of
    /// `handle_mouse`'s left click: a tab, the search field, a task or one
    /// of its subagents, a repo or a job. Only in the list, like the mouse;
    /// selecting follows the selection, and nothing opens on a click.
    pub(crate) fn handle_click(&mut self, click: &Click) {
        if let Click::Field { index } = click {
            self.focus_field(*index);
            return;
        }
        if !matches!(self.mode, Mode::List) {
            return;
        }
        match click {
            Click::Tab { index } => {
                if let Some(tab) = Tab::ALL.get(*index).copied() {
                    self.select_tab(tab);
                }
            }
            Click::Search => self.focus_search(),
            Click::Task { id, sub } => {
                if self.tab != Tab::Tasks {
                    return;
                }
                let Some(pos) = self.filtered.iter().position(|&i| row_id(&self.rows[i]) == *id) else { return };
                self.focus_list();
                self.set_cur_sel(pos);
                self.sub = sub.clone();
                self.follow_selection();
            }
            Click::Item { pos } => {
                if self.tab == Tab::Tasks || *pos >= self.cur_len() {
                    return;
                }
                self.focus_list();
                self.set_cur_sel(*pos);
            }
            // Only the forms have fields; handled above.
            Click::Field { .. } => {}
        }
    }

    /// A web form's edit (`FormOp`), applied to the open form's state — the
    /// same fields the keyboard edits in the terminal, set whole rather than
    /// a key at a time. Submit and cancel go through the form's own Enter and
    /// Escape, so they run exactly the terminal's paths (jobs, errors,
    /// where it lands). Anything that doesn't match the open form is ignored.
    pub(crate) fn handle_form(&mut self, op: &FormOp) -> Result<bool> {
        let is_form = matches!(
            self.mode,
            Mode::Create(_)
                | Mode::AddRepo(_)
                | Mode::NewWorkspace(_)
                | Mode::EditRepos(_)
                | Mode::Rename(_)
                | Mode::Reject(_)
        );
        if !is_form {
            return Ok(false);
        }
        match op {
            FormOp::Submit => return self.handle_key(KeyEvent::from(KeyCode::Enter)),
            FormOp::Cancel => return self.handle_key(KeyEvent::from(KeyCode::Esc)),
            FormOp::Set { field, value } => {
                // One line, as the terminal types it: no control characters.
                let value: String = value.chars().filter(|c| !c.is_control()).collect();
                match (&mut self.mode, field.as_str()) {
                    (Mode::Create(f), "name") => (f.name, f.focus) = (value, CreateForm::NAME),
                    (Mode::AddRepo(f), "url") => (f.url, f.focus) = (value, 0),
                    (Mode::AddRepo(f), "name") => (f.name, f.focus) = (value, 1),
                    (Mode::NewWorkspace(f), "path") => (f.path, f.focus) = (value, 0),
                    (Mode::NewWorkspace(f), "name") => (f.name, f.focus) = (value, 1),
                    (Mode::NewWorkspace(f), "repo_url") => (f.repo_url, f.focus) = (value, 2),
                    (Mode::Rename(f), "title") => f.buffer = value,
                    (Mode::Reject(f), "note") => f.buffer = value,
                    _ => {}
                }
            }
            FormOp::Check { field, index, on } => match (&mut self.mode, field.as_str()) {
                (Mode::Create(f), "repo") => {
                    if let Some(r) = f.repos.get_mut(*index) {
                        r.1 = *on;
                        f.focus = 2 + index;
                    }
                }
                (Mode::EditRepos(f), "repo") if !f.confirm => {
                    if let Some(p) = f.picks.get_mut(*index) {
                        p.checked = *on;
                        f.focus = *index;
                    }
                }
                (Mode::NewWorkspace(f), "skills") => (f.skills, f.focus) = (*on, NewWorkspaceForm::SKILLS),
                _ => {}
            },
            FormOp::Pick { field, index } => {
                let count = self.workspaces.len();
                let mut reload = None;
                match (&mut self.mode, field.as_str()) {
                    (Mode::Create(f), "workspace") if *index < count => {
                        f.focus = CreateForm::WORKSPACE;
                        if f.ws_idx != *index {
                            f.ws_idx = *index;
                            reload = Some(*index);
                        }
                    }
                    (Mode::Create(f), "agent") if *index < CreateForm::AGENTS.len() => {
                        f.agent = CreateForm::AGENTS[*index];
                        f.focus = f.agent_field();
                    }
                    _ => {}
                }
                // A new workspace brings its own repo checklist, as ←/→ does.
                if let Some(ws) = reload {
                    let repos = self.ws_repos(ws);
                    let inherited = self.ws_agent(ws);
                    if let Mode::Create(f) = &mut self.mode {
                        f.repos = repos;
                        f.inherited = inherited;
                        f.focus = CreateForm::WORKSPACE;
                    }
                }
            }
        }
        Ok(false)
    }

    /// A form field was clicked: give it the focus. Out-of-range indexes and
    /// clicks outside a form are ignored.
    fn focus_field(&mut self, index: usize) {
        match &mut self.mode {
            Mode::Create(form) if index < form.field_count() => form.focus = index,
            Mode::AddRepo(form) if index < 2 => form.focus = index,
            Mode::NewWorkspace(form) if index < NewWorkspaceForm::FIELDS => form.focus = index,
            Mode::EditRepos(form) if !form.confirm && index < form.picks.len() => form.focus = index,
            _ => {}
        }
    }

    /// A front end's row or tab-bar button: its key(s), from the list. Nothing outside list
    /// mode — a form or a prompt has its own buttons.
    pub(crate) fn handle_action(&mut self, action: Action) -> anyhow::Result<()> {
        if !matches!(self.mode, Mode::List) {
            return Ok(());
        }
        // `^n` works from either place; everything else is a list key.
        if action != Action::New {
            self.focus_list();
        }
        // Not `D`: on a row that also waits on a permission prompt, `D`
        // answers that first. The button says what it does.
        if action == Action::Reject {
            self.start_reject();
            return Ok(());
        }
        for &(key, ctrl) in action.keys() {
            self.handle_web_key(&WebKey { key: key.into(), ctrl, ..Default::default() })?;
        }
        Ok(())
    }
}

/// A key press as a browser reports it (`KeyboardEvent.key` and its
/// modifiers).
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct WebKey {
    pub(crate) key: String,
    #[serde(default)]
    pub(crate) ctrl: bool,
    #[serde(default)]
    pub(crate) alt: bool,
    #[serde(default)]
    pub(crate) shift: bool,
}

impl WebKey {
    /// The crossterm event a terminal would have sent for it. A Ctrl+letter
    /// is lowercased (browsers report `A` with Shift held), and Shift+Tab is
    /// `BackTab`, as crossterm reports them.
    pub(crate) fn to_event(&self) -> Option<KeyEvent> {
        let code = match self.key.as_str() {
            "Enter" => KeyCode::Enter,
            "Escape" | "Esc" => KeyCode::Esc,
            "Backspace" => KeyCode::Backspace,
            "Delete" => KeyCode::Delete,
            "Tab" if self.shift => KeyCode::BackTab,
            "Tab" => KeyCode::Tab,
            "ArrowUp" | "Up" => KeyCode::Up,
            "ArrowDown" | "Down" => KeyCode::Down,
            "ArrowLeft" | "Left" => KeyCode::Left,
            "ArrowRight" | "Right" => KeyCode::Right,
            "PageUp" => KeyCode::PageUp,
            "PageDown" => KeyCode::PageDown,
            "Home" => KeyCode::Home,
            "End" => KeyCode::End,
            other => {
                let mut chars = other.chars();
                let c = chars.next()?;
                if chars.next().is_some() {
                    return None; // "Shift", "F5", "Dead", …
                }
                KeyCode::Char(if self.ctrl { c.to_ascii_lowercase() } else { c })
            }
        };
        let mut modifiers = KeyModifiers::NONE;
        if self.ctrl {
            modifiers |= KeyModifiers::CONTROL;
        }
        if self.alt {
            modifiers |= KeyModifiers::ALT;
        }
        if self.shift {
            modifiers |= KeyModifiers::SHIFT;
        }
        Some(KeyEvent::new(code, modifiers))
    }
}

/// An edit from a front end's own form (`handle_form`): a whole text value,
/// a checkbox, a picker choice, or submit / cancel.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(crate) enum FormOp {
    /// A text field's whole value: `name` (create, add repo, new workspace),
    /// `url` (add repo), `path` / `repo_url` (new workspace), `title` (rename).
    Set { field: String, value: String },
    /// A checkbox, set (not toggled, so a repeated message can't flip it
    /// back): `repo` by index (create, edit repos), `skills` (new workspace).
    Check {
        field: String,
        #[serde(default)]
        index: usize,
        on: bool,
    },
    /// A picker: `workspace` (index into `workspace_options`) or `agent`
    /// (index into `agent_options`), on the create form.
    Pick { field: String, index: usize },
    Submit,
    Cancel,
}

/// A click on something a [`ColumnView`] named.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Click {
    /// A tab, by its place in the bar.
    Tab { index: usize },
    Search,
    /// A task row (`sub: None`) or one of its subagent lines.
    Task {
        id: String,
        #[serde(default)]
        sub: Option<String>,
    },
    /// A repo or a job, by its `pos`.
    Item { pos: usize },
    /// A form's field, by its place in the form (the order ⇥ walks): the
    /// field takes the focus, as if ⇥ had been pressed until it had it.
    Field { index: usize },
}

/// What a row or tab-bar button in a front end does: the list-mode key it
/// stands for, pressed with the list (not the search field) in focus — so a
/// tap on "rename" renames the selection rather than typing `r` into the
/// filter.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Action {
    Open,
    Approve,
    Deny,
    Rename,
    EditRepos,
    Close,
    Delete,
    Unlock,
    Transcript,
    Next,
    New,
    AddRepo,
    NewWorkspace,
    Help,
    /// Reject every pending secrets request of the row (`:reject`).
    Reject,
}

impl Action {
    fn keys(self) -> &'static [(&'static str, bool)] {
        match self {
            Action::Open => &[("Enter", false)],
            Action::Approve => &[("A", false)],
            Action::Deny => &[("D", false)],
            Action::Rename => &[("r", false)],
            Action::EditRepos => &[("e", false)],
            Action::Close => &[("x", false)],
            Action::Delete => &[("d", false), ("d", false)],
            Action::Unlock => &[("u", false)],
            Action::Transcript => &[("t", false)],
            Action::Next => &[("n", false)],
            Action::New => &[("n", true)],
            Action::AddRepo => &[("a", false)],
            Action::NewWorkspace => &[("W", false)],
            Action::Help => &[("?", false)],
            Action::Reject => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(k: &str) -> WebKey {
        WebKey { key: k.into(), ..Default::default() }
    }

    fn selection(v: &ColumnView) -> Option<(String, Option<String>)> {
        v.items.iter().find_map(|i| match i {
            Item::Task(t) if t.selected => Some((t.id.clone(), None)),
            Item::Sub(s) if s.selected => Some((s.task.clone(), Some(s.id.clone()))),
            _ => None,
        })
    }

    /// The same keys, from a browser or from crossterm, leave the column in
    /// the same state — and the view shows it.
    #[test]
    fn web_keys_drive_the_same_state_machine() {
        let mut web = screenshot::fixture_column();
        let mut term = screenshot::fixture_column();
        web.offline = true;
        term.offline = true;
        let steps: [(&str, KeyEvent); 5] = [
            ("Escape", KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            ("j", KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            ("j", KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            ("k", KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)),
            ("n", KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE)),
        ];
        for (k, ev) in steps {
            web.handle_web_key(&key(k)).unwrap();
            term.handle_key(ev).unwrap();
            assert_eq!(selection(&web.view()), selection(&term.view()), "after {k}");
            assert_eq!(web.view().footer, term.view().footer, "after {k}");
        }
        let v = web.view();
        assert_eq!(v.focus, "list");
        assert_eq!(v.footer.tag, Some("NORMAL"));
        assert!(selection(&v).is_some());

        // `?` opens the key table; any other key closes it.
        web.handle_web_key(&key("?")).unwrap();
        assert!(matches!(web.view().mode, ModeView::Help { scroll: 0 }));
        web.handle_web_key(&key("x")).unwrap();
        assert!(matches!(web.view().mode, ModeView::List));

        // Ctrl+n from the browser is the new-task form, named field by field.
        web.handle_web_key(&WebKey { key: "n".into(), ctrl: true, ..Default::default() }).unwrap();
        let ModeView::Create { focus, .. } = web.view().mode else { panic!("create form") };
        assert_eq!(focus, "name");
        assert_eq!(web.view().footer.text, CREATE_HINT);
    }

    /// A button presses its key from the list, even while the search field
    /// has the cursor — it never types into the filter.
    #[test]
    fn an_action_acts_on_the_selection_not_the_filter() {
        let mut c = screenshot::fixture_column();
        c.offline = true;
        c.handle_click(&Click::Search);
        assert_eq!(c.view().focus, "search");
        c.handle_action(Action::Rename).unwrap();
        assert!(c.view().filter.is_empty(), "no `r` in the filter");
        assert!(matches!(c.view().mode, ModeView::Rename { .. }));
        // In a form the bar does nothing; the form has its own buttons.
        c.handle_action(Action::Help).unwrap();
        assert!(matches!(c.view().mode, ModeView::Rename { .. }));
    }

    #[test]
    fn a_field_click_moves_the_forms_focus() {
        let mut c = screenshot::fixture_column();
        c.offline = true;
        c.handle_action(Action::New).unwrap();
        let ModeView::Create { focus, .. } = c.view().mode else { panic!("create form") };
        assert_eq!(focus, "name");
        c.handle_click(&Click::Field { index: 0 });
        let ModeView::Create { focus, .. } = c.view().mode else { panic!("create form") };
        assert_eq!(focus, "workspace");
        c.handle_click(&Click::Field { index: 999 });
        let ModeView::Create { focus, .. } = c.view().mode else { panic!("create form") };
        assert_eq!(focus, "workspace", "an index past the last field is ignored");
    }

    /// A fixture column with two workspaces of their own repos, so the create
    /// form has a workspace to switch and a checklist to reload.
    fn with_workspaces() -> Column {
        use crate::workspace::{RepoConfig, Workspace, WorkspaceConfig};
        let ws = |name: &str, repos: &[&str]| Workspace {
            dir: PathBuf::from(format!("/work/{name}")),
            config: WorkspaceConfig {
                name: name.into(),
                repos: repos.iter().map(|r| RepoConfig { name: (*r).into(), url: format!("git@x:{r}") }).collect(),
                ..Default::default()
            },
        };
        let mut c = screenshot::fixture_column();
        c.offline = true;
        c.workspaces = vec![ws("acme", &["api", "web"]), ws("notes", &["notes"])];
        c.mode = Mode::Create(CreateForm {
            ws_idx: 0,
            name: String::new(),
            repos: c.ws_repos(0),
            agent: None,
            inherited: crate::agent::AgentKind::Claude,
            focus: CreateForm::NAME,
        });
        c
    }

    fn form(c: &mut Column, json: &str) {
        let op: FormOp = serde_json::from_str(json).unwrap();
        c.handle_form(&op).unwrap();
    }

    #[test]
    fn a_web_form_sets_whole_text_values() {
        let mut c = with_workspaces();
        form(&mut c, r#"{"op":"set","field":"name","value":"Fix login\n timeout"}"#);
        let ModeView::Create { name, focus, .. } = c.view().mode else { panic!("create form") };
        assert_eq!(name, "Fix login timeout", "one line: control characters dropped");
        assert_eq!(focus, "name");
        // A field the open form doesn't have is ignored.
        form(&mut c, r#"{"op":"set","field":"url","value":"x"}"#);
        assert!(matches!(c.view().mode, ModeView::Create { .. }));
    }

    #[test]
    fn the_create_form_previews_the_slug_and_what_default_means() {
        let mut c = with_workspaces();
        form(&mut c, r#"{"op":"set","field":"name","value":"Rate limit: login!"}"#);
        let ModeView::Create { slug, agent_default, .. } = c.view().mode else { panic!("create form") };
        assert_eq!(slug, tenx_core::slug::slugify("Rate limit: login!"), "the rule task creation uses");
        assert_eq!(agent_default, "claude");
        // A workspace with its own agent: "default" now means that one.
        c.workspaces[1].config.agent = "codex".into();
        form(&mut c, r#"{"op":"pick","field":"workspace","index":1}"#);
        let ModeView::Create { agent_default, .. } = c.view().mode else { panic!("create form") };
        assert_eq!(agent_default, "codex");
        form(&mut c, r#"{"op":"set","field":"name","value":"   "}"#);
        let ModeView::Create { slug, .. } = c.view().mode else { panic!("create form") };
        assert_eq!(slug, "", "no slug until the name makes one");
    }

    #[test]
    fn a_web_form_checks_repos_and_picks_workspace_and_agent() {
        let mut c = with_workspaces();
        form(&mut c, r#"{"op":"check","field":"repo","index":1,"on":false}"#);
        form(&mut c, r#"{"op":"check","field":"repo","index":1,"on":false}"#);
        let ModeView::Create { repos, focus_repo, .. } = c.view().mode else { panic!() };
        assert_eq!(repos.iter().map(|r| r.checked).collect::<Vec<_>>(), [true, false], "set, not toggled");
        assert_eq!(focus_repo, Some(1));
        form(&mut c, r#"{"op":"check","field":"repo","index":9,"on":true}"#);

        // Another workspace brings its own checklist, as ←/→ does.
        form(&mut c, r#"{"op":"pick","field":"workspace","index":1}"#);
        let ModeView::Create { workspace, repos, workspace_options, .. } = c.view().mode else { panic!() };
        assert_eq!(workspace, "notes");
        assert_eq!(repos.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["notes"]);
        assert_eq!(workspace_options.len(), 2);
        form(&mut c, r#"{"op":"pick","field":"workspace","index":7}"#);
        let ModeView::Create { workspace, .. } = c.view().mode else { panic!() };
        assert_eq!(workspace, "notes", "out of range is ignored");

        form(&mut c, r#"{"op":"pick","field":"agent","index":2}"#);
        let ModeView::Create { agent, agent_index, agent_options, .. } = c.view().mode else { panic!() };
        assert_eq!((agent.as_str(), agent_index), ("codex", 2));
        assert_eq!(agent_options, ["default", "claude", "codex", "pi"]);
    }

    #[test]
    fn web_form_cancel_and_submit_are_the_forms_own_keys() {
        let mut c = with_workspaces();
        form(&mut c, r#"{"op":"cancel"}"#);
        assert!(matches!(c.view().mode, ModeView::List));
        // Outside a form, submit is not ⏎ on the list (which would open a task).
        form(&mut c, r#"{"op":"submit"}"#);
        assert!(matches!(c.view().mode, ModeView::List));

        // Rename: a whole title; an empty one stays open with the error.
        c.mode = Mode::Rename(RenameForm { slug: "x".into(), path: PathBuf::from("/nowhere"), buffer: "old".into() });
        form(&mut c, r#"{"op":"set","field":"title","value":"  "}"#);
        form(&mut c, r#"{"op":"submit"}"#);
        assert!(matches!(c.view().mode, ModeView::Rename { .. }));
        assert_eq!(c.view().status.as_deref(), Some("title cannot be empty"));
    }

    #[test]
    fn web_form_checks_edit_repos_and_the_skills_box() {
        let mut c = with_workspaces();
        c.mode = Mode::EditRepos(EditReposForm {
            ws_idx: 0,
            slug: "t".into(),
            title: "T".into(),
            picks: vec![RepoPick { name: "api".into(), checked: true, present: true }],
            focus: 0,
            confirm: false,
        });
        form(&mut c, r#"{"op":"check","field":"repo","index":0,"on":false}"#);
        let ModeView::EditRepos { picks, .. } = c.view().mode else { panic!() };
        assert!(!picks[0].checked);

        c.mode = Mode::NewWorkspace(NewWorkspaceForm { path: String::new(), name: String::new(), repo_url: String::new(), skills: true, focus: 0 });
        form(&mut c, r#"{"op":"check","field":"skills","on":false}"#);
        form(&mut c, r#"{"op":"set","field":"repo_url","value":"git@x:a"}"#);
        let ModeView::NewWorkspace { skills, repo_url, focus, .. } = c.view().mode else { panic!() };
        assert!(!skills);
        assert_eq!((repo_url.as_str(), focus), ("git@x:a", "repo_url"));
    }

    /// The cursor on a closed task: the view names it for the empty screen,
    /// and the marker (`current`) moves onto it; back on the terminal side,
    /// both go back to the window the session shows.
    #[test]
    fn a_closed_task_under_the_cursor_is_what_the_view_shows() {
        let mut c = screenshot::fixture_column();
        c.offline = true;
        let shown = c.view().current.clone().expect("the current window's task");
        assert_eq!(c.view().shown_closed, None);
        let closed = c.filtered.iter().position(|&i| c.rows[i].window_id.is_none() && !c.rows[i].pending).unwrap();
        c.selected = closed;
        let row = &c.rows[c.filtered[closed]];
        let v = c.view();
        let sc = v.shown_closed.expect("the empty screen");
        assert_eq!(sc.id, row_id(row));
        assert_eq!(sc.title, row.title);
        assert_eq!(v.current.as_deref(), Some(sc.id.as_str()), "the marker is on what's shown");
        let marked: Vec<String> = v
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Task(t) if t.current => Some(t.id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(marked, std::slice::from_ref(&sc.id), "one marker, on the closed task");
        // The keyboard goes back to the terminal: the session's window again.
        c.blur();
        let v = c.view();
        assert_eq!(v.shown_closed, None);
        assert_eq!(v.current.as_deref(), Some(shown.as_str()));
    }

    #[test]
    fn view_lists_sections_rows_and_subagents() {
        let c = screenshot::fixture_column();
        let v = c.view();
        assert_eq!(v.tabs.iter().filter(|t| t.active).count(), 1);
        assert!(matches!(v.items.first(), Some(Item::Header { .. })));
        let subs = v.items.iter().filter(|i| matches!(i, Item::Sub(_))).count();
        assert!(subs >= 2, "the fixture's subagents are listed");
        let headers: Vec<&str> = v
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Header { label, .. } => Some(label.as_str()),
                _ => None,
            })
            .collect();
        assert!(headers.contains(&"WAITING FOR INPUT"));
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["items"][0]["kind"], "header");
        assert_eq!(json["mode"]["kind"], "list");
    }

    /// A click names what it selects; the same row a mouse click would pick.
    #[test]
    fn clicks_select_by_id() {
        let mut c = screenshot::fixture_column();
        c.offline = true;
        let target = c.rows.iter().find(|r| r.slug == "column-screenshot").map(row_id).unwrap();
        c.handle_click(&Click::Task { id: target.clone(), sub: Some("a2".into()) });
        assert_eq!(selection(&c.view()), Some((target.clone(), Some("a2".into()))));
        c.handle_click(&Click::Task { id: target.clone(), sub: None });
        assert_eq!(selection(&c.view()), Some((target, None)));
        c.handle_click(&Click::Tab { index: 2 });
        assert!(c.view().tabs[2].active);
        c.handle_click(&Click::Search);
        assert_eq!(c.view().focus, "search");
    }

    #[test]
    fn browser_keys_map_to_crossterm_events() {
        let ev = |k: &str, ctrl, shift| WebKey { key: k.into(), ctrl, alt: false, shift }.to_event().map(|e| e.code);
        assert_eq!(ev("Tab", false, true), Some(KeyCode::BackTab));
        assert_eq!(ev("N", true, true), Some(KeyCode::Char('n')));
        assert_eq!(ev("A", false, true), Some(KeyCode::Char('A')));
        assert_eq!(ev("ArrowDown", false, false), Some(KeyCode::Down));
        assert_eq!(ev("Shift", false, true), None);
    }

    /// The web's reject form: opened by the row's button (not `D`, which
    /// answers a permission prompt first), lists every pending name with the
    /// agent's reason, takes a note, and rejects them all on submit.
    #[test]
    fn rejecting_a_secrets_request_from_the_web() {
        let mut c = screenshot::fixture_column();
        c.offline = true;
        let row = c.rows.iter_mut().find(|r| !r.secrets_pending.is_empty()).unwrap();
        row.secrets_why = vec![("STRIPE_WEBHOOK_SECRET".into(), "verify webhook signatures".into())];
        let id = row_id(row);
        let task = c.view().items.into_iter().find_map(|i| match i {
            Item::Task(t) if t.id == id => Some(t),
            _ => None,
        });
        let wants = task.unwrap().wants;
        assert_eq!(wants.len(), 1);
        assert_eq!(wants[0].why, "verify webhook signatures", "the row carries the reason");

        c.handle_click(&Click::Task { id: id.clone(), sub: None });
        c.handle_action(Action::Reject).unwrap();
        let ModeView::Reject { names, note, id: form_id, .. } = c.view().mode else { panic!("reject form") };
        assert_eq!(form_id, id);
        assert_eq!(names[0].name, "STRIPE_WEBHOOK_SECRET");
        assert_eq!(names[0].why, "verify webhook signatures");
        assert!(note.is_empty());
        assert!(c.view().footer.text.contains("reject STRIPE_WEBHOOK_SECRET"));

        c.handle_form(&FormOp::Set { field: "note".into(), value: "use the test key".into() }).unwrap();
        let ModeView::Reject { note, .. } = c.view().mode else { panic!("reject form") };
        assert_eq!(note, "use the test key");

        // Cancel keeps the request; submit rejects it.
        c.handle_form(&FormOp::Cancel).unwrap();
        assert!(matches!(c.view().mode, ModeView::List));
        assert!(c.rows.iter().any(|r| row_id(r) == id && !r.secrets_pending.is_empty()));
        c.handle_action(Action::Reject).unwrap();
        c.handle_form(&FormOp::Submit).unwrap();
        assert!(matches!(c.view().mode, ModeView::List));
        assert_eq!(c.status_msg.as_deref(), Some("rejected STRIPE_WEBHOOK_SECRET for 'stripe-webhook-signing'"));
        assert!(c.rows.iter().any(|r| row_id(r) == id && r.secrets_pending.is_empty()));
    }

    #[test]
    fn reject_on_a_row_without_secrets_says_so() {
        let mut c = screenshot::fixture_column();
        c.offline = true;
        let id = c.rows.iter().find(|r| r.secrets_pending.is_empty()).map(row_id).unwrap();
        c.handle_click(&Click::Task { id, sub: None });
        c.handle_action(Action::Reject).unwrap();
        assert!(matches!(c.view().mode, ModeView::List));
        assert_eq!(c.status_msg.as_deref(), Some("no pending secrets for this task"));
    }
}
