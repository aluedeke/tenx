//! `POST /secrets` and `POST /secrets/answer`: a task's pending secrets
//! requests answered in a web form instead of the terminal's `fulfill`
//! sheet — on a phone a real form can paste, and offer the password manager,
//! where a terminal can't. The page opens the form when the column asks for
//! an unlock (`tab.rs`). The answering is `cli::secrets::answer_in`, the
//! values and the passphrase pass through here and are never logged.

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::cli::secrets::{self, Answered, Sitting};
use crate::workspace::{Task, Workspace};

/// What the form sends.
#[derive(Deserialize)]
pub(super) struct Answer {
    /// The names the form listed.
    shown: Vec<String>,
    /// Those of them granted; the rest are denied.
    granted: Vec<String>,
    /// A value for each granted value request, by name.
    #[serde(default)]
    values: Vec<(String, String)>,
    #[serde(default)]
    note: String,
    #[serde(default)]
    passphrase: String,
}

/// The task's pending requests (`task_id`: a column row's id).
pub(super) fn sitting(task_id: &str) -> Result<Sitting> {
    let (ws, task) = task(task_id)?;
    secrets::sitting_in(&ws, &task)
}

/// Answer the task's requests.
pub(super) fn answer(task_id: &str, answer: Answer) -> Result<Answered> {
    let (ws, task) = task(task_id)?;
    let note = Some(answer.note.trim()).filter(|n| !n.is_empty());
    let passphrase = Some(answer.passphrase.as_str()).filter(|p| !p.is_empty());
    secrets::answer_in(&ws, &task, &answer.shown, &answer.granted, &answer.values, note, passphrase)
}

fn task(task_id: &str) -> Result<(Workspace, Task)> {
    let Some((ws_name, slug)) = tenx_core::web::task_id(task_id) else {
        bail!("no such task: {task_id:?}");
    };
    let ws = crate::cli::task::load_ws_arg(ws_name)?;
    let task = ws.find_task(slug)?;
    Ok((ws, task))
}
