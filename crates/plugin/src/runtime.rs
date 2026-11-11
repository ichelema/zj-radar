//! Deep runtime module: repo-owned events in, ordered host effects out.
//! No zellij-tile dependency.
//!
//! [`PluginRuntime`] is the pure state machine behind the wasm glue in
//! `lib.rs`. The glue translates each Zellij host event into one method call
//! here, and every mutating call returns an [`Outcome`] (a render flag plus an
//! ordered list of [`Effect`]s the glue replays against the host). Because the
//! mapping is total and side-effect-free, host tests drive these entry points
//! directly and assert on the returned `Outcome` — no Zellij needed.
//!
//! # Event entry points
//! - [`load`](PluginRuntime::load) — first run: seed config, request
//!   permissions.
//! - [`tabs_changed`](PluginRuntime::tabs_changed) /
//!   [`panes_changed`](PluginRuntime::panes_changed) /
//!   [`cwd_changed`](PluginRuntime::cwd_changed) — Zellij topology updates.
//! - [`attention_changed`](PluginRuntime::attention_changed) — one pane's
//!   native attention moved (`PaneAttentionChanged`); the manifest in
//!   `panes_changed` carries the rest.
//! - [`config_pipe`](PluginRuntime::config_pipe) /
//!   [`control`](PluginRuntime::control) /
//!   [`control_pipe`](PluginRuntime::control_pipe) — runtime config + remote
//!   commands.
//! - [`mode_changed`](PluginRuntime::mode_changed) —
//!   `Event::ModeUpdate`'s input mode (the `show_mode` footer label) and its
//!   `session_name`, the push-style source that
//!   replaced `SessionUpdate` for learning this session's own name
//!   (`SessionUpdate`'s peer list never populates without a plugin calling
//!   the blocking `get_session_list()`, which stock zj-radar never does —
//!   see `docs/design.md`, "Why not SessionUpdate").
//! - [`global_attention_changed`](PluginRuntime::global_attention_changed) —
//!   every other session, from Zellij's `GlobalAttentionUpdate`.
//! - [`timer`](PluginRuntime::timer) — periodic tick (animation +
//!   needs-permission repaint + cross-session cycle commit).
//! - [`mouse_click`](PluginRuntime::mouse_click) — resolved against the cached
//!   [`RenderedRail`] for click-to-switch.
//! - [`mouse_right_click`](PluginRuntime::mouse_right_click) — the
//!   acknowledge verb, resolved against the same cached rail.
//! - [`permission_result`](PluginRuntime::permission_result) — Zellij's grant /
//!   deny verdict.

use crate::config;
use crate::control::Verb;
use crate::radar_state::{Direction, PaneUpdate, RadarChange, RadarState, RadarTab, TabId};
use crate::render::{self, RenderedRail};
use crate::rollup::{LedgerLine, TabRow};
use crate::sessions::SessionSummary;
use crate::sessions::{BadgeEntry, CommitTarget, Sessions};
use crate::status::Status;
use crate::tab_namer::TabRename;
use crate::theme;

/// How urgently the one-shot timer should re-fire. `Fast` is the 1 Hz tick
/// that drives animation and debounce/TTL bookkeeping; `Slow` backs off to a
/// once-a-minute heartbeat when nothing needs per-second resolution but a
/// ledger age is still changing. `Frame(fps)` is `Fast` subdivided for the
/// spinner: armed only while this rail is visible and something spins at
/// full speed, it fires `fps` times a second, and `timer` still runs the
/// full tick only once per real second — the in-between fires just advance
/// the spinner and repaint. `desired_cadence` selects among them (or `None`
/// to fully disarm).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cadence {
    Fast,
    Frame(u32),
    Slow,
}

impl Cadence {
    pub(crate) fn seconds(self) -> f64 {
        match self {
            Cadence::Fast => 1.0,
            Cadence::Frame(fps) => 1.0 / f64::from(fps.max(1)),
            Cadence::Slow => 60.0,
        }
    }
}

/// Slack for "a full second has accumulated" in `timer`'s frame math: four
/// 0.25s fires must sum to a tick despite float rounding and scheduler
/// jitter, and a frame fire that lands a hair early must not be mistaken for
/// a tick's worth.
const SUBTICK_SLACK_S: f64 = 0.05;

/// A `Timer` fire whose reported elapsed exceeds this came from a Slow (60s)
/// arm. Fast fires report ~1s and Slow ~60s, so any threshold safely between
/// the two works; 5s tolerates heavy scheduler delay on a fast fire (a fast
/// fire arriving >5s late is pathological) while never mistaking a slow fire
/// for fast. Used only to decide which of two in-flight fires is the stale
/// one — see [`PluginRuntime::timer`].
const STALE_FIRE_ELAPSED_S: f64 = 5.0;

/// Our permission request: every instance requests at `load`, and Zellij
/// answers `Granted` from its cache when `zj-radar setup` pre-wrote the grant
/// (`setup/preseed.rs`), so only an ungranted first run shows a prompt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PermissionState {
    /// Before `load`: not granted, not requesting.
    #[default]
    Unprompted,
    /// Our request is in flight; the pane is selectable and each tick
    /// repaints the needs-permission face until Zellij answers.
    Requesting,
    /// Terminal: Zellij (or the user) answered.
    Resolved { granted: bool },
}

impl PermissionState {
    /// True once permission is granted (gates clicks, pipes, and the live rail).
    pub(crate) fn granted(&self) -> bool {
        matches!(self, PermissionState::Resolved { granted: true })
    }

    /// True once permission is definitively denied: the rail is a static
    /// needs-permission face and the events that clear domain work never
    /// arrive, so the cadence disarms fully (see `desired_cadence`).
    pub(crate) fn denied(&self) -> bool {
        matches!(self, PermissionState::Resolved { granted: false })
    }

