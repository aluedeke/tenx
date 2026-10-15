//! The speaker in `tenx web`'s header: the last reply of a task's agent,
//! read aloud. `POST /speak?task=<id>` answers the reply's text in pieces —
//! from the agent's transcript, as `tenx task output` reads it
//! (`cli::drive::last_turn`), made speakable (`tenx_core::readaloud`) and
//! cut into sentences (`tenx_core::tts::pieces`) — and `POST /speak/audio`
//! turns one piece into audio with `tenx-tts` (`tts.rs`). The page plays a
//! piece while it asks for the next, so a long reply starts at once.

use anyhow::{Result, bail};

/// The pieces of the task's last reply; starts `tenx-tts` loading meanwhile.
pub(super) fn pieces(task_id: &str) -> Result<Vec<String>> {
    let config = super::tts::config()?;
    std::thread::spawn(move || super::tts::warm(&config));
    let text = tenx_core::readaloud::spoken_text(&last_reply(task_id)?);
    let pieces = tenx_core::tts::pieces(&text);
    if pieces.is_empty() {
        bail!("the last reply has nothing to read aloud");
    }
    Ok(pieces)
}

/// One piece read aloud, as a WAV.
pub(super) fn audio(text: &str) -> Result<Vec<u8>> {
    super::tts::speak(&super::tts::config()?, text)
}

/// The agent's last message since the last prompt.
fn last_reply(task_id: &str) -> Result<String> {
    let Some((ws_name, slug)) = tenx_core::web::task_id(task_id) else {
        bail!("no such task: {task_id:?}");
    };
    let ws = crate::cli::task::load_ws_arg(ws_name)?;
    let task = ws.find_task(slug)?;
    let turn = crate::cli::drive::last_turn(&ws, &task)?;
    match turn.replies.last() {
        Some(reply) => Ok(reply.clone()),
        None => bail!("the agent has not replied since the last prompt"),
    }
}
