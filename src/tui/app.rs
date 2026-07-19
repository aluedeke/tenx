use std::collections::HashSet;
use ratatui::{layout::Rect, widgets::ListState};
use crate::workspace::{Task, Workspace};
use super::mouse::ClickTracker;

#[derive(Debug, Clone, PartialEq)]
pub enum View { List, Create }

#[derive(Debug, Clone, PartialEq)]
pub enum ConfirmAction { Delete(String) }

pub struct App {
    pub workspace: Workspace,
    pub tasks: Vec<Task>,
    pub selected: usize,
    pub view: View,
    pub zellij_tabs: HashSet<String>,
    pub status_msg: Option<String>,

    pub create_name: String,
    pub create_repos: Vec<(String, bool)>,
    pub create_focus: usize,

    pub confirm: Option<ConfirmAction>,

    // Mouse support: the last-rendered list area + persisted scroll state let a
    // click's row map back to a task index; the areas of the create-popup fields
    // let a click focus/toggle them. Populated during render.
    pub list_state: ListState,
    pub list_area: Rect,
    pub create_name_area: Rect,
    pub create_repo_areas: Vec<Rect>,
    pub click: ClickTracker,
}

impl App {
    pub fn new(workspace: Workspace) -> Self {
        let create_repos = workspace.config.repos.iter()
            .map(|r| (r.name.clone(), true))
            .collect();
        App {
            workspace,
            tasks: vec![],
            selected: 0,
            view: View::List,
            zellij_tabs: HashSet::new(),
            status_msg: None,
            create_name: String::new(),
            create_repos,
            create_focus: 0,
            confirm: None,
            list_state: ListState::default(),
            list_area: Rect::default(),
            create_name_area: Rect::default(),
            create_repo_areas: Vec::new(),
            click: ClickTracker::new(),
        }
    }

    pub fn reload_tasks(&mut self) {
        match self.workspace.tasks() {
            Ok(tasks) => self.tasks = tasks,
            Err(e) => self.status_msg = Some(format!("error: {e}")),
        }
        if !self.tasks.is_empty() && self.selected >= self.tasks.len() {
            self.selected = self.tasks.len() - 1;
        }
    }

    pub fn reload_tabs(&mut self) {
        if crate::zellij::is_inside_session() {
            if let Ok(tabs) = crate::zellij::list_tabs() {
                self.zellij_tabs = tabs.into_iter().map(|t| t.name).collect();
            }
        }
    }

    pub fn task_is_open(&self, name: &str) -> bool {
        // Compare ignoring a leading 💬 notification prefix, which the Stop hook
        // adds to an active tab's name.
        self.zellij_tabs
            .iter()
            .any(|t| t.trim_start_matches('\u{1F4AC}').trim() == name)
    }

    pub fn selected_task(&self) -> Option<&Task> {
        self.tasks.get(self.selected)
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 { self.selected -= 1; }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.tasks.len() { self.selected += 1; }
    }

    pub fn enter_create(&mut self) {
        self.create_name.clear();
        self.create_focus = 0;
        // Reload repos in case they changed since we started
        if let Ok(cwd) = std::env::current_dir() {
            if let Ok(ws) = crate::workspace::find(&cwd) {
                self.create_repos = ws.config.repos.iter()
                    .map(|r| (r.name.clone(), true))
                    .collect();
                self.workspace = ws;
            }
        }
        self.status_msg = None;
        self.view = View::Create;
    }

    pub fn cancel_create(&mut self) {
        self.view = View::List;
        self.status_msg = None;
    }

    pub fn selected_repo_names(&self) -> Vec<String> {
        self.create_repos.iter()
            .filter(|(_, checked)| *checked)
            .map(|(name, _)| name.clone())
            .collect()
    }

    pub fn create_focus_next(&mut self) {
        let max = self.create_repos.len();
        self.create_focus = (self.create_focus + 1) % (max + 1);
    }

    pub fn create_focus_prev(&mut self) {
        let max = self.create_repos.len();
        self.create_focus = if self.create_focus == 0 { max } else { self.create_focus - 1 };
    }

    pub fn toggle_repo(&mut self) {
        let idx = self.create_focus.saturating_sub(1);
        if self.create_focus > 0 && idx < self.create_repos.len() {
            self.create_repos[idx].1 = !self.create_repos[idx].1;
        }
    }

    pub fn do_create(&mut self) -> Result<(), String> {
        let name = self.create_name.trim().to_string();
        if name.is_empty() { return Err("task name cannot be empty".into()); }
        let repos = self.selected_repo_names();
        if repos.is_empty() { return Err("select at least one repo".into()); }
        match crate::cli::task::new(&name, Some(&repos), false) {
            Ok(_) => {
                self.view = View::List;
                self.reload_tasks();
                self.reload_tabs();
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn do_open(&mut self) -> Result<(), String> {
        let task = match self.selected_task() {
            Some(t) => t.clone(),
            None => return Err("no task selected".into()),
        };
        match crate::cli::task::open(&task.name) {
            Ok(_) => { self.reload_tabs(); Ok(()) }
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn do_close(&mut self) -> Result<(), String> {
        let task = match self.selected_task() {
            Some(t) => t.clone(),
            None => return Err("no task selected".into()),
        };
        match close_task_tab(&task.path, &task.display_name) {
            Ok(_) => { self.reload_tabs(); Ok(()) }
            Err(e) => Err(e),
        }
    }

    pub fn do_delete(&mut self) -> Result<(), String> {
        let slug = match self.confirm.take() {
            Some(ConfirmAction::Delete(n)) => n,
            None => return Err("no pending delete".into()),
        };
        // Close the tab before rm removes the task dir (and its .tenx-tab-id).
        // Best-effort: a task with no open tab just no-ops.
        if let Some(task) = self.tasks.iter().find(|t| t.name == slug) {
            let _ = close_task_tab(&task.path, &task.display_name);
        }
        match crate::cli::task::rm(&slug, true) {
            Ok(_) => {
                self.reload_tasks();
                self.reload_tabs();
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Close a task's zellij tab, preferring the stable `.tenx-tab-id` so a tab
/// carrying the 💬 notification prefix still gets closed. Falls back to
/// (emoji-tolerant) name matching if the id file is missing or stale.
fn close_task_tab(task_path: &std::path::Path, display_name: &str) -> Result<(), String> {
    let id = std::fs::read_to_string(task_path.join(".tenx-tab-id"))
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok());
    if let Some(id) = id {
        if crate::zellij::close_tab_by_id(id).is_ok() {
            return Ok(());
        }
    }
    crate::zellij::close_tab(display_name).map_err(|e| e.to_string())
}
