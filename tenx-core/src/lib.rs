//! `tenx-core`: the pure, side-effect-free half of tenx.
//!
//! The `tenx` binary shells out to `git`/`tmux`/`age`/`sops` and reads tenx's
//! session registry (fed by every agent's hooks); everything it then *decides*
//! — what a task's status is, which tabs are safe to sweep, what a slug looks
//! like, what a fresh `TASK.md` contains — lives here, as functions over plain
//! data, so it can be tested with fixtures and reused by any future front end.

pub mod codex;
pub mod dialog;
pub mod live;
pub mod column;
pub mod secrets;
pub mod session_event;
pub mod slug;
pub mod status;
pub mod sweep;
pub mod taskmd;
pub mod time;
pub mod transcript;
pub mod trust;
