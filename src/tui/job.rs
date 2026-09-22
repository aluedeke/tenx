//! One long operation, running off the UI thread.
//!
//! Creating a task clones repos; adding one clones a repo; a checklist can do
//! both. Any of those is seconds to minutes of network, and all of them used
//! to run inside the column's key handler — so the client's event loop didn't
//! turn, the embedded tmux pane froze, no key worked, and the spinner printed
//! itself over ratatui's screen (see `crate::progress`).
//!
//! A [`Job`] moves that work to a worker thread and leaves the loop free. The
//! worker owns everything it touches (paths and names, cloned before the
//! spawn) and reports through a channel; the column holds the receiving end
//! and a [`Plan`] it updates from what arrives. Nothing is shared but the
//! channel, so there is no lock between the UI and the work, and the UI stays
//! the only writer of its own state — when the job finishes, the column
//! rebuilds its rows itself, on its own thread, from disk.
//!
//! A job cannot be cancelled mid-git: `git` is a child process doing
//! filesystem work, and killing it partway is how a half-written bare repo
//! happens. So there is no cancel — a job started is a job finished, watched
//! from the Work tab or not. Several run at once; whatever would actually
//! collide is serialised by `git::lock_repo`, not by queueing them here.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use tenx_core::progress::{Plan, Snapshot, StepState};

use crate::progress::{Event, Reporter};

/// What the column does with its own state once a job lands — after the rows
/// have been rebuilt from disk, on the UI thread.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Then {
    /// Just the message. Deleting a task, detaching repos.
    Nothing,
    /// Put the selection on this task (workspace index, slug) — the one the
    /// job was building, whose ghost row has just been replaced by the real
    /// one. Positions moved in the rebuild, so it is found by slug.
    SelectTask(usize, String),
    /// As `SelectTask`, but give the task a window first — detached, so it is
    /// open and its agent is running without the terminal leaving wherever it
    /// was. What a freshly created task wants.
    OpenTask(usize, String),
    /// A workspace was created here: re-read the registry, then land on its
    /// repos — or on the add-repo form, when it was created without one.
    Workspace(PathBuf),
}

/// What the worker sends back: a step event, or the whole job's outcome.
pub(super) enum Message {
    Step(Event),
    /// The work returned. `Ok` carries a closing line for the footer.
    Done(Result<String, String>),
}

/// The reporter handed to `cli::task`/`cli::init` when the column runs them:
/// every event goes down the channel instead of to a terminal.
///
/// A send that fails means the column dropped the job (the client is quitting,
/// or the panel was replaced). The work carries on regardless — it is git, and
/// stopping it halfway is worse than finishing it unwatched — so the error is
/// deliberately ignored rather than turned into a cancellation.
struct ChannelReporter(Sender<Message>);

impl Reporter for ChannelReporter {
    fn emit(&self, ev: Event) {
        let _ = self.0.send(Message::Step(ev));
    }
}

/// A long operation the column started, and what is known about it so far.
pub(super) struct Job {
    /// The steps and their states, updated from the channel. This is what the
    /// panel draws.
    pub(super) plan: Plan,
    /// Set once the worker reports the whole job's outcome.
    pub(super) outcome: Option<Result<String, String>>,
    /// Set once `take_landing` has handed the outcome to the column, so a
    /// job's follow-up runs exactly once while the job itself stays listed.
    landing_taken: bool,
    /// What the column should do once the job lands, beyond rebuilding its
    /// rows. Kept with the job rather than in a callback because it runs on
    /// the UI thread, against column state the worker never sees.
    pub(super) then: Then,
    rx: Receiver<Message>,
}

