//! Session radar state: live tabs/panes plus source-specific observations.
//! No zellij-tile dependency.

use crate::attention::NativePane;
use crate::config;
use crate::ledger::{Ledger, LedgerEntry, LedgerOutcome};
use crate::observation::TrackedObservation;
use crate::payload;
use crate::rollup::{self, LedgerLine, TabDisplay, TabRow, TerminalPane};
use crate::status::Status;
use crate::tab_namer::{PaneFacts, TabFacts, TabNamer, TabRename};
use crate::theme;
use std::collections::{HashMap, HashSet};

/// Direction for attention-tab cycling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    Next,
    Prev,
}

/// Pick the next/previous tab position that needs attention, relative to the
/// active tab, wrapping at the ends. Pure over `(position, status)` pairs so it
/// is trivially testable and deterministic — every per-tab plugin instance that
/// receives the same broadcast computes the identical target (idempotent switch).
///
/// Returns `None` when no tab needs attention, or when the only attention tab is
/// already active (a no-op).
fn cycle_attention(tabs: &[(usize, Status)], active: Option<usize>, dir: Direction) -> Option<usize> {
    let mut members: Vec<usize> = tabs.iter().filter(|(_, s)| s.needs_attention()).map(|(p, _)| *p).collect();
    members.sort_unstable();
    members.dedup();
    if members.is_empty() {
        return None;
    }
    let target = match (dir, active) {
        (Direction::Next, Some(a)) => members.iter().copied().find(|&p| p > a).or_else(|| members.first().copied()),
        (Direction::Next, None) => members.first().copied(),
        (Direction::Prev, Some(a)) => {
            members.iter().rev().copied().find(|&p| p < a).or_else(|| members.last().copied())
        }
        (Direction::Prev, None) => members.last().copied(),
    };
    match target {
        Some(t) if Some(t) != active => Some(t),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct TabId(usize);

impl TabId {
    pub(crate) fn new(raw: usize) -> Self {
        Self(raw)
    }

    /// The host's raw id, for id-addressed host calls (`rename_tab_with_id`).
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub(crate) fn raw(self) -> usize {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RadarTab {
    pub id: TabId,
    pub position: usize,
    pub name: String,
    pub active: bool,
    pub has_bell: bool,
    /// `TabInfo::are_floating_panes_visible` — whether a focused floating
    /// pane holds the user's focus (`rollup::effective_focus`).
    pub floating_visible: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct PaneUpdate {
    pub tab_panes: HashMap<usize, Vec<TerminalPane>>,
    pub live: HashSet<u32>,
    pub theme: Option<theme::DerivedColors>,
    /// Native attention per terminal pane (Zellij ≥ 0.46); a pane missing here
    /// has nothing to show.
    pub native: HashMap<u32, NativePane>,
}

/// One terminal pane as the Zellij host reports it — the few `PaneInfo` fields
/// the radar consumes, copied into plain owned data. The wasm adapter (`lib.rs`)
/// does nothing but this field copy; every bit of *policy* lives in
/// [`PaneUpdate::from_raw`], so it stays host-testable.
pub(crate) struct RawPane {
    pub tab_pos: usize,
    pub id: u32,
    pub title: String,
    pub is_plugin: bool,
    pub is_focused: bool,
    pub is_floating: bool,
    pub default_bg: Option<String>,
    pub default_fg: Option<String>,
    pub attention: Option<NativePane>,
}

impl PaneUpdate {
    /// Fold the host's panes into a `PaneUpdate`: drop plugin panes (the rail
    /// itself), sanitize titles, collect live ids and exits, and derive the
    /// terminal theme from the focused pane's reported bg/fg — falling back to
    /// the first terminal pane that reports both. Pure over `RawPane`, so the
    /// color precedence and plugin-skip are unit-testable without a live Zellij.
    pub(crate) fn from_raw(panes: Vec<RawPane>) -> PaneUpdate {
        let mut tab_panes: HashMap<usize, Vec<TerminalPane>> = HashMap::new();
        let mut live: HashSet<u32> = HashSet::new();
        let mut native: HashMap<u32, NativePane> = HashMap::new();
        let mut focused_colors: Option<(theme::Rgb, theme::Rgb)> = None;
        let mut any_colors: Option<(theme::Rgb, theme::Rgb)> = None;
        for p in panes {
            if p.is_plugin {
                continue;
            }
            let colors = match (
                p.default_bg.as_deref().and_then(theme::parse_hex),
                p.default_fg.as_deref().and_then(theme::parse_hex),
            ) {
                (Some(bg), Some(fg)) => Some((bg, fg)),
                _ => None,
            };
            if let Some(c) = colors {
                any_colors.get_or_insert(c);
                // Zellij reports one focused pane PER TAB, so this is
                // "last focused pane with colors wins" across tabs, following
                // iteration order — an accepted simplification: terminal
                // colors are uniform across panes in practice, so which
                // focused pane wins doesn't change the derived theme.
                if p.is_focused {
                    focused_colors = Some(c);
                }
            }
            let mut title = p.title;
            payload::sanitize_in_place(&mut title, payload::MAX_TAB_NAME_CHARS);
            tab_panes.entry(p.tab_pos).or_default().push(TerminalPane {
                id: p.id,
                title,
                focused_in_tab: p.is_focused,
                floating: p.is_floating,
            });
            live.insert(p.id);
            if let Some(a) = p.attention {
                native.insert(p.id, a);
            }
        }
        let theme = focused_colors.or(any_colors).map(|(bg, fg)| theme::DerivedColors::from_bg_fg(bg, fg));
        PaneUpdate { tab_panes, live, theme, native }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RadarChange {
    pub render: bool,
    pub renames: Vec<TabRename>,
    /// Terminal panes whose working directory should be read once (via a
    /// blocking `get_pane_cwd` host call in the wasm glue) to bootstrap a name
    /// for a freshly-opened tab that has not emitted a `CwdChanged` yet.
    pub cwd_bootstrap: Vec<u32>,
    /// Bypass the runtime's rows-diff render gate: this render request changes
    /// the *drawing* without necessarily changing the rows. Two setters — the
    /// timer (spinner frames and ages come from `now_tick`/`now_epoch_s` at
    /// render time, not from the rows) and a config-pipe override (density/
    /// glyph changes redraw identical rows differently).
    pub force_render: bool,
}

/// Upper bound on the number of one-shot `get_pane_cwd` reads requested per
/// `PaneUpdate`. Each read is a blocking host round-trip, so we cap a single
/// update's fan-out (e.g. a session restore surfacing many panes at once),
/// resolving focused panes first. The overflow is picked up on the next
/// `PaneUpdate` that occurs — so in the rare case of a large burst followed by
/// total inactivity, the tail stays unnamed until the next interaction. This is
/// the postmortem's "cap concurrent in-flight host calls" rule; the cap is
/// generous enough that opening tabs one at a time never hits it.
const MAX_CWD_BOOTSTRAP_PER_UPDATE: usize = 8;

/// How many ticks a tab's ping flash stays lit after a live not-Pending →
/// Pending flip (`native_changed` arms `flash_until` at `tick + FLASH_TICKS`;
/// expiry is exclusive — `rows` reads `now_tick < expiry`). Two ticks ≈ two
/// seconds at the Fast cadence: long enough to catch the eye, short enough
/// that the ping never competes with the steady attention tint that follows.
const FLASH_TICKS: u64 = 2;

#[derive(Default)]
pub(crate) struct RadarState {
    /// The latest manifest's native attention, for each pane it has something
    /// to show for.
    native: HashMap<u32, TrackedObservation>,
    /// The raw native attention `native` derives from: the latest manifest,
    /// with `PaneAttentionChanged` patching a pane's attention in between.
    native_raw: HashMap<u32, NativePane>,
    /// Native completions whose pane no known tab holds yet — a hidden rail
    /// hears `PaneAttentionChanged` before any manifest. Filed once a
    /// manifest and the tabs seat the pane (`file_unfiled`).
    unfiled: Vec<(u32, Status, String, u64)>,
    tabs: Vec<RadarTab>,
    tab_panes: HashMap<usize, Vec<TerminalPane>>,
    pane_cwd: HashMap<u32, String>,
    /// Project name from each cwd's git `origin` remote, read once per cwd
    /// (`None` while the read is in flight, or when there is no remote).
    repo_names: HashMap<String, Option<String>>,
    /// Cwds whose remote should be read (`Effect::ResolveRepo`), drained by
    /// the runtime after every event (`take_repo_lookups`).
    repo_lookups: Vec<String>,
    namer: TabNamer,
    /// The stable identity of the one tab this sidebar instance may rename.
    /// Resolved once by correlating this plugin pane's manifest position with
    /// `TabUpdate`; keeping the id (not the position) makes later tab reorders
    /// unable to redirect a stale instance's rename effects at a neighbour.
    naming_tab_id: Option<TabId>,
    /// Manifest position held until the corresponding tab facts arrive. A new
    /// plugin can receive `PaneUpdate` before its first `TabUpdate`.
    naming_tab_position: Option<usize>,
    /// Pane ids we have already requested an initial `get_pane_cwd` read for.
    /// Tracks *attempts*, not successes, so a pane that has no cwd yet is never
    /// re-polled; pruned with `pane_cwd` so a recycled id can bootstrap again.
    cwd_bootstrap_attempted: HashSet<u32>,
    /// Completions that have left the rail — every edge that removes a
    /// Done/Error from a card (seen, replaced or gone from the manifest) files
    /// an entry here. Memory only.
    ledger: Ledger,
    /// Tab ids currently mid-flash, mapped to the tick their flash expires at
    /// (exclusive — `rows` reads `now_tick < expiry`). Set ONLY on a live
    /// not-Pending → Pending edge for one of the tab's panes
    /// (`native_changed`), so a re-delivery of an already-Pending pane never
    /// re-flashes. Expired entries are pruned
    /// lazily by `timer` — `rows` stays `&self` so it can be called freely from
    /// render paths without needing `&mut`.
    flash_until: HashMap<TabId, u64>,
    /// Monotonic mutation counter over everything `rows()` reads — bumped by
    /// [`touch`](Self::touch) at every entry point that changed row-visible
    /// state. Two consumers: the [`rows`](Self::rows) memo below, and the
    /// runtime's own-summary gate (counts can only move when this does).
    /// Wall-clock animation (spinner frames, ages) deliberately does NOT bump
    /// it — those read `now_tick` at render time.
    generation: u64,
    /// Memo for [`rows`](Self::rows): the last `(generation, lit flashes)`
    /// it was computed at, with its result. Events that consult rows more than once
    /// per intake (the runtime's summary derivation, the render-gate compare,
    /// and the render itself, all under a wasm *interpreter* on the host side)
    /// pay the rollup once — and the `Rc` makes each hit a refcount bump, not
    /// a deep clone of every row. `RefCell` keeps `rows(&self)` callable from
    /// render paths; the plugin is single-threaded by construction (one wasm
    /// instance per tab).
    rows_memo: std::cell::RefCell<Option<RowsMemo>>,
    /// Memo for [`ledger_lines`](Self::ledger_lines), keyed on `generation`
    /// alone: its inputs are the ledger (every push bumps the generation —
    /// see `ledger_native`) and `tabs` (for the
    /// live tab-position lookup). The render gate consults it on every
    /// render-requesting event in every instance.
    ledger_memo: std::cell::RefCell<Option<(u64, std::rc::Rc<Vec<LedgerLine>>)>>,
}

/// The [`RadarState::rows_memo`] entry: the generation and the count of
/// mid-flash tabs (`RadarState::lit_flashes`, the only thing the tick
/// changes about a row) the rows were computed at, with the shared result.
struct RowsMemo {
    generation: u64,
    lit_flashes: usize,
    rows: std::rc::Rc<Vec<TabRow>>,
}

impl RadarState {
    /// Record that row-visible state changed — invalidates the `rows` memo and
    /// advances [`generation`](Self::generation). Every `&mut self` entry point
    /// that mutates something `rows()` reads must call this (the memo makes a
    /// missed call a *stale rail* bug, not just a slow one — keep the call
    /// sites audited against `rows`'s inputs: `tabs`, `tab_panes`,
    /// `flash_until`, and `native` — plus the ledger, for the
    /// [`ledger_lines`](Self::ledger_lines) memo).
    fn touch(&mut self) {
        self.generation += 1;
    }

    /// The current mutation generation — see the field doc. The runtime uses
    /// it to skip recomputing derived-from-rows values (own summary) on
    /// events that provably changed nothing.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// Target tab position for an `attention-next`/`attention-prev` command, or
    /// `None` for a no-op. Reads the live active tab and per-tab rollup; the
    /// pure `cycle_attention` owns the ordering/wrap logic.
    pub(crate) fn next_attention_tab(&self, dir: Direction) -> Option<usize> {
        let active = self.tabs.iter().find(|t| t.active).map(|t| t.position);
        // Order is irrelevant here: `cycle_attention` sorts the attention
        // members itself, so we gather `(position, status)` pairs as-is.
        let pairs: Vec<(usize, Status)> =
            self.tabs.iter().map(|t| (t.position, self.tab_display_for(t.position).status)).collect();
        cycle_attention(&pairs, active, dir)
    }

    /// Rows in position order — `tabs_changed` (the only writer of `self.tabs`)
    /// sorts at intake, so this render-path call is a plain iteration.
    /// Memoized on `(generation, lit flashes)` and shared via `Rc`: one intake
    /// event consults rows several times (summary derivation, the render-gate
    /// diff, the render itself), and under Zellij's interpreted-wasm runtime
    /// both the repeat rollups AND per-hit deep clones were a measurable share
    /// of per-event cost.
    pub(crate) fn rows(&self, now_tick: u64) -> std::rc::Rc<Vec<TabRow>> {
        // `now_tick` reaches the rows through exactly one field — `flash` —
        // so the memo is keyed on how many tabs are mid-flash at this tick,
        // not on the tick itself: a Fast tick with no flash in flight (the
        // steady state of an animating rail) is a memo hit, where keying on
        // the tick made every one of them a full re-rollup.
        let lit_flashes = self.lit_flashes(now_tick);
        if let Some(memo) = self.rows_memo.borrow().as_ref() {
            if memo.generation == self.generation && memo.lit_flashes == lit_flashes {
                return memo.rows.clone();
            }
        }
        let rows = std::rc::Rc::new(self.rows_uncached(now_tick));
        *self.rows_memo.borrow_mut() = Some(RowsMemo { generation: self.generation, lit_flashes, rows: rows.clone() });
        rows
    }

    /// How many tabs' ping flash is lit at `now_tick` — the only
    /// tick-dependent input to `rows()`, and so the tick half of its memo
    /// key. A count identifies the SET within one generation: `flash_until`
    /// only changes through `touch()`ing paths (the arm in `native_changed`)
    /// and the timer's retain of already-unlit entries, so at a fixed
    /// generation the lit set at tick t is `{id : t < until[id]}`, which
    /// only ever shrinks as t grows — two ticks with the same count have
    /// the same set.
    fn lit_flashes(&self, now_tick: u64) -> usize {
        self.flash_until.values().filter(|&&until| now_tick < until).count()
    }

    fn rows_uncached(&self, now_tick: u64) -> Vec<TabRow> {
        self.tabs
            .iter()
            .map(|t| TabRow {
                number: t.position as u32 + 1,
                name: t.name.clone(),
                active: t.active,
                has_bell: t.has_bell,
                flash: self.flash_until.get(&t.id).is_some_and(|&u| now_tick < u),
                display: self.tab_display_for(t.position),
            })
            .collect()
    }

    /// True while any tab is still mid-flash at `now_tick` — ORed into
    /// `PluginRuntime::timer_should_continue` so the ping's full window renders
    /// even though nothing else may be animating (see `flash_until`'s doc).
    pub(crate) fn has_active_flash(&self, now_tick: u64) -> bool {
        self.flash_until.values().any(|&u| now_tick < u)
    }

    pub(crate) fn tabs_changed(&mut self, mut tabs: Vec<RadarTab>) -> RadarChange {
        // Tab names are externally writable (`zellij rename-tab`, layouts) and
        // reach the render grid verbatim, so they get the same intake sanitize
        // as pane titles: a raw ESC would inject ANSI into the rail, and a
        // newline would desync the line↔click-target lockstep.
        for t in &mut tabs {
            payload::sanitize_in_place(&mut t.name, payload::MAX_TAB_NAME_CHARS);
        }
        // Sort before the no-op compare below — `self.tabs` is stored sorted
        // (the per-render `rows()` path iterates in stored order instead of
        // paying a clone+sort on every repaint and tick), so the compare must
        // be order-normalized too.
        tabs.sort_unstable_by_key(|t| t.position);
        // Zellij fires `TabUpdate` far more often than tabs actually change
        // (focus moves, pane-group churn). An identical set is a strict no-op:
        // nothing rows-visible moved, so don't invalidate the memo or repaint.
        if tabs == self.tabs {
            return RadarChange::default();
        }
        self.tabs = tabs;
        self.touch();
        self.resolve_naming_tab();
        // Drop naming state for tabs that closed (the update carries the full
        // current set), so `applied` doesn't accrete gone tabs.
        let live: HashSet<TabId> = self.tabs.iter().map(|t| t.id).collect();
        self.namer.retain_tabs(&live);
        self.file_unfiled();
        RadarChange { render: true, ..RadarChange::default() }
    }

    /// Record which tab contains this plugin pane. Ownership resolves once and
    /// then rests on Zellij's stable `TabId`: every tab has one sidebar, so no
    /// instance ever needs write authority over a peer's name.
    pub(crate) fn own_plugin_tab_changed(&mut self, position: Option<usize>) {
        if self.naming_tab_id.is_some() {
            return;
        }
        self.naming_tab_position = position;
        self.resolve_naming_tab();
    }

    fn resolve_naming_tab(&mut self) {
        if self.naming_tab_id.is_some() {
            return;
        }
        let Some(position) = self.naming_tab_position else {
            return;
        };
        self.naming_tab_id = self.tabs.iter().find(|tab| tab.position == position).map(|tab| tab.id);
    }

    pub(crate) fn has_naming_tab(&self) -> bool {
        self.naming_tab_id.is_some()
    }

    pub(crate) fn panes_changed(&mut self, update: PaneUpdate, tick: u64, naming: config::NamingMode) -> RadarChange {
        // A manifest always invalidates the rows memo. (The runtime's rows-diff
        // render gate still suppresses the *repaint* when the re-derived rows
        // come out identical.)
        self.touch();
        // Captured BEFORE `self.tab_panes` is overwritten below: a pane gone
        // from the manifest ledgers under the tab it was last seen in.
        let old_index = self.pane_tab_index();
        self.tab_panes = update.tab_panes;
        // A live pane ledgers under its current tab, one gone from the
        // manifest under the tab it was last seen in. (A hidden rail's first
        // manifest is the one its reveal brings, so `old_index` can be empty.)
        self.native_raw = update.native;
        let mut index = self.pane_tab_index();
        for (id, tab) in &old_index {
            index.entry(*id).or_insert_with(|| tab.clone());
        }
        let mut ids: Vec<u32> = self.native.keys().chain(self.native_raw.keys()).copied().collect();
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            self.native_changed(id, tick, &index);
        }
        self.file_unfiled();
        self.pane_cwd.retain(|id, _| update.live.contains(id));
        self.cwd_bootstrap_attempted.retain(|id| update.live.contains(id));

        RadarChange {
            render: true,
            renames: self.rename_tabs(naming),
            cwd_bootstrap: self.cwd_bootstrap_targets(naming),
            ..RadarChange::default()
        }
    }

    /// Timer tick: expires the ping flashes.
    pub(crate) fn timer(&mut self, tick: u64) {
        // Lazy expiry: `rows`/`has_active_flash` stay `&self` (read paths), so
        // the map itself is only ever pruned here, on the one `&mut self` tick
        // that already runs regardless of flash state.
        // No `touch()` needed: retain drops only entries with `u <= tick`, and
        // `rows` reads `now_tick < u` — with `tick` monotonic, every dropped
        // entry was already invisible at this tick and stays so at every later
        // one, so the removal can never make a memoized rows() result stale.
        self.flash_until.retain(|_, &mut u| tick < u);
    }

    /// `PaneAttentionChanged`: one pane's attention moved. Unlike the
    /// manifest it reaches hidden tabs' instances too, so the ledger and
    /// counts there stay current. Returns whether rows changed.
    pub(crate) fn attention_changed(
        &mut self,
        pane_id: u32,
        attention: crate::attention::PaneAttention,
        tick: u64,
    ) -> bool {
        self.native_raw.entry(pane_id).or_default().attention = attention;
        let index = self.pane_tab_index();
        let changed = self.native_changed(pane_id, tick, &index);
        if changed {
            self.touch();
        }
        changed
    }

    /// Re-derive one pane's native observation from `native_raw` and act on
    /// the edge: a Done/Error that changes status or disappears leaves the
    /// card for the ledger (a seen Done reads Idle), and a flip to Pending
    /// flashes the tab. Returns whether the observation changed.
    fn native_changed(&mut self, pane_id: u32, tick: u64, index: &HashMap<u32, (TabId, String)>) -> bool {
        let prev = self.native.remove(&pane_id);
        let obs = self.native_raw.get(&pane_id).and_then(|p| p.observation(prev.as_ref(), tick));
        let status = obs.as_ref().map(|o| o.status);
        let changed = prev != obs;
        if status == Some(Status::Pending) && prev.as_ref().is_none_or(|p| p.status != Status::Pending) {
            self.arm_flash(pane_id, tick);
        }
        if let Some(obs) = obs {
            self.native.insert(pane_id, obs);
        }
        if let Some(prev) = prev.filter(|p| p.status.is_completion() && Some(p.status) != status) {
            self.ledger_native(pane_id, prev.status, &prev.msg, prev.completed_epoch_s, index);
        }
        let remembered = self.native_raw.get(&pane_id).and_then(|p| p.attention.remembered.clone());
        if let Some(r) = remembered.filter(|r| r.status.is_completion() && Some(r.status) != status) {
            self.ledger_native(pane_id, r.status, &r.text, Some(r.since_ms / 1000), index);
        }
        changed
    }

    /// Retry the completions no tab could hold yet: on reveal a rail's
    /// `PaneUpdate` can come before its first `TabUpdate`, so either may be
    /// the one that seats the pane.
    fn file_unfiled(&mut self) {
        let index = self.pane_tab_index();
        for (id, status, label, at) in std::mem::take(&mut self.unfiled) {
            self.ledger_native(id, status, &label, Some(at), &index);
        }
    }

    /// File one native completion under the pane's tab. Both edges in
    /// `native_changed` can report the same completion; `Ledger::push`
    /// merges them (same pane, outcome and label).
    fn ledger_native(
        &mut self,
        pane_id: u32,
        status: Status,
        text: &str,
        at_epoch_s: Option<u64>,
        index: &HashMap<u32, (TabId, String)>,
    ) {
        let Some(at_epoch_s) = at_epoch_s else { return };
        // Cap before dropping the head, as the observation's `msg` was: the
        // two edges in `native_changed` must yield the same label to merge.
        let text = crate::payload::sanitize(text, crate::payload::MAX_MSG_CHARS);
        let label = crate::attention::completion_label(&text).to_string();
        let Some((tab_id, tab_name)) = index.get(&pane_id) else {
            // `remembered` re-fires on every event until the pane is seen: queue it once.
            let item = (pane_id, status, label, at_epoch_s);
            if !self.unfiled.contains(&item) {
                // ponytail: a pane no manifest ever seats stays queued (one entry per completion); prune by `live` if it grows
                self.unfiled.push(item);
            }
            return;
        };
        let outcome = if status == Status::Error { LedgerOutcome::Error } else { LedgerOutcome::Done };
        let entry = LedgerEntry { at_epoch_s, outcome, tab_id: *tab_id, tab_name: tab_name.clone(), label, pane_id };
        if self.ledger.push(entry.sanitized()) {
            self.touch();
        }
    }

    pub(crate) fn cwd_changed(&mut self, pane_id: u32, path: String, naming: config::NamingMode) -> RadarChange {
        self.pane_cwd.insert(pane_id, path);
        // A cwd feeds *naming* only — nothing `rows()` reads. The rail repaint
        // (if any) arrives via the `RenameTab` effect's own `TabUpdate` echo,
        // so requesting one here just re-drew an identical rail.
        RadarChange { render: false, renames: self.rename_tabs(naming), ..RadarChange::default() }
    }

    /// True while any pane is actively *working* — a native `Running` record
    /// or command. This is the animated set (the spinner), so it wants a
    /// per-tick repaint. Deliberately narrow: a finished `Done`/`Error` or a
    /// waiting `Pending` is not "animating" work.
    pub(crate) fn needs_fast_ticks(&self) -> bool {
        self.native.values().any(|o| o.animating())
    }

    /// Some observation spins at full speed right now: animating
    /// ([`TrackedObservation::animating`]) and not yet eased to the slow blink
    /// (`render::EASE_AFTER_TICKS`). The one reason to paint sub-second
    /// spinner frames (`spinner_fps`): an eased blink or a steady row moves
    /// at most once per tick, so frames between ticks would redraw an
    /// identical rail.
    pub(crate) fn has_full_speed_spinner(&self, now_tick: u64) -> bool {
        self.native
            .values()
            .any(|o| o.animating() && now_tick.saturating_sub(o.last_change_tick) <= crate::render::EASE_AFTER_TICKS)
    }

    /// A cwd's remote read came back (`name` is `None` without a remote).
    pub(crate) fn repo_resolved(
        &mut self,
        cwd: String,
        name: Option<String>,
        naming: config::NamingMode,
    ) -> RadarChange {
        // The cwd basename may already name the tab and would stick over the
        // remote's name (`TabNamer`'s stickiness): let it re-pick once.
        if let (Some(_), Some(id)) = (&name, self.naming_tab_id) {
            self.namer.repick(id);
        }
        self.repo_names.insert(cwd, name);
        RadarChange { renames: self.rename_tabs(naming), ..RadarChange::default() }
    }

    pub(crate) fn take_repo_lookups(&mut self) -> Vec<String> {
        std::mem::take(&mut self.repo_lookups)
    }

    pub(crate) fn recompute_renames(&mut self, naming: config::NamingMode) -> Vec<TabRename> {
        self.rename_tabs(naming)
    }

    /// The tab currently seating `pane_id`, by the `tabs × tab_panes` position
    /// join — a single-tab lookup with no allocation, for edge paths;
    /// `pane_tab_index` is the same join materialized for whole-topology
    /// captures. `None` when the pane is in no tab (mid break-pane, or gone).
    fn tab_of_pane(&self, pane_id: u32) -> Option<TabId> {
        self.tabs.iter().find_map(|tab| {
            self.tab_panes
                .get(&tab.position)
                .is_some_and(|panes| panes.iter().any(|p| p.id == pane_id))
                .then_some(tab.id)
        })
    }

    /// What the rail shows for `pane_id`: Zellij's native attention, which
    /// already picked OSC 7501 over a shell command. The roll-up
    /// (`tab_display`) reads through it.
    fn resolve(&self, pane_id: u32) -> Option<&TrackedObservation> {
        self.native.get(&pane_id)
    }

    /// Prepared ledger rows for rendering, newest first. Each row's tab
    /// position is a *live* lookup of the stored `tab_id` against `self.tabs`
    /// — `None` once that tab has closed, which the renderer reads as
    /// click-inert (the ledger itself never forgets an entry just because its
    /// tab went away). Memoized on the generation, like [`rows`](Self::rows).
    pub(crate) fn ledger_lines(&self) -> std::rc::Rc<Vec<LedgerLine>> {
        if let Some((generation, lines)) = self.ledger_memo.borrow().as_ref() {
            if *generation == self.generation {
                return lines.clone();
            }
        }
        let lines: std::rc::Rc<Vec<LedgerLine>> = std::rc::Rc::new(
            self.ledger
                .entries()
                .map(|e| LedgerLine {
                    at_epoch_s: e.at_epoch_s,
                    error: e.outcome == LedgerOutcome::Error,
                    tab_name: e.tab_name.clone(),
                    label: e.label.clone(),
                    tab_position: self.tabs.iter().find(|t| t.id == e.tab_id).map(|t| t.position),
                })
                .collect(),
        );
        *self.ledger_memo.borrow_mut() = Some((self.generation, lines.clone()));
        lines
    }

    /// Any ledger entry still younger than the saturate window — drives the
    /// Slow cadence (spec §4.4/§10) so the timer stays armed only while a row's
    /// displayed age can still change. Consumed by `PluginRuntime::desired_cadence`.
    pub(crate) fn ledger_any_unsaturated(&self, now_epoch_s: u64) -> bool {
        self.ledger.any_unsaturated(now_epoch_s)
    }

    /// Any `Pending` row whose `· Nm` wait tag is still counting (age
    /// under the ledger's saturate window)? The pending twin of
    /// [`ledger_any_unsaturated`](Self::ledger_any_unsaturated): both feed the
    /// Slow cadence, and both freeze at `1h+` so the timer can disarm.
    pub(crate) fn pending_wait_unsaturated(&self, now_epoch_s: u64) -> bool {
        self.native.values().any(|o| {
            o.status == Status::Pending
                && o.pending_epoch_s.is_some_and(|t| now_epoch_s.saturating_sub(t) < crate::ledger::SATURATE_S)
        })
    }

    /// Arm the tab-level ping flash for the tab holding `pane_id`, if it is
    /// still seated in one: the live not-Pending → Pending edge, the "make
    /// the user aware" signal.
    fn arm_flash(&mut self, pane_id: u32, tick: u64) {
        if let Some(tab_id) = self.tab_of_pane(pane_id) {
            self.flash_until.insert(tab_id, tick + FLASH_TICKS);
        }
    }

    /// Pane → (tab id, tab name) for every pane currently in `self.tab_panes`,
    /// joined against `self.tabs` for the name. Callers that need the topology
    /// as of *before* a mutation (`panes_changed`'s prune edges) must capture
    /// this BEFORE applying that mutation — the index itself is always just a
    /// snapshot of current `self` state.
    fn pane_tab_index(&self) -> HashMap<u32, (TabId, String)> {
        let mut index = HashMap::new();
        for tab in &self.tabs {
            if let Some(panes) = self.tab_panes.get(&tab.position) {
                for pane in panes {
                    index.insert(pane.id, (tab.id, tab.name.clone()));
                }
            }
        }
        index
    }

    /// Pane ids currently seated in tab `position`, in `self.tab_panes`'s
    /// stored order — `PluginRuntime::mouse_right_click` reads this to
    /// resolve a tab-row acknowledge (no `pane_id` on the click target) into
    /// the set of panes it should check for a dismissible `Pending`. Empty
    /// for an unknown position, same as every other `tab_panes` lookup here.
    pub(crate) fn pane_ids_for_tab(&self, position: usize) -> Vec<u32> {
        self.tab_panes.get(&position).map(|panes| panes.iter().map(|p| p.id).collect()).unwrap_or_default()
    }

    /// The observation the rail shows for `pane_id` — the right-click
    /// acknowledge gesture reads it to find a `Pending` pane to mark seen.
    pub(crate) fn observation(&self, pane_id: u32) -> Option<&TrackedObservation> {
        self.resolve(pane_id)
    }

    #[cfg(test)]
    pub(crate) fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// Test-only: build a ledger entry directly (bypassing the recede path) so
    /// cadence tests can pin an exact `at_epoch_s` without driving a whole
    /// pane lifecycle.
    #[cfg(test)]
    pub(crate) fn ledger_mut(&mut self) -> &mut Ledger {
        &mut self.ledger
    }

    #[cfg(test)]
    pub(crate) fn set_tab_panes_for_position(&mut self, position: usize, panes: Vec<TerminalPane>) {
        self.tab_panes.insert(position, panes);
        self.touch();
    }

    #[cfg(test)]
    pub(crate) fn applied_name(&self, tab_id: TabId) -> Option<&str> {
        self.namer.applied_name(tab_id)
    }

    /// Live terminal panes whose cwd we have neither learned (via `CwdChanged`)
    /// nor yet attempted to read. Focused panes come first — they name their tab
    /// — and the result is capped at `MAX_CWD_BOOTSTRAP_PER_UPDATE` so one update
    /// never fans out an unbounded number of blocking host calls. Every returned
    /// id is recorded as attempted, so it is requested at most once per lifetime.
    ///
    /// Bootstrap exists to name tabs, so it is a no-op (and pays for no
    /// blocking reads) when naming is `Off` — mirroring `rename_tabs`. Unlike
    /// renames it is NOT scoped to the owned tab.
    fn cwd_bootstrap_targets(&mut self, naming_mode: config::NamingMode) -> Vec<u32> {
        if naming_mode == config::NamingMode::Off {
            return Vec::new();
        }
        let mut focused = Vec::new();
        let mut others = Vec::new();
        for panes in self.tab_panes.values() {
            for p in panes {
                if self.pane_cwd.contains_key(&p.id) || self.cwd_bootstrap_attempted.contains(&p.id) {
                    continue;
                }
                if p.focused_in_tab {
                    focused.push(p.id);
                } else {
                    others.push(p.id);
                }
            }
        }
        // Deterministic order regardless of HashMap iteration; focused first.
        focused.sort_unstable();
        others.sort_unstable();
        let targets: Vec<u32> = focused.into_iter().chain(others).take(MAX_CWD_BOOTSTRAP_PER_UPDATE).collect();
        for id in &targets {
            self.cwd_bootstrap_attempted.insert(*id);
        }
        targets
    }

    /// Delegate naming to the [`TabNamer`]: assemble resolved facts, then let the
    /// naming module pick and remember names. `Off` short-circuits before any
    /// fact-building (the namer also no-ops on `Off`).
    fn rename_tabs(&mut self, naming_mode: config::NamingMode) -> Vec<TabRename> {
        if naming_mode == config::NamingMode::Off {
            return Vec::new();
        }
        let Some(naming_tab_id) = self.naming_tab_id else {
            return Vec::new();
        };
        // Only this instance's own tab: each instance names just the tab it
        // lives in, so building facts for every other tab (cloning names,
        // titles, cwds, repos) only to discard them was pure per-event waste
        // multiplied across instances.
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == naming_tab_id) else {
            return Vec::new();
        };
        let facts = [self.tab_facts(tab)];
        // One remote read per new cwd of this instance's own tab, never more.
        for cwd in facts[0].panes.iter().filter_map(|p| p.cwd.as_ref()) {
            if !self.repo_names.contains_key(cwd) {
                self.repo_names.insert(cwd.clone(), None);
                self.repo_lookups.push(cwd.clone());
            }
        }
        self.namer.rename(&facts, naming_mode)
    }

    /// Join one tab's pane topology, status observations, and known cwds into
    /// the resolved [`TabFacts`] the [`TabNamer`] consumes. `repo` is the cwd's
    /// git remote name; the
    /// raw `cwd`/`title` are processed inside the namer.
    fn tab_facts(&self, tab: &RadarTab) -> TabFacts {
        let empty = Vec::new();
        let panes = self.tab_panes.get(&tab.position).unwrap_or(&empty);
        TabFacts {
            id: tab.id,
            name: tab.name.clone(),
            panes: panes
                .iter()
                .map(|p| PaneFacts {
                    repo: self.pane_cwd.get(&p.id).and_then(|cwd| self.repo_names.get(cwd).cloned().flatten()),
                    cwd: self.pane_cwd.get(&p.id).cloned(),
                    title: p.title.clone(),
                    focused: p.focused_in_tab,
                })
                .collect(),
        }
    }

    /// Roll this tab's panes up into a `TabDisplay`; `rollup::roll_up` only
    /// sees "is there an observation for this pane?", keeping the aggregation
    /// rules behind the Tab Roll-Up seam.
    fn tab_display(&self, panes: &[TerminalPane]) -> TabDisplay {
        rollup::roll_up(panes, |id| self.resolve(id))
    }

    /// Roll a tab up by position, treating an absent pane list as no panes.
    /// The per-position lookup is shared by `rows` and `next_attention_tab`.
    fn tab_display_for(&self, position: usize) -> TabDisplay {
        let empty = Vec::new();
        let panes = self.tab_panes.get(&position).unwrap_or(&empty);
        TabDisplay { focused_pane: self.focused_pane_at(position, panes), ..self.tab_display(panes) }
    }

    /// The pane the user is in on the tab at `position` — per-layer focus
    /// resolved against that tab's floating-layer visibility.
    fn focused_pane_at(&self, position: usize, panes: &[TerminalPane]) -> Option<u32> {
        let floating_visible = self.tabs.iter().find(|t| t.position == position).is_some_and(|t| t.floating_visible);
        rollup::effective_focus(panes, floating_visible)
    }
}

#[cfg(test)]
mod tests;