    /// True only while our request is in flight: the tick repaints the
    /// needs-permission face, holds Fast cadence, and keeps the pane
    /// selectable (the user must be able to reach Zellij's y/n prompt).
    pub(crate) fn is_requesting(&self) -> bool {
        matches!(self, PermissionState::Requesting)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    RequestPermission,
    SetSelectable(bool),
    SetTimeout(Cadence),
    RenameTab {
        tab_id: TabId,
        name: String,
    },
    SwitchTab {
        position: usize,
    },
    ShowPane {
        pane_id: u32,
    },
    /// Read these panes' working directories once (blocking `get_pane_cwd`) to
    /// bootstrap a name for a freshly-opened tab before it emits `CwdChanged`.
    ///
    /// Unlike the other (fire-and-forget) effects, this one is a *request*: the
    /// glue feeds each result back through `cwd_changed`, which re-enters the
    /// runtime and may itself emit `RenameTab`. The recursion is bounded —
    /// `cwd_changed` never emits another `ResolveCwd` — but note that this
    /// effect's full consequences are realized in that second pass, not in the
    /// `Outcome` that carried it.
    ResolveCwd {
        pane_ids: Vec<u32>,
    },
    /// Read `cwd`'s git `origin` remote URL with an async `run_command`; the
    /// `RunCommandResult` comes back as `repo_resolved`. Once per cwd and
    /// instance (`RadarState::repo_names`) — the second exception to the
    /// push-driven rule, after `ResolveCwd`.
    ResolveRepo {
        cwd: String,
    },
    /// Commit a cross-session cycle selection: switch to `name` and, once
    /// there, jump straight to the tab (or pane) that needs attention, if any.
    /// Emitted by `timer` when `Sessions::tick` reports an idle commit, and
    /// by a click on a peer line.
    SwitchSession {
        name: String,
        tab_position: Option<usize>,
        pane_id: Option<u32>,
    },
    /// Mark a pane seen in Zellij's own attention state
    /// (`mark_session_pane_seen`). Emitted by `mouse_right_click`'s
    /// pending-pane acknowledge and the `✓` hotspot: Zellij clears the pane's
    /// attention and every rail converges on the resulting `PaneUpdate`, so
    /// the click mutates nothing here.
    MarkSeen {
        session: String,
        pane_id: u32,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub render: bool,
    pub effects: Vec<Effect>,
}

impl Outcome {
    fn none() -> Self {
        Self::default()
    }

    fn with_effects(render: bool, effects: Vec<Effect>) -> Self {
        Self { render, effects }
    }
}

/// The one-shot timer chain. Zellij's `set_timeout` is non-cancellable, so a
/// cadence change can leave TWO fires in flight; the pairing "arming grows the
/// in-flight count exactly where a `SetTimeout` is emitted, a fire retires
/// exactly one, a stale fire is swallowed whole" is a single invariant — held
/// structurally: the fields are private, so [`TimerChain::arm`] and
/// [`TimerChain::on_fire`] are the only ways to move them.
#[derive(Default)]
struct TimerChain {
    /// The cadence the last-scheduled `SetTimeout` was armed with, or `None`
    /// if the timer is fully disarmed.
    armed: Option<Cadence>,
    /// How many `SetTimeout` fires are still in flight. The Slow→Fast top-up
    /// leaves two; the slow-armed one is stale and must be swallowed by
    /// `on_fire` — ticking on it would re-arm a second persistent chain
    /// (N chains → N Hz, every tick-window elapsing N× too fast).
    pending_fires: u32,
}

/// What a `Timer` event means for the runtime — see [`TimerChain::on_fire`].
enum Fire {
    /// The live chain: process the tick and re-arm.
    Live,
    /// A stale leftover from before a cadence top-up: swallow it whole —
    /// no tick, no re-arm, the live arm untouched.
    Stale,
}

impl TimerChain {
    /// Arm (or re-arm) toward `desired`. `Some(c)` means the caller MUST emit
    /// `Effect::SetTimeout(c)` — returning the cadence from the same place
    /// the count grows is what keeps them paired. Compares the *cadence* the
    /// previous arm used, not just "is anything armed": a Slow-armed timer
    /// that should now be Fast gets a fresh fast arm immediately, rather than
    /// waiting for the (harmless, spurious) slow fire to notice. Every other
    /// transition — first arm, already-correct cadence, nothing desired — is
    /// a no-op.
    ///
    /// Only a Slow arm is ever topped up — to `Fast` or `Frame` alike. Two
    /// sub-second-to-1s fires in flight could never be told apart (staleness
    /// is read off a fire's elapsed, and only a Slow fire's is distinctive),
    /// so a `Fast`↔`Frame` switch waits for the in-flight fire (≤1s) to
    /// re-arm at the new cadence instead.
    fn arm(&mut self, desired: Option<Cadence>) -> Option<Cadence> {
        let arm = match (self.armed, desired) {
            (Some(Cadence::Slow), Some(c @ (Cadence::Fast | Cadence::Frame(_)))) => Some(c),
            (None, Some(cadence)) => Some(cadence),
            _ => None,
        };
        if let Some(cadence) = arm {
            self.armed = Some(cadence);
            self.pending_fires += 1;
        }
        arm
    }

    /// Retire one in-flight fire, pairing the count with the fire's reported
    /// `elapsed_s` to identify a stale one: swallow ([`Fire::Stale`]) only
    /// when BOTH hold — its elapsed marks it slow-armed
    /// (> `STALE_FIRE_ELAPSED_S`) and another fire is still out
    /// (post-decrement count > 0). A fast fire always processes: swallowing
    /// by count alone would freeze the tick clock in the common order, where
    /// the live fast fire lands first and the stale slow fire lands up to 59s
    /// later. A slow fire with nothing else in flight IS the live chain.
    ///
    /// Convergence, common order: fast fire lands (2→1), processes, re-arms
    /// (→2); each following fast fire repeats that; the stale slow finally
    /// lands (2→1), swallowed → steady single chain. Rare order (a top-up in
    /// the slow window's final second): the stale slow lands first (2→1),
    /// swallowed; the fast fire then processes (1→0) and re-arms (→1). The
    /// Fast→Slow wind-down converges too: an older slow always lands before a
    /// newer slow, so the older one still sees a fire in flight and is the one
    /// swallowed. `saturating_sub` keeps a direct `timer()` call with nothing
    /// armed (tests drive the entry point that way) counting as a live fire.
    fn on_fire(&mut self, elapsed_s: f64) -> Fire {
        self.pending_fires = self.pending_fires.saturating_sub(1);
        if elapsed_s > STALE_FIRE_ELAPSED_S && self.pending_fires > 0 {
            return Fire::Stale;
        }
        self.armed = None;
        Fire::Live
    }

    /// The currently armed cadence (`None` = fully disarmed). Read-only —
    /// the battery-property tests assert arm/disarm through this.
    #[cfg(test)]
    fn armed(&self) -> Option<Cadence> {
        self.armed
    }

    /// Test-only escape hatch: force the disarmed state so a test can watch
    /// an arm happen in isolation. Explicit and greppable, unlike the raw
    /// field write it replaced — production code has no way to do this.
    #[cfg(test)]
    fn disarm_for_test(&mut self) {
        self.armed = None;
    }
}

#[derive(Default)]
pub(crate) struct PluginRuntime {
    pub(crate) radar: RadarState,
    pub(crate) tick: u64,
    /// The spinner's animation clock: +1 on EVERY live timer fire, ticks and
    /// sub-second frame fires alike (`Cadence::Frame`). At 1 Hz it moves in
    /// step with `tick`; at `spinner_fps` N it moves N× per tick. Render-only
    /// — nothing but the spinner glyph reads it, so `tick` keeps meaning
    /// "seconds" for every grace/TTL/interval clock.
    frame: u64,
    /// Real seconds accumulated by frame fires since the last tick. A frame
    /// fire whose total reaches a second runs the full tick instead — so the
    /// tick rate stays ~1 Hz no matter the frame rate (see `timer`).
    subtick_elapsed_s: f64,
    /// The one-shot `SetTimeout` chain — see [`TimerChain`]. Every arm goes
    /// through `arm_timer_if_needed` (`load` and `project`
    /// both arm through it); every fire retires through `timer`.
    timer_chain: TimerChain,
    pub(crate) last_render_height: usize,
    pub(crate) config: config::Config,
    pub(crate) permission: PermissionState,
    /// The colors the renderer uses: always [`resolve_theme`](Self::resolve_theme)'s
    /// pick of `pane_theme` > `palette_theme` > the neutral fallback.
    pub(crate) theme: theme::DerivedColors,
    /// Colors from a pane's reported `default_bg`/`default_fg`. Sticky: a
    /// manifest with no reporting pane leaves the last one in place.
    pane_theme: Option<theme::DerivedColors>,
    /// Colors from the Zellij theme palette on the latest `ModeUpdate`
    /// (`None` = no usable truecolor palette). Each `ModeUpdate` replaces it,
    /// so a switch to an 8-bit theme drops back to the fallback.
    palette_theme: Option<theme::DerivedColors>,
    last_rendered: RenderedRail,
    /// Cross-session peer state + the Alt+[/] cycle selection machine.
    sessions: Sessions,
    /// This session's own name, learned from `learn_session_name`
    /// (`Event::ModeUpdate`'s `ModeInfo.session_name`). Empty until Zellij's
    /// first `ModeUpdate` lands — `project` withholds the own badge row
    /// while empty.
    own_session_name: String,
    /// The Zellij input mode from the latest `ModeUpdate` (`mode_changed`);
    /// `None` until the first one lands. Tracked whether or not `show_mode`
    /// is on, so a live toggle shows the current mode at once — but it reaches
    /// the render key and the footer only through [`shown_mode`](Self::shown_mode).
    mode: Option<crate::mode::Mode>,
    /// The most recent `now_epoch_s` any entry point has captured. Reused
    /// (never re-read from the clock) by call paths that have no epoch of
    /// their own to work with — `learn_session_name`,
    /// `global_attention_changed` — so a single event's "now" never forks in two
    /// directions.
    last_now_epoch_s: u64,
    /// The `radar.generation()` the own badge counts were derived at. They
    /// are a pure function of radar state, so an event that didn't advance
    /// the generation can skip the whole derive — under interpreted wasm that
    /// derive (a full `rows()` pass) was the bulk of `project`'s per-event
    /// cost. `None` forces a recompute (start-up, or a session-name change,
    /// which alters the counts' identity without touching radar state).
    summary_gen: Option<u64>,
    /// Everything content-derived the last actual `render()` drew: rows,
    /// ledger lines, badge, theme, shown mode. Stamped by `render`, consulted by
    /// `project`'s render gate — a change whose re-derived key equals what is
    /// already on screen requests no repaint (`RadarChange::force_render`
    /// bypasses, for tick-driven frames). Zellij delivers every broadcast
    /// and every `PaneAttentionChanged` to EVERY tab's instance, so vetoing
    /// identical repaints here is one of the two big levers on session-wide
    /// plugin CPU; the other is `hidden`.
    last_render_key: Option<RenderKey>,
    /// Zellij told this instance its tab is off-screen (`Event::Visible(false)`
    /// — sent on tab switch, tab close, layout apply, and client detach;
    /// never at load, so a fresh instance counts as visible). Hidden gates
    /// two things: the paint (>95% of a tick's interpreter cost, and nobody
    /// sees it), and the `cmd.v1` verbs (see `control`: every tab's rail
    /// receives each command, and a hidden rail's frozen `tabs` still mark
    /// its own tab active, so letting it act would race the visible rail's
    /// `SwitchTab` with a wrong one). State keeps ticking at full cadence, so a slowed
    /// instance would drift from its siblings until reveal. `Visible(true)` repaints the skipped frame.
    /// Zellij 0.44 routes `TabUpdate`/`PaneUpdate` to the plugins in a
    /// client's *active* tab only, so receiving one proves this instance
    /// visible and clears the flag even if a `Visible(true)` was lost — a
    /// wrongly-hidden rail would be a stale screen, so the bias is toward
    /// visible.
    hidden: bool,
    /// The tab position the latest manifest located this plugin's own pane
    /// in (`None` until one has). Refreshed by `own_plugin_tab_changed` on
    /// every `PaneUpdate`, so — unlike `RadarState`'s naming ownership, which
    /// freezes to a `TabId` — it tracks the tab as positions shift.
    own_tab_position: Option<usize>,
    /// Latched once any manifest has shown a terminal pane sharing the rail's
    /// tab; never cleared. The evidence
    /// [`desired_selectable`](Self::desired_selectable) needs: Zellij closes a
    /// tab whose tiled panes are all non-selectable, so a rail that goes
    /// passive with no terminal neighbor closes its own tab — and, as the last
    /// tab, the session (issue #46). A LATCH, not a level, on purpose: when the
    /// tab's last shell exits, Zellij's `ClosePane` arm reports the new
    /// manifest to plugins synchronously and defers the tab-closing render
    /// (debounced), so a rail that flipped back to selectable on "no terminals
    /// left" would beat that render and keep a dead tab alive — verified
    /// live: `exit` in the only shell left a rail-only session running.
    /// Once a terminal has lived here, the tab's fate on emptying is Zellij's.
    own_tab_saw_terminal: bool,
    /// The last `SetSelectable` value emitted (`None` = never). `sync_selectable`
    /// emits on change only — the first sync always emits, declaring the
    /// initial state explicitly rather than trusting the host's default.
    selectable_sent: Option<bool>,
}

/// See [`PluginRuntime::last_render_key`].
/// The last field is the *shown* mode (`shown_mode`): `None` whenever
/// `show_mode` is off, so mode changes never move the key then.
type RenderKey = (
    std::rc::Rc<Vec<TabRow>>,
    std::rc::Rc<Vec<LedgerLine>>,
    Vec<BadgeEntry>,
    theme::DerivedColors,
    Option<crate::mode::Mode>,
);

/// The ONE derivation of the own badge row's counts. They are deliberately pane-level
/// while the rail itself remains tab-rolled-up. `rows()` is already
/// constrained by the current pane topology, so a stale pane id cannot
/// inflate cross-session counts. Panes count by the rule peers count by
/// (`sessions::reported`): a running command is left out, a failed one waits.
/// `attention_tab_position` is the first (lowest-position) row that
/// `needs_you()`.
fn summary_counts(rows: &[TabRow]) -> (usize, usize, Option<usize>) {
    let mut running = 0usize;
    let mut attention = 0usize;
    let mut attention_tab_position = None;
    for r in rows {
        let mut tab_attention = 0usize;
        for p in &r.display.panes {
            let Some(st) = p.status() else { continue };
            let command = !p.is_status_origin();
            if !crate::sessions::reported(st, command) {
                continue;
            }
            running += usize::from(st == Status::Running && !command);
            tab_attention += usize::from(st.needs_you());
        }
        if tab_attention > 0 {
            attention += tab_attention;
            attention_tab_position.get_or_insert(r.tab_position());
        }
    }
    (running, attention, attention_tab_position)
}

impl PluginRuntime {
    pub(crate) fn load(&mut self, config: config::Config) -> Outcome {
        self.config = config;
        self.sessions.set_show_trees(self.config.session_tree);
        // Load's single clock capture, stored eagerly (and reused by the arm
        // below, keeping one "now" per event).
        self.last_now_epoch_s = crate::clock::now_epoch_s();
        self.permission = PermissionState::Requesting;
        let mut effects = vec![Effect::RequestPermission];
        // Pre-grant Zellij withholds the state events that would otherwise
        // trigger a paint (they need ReadApplicationState), so this timer is
        // the only thing that gets the needs_permission screen onto the rail.
        self.arm_timer_if_needed(self.last_now_epoch_s, &mut effects);
        // Load always declares the sidebar's selectability — and before any
        // manifest it can only be *selectable*: going passive here is what
        // closed a rail-only tab (`desired_selectable`).
        self.sync_selectable(&mut effects);
        Outcome::with_effects(false, effects)
    }

    pub(crate) fn build_rows(&self) -> std::rc::Rc<Vec<TabRow>> {
        self.radar.rows(self.tick)
    }

    pub(crate) fn own_plugin_tab_changed(&mut self, position: Option<usize>) {
        self.own_tab_position = position;
        self.radar.own_plugin_tab_changed(position);
    }

    /// Whether the rail pane should be selectable right now — THE rule, as a
    /// pure function of state. Selectable while our own permission request is
    /// in flight (Zellij's y/n prompt is tied to the requesting pane) and
    /// until a manifest proves a terminal pane shares the tab: a passive pane
    /// is one Zellij won't focus, and a tab with nothing focusable is a tab
    /// Zellij closes (`Screen::render` → `tabs_to_close`). A `children`
    /// placeholder nested in a split spawns no terminal when the tab body is
    /// empty (zellij#5618, #3247) — the rail must never be what turns that
    /// layout quirk into a dead session. Once a terminal neighbor has been
    /// seen the rail goes passive so it never steals focus from the shell,
    /// and stays passive (see `own_tab_saw_terminal` for why it must not come
    /// back when the neighbors vanish). Rails that never see a manifest —
    /// denied permission, a tab never activated — simply stay selectable,
    /// the harmless side.
    fn desired_selectable(&self) -> bool {
        self.permission.is_requesting() || !self.own_tab_saw_terminal
    }

    /// Emit `SetSelectable` when [`desired_selectable`](Self::desired_selectable)
    /// differs from the last value sent (always on the first call). Called
    /// from exactly the entry points that can move the rule's inputs — the
    /// permission transitions and `panes_changed` — not from `project`, so an
    /// unrelated broadcast never grows a spurious host call.
    fn sync_selectable(&mut self, effects: &mut Vec<Effect>) {
        let want = self.desired_selectable();
        if self.selectable_sent != Some(want) {
            self.selectable_sent = Some(want);
            effects.push(Effect::SetSelectable(want));
        }
    }

    pub(crate) fn tabs_changed(&mut self, tabs: Vec<RadarTab>) -> Outcome {
        self.hidden = false; // manifests reach the active tab's plugins only (see `hidden`)
        let had_naming_tab = self.radar.has_naming_tab();
        let mut change = self.radar.tabs_changed(tabs);
        // Ownership just resolved: a cwd that arrived before it (bootstrap
        // fires from the PaneUpdate that precedes this TabUpdate) computed no
        // rename, so re-run naming now rather than waiting for an unrelated
        // event to leave the tab on `Tab #N`.
        if !had_naming_tab && self.radar.has_naming_tab() {
            change.renames = self.radar.recompute_renames(self.config.naming);
        }
        self.project(vec![], change, crate::clock::now_epoch_s())
    }

    pub(crate) fn panes_changed(&mut self, update: PaneUpdate) -> Outcome {
        self.hidden = false; // manifests reach the active tab's plugins only (see `hidden`)
        let now = crate::clock::now_epoch_s();
        if let Some(theme) = update.theme.clone() {
            self.pane_theme = Some(theme);
        }
        self.resolve_theme();
        // `PaneUpdate::from_raw` already dropped plugin panes, so a non-empty
        // entry for our own tab position IS a terminal neighbor. Latch only.
        self.own_tab_saw_terminal |=
            self.own_tab_position.is_some_and(|pos| update.tab_panes.get(&pos).is_some_and(|panes| !panes.is_empty()));
        let mut effects = Vec::new();
        self.sync_selectable(&mut effects);
        let change = self.radar.panes_changed(update, self.tick, self.config.naming);
        self.project(effects, change, now)
    }

    /// `elapsed_s` is the duration Zellij reports on `Event::Timer` — the
    /// seconds the fired `set_timeout` was armed with, i.e. which cadence
    /// scheduled this fire. `now_epoch_s` is the event's single clock
    /// capture, taken by the caller —
    /// lib.rs passes `clock::now_epoch_s()`; the virtual-time harness
    /// (`runtime/tests.rs`'s `FireSim`) passes its own advancing epoch, so
    /// the wall-clock-keyed gates in here move in lockstep with simulated
    /// fires without any clock to mock.
    pub(crate) fn timer(&mut self, elapsed_s: f64, now_epoch_s: u64) -> Outcome {
        // Retire one in-flight fire; a stale one (see `TimerChain::on_fire`)
        // is swallowed whole.
        if let Fire::Stale = self.timer_chain.on_fire(elapsed_s) {
            return Outcome::none();
        }
        // Every live fire advances the spinner. A sub-second fire (a
        // `Cadence::Frame` arm) is frame-only until the real time it and its
        // predecessors reported adds up to a second; that fire runs the full
        // tick below. Summing reported elapsed (not counting fires) keeps
        // `tick` at ~1 Hz through late fires and Frame↔Fast switches. Not
        // exactly 1 Hz: Zellij's elapsed is the timer's sleep alone, so each
        // fire's processing and re-arm go uncounted — a tick costs
        // `1s + fires × overhead`. At ~1ms per frame (measured with the former wasm-fuel bench) that is a
        // few ms per tick more than the plain Fast chain's one fire, a
        // sub-percent skew against hidden siblings that the grace/TTL windows
        // (seconds wide) absorb.
        self.frame += 1;
        if elapsed_s + SUBTICK_SLACK_S < Cadence::Fast.seconds() {
            self.subtick_elapsed_s += elapsed_s;
            if self.subtick_elapsed_s + SUBTICK_SLACK_S < Cadence::Fast.seconds() {
                return self.frame_fire(now_epoch_s);
            }
        }
        self.subtick_elapsed_s = 0.0;
        // Which arm produced this live fire. `elapsed_s` is the same signal
        // `TimerChain::on_fire` just consumed for staleness: Fast fires
        // report ~1s, Slow ~60s, and `STALE_FIRE_ELAPSED_S` sits safely
        // between the two — reused here (rather than inventing parallel
        // state) as the cadence discrimination for the gates below.
        let is_slow_fire = elapsed_s > STALE_FIRE_ELAPSED_S;
        // One clock capture per event (the caller's): every consumer below
        // (cadence decision, re-arm via project) sees the same "now".
        let now = now_epoch_s;
        let mut effects = Vec::new();
        self.tick += 1;
        self.radar.timer(self.tick);
        // BEFORE re-arming below, commit an idle cycle selection if one is
        // pending. Committing here — not after `project` re-arms — matters:
        // a commit clears `Sessions::wants_fast_cadence`, and the chain must
        // be free to decay to Slow on this same pass rather than one fire
        // late.
        let session_commit = self.sessions.tick();
        if let Some(CommitTarget { name, attention_tab_position, attention_pane_id }) = session_commit {
            effects.push(Effect::SwitchSession {
                name,
                tab_position: attention_tab_position,
                pane_id: attention_pane_id,
            });
        }
        // Capture before re-arming: an in-flight permission request must repaint
        // the needs_permission screen each tick until the user answers.
        let awaiting_permission = self.permission.is_requesting();
        let render = awaiting_permission
            || self.timer_should_continue()
            // A Slow fire exists to repaint ledger ages — even when nothing
            // else changed, `format_age` output may have moved.
            // Keyed on the fire that actually landed, not `desired_cadence`:
            // the desire can have moved on (in either direction) between the
            // arm and the fire.
            || is_slow_fire;
        let change = RadarChange {
            render,
            // Tick-driven frames (spinner, ages) redraw identical rows —
            // exempt from the rows-diff render gate by definition.
            force_render: true,
            ..RadarChange::default()
        };
        self.project(effects, change, now)
    }

    /// A frame-only fire: nothing changed but the spinner's frame, so skip
    /// every per-tick duty
    /// and just re-arm and repaint. The repaint bypasses `project`'s rows-diff
    /// gate by construction — the rows are identical, the frame is not — and
    /// honors the visibility gate (a rail that went hidden mid-frame paints
    /// nothing; its re-arm then falls back to `Fast`).
    fn frame_fire(&mut self, now_epoch_s: u64) -> Outcome {
        let mut effects = Vec::new();
        self.arm_timer_if_needed(now_epoch_s, &mut effects);
        Outcome::with_effects(!self.hidden, effects)
    }

    pub(crate) fn mouse_click(&mut self, line: isize, col: usize) -> Outcome {
        if !self.permission.granted() {
            return Outcome::none();
        }
        if let Some(action) = self.last_rendered.hotspot_at(line, col) {
            // A hotspot whose action raced away deliberately consumes the
            // click instead of falling through to row navigation: the user
            // aimed at an action, not at the row behind it.
            return match action {
                render::HotspotAction::Acknowledge { target } => {
                    Outcome::with_effects(false, self.acknowledge_pending_targets(&target))
                }
            };
        }
        let Some(target) = self.last_rendered.target_at_line(line) else {
            return Outcome::none();
        };
        // A cross-session badge line (`session: Some(name)`) always wins the
        // match — those targets never carry a `pane_id`, but checking
        // `session` first (rather than falling into the `pane_id`/`tab_position`
        // arms below) keeps the three cases visibly mutually exclusive rather
        // than relying on that absence. `tab_position` is read through
        // `session_tab_position` (not the raw field), which undoes the
        // `RailTarget::for_session` sentinel encoding back into the
        // `Option<usize>` the effect needs — see both docs.
        let effect = if let Some(name) = target.session.clone() {
            Effect::SwitchSession { name, tab_position: target.session_tab_position(), pane_id: target.pane_id }
        } else if let Some(pane_id) = target.pane_id {
            Effect::ShowPane { pane_id }
        } else {
            Effect::SwitchTab { position: target.tab_position }
        };
        Outcome::with_effects(false, vec![effect])
    }

    /// Right-click: the rail's acknowledge verb (left-click stays pure
    /// navigation). Resolves the clicked line exactly like `mouse_click`
    /// (same cached `RenderedRail`, same permission gate), then acknowledges
    /// a pending pane or tab row: see `acknowledge_pending_targets`. This is
    /// issue #5's fix for the completed-turn-ending-in-a-courtesy-question
    /// trap: a click that means "I saw it, stop asking" marks the pane seen in
    /// Zellij. A peer-session badge line
    /// (`session: Some(_)`) is not a pane or tab: a no-op.
    pub(crate) fn mouse_right_click(&mut self, line: isize, _col: usize) -> Outcome {
        if !self.permission.granted() {
            return Outcome::none();
        }
        let Some(target) = self.last_rendered.target_at_line(line) else {
            return Outcome::none();
        };
        if target.session.is_some() {
            return Outcome::none();
        }
        Outcome::with_effects(false, self.acknowledge_pending_targets(&target))
    }

    /// The pane-or-tab half of `mouse_right_click`: every pane a click on
    /// `target` should check for a dismissible `Pending` — one pane for a
    /// pane row (`target.pane_id: Some(p)`), every pane in the tab for a tab
    /// row (`target.pane_id: None` — a tab's header line, or a plain tab with
    /// no per-pane detail line at all) — turned into one `Effect::MarkSeen`
    /// per pane whose native observation currently reads `Pending`. A target
    /// with nothing Pending on it produces no effects, and so does a session
    /// whose name is not known yet (`MarkSeen` addresses a pane by session).
    /// Deliberately returns `render: false` from the caller regardless: this
    /// gesture never mutates `radar` — Zellij's own seen mark comes back as
    /// the next `PaneUpdate`.
    fn acknowledge_pending_targets(&self, target: &render::RailTarget) -> Vec<Effect> {
        if self.own_session_name.is_empty() {
            return Vec::new();
        }
        let pane_ids = match target.pane_id {
            Some(pane_id) => vec![pane_id],
            None => self.radar.pane_ids_for_tab(target.tab_position),
        };
        pane_ids
            .into_iter()
            .filter(|&pane_id| self.radar.observation(pane_id).is_some_and(|o| o.status == Status::Pending))
            .map(|pane_id| Effect::MarkSeen { session: self.own_session_name.clone(), pane_id })
            .collect()
    }

    /// Run an imperative command verb. `AttentionNext/Prev` resolve a
    /// deterministic target tab and emit `SwitchTab`; `SessionNext/Prev`
    /// advance the cross-session cycle selection (`Sessions::cycle`) and
    /// render the highlight move — the actual switch is a *later* idle-commit
    /// (see `timer`), not an immediate effect here. Inert until permission is
    /// granted, mirroring `mouse_click`: session switching is
    /// `ChangeApplicationState` territory, same as `SwitchTab`.
    ///
    /// Also inert on a hidden rail. A keybind's `MessagePlugin` and
    /// `zellij action pipe` reach EVERY tab's rail instance, but only the
    /// visible one's view is current: `TabUpdate` reaches the active tab's
    /// plugins only, so a hidden rail's `tabs` still mark its own tab active
    /// and it would vote for the attention tab after *its* tab — Zellij
    /// applies each `SwitchTab` in arrival order, the last one wins (#89).
    /// The session verbs are gated the same way so one press moves one
    /// cycle selection and commits one `SwitchSession`, not one per tab. A
    /// rail that was never shown counts as visible but has no tabs, so the
    /// attention verbs already resolve to nothing there. Two clients on
    /// different tabs each have a visible rail, and `PipeSource::Keybind`
    /// names no client, so a press can still move the other client: no
    /// plugin-side signal tells the rails apart.
    pub(crate) fn control(&mut self, verb: Verb) -> Outcome {
        if !self.permission.granted() || self.hidden {
            return Outcome::none();
        }
        let dir = match verb {
            Verb::AttentionNext | Verb::SessionNext => Direction::Next,
            Verb::AttentionPrev | Verb::SessionPrev => Direction::Prev,
        };
        match verb {
            Verb::AttentionNext | Verb::AttentionPrev => match self.radar.next_attention_tab(dir) {
                Some(position) => Outcome::with_effects(false, vec![Effect::SwitchTab { position }]),
                None => Outcome::none(),
            },
            Verb::SessionNext | Verb::SessionPrev => {
                let render = self.sessions.cycle(dir);
                // A fresh tap must arm Fast immediately (not wait for the next
                // domain change to pass through `project`), so the idle-commit
                // in `timer` fires promptly rather than stalling behind a Slow
                // or fully-disarmed chain.
                let mut effects = Vec::new();
                self.arm_timer_if_needed(self.last_now_epoch_s, &mut effects);
                Outcome::with_effects(render, effects)
            }
        }
    }

    /// Parse a `cmd.v1` payload and dispatch it. Unknown verbs are a no-op.
    pub(crate) fn control_pipe(&mut self, payload: &str) -> Outcome {
        match crate::control::parse(payload) {
            Some(verb) => self.control(verb),
            None => Outcome::none(),
        }
    }

    /// Test-only: a `ModeUpdate` reduced to its session name — what most
    /// tests drive, with no input mode in play.
    #[cfg(test)]
    pub(crate) fn session_name_changed(&mut self, name: Option<String>) -> Outcome {
        self.learn_session_name(name, RadarChange::default())
    }

    /// `Event::ModeUpdate`: the session name (`learn_session_name`) and the
    /// current input mode, in one pass. A new mode requests a repaint only
    /// while `show_mode` is on, and the render gate drops even that when the
    /// label on screen already matches (the shown mode is in the render key),
    /// so with the option off, or on a repeat, a mode change costs nothing.
    ///
    /// `palette` is the theme-palette color source (`DerivedColors::from_theme_palette`
    /// over `ModeInfo.style.colors`); a change in the resolved colors repaints.
    pub(crate) fn mode_changed(
        &mut self,
        name: Option<String>,
        mode: crate::mode::Mode,
        palette: Option<theme::DerivedColors>,
    ) -> Outcome {
        let moved = self.mode.replace(mode) != Some(mode);
        self.palette_theme = palette;
        let recolored = self.resolve_theme();
        let change = RadarChange { render: (moved && self.config.show_mode) || recolored, ..RadarChange::default() };
        self.learn_session_name(name, change)
    }

    /// Recompute [`theme`](Self::theme) from its sources: pane-reported colors
    /// win, then the Zellij theme palette, then the neutral fallback (issue
    /// #85: Zellij leaves pane colors `null` unless a program sends OSC 10/11).
    /// Returns whether the colors changed.
    fn resolve_theme(&mut self) -> bool {
        let next = self.pane_theme.clone().or_else(|| self.palette_theme.clone()).unwrap_or_default();
        let moved = next != self.theme;
        self.theme = next;
        moved
    }

    /// The mode the footer draws: the latest one, only while `show_mode` is
    /// on. The render key and `RenderOpts` both read it, so a mode change
    /// with the option off never differs from what is on screen.
    fn shown_mode(&self) -> Option<crate::mode::Mode> {
        self.mode.filter(|_| self.config.show_mode)
    }

    /// Learn (or relearn) this session's own name — the push-style source
    /// that replaced `SessionUpdate` (see `docs/design.md`, "Why not
    /// SessionUpdate"): `Event::ModeUpdate`'s
    /// `ModeInfo.session_name`. `None` is a no-op unless `change` asks for a
    /// repaint (a shown mode moved): Zellij can in principle fire
    /// `ModeUpdate` before the session has a name, and there is nothing to
    /// learn from that — re-projecting on every such event would waste a
    /// `project()` pass for nothing new to report. A `Some`
    /// always re-projects, even a repeat of the already-known name:
    /// `ModeUpdate` fires on far more than name changes (any mode/keybind/
    /// pane-group change too), and letting `project`'s own idempotent
    /// content-compare (`Sessions::set_own`'s badge diff) absorb the repeats is simpler, and no less correct, than
    /// hand-rolling an equality check here.
    fn learn_session_name(&mut self, name: Option<String>, change: RadarChange) -> Outcome {
        let Some(name) = name else {
            // Nameless: nothing to learn, but a shown mode may still repaint.
            return if change.render { self.project(vec![], change, self.last_now_epoch_s) } else { Outcome::none() };
        };
        if name != self.own_session_name {
            // The name is the own row's identity, which no radar mutation
            // tracks — drop the cached generation so `project` re-derives it
            // even though `radar.generation()` hasn't moved.
            self.summary_gen = None;
        }
        self.own_session_name = name;
        self.project(vec![], change, self.last_now_epoch_s)
    }

    /// `Event::Visible`: Zellij's word on whether this instance's tab is on
    /// some client's screen. See the `hidden` field for what a hidden rail
    /// skips. A reveal repaints once, force-rendered: the rows-diff gate
    /// would otherwise veto it (the key may equal what was drawn before the
    /// tab went dark, while the frames in between were never painted).
    pub(crate) fn visibility_changed(&mut self, visible: bool) -> Outcome {
        let was_hidden = std::mem::replace(&mut self.hidden, !visible);
        if !(was_hidden && visible) {
            return Outcome::none(); // going dark, or already in that state
        }
        let change = RadarChange { render: true, force_render: true, ..RadarChange::default() };
        self.project(vec![], change, self.last_now_epoch_s)
    }

    /// `GlobalAttentionUpdate`: every other session on this Zellij server,
    /// already converted to the badge's `SessionSummary` shape.
    pub(crate) fn global_attention_changed(&mut self, peers: Vec<SessionSummary>) -> Outcome {
        self.sessions.set_show_trees(self.config.session_tree);
        let change = RadarChange { render: self.sessions.update_global(peers), ..RadarChange::default() };
        self.project(vec![], change, self.last_now_epoch_s)
    }

    /// The own badge row's counts, derived from the same rows the rail
    /// renders (`radar.rows`) — never a separately-tracked count that could
    /// drift from what's on screen. `attention_tab_position` is the first
    /// (lowest-position) row that `needs_you()`, matching the position
    /// `Sessions`/`BadgeEntry` expect for a same-repo jump-on-arrival. Counts
    /// only (empty `tabs`): the current session's own tree is never rendered.
    fn own_counts(&self) -> SessionSummary {
        let (running, attention, attention_tab_position) = summary_counts(&self.radar.rows(self.tick));
        SessionSummary {
            session_name: self.own_session_name.clone(),
            running,
            attention,
            attention_tab_position,
            ..SessionSummary::default()
        }
    }

    pub(crate) fn permission_result(&mut self, granted: bool) -> Outcome {
        self.record_permission_result(granted);
        let mut effects = Vec::new();
        // The prompt just resolved, so the rail may go passive — IF a
        // terminal neighbor is known (`desired_selectable`); a rail-only tab
        // keeps it reachable.
        self.sync_selectable(&mut effects);
        Outcome::with_effects(true, effects)
    }

    pub(crate) fn cwd_changed(&mut self, pane_id: u32, path: String) -> Outcome {
        let change = self.radar.cwd_changed(pane_id, path, self.config.naming);
        self.project(vec![], change, crate::clock::now_epoch_s())
    }

    /// A `ResolveRepo` read came back: `url` is the remote URL on success.
    pub(crate) fn repo_resolved(&mut self, cwd: String, url: Option<&str>) -> Outcome {
        let name = url.and_then(crate::tab_namer::repo_from_remote);
        let change = self.radar.repo_resolved(cwd, name, self.config.naming);
        self.project(vec![], change, crate::clock::now_epoch_s())
    }

    /// `PaneAttentionChanged` — see `RadarState::attention_changed`.
    pub(crate) fn attention_changed(&mut self, pane_id: u32, attention: crate::attention::PaneAttention) -> Outcome {
        let render = self.radar.attention_changed(pane_id, attention, self.tick);
        self.project(vec![], RadarChange { render, ..RadarChange::default() }, crate::clock::now_epoch_s())
    }

    pub(crate) fn config_pipe(&mut self, raw: &str) -> Outcome {
        let Some(kv) = crate::config::overrides_from_json(raw) else {
            return Outcome::none();
        };
        self.config.apply_overrides(&kv);
        // Display-only now (peers always publish trees): just re-derive the
        // badge; `force_render` below repaints.
        self.sessions.set_show_trees(self.config.session_tree);
        let renames = self.radar.recompute_renames(self.config.naming);
        let change = RadarChange {
            render: true,
            renames,
            // A config override (density, glyphs, header…) redraws identical
            // rows differently — bypass the rows-diff render gate.
            force_render: true,
            ..RadarChange::default()
        };
        self.project(vec![], change, crate::clock::now_epoch_s())
    }

    /// The ledger the rail draws: empty when `ledger false`. The ring is still
    /// recorded, so flipping it back on shows history.
    fn shown_ledger(&self) -> std::rc::Rc<Vec<crate::rollup::LedgerLine>> {
        if self.config.ledger {
            self.radar.ledger_lines()
        } else {
            Default::default()
        }
    }

    /// The content-derived inputs a `render()` at this instant would draw
    /// from — everything except geometry and the wall-clock animation frame.
    /// `rows()` and `ledger_lines()` are memoized on the radar generation, so
    /// consulting this per event costs a compare, not a rollup.
    fn current_render_key(&self) -> RenderKey {
        (self.radar.rows(self.tick), self.shown_ledger(), self.sessions.badge(), self.theme.clone(), self.shown_mode())
    }

    /// The `RenderOpts` a paint at `width`×`height` would use right now — the
    /// single derivation shared by [`render`](Self::render) and the test-only
    /// [`natural_height`](Self::natural_height), so the two can never disagree
    /// on which config/theme/clock inputs a frame is built from.
    fn render_opts(&self, width: usize, height: usize) -> render::RenderOpts {
        render::RenderOpts {
            width: width.max(1),
            height,
            now_tick: self.tick,
            now_frame: self.frame,
            glyphs: self.config.glyphs,
            header: self.config.header,
            density: self.config.density,
            theme: self.theme.clone(),
            now_epoch_s: crate::clock::now_epoch_s(),
            jump_hint: self.config.jump_hint.shows(),
            badge: self.sessions.badge(),
            task_lines: self.config.task_lines,
            session_tree: self.config.session_tree,
            mode: self.shown_mode(),
        }
    }

    pub(crate) fn render(&mut self, rows: usize, cols: usize) -> String {
        self.last_render_height = rows;
        let tabrows = self.build_rows();
        let ledger = self.shown_ledger();
        let opts = self.render_opts(cols, rows);
        // Stamp the render gate's baseline from the very values this pass
        // draws — the key IS what's on screen, by construction (`project`
        // compares `current_render_key` against it).
        self.last_render_key =
            Some((tabrows.clone(), ledger.clone(), opts.badge.clone(), self.theme.clone(), opts.mode));
        let rail = if !self.permission.granted() {
            render::needs_permission(&opts)
        } else if tabrows.is_empty() && ledger.is_empty() && !opts.shows_peer_tree() {
            render::onboarding(&opts)
        } else {
            render::render_rail(&tabrows, &ledger, &opts)
        };
        let ansi = rail.ansi.clone();
        self.last_rendered = rail;
        ansi
    }

    /// Test-only: this session's natural content height at `cols` — enough to
    /// show every row with no overflow folding and no bottom-region padding
    /// (spec §9). Click-mapping tests want a "big enough, no overflow" height
    /// without hard-coding one; passing a merely-large sentinel (the old
    /// `usize::MAX / 2` convention) now lands in the bottom region's
    /// unbounded-filler branch, so this asks `render::body_line_count` for the
    /// real number instead.
    #[cfg(test)]
    pub(crate) fn natural_height(&self, cols: usize) -> usize {
        let tabrows = self.build_rows();
        let opts = self.render_opts(cols, usize::MAX / 2);
        render::body_line_count(&tabrows, &self.shown_ledger(), &opts)
    }

    /// Test-only: force the in-flight `Requesting` state without a `load`.
    #[cfg(test)]
    pub(crate) fn record_permission_request_started(&mut self) {
        self.permission = PermissionState::Requesting;
    }

    pub(crate) fn record_permission_result(&mut self, granted: bool) {
        self.permission = PermissionState::Resolved { granted };
    }

    #[cfg(test)]
    pub(crate) fn target_at_line(&self, line: isize) -> Option<(usize, Option<u32>)> {
        let t = self.last_rendered.target_at_line(line)?;
        Some((t.tab_position, t.pane_id))
    }

    #[cfg(test)]
    pub(crate) fn tab_position_at_line(&self, line: isize) -> Option<usize> {
        self.target_at_line(line).map(|(pos, _)| pos)
    }

    /// Spec §10 cadence function. Fast (1s) while anything tick-windowed is
    /// live; Slow (60s) while ledger ages are still changing; None — the
    /// battery property's full-disarm state — otherwise. A *denied* rail
    /// disarms unconditionally: without
    /// `ReadApplicationState` none of the events that clear domain work ever
    /// arrive, so a stale `Running` would otherwise
    /// pin Fast ticks and repaints forever behind a static needs-permission
    /// face.
    /// `now_epoch_s` is the event's single clock capture, so one event never
    /// sees two different "now"s.
    fn desired_cadence(&self, now_epoch_s: u64) -> Option<Cadence> {
        if self.permission.denied() {
            return None;
        }
        if self.permission.is_requesting()
            || self.timer_should_continue()
            // A pending cross-session cycle selection needs the idle-commit
            // in `timer` to fire promptly, not wait out a Slow (or fully
            // disarmed) chain.
            || self.sessions.wants_fast_cadence()
        {
            Some(self.fast_cadence())
        } else if self.radar.ledger_any_unsaturated(now_epoch_s) || self.radar.pending_wait_unsaturated(now_epoch_s) {
            // Slow ticks advance minute-granular ages (ledger rows' relative
            // ages, pending rows' `· Nm` wait tags — both freeze at 1h+
            // saturation, which is what lets the timer disarm fully).
            Some(Cadence::Slow)
        } else {
            None
        }
    }

    /// The fast cadence to arm: `Frame(spinner_fps)` when sub-second spinner
    /// frames would actually show — a rate above 1, this rail on screen (a
    /// hidden rail paints nothing, so it stays on the plain 1 Hz tick its
    /// grace clocks need), granted (the needs-permission face has no
    /// spinner), and something spinning at full speed — else plain `Fast`.
    fn fast_cadence(&self) -> Cadence {
        let fps = self.config.spinner_fps;
        if fps > 1 && !self.hidden && self.permission.granted() && self.radar.has_full_speed_spinner(self.tick) {
            Cadence::Frame(fps)
        } else {
            Cadence::Fast
        }
    }

    /// Arm (or re-arm) the one-shot timer at `desired_cadence()` — a thin
    /// bridge from [`TimerChain::arm`]'s decision to the effect vec, so the
    /// "arm returned ⇒ SetTimeout emitted" pairing has exactly one home.
    fn arm_timer_if_needed(&mut self, now_epoch_s: u64, effects: &mut Vec<Effect>) {
        if let Some(cadence) = self.timer_chain.arm(self.desired_cadence(now_epoch_s)) {
            effects.push(Effect::SetTimeout(cadence));
        }
    }

    /// Whether the one-shot timer should (re-)arm for *domain* reasons — the
    /// "tick only while there's something to do" rule that keeps an idle rail from
    /// waking every second. Two triggers:
    ///
    /// - **animating work** — a `Running` agent/command whose glyph spins each
    ///   tick (`RadarState::needs_fast_ticks`);
    /// - **an active ping flash** (`RadarState::has_active_flash`) — the
    ///   flip-to-pending glance-catcher is a two-tick visual, not a card fact,
    ///   so nothing else here would otherwise keep the timer awake for it.
    fn timer_should_continue(&self) -> bool {
        self.radar.needs_fast_ticks() || self.radar.has_active_flash(self.tick)
    }

    fn effects_from_renames(&self, renames: Vec<TabRename>) -> Vec<Effect> {
        renames.into_iter().map(|TabRename { id, name }| Effect::RenameTab { tab_id: id, name }).collect()
    }

    /// The sole projection from a domain [`RadarChange`] to host [`Effect`]s.
    /// `fx` is a caller-supplied seed so the `timer` handler's session-switch
    /// effects come first, without a post-hoc splice. Canonical order: renames
    /// → cwd → `SetTimeout` (pinned by
    /// `project_emits_effects_in_canonical_order`). This is the only
    /// *domain-change* path that arms the timer
    /// (`load` arms it separately).
    /// `TimerChain::arm` self-guards on the armed cadence and on whether
    /// there's anything to arm for, so calling it unconditionally here is a
    /// no-op wherever a handler has no pending work to arm for.
    ///
    /// Every domain-change entry point funnels through here with its
    /// `now_epoch_s`, which this stores (`last_now_epoch_s`) for the call
    /// paths that have no epoch of their own — `learn_session_name`,
    /// `global_attention_changed`.
    ///
    /// Also feeds `Sessions::set_own` — the single path for own counts into
    /// the OWN badge row (nothing else calls it), once the name is known.
    /// `Sessions::set_own` does its own badge-derived content-compare and
    /// reports whether the badge actually changed, so calling it every pass
    /// the radar generation moved is what keeps the own row live as
    /// running/attention move.
    fn project(&mut self, mut fx: Vec<Effect>, c: RadarChange, now_epoch_s: u64) -> Outcome {
        self.last_now_epoch_s = now_epoch_s;
        fx.extend(self.effects_from_renames(c.renames));
        let mut render = c.render;
        // The own badge counts are a pure function of radar state (plus the
        // session name, handled in `learn_session_name`), so re-derive them
        // only when the radar generation moved — the derive is a full
        // `rows()` pass, which every broadcast in an 8-tab session used to
        // pay twice.
        if !self.own_session_name.is_empty() && self.summary_gen != Some(self.radar.generation()) {
            self.summary_gen = Some(self.radar.generation());
            render |= self.sessions.set_own(self.own_counts());
        }
        // Rows-diff render gate: Zellij delivers every
        // `PaneAttentionChanged` to every tab's instance, and most deliveries
        // change nothing this instance would DRAW differently. When the
        // content-derived render inputs match what the last `render()`
        // actually drew (`last_render_key`), drop the repaint request.
        // `force_render` bypasses — tick-driven frames and config overrides
        // change the drawing without changing the key's content. Gate only
        // ever downgrades a requested render; it never invents one.
        if render && !c.force_render {
            if let Some(last) = &self.last_render_key {
                if *last == self.current_render_key() {
                    render = false;
                }
            }
        }
        // Visibility gate, after the rows-diff gate and above `force_render`:
        // a hidden tab's rail paints nothing, ticks included — the paint is
        // the bulk of a tick's interpreter cost and no one can see it.
        // `visibility_changed(true)` repaints on reveal.
        if self.hidden {
            render = false;
        }
        if !c.cwd_bootstrap.is_empty() {
            fx.push(Effect::ResolveCwd { pane_ids: c.cwd_bootstrap });
        }
        fx.extend(self.radar.take_repo_lookups().into_iter().map(|cwd| Effect::ResolveRepo { cwd }));
        self.arm_timer_if_needed(now_epoch_s, &mut fx);
        Outcome::with_effects(render, fx)
    }
}

#[cfg(test)]
mod tests;
