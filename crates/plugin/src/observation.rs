//! Resolved per-pane observation vocabulary shared by status and command sources.

use crate::kind::Kind;
use crate::payload::{sanitize_in_place, MAX_MSG_CHARS};
use crate::status::Status;
use crate::task::BgTasks;

/// Which source produced an observation: a program reporting through OSC 7501
/// (an agent, a build) or a tracked shell command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ObservationOrigin {
    Program,
    Command,
}

/// A resolved observation for one pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackedObservation {
    pub origin: ObservationOrigin,
    pub status: Status,
    pub msg: String,
    /// The source kind that produced this observation. Classified once at intake
    /// (`attention::kind_for_app`, or `attention::command_kind` for a shell
    /// command); the renderer reads it directly rather than
    /// re-parsing a string.
    pub kind: Kind,
    pub last_change_tick: u64,
    pub ever_active: bool,
    /// Exit code of a finished command pane, when known; `None` for programs
    /// and for commands that finish by returning to the shell prompt (no exit
    /// code is reported). Drives the `exit N` outcome tag on error rows.
    pub exit_code: Option<i32>,
    /// Wall-clock second (Unix epoch) at which this observation first became
    /// `Done`/`Error`. `None` while live/idle.
    pub completed_epoch_s: Option<u64>,
    /// Wall-clock second at which this observation entered `Pending` — when
    /// the agent started waiting on the user. `None` for every other status.
    /// Drives the rail's `· 12m` wait tag.
    pub pending_epoch_s: Option<u64>,
    /// Background tasks the program reports as OSC 7501 child records
    /// (`crate::task`). Program origin only.
    pub tasks: BgTasks,
}

impl TrackedObservation {
    /// Running work that draws an *animated* glyph — the one observation state
    /// that needs per-second ticks. Running steady rows (services, remote
    /// sessions) are excluded: they render a steady mark, so nothing about them
    /// animates (`docs/activity-model.md` §3). This is the "never poll" guarantee for a live remote
    /// session: without it, an ssh connection left open overnight would pin
    /// the 1 Hz Fast cadence in every tab's plugin instance
    /// (`AGENTS.md` non-negotiables, `docs/smart-tabs-postmortem.md`).
    pub fn animating(&self) -> bool {
        self.status == Status::Running && !self.kind.is_steady()
    }

    /// Scrub the free-text fields (they come from the pane's own program, so
    /// may carry control characters) and cap their length before they reach
    /// the rendered grid.
    pub fn sanitized(mut self) -> Self {
        sanitize_in_place(&mut self.msg, MAX_MSG_CHARS);
        self.tasks.sanitize();
        self
    }
}
