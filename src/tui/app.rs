use std::collections::HashSet;
use crate::workspace::{Task, Workspace};

#[derive(Debug, Clone, PartialEq)]
pub enum View {
    List,
    Create,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConfirmAction {
    Delete(String),
}

pub struct App {
    pub workspace: Workspace,
    pub tasks: Vec<Task>,
    pub selected: usize,
    pub view: View,
    pub zellij_tabs: HashSet<String>,
    pub status_msg: Option<String>,

    // Create view state
    pub create_name: String,
    pub create_repos: Vec<(String, bool)>, // (name, checked)
    pub create_focus: usize,               // 0=name, 1..=n=repo checkboxes

    // Confirm action
    pub confirm: Option<ConfirmAction>,
}

impl App {
    pub fn new(workspace: Workspace) -> Self {
        let create_repos = workspace
            .config
            .repos
            .iter()
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
        }
    }

    pub fn reload_tasks(&mut self) {
        match self.workspace.tasks() {
            Ok(tasks) => self.tasks = tasks,
            Err(e) => self.status_msg = Some(format!("error: {e}")),
        }
        // Clamp selection
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
        self.zellij_tabs.contains(name)
    }

    pub fn selected_task(&self) -> Option<&Task> {
        self.tasks.get(self.selected)
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.tasks.len() {
            self.selected += 1;
        }
    }

    pub fn enter_create(&mut self) {
        self.create_name.clear();
        self.create_focus = 0;
        // Reset repos to all-checked
        for (_, checked) in &mut self.create_repos {
            *checked = true;
        }
        self.status_msg = None;
        self.view = View::Create;
    }

    pub fn cancel_create(&mut self) {
        self.view = View::List;
        self.status_msg = None;
    }

    pub fn selected_repo_names(&self) -> Vec<String> {
        self.create_repos
            .iter()
            .filter(|(_, checked)| *checked)
            .map(|(name, _)| name.clone())
            .collect()
    }

    pub fn create_focus_next(&mut self) {
        let max = self.create_repos.len(); // 0=name, 1..=n=repos
        self.create_focus = (self.create_focus + 1) % (max + 1);
    }

    pub fn create_focus_prev(&mut self) {
        let max = self.create_repos.len();
        if self.create_focus == 0 {
            self.create_focus = max;
        } else {
            self.create_focus -= 1;
        }
    }

    pub fn toggle_repo(&mut self) {
        let idx = self.create_focus.saturating_sub(1);
        if self.create_focus > 0 && idx < self.create_repos.len() {
            self.create_repos[idx].1 = !self.create_repos[idx].1;
        }
    }

    /// Execute the create task action. Returns error message if failed.
    pub fn do_create(&mut self) -> Result<(), String> {
        let name = self.create_name.trim().to_string();
        if name.is_empty() {
            return Err("task name cannot be empty".into());
        }
        let repos = self.selected_repo_names();
        if repos.is_empty() {
            return Err("select at least one repo".into());
        }
        // Delegate to CLI task::new (no-open=false to open zellij tab)
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

    /// Open or switch to the selected task's zellij tab.
    pub fn do_open(&mut self) -> Result<(), String> {
        let task = match self.selected_task() {
            Some(t) => t.clone(),
            None => return Err("no task selected".into()),
        };
        match crate::cli::task::open(&task.name) {
            Ok(_) => {
                self.reload_tabs();
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// Close the selected task's zellij tab (keep worktrees).
    pub fn do_close(&mut self) -> Result<(), String> {
        let name = match self.selected_task() {
            Some(t) => t.name.clone(),
            None => return Err("no task selected".into()),
        };
        match crate::zellij::close_tab(&name) {
            Ok(_) => {
                self.reload_tabs();
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// Delete the selected task (remove worktrees + dir).
    pub fn do_delete(&mut self) -> Result<(), String> {
        let name = match self.confirm.take() {
            Some(ConfirmAction::Delete(n)) => n,
            None => return Err("no pending delete".into()),
        };
        match crate::cli::task::rm(&name, true) {
            Ok(_) => {
                self.reload_tasks();
                self.reload_tabs();
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    }
}
