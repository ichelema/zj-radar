//! Background tasks: work an agent started that runs without its attention (a
//! backgrounded test run, a background subagent, a dev server). They arrive as
//! the child records of a pane's OSC 7501 status (`attention::Record`) and are
//! kept per pane inside `TrackedObservation` as [`BgTask`]s.
//!
//! "Background task", not "job": the activity model already uses *Job* for
//! bounded work as opposed to a *Service*, and a background task can be either
//! (the `holds` flag is that split). No zellij-tile dependency.

use crate::payload::sanitize_in_place;

/// A pane keeps at most this many tasks.
pub const MAX_TASKS: usize = 16;
/// A task `id` truncates to this many chars.
pub const MAX_TASK_ID_CHARS: usize = 32;
/// A task `label` truncates to this many chars.
pub const MAX_TASK_LABEL_CHARS: usize = 64;

/// Where a background task is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TaskState {
    Running,
    Completed,
    Failed,
    /// Gone with no reported outcome. Deliberately not `Completed`: a lost
    /// outcome must never read as success.
    Ended,
}

impl TaskState {
    /// Every state, in declaration order.
    #[cfg(test)]
    pub const ALL: &'static [TaskState] =
        &[TaskState::Running, TaskState::Completed, TaskState::Failed, TaskState::Ended];

    /// Every state but `Running` is over: nothing revives an ended task.
    #[cfg(test)]
    pub fn is_ended(self) -> bool {
        self != TaskState::Running
    }
}

/// One stored background task. Ages are wall-clock epoch seconds (ticks are
/// per plugin instance and would disagree across tabs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BgTask {
    pub id: String,
    pub label: String,
    pub holds: bool,
    pub state: TaskState,
    pub started_epoch_s: u64,
    pub ended_epoch_s: Option<u64>,
}

/// A pane's background tasks plus whether the agent is waiting on them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BgTasks {
    pub items: Vec<BgTask>,
    /// The agent's turn is over and it is waiting on a holding task.
    pub waiting: bool,
}

impl BgTasks {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Scrub stored text and cap the list (see `TrackedObservation::sanitized`).
    pub fn sanitize(&mut self) {
        for task in &mut self.items {
            sanitize_in_place(&mut task.id, MAX_TASK_ID_CHARS);
            sanitize_in_place(&mut task.label, MAX_TASK_LABEL_CHARS);
        }
        self.items.truncate(MAX_TASKS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_scrubs_and_caps_stored_tasks() {
        let task = |id: String, label: &str| BgTask {
            id,
            label: label.into(),
            holds: true,
            state: TaskState::Running,
            started_epoch_s: 0,
            ended_epoch_s: None,
        };
        let mut t = BgTasks {
            items: (0..MAX_TASKS + 4).map(|i| task(format!("t{i}"), "\x1b[2Jtests\n")).collect(),
            waiting: false,
        };
        t.items[0].id = "x".repeat(MAX_TASK_ID_CHARS + 10);
        t.sanitize();
        assert_eq!(t.items.len(), MAX_TASKS);
        assert!(t.items[0].id.chars().count() <= MAX_TASK_ID_CHARS);
        assert!(t.items.iter().all(|x| !x.label.contains('\x1b') && !x.label.contains('\n')));
    }
}
