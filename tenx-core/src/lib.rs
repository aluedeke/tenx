//! `tenx-core`: the pure, side-effect-free half of tenx.
//!
//! The `tenx` binary shells out to `git`/`tmux`/`age`/`sops` and reads Claude
//! Code's session registry; everything it then *decides* — what a task's
//! status is, which tabs are safe to sweep, what a slug looks like, what a
//! fresh `TASK.md` contains — lives here, as functions over plain data, so it
//! can be tested with fixtures and reused by any future front end.

pub mod live;
pub mod slug;
pub mod status;
pub mod sweep;
pub mod taskmd;
pub mod time;