impl Job {
    /// Run `work` on a worker thread, reporting into a fresh [`Job`].
    ///
    /// `work` gets the reporter to pass down to `cli::task`/`cli::init`, and
    /// returns the line the footer shows when it lands. It runs on the worker,
    /// so it must own its inputs: capture owned copies (`String`, `PathBuf`,
    /// a cloned `Workspace`) rather than borrowing the column.
    pub(super) fn spawn<F>(plan: Plan, then: Then, work: F) -> Job
    where
        F: FnOnce(&dyn Reporter) -> Result<String, String> + Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::channel();
        let done = tx.clone();
        std::thread::spawn(move || {
            let reporter = ChannelReporter(tx);
            let result = work(&reporter);
            let _ = done.send(Message::Done(result));
        });
        Job { plan, outcome: None, landing_taken: false, then, rx }
    }

    /// Fold everything the worker has sent since the last call into the plan.
    /// Returns true if anything changed, so the caller can skip a redraw.
    ///
    /// Non-blocking by construction: this runs once per frame on the UI
    /// thread, which must never wait on the work.
    pub(super) fn drain(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.rx.try_recv() {
                Ok(Message::Step(ev)) => {
                    self.apply(ev);
                    changed = true;
                }
                Ok(Message::Done(result)) => {
                    self.outcome = Some(result);
                    changed = true;
                }
                // Disconnected without a `Done` means the worker panicked.
                // Say so rather than leaving the panel spinning forever.
                Err(TryRecvError::Disconnected) => {
                    if self.outcome.is_none() {
                        self.outcome = Some(Err("the operation stopped unexpectedly".into()));
                        changed = true;
                    }
                    break;
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        changed
    }

    /// Move one event into the plan. An event for a step the plan doesn't have
    /// is dropped: the plan is built from the same list the work walks, but a
    /// repo that vanished between the two would otherwise panic the client.
    fn apply(&mut self, ev: Event) {
        let (idx, state) = match ev {
            Event::Start { step, .. } => (step, StepState::Running(None)),
            Event::Update { step, snap } => (step, StepState::Running(Some(snap))),
            Event::Done { step, note } => (step, StepState::Done(note)),
            Event::Failed { step, err } => (step, StepState::Failed(err)),
        };
        if let Some(s) = self.plan.steps.get_mut(idx) {
            s.state = state;
        }
    }

    /// The job has reported its outcome and there is nothing left to wait for.
    pub(super) fn landed(&self) -> bool {
        self.outcome.is_some()
    }

    /// The outcome, the first time it is asked for after the job lands.
    ///
    /// The job stays in the list afterwards — the Work tab is the only record
    /// of what happened once the footer has moved on — so the column needs a
    /// way to run the follow-up once rather than on every tick.
    pub(super) fn take_landing(&mut self) -> Option<Result<String, String>> {
        if self.landing_taken {
            return None;
        }
        let outcome = self.outcome.clone()?;
        self.landing_taken = true;
        Some(outcome)
    }

    /// What to show beside the job's title: its outcome once settled.
    pub(super) fn outcome_note(&self) -> Option<&str> {
        match self.outcome.as_ref()? {
            Ok(msg) => Some(msg),
            Err(e) => Some(e),
        }
    }

    pub(super) fn failed(&self) -> bool {
        matches!(self.outcome, Some(Err(_)))
    }

    /// The snapshot of the step running now, for the panel's bar.
    pub(super) fn active_snapshot(&self) -> Option<Snapshot> {
        match self.plan.active().and_then(|i| self.plan.steps.get(i)) {
            Some(s) => match &s.state {
                StepState::Running(snap) => *snap,
                _ => None,
            },
            None => None,
        }
    }
}

/// A job in a state a test can set up by hand, with the sending end returned
/// so the channel stays connected (a dropped sender reads as a dead worker).
#[cfg(test)]
pub(super) fn fixture(plan: Plan) -> (Job, Sender<Message>) {
    let (tx, rx) = std::sync::mpsc::channel();
    (Job { plan, outcome: None, landing_taken: false, then: Then::Nothing, rx }, tx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use tenx_core::progress::Phase;

    /// Wait for `f` to hold, draining the job — the worker is a real thread,
    /// so the test has to let it run.
    fn settle(job: &mut Job, f: impl Fn(&Job) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            job.drain();
            if f(job) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("job did not settle in time");
    }

    #[test]
    fn events_land_in_the_plan_in_order() {
        let plan = Plan::new("creating 'x'", ["a".to_string(), "b".to_string()]);
        let mut job = Job::spawn(plan, Then::Nothing, |rep| {
            for (step, label) in ["a", "b"].iter().enumerate() {
                rep.emit(Event::Start { step, label: (*label).into(), verb: "cloning" });
                rep.emit(Event::Update {
                    step,
                    snap: Snapshot { phase: Phase::Receiving, percent: Some(50), bytes: None, rate: None },
                });
                rep.emit(Event::Done { step, note: "cloned".into() });
            }
            Ok("created 'x'".into())
        });

        settle(&mut job, |j| j.landed());
        assert!(job.plan.finished());
        assert_eq!(job.plan.counter(), "2/2");
        assert_eq!(job.outcome.as_ref().unwrap().as_deref(), Ok("created 'x'"));
    }

    #[test]
    fn a_failure_reaches_both_the_step_and_the_outcome() {
        let plan = Plan::new("creating 'x'", ["a".to_string()]);
        let mut job = Job::spawn(plan, Then::Nothing, |rep| {
            rep.emit(Event::Start { step: 0, label: "a".into(), verb: "cloning" });
            rep.emit(Event::Failed { step: 0, err: "no such repo".into() });
            Err("no such repo".into())
        });

        settle(&mut job, |j| j.landed());
        assert_eq!(job.plan.failure(), Some("no such repo"));
        assert!(job.outcome.as_ref().unwrap().is_err());
    }

    #[test]
    fn an_event_for_a_step_the_plan_lacks_is_dropped() {
        // The plan is built from the same list the work walks, but it must not
        // be possible for a stale index to take the client down.
        let plan = Plan::new("creating 'x'", ["a".to_string()]);
        let mut job = Job::spawn(plan, Then::Nothing, |rep| {
            rep.emit(Event::Done { step: 7, note: "cloned".into() });
            Ok("done".into())
        });
        settle(&mut job, |j| j.landed());
        assert_eq!(job.plan.counter(), "0/1");
    }

    #[test]
    fn a_panicking_worker_lands_as_an_error_rather_than_hanging() {
        let plan = Plan::new("creating 'x'", ["a".to_string()]);
        let mut job = Job::spawn(plan, Then::Nothing, |_| panic!("boom"));
        settle(&mut job, |j| j.landed());
        assert!(job.outcome.as_ref().unwrap().is_err());
    }

    #[test]
    fn the_active_snapshot_is_the_running_steps() {
        let plan = Plan::new("creating 'x'", ["a".to_string(), "b".to_string()]);
        let (tx, rx) = std::sync::mpsc::channel();
        // Drive the plan by hand: `spawn`'s worker would race the assertions.
        let mut job = Job { plan, outcome: None, landing_taken: false, then: Then::Nothing, rx };
        tx.send(Message::Step(Event::Start { step: 0, label: "a".into(), verb: "cloning" })).unwrap();
        job.drain();
        assert_eq!(job.active_snapshot(), None, "running, but git hasn't spoken");

        let snap = Snapshot { phase: Phase::Receiving, percent: Some(12), bytes: None, rate: None };
        tx.send(Message::Step(Event::Update { step: 0, snap })).unwrap();
        job.drain();
        assert_eq!(job.active_snapshot(), Some(snap));

        tx.send(Message::Step(Event::Done { step: 0, note: "cloned".into() })).unwrap();
        job.drain();
        assert_eq!(job.active_snapshot(), None, "nothing running between steps");
    }

    #[test]
    fn drain_reports_whether_anything_moved() {
        let plan = Plan::new("x", ["a".to_string()]);
        let (tx, rx) = std::sync::mpsc::channel();
        let mut job = Job { plan, outcome: None, landing_taken: false, then: Then::Nothing, rx };
        assert!(!job.drain(), "an empty channel is not a redraw");
        tx.send(Message::Step(Event::Start { step: 0, label: "a".into(), verb: "cloning" })).unwrap();
        assert!(job.drain());
        assert!(!job.drain());
    }
}
