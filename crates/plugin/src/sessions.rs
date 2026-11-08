//! Cross-session peer state + the Alt+[/] cycle state machine — pure: no
//! zellij-tile, no filesystem. The runtime feeds this module parsed facts
//! (the peers Zellij reports via `GlobalAttentionUpdate`, this session's own
//! counts) and it derives the ordered badge on demand, mirroring the
//! rows-derived-on-render doctrine (`CONTEXT.md`) rather than caching a
//! badge that could drift from `peers`/`own`.
//!
//! Zellij owns peer liveness: a session it reports is live, one it stops
//! reporting is gone, so there is no staleness grading here.
//!
//! Ordering is a single source of truth shared by `badge()` (what's shown)
//! and `cycle()` (what Alt+[/] steps through): current session first, then
//! `attention > 0` sessions by name, then the rest by name.

use crate::radar_state::Direction;

pub(crate) const MAX_TABS: usize = 64;
pub(crate) const MAX_PANES_PER_TAB: usize = 16;
pub(crate) const MAX_LABEL_CHARS: usize = 96;

/// A bounded, display-only view of another session's tab tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(test, derive(serde::Deserialize), serde(default))]
pub(crate) struct SummaryTab {
    pub position: usize,
    pub name: String,
    pub panes: Vec<SummaryPane>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(test, derive(serde::Deserialize), serde(default))]
pub(crate) struct SummaryPane {
    /// Zellij terminal pane id.
    pub pane_id: Option<u32>,
    pub kind: String,
    pub status: String,
    pub label: String,
}

/// What the badge knows about one session (own or peer): its name and counts,
/// plus, for peers, a bounded tab tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(test, derive(serde::Deserialize), serde(default))]
pub(crate) struct SessionSummary {
    pub session_name: String,
    pub running: usize,
    pub attention: usize,
    pub attention_tab_position: Option<usize>,
    pub tabs: Vec<SummaryTab>,
}

/// A row in the shared ordering: a [`SessionSummary`] (own or peer) plus whether
/// it's the current session. `own` contributes at most one of these (via
/// `set_own`); every peer in `peers` contributes one. A `SessionSummary` IS the
/// identity (its `session_name`) as well as the counts.
struct OrderedEntry<'a> {
    summary: &'a SessionSummary,
    is_current: bool,
}

/// One row of the cross-session badge, in display order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BadgeEntry {
    pub name: String,
    pub running: usize,
    pub attention: usize,
    pub attention_tab_position: Option<usize>,
    /// The pane to focus on arrival when no tab position is known
    /// ([`attention_pane`]).
    pub attention_pane_id: Option<u32>,
    pub is_current: bool,
    pub selected: bool,
    pub tabs: Vec<SummaryTab>,
}

/// Where a committed cycle gesture lands — enough for the runtime to switch
/// sessions and then jump straight to the tab that needs attention there.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CommitTarget {
    pub name: String,
    pub attention_tab_position: Option<usize>,
    pub attention_pane_id: Option<u32>,
}

/// The first pane waiting on the user in a peer's tree — the arrival focus
/// for a peer from `GlobalAttentionUpdate`, which knows pane ids but not tab
/// positions. `None` when a tab position is already known.
fn attention_pane(p: &SessionSummary) -> Option<u32> {
    if p.attention_tab_position.is_some() {
        return None;
    }
    let panes = p.tabs.iter().flat_map(|t| &t.panes);
    panes.filter(|pane| crate::status::Status::from_wire(&pane.status).needs_you()).find_map(|pane| pane.pane_id)
}

/// A pending (not yet committed) cycle selection. `name` identifies the
/// selected session directly rather than its position in the derived order —
/// `update_global`/`set_own` can reorder that list between the
/// `cycle()` tap and the commit `tick()` (e.g. a new session sorting ahead of
/// the selected one), and a positional index would then silently retarget
/// whatever session ended up at the old index. Re-resolved by name against
/// the fresh order on every `cycle()`/`tick()` call instead.
///
/// `taps_since_last_fire` replaces an earlier tick-counter comparison
/// (`now_tick > last_tap_tick`) that had two field-observed bugs: (a) a tap
/// landing just before an already-scheduled fire compared as "idle" and
/// committed instantly, and (b) taps arriving faster than the 1s tick grain
/// all shared one tick number, so the deadline never extended past the
/// FIRST tap in a burst. `cycle()` sets this flag on every tap; `tick()`
/// clears it and skips (no commit) whenever it's set, so a fire only ever
/// commits when its entire covered interval was tap-free — guaranteeing at
/// least one full quiet interval after the LAST tap, at the cost of at most
/// one extra (skipped) fire.
struct SelectionState {
    name: String,
    taps_since_last_fire: bool,
}

#[derive(Default)]
pub(crate) struct Sessions {
    peers: Vec<SessionSummary>,
    own: Option<SessionSummary>,
    selection: Option<SelectionState>,
    /// Mirrors the local `session_tree` option. Off, peer trees are dropped
    /// from the badge — and so from `update_global`'s change compare and the
    /// render key — so a peer's label edges never repaint a rail that
    /// doesn't draw them.
    show_trees: bool,
}

impl Sessions {
    /// Replace the peer set with every other session Zellij reports
    /// (`attention::PeerSession::summary`). Returns whether the badge changed.
    pub(crate) fn update_global(&mut self, peers: Vec<SessionSummary>) -> bool {
        let before = self.badge();
        self.peers = peers;
        self.badge() != before
    }

    /// Sync the local `session_tree` option. Returns whether the badge changed.
    pub(crate) fn set_show_trees(&mut self, on: bool) -> bool {
        if self.show_trees == on {
            return false;
        }
        let before = self.badge();
        self.show_trees = on;
        self.badge() != before
    }

    /// Record this session's own counts (never taken from a peer — the
    /// current session knows its own state directly). The single path for own
    /// counts into the badge — the runtime calls this every time it
    /// re-derives them (`own_counts`), not just on a name change, so the own
    /// row stays live as running/attention move. Receives a counts-only
    /// `SessionSummary` (empty `tabs`): the current session's own tree is never
    /// rendered.
    pub(crate) fn set_own(&mut self, p: SessionSummary) -> bool {
        let before = self.badge();
        self.own = Some(p);
        self.badge() != before
    }

    /// Advance (or start) the cycle selection and arm its tap-since-fire
    /// flag (see `SelectionState`'s doc). Re-derives the shared ordering
    /// fresh each call, so a selection started before an
    /// `update_global`/`set_own` change always steps relative to the
    /// current membership, never a stale snapshot. The pending selection's
    /// *name* is re-resolved against that fresh order (not trusted as a
    /// stale index) before advancing from it.
    pub(crate) fn cycle(&mut self, dir: Direction) -> bool {
        let before = self.badge();
        let order = self.ordered();
        // A previously selected name that no longer appears among the
        // entries (its session closed) is treated the same as "nothing
        // selected" — restart below rather than advance from a position it
        // no longer occupies.
        let current_position =
            self.selection.as_ref().and_then(|sel| order.iter().position(|s| s.summary.session_name == sel.name));
        self.selection = match (current_position, order.len()) {
            (_, 0) => None,
            (None, _) => {
                // Nothing selected yet (or the selection vanished): start at
                // the first non-current entry (index 0 is the current
                // session, when own is known).
                order.iter().position(|s| !s.is_current).map(|index| SelectionState {
                    name: order[index].summary.session_name.clone(),
                    taps_since_last_fire: true,
                })
            }
            (Some(index), len) => {
                let next = match dir {
                    Direction::Next => (index + 1) % len,
                    Direction::Prev => (index + len - 1) % len,
                };
                Some(SelectionState { name: order[next].summary.session_name.clone(), taps_since_last_fire: true })
            }
        };
        self.badge() != before
    }

    /// Idle-commit: called on every timer fire while a selection is pending.
    /// A fire whose covered interval saw a tap (`taps_since_last_fire` set —
    /// by `cycle()`, possibly including THIS very fire's own tap) clears the
    /// flag and skips: the deadline resets, and only the NEXT, fully quiet
    /// fire may commit. The selected *name* is re-resolved against a
    /// freshly derived order rather than a remembered index, so a reorder
    /// between the tap and this tick can never retarget a different
    /// session; if the name is no longer live, the selection is dropped and
    /// nothing commits. Landing on the current session is the cancel
    /// gesture (`None`, no effect). Either way a commit (or drop) clears the
    /// pending selection.
    pub(crate) fn tick(&mut self) -> Option<CommitTarget> {
        let sel = self.selection.as_mut()?;
        if sel.taps_since_last_fire {
            sel.taps_since_last_fire = false;
            return None;
        }
        let name = sel.name.clone();
        self.selection = None; // committing, cancelling, or dropping a vanished selection all clear
        let order = self.ordered();
        let entry = order.iter().find(|s| s.summary.session_name == name)?; // vanished session: nothing to commit
        if entry.is_current {
            return None; // cancel gesture
        }
        Some(CommitTarget {
            name: entry.summary.session_name.clone(),
            attention_tab_position: entry.summary.attention_tab_position,
            attention_pane_id: attention_pane(entry.summary),
        })
    }

    /// The ordered badge: current session first, then attention>0 sessions
    /// by name, then the rest by name. Derived fresh from `peers`/`own` every
    /// call — never cached — so it can never drift from its inputs.
    pub(crate) fn badge(&self) -> Vec<BadgeEntry> {
        let order = self.ordered();
        let selected_name = self.selection.as_ref().map(|sel| sel.name.as_str());
        order
            .into_iter()
            .map(|s| BadgeEntry {
                name: s.summary.session_name.clone(),
                running: s.summary.running,
                attention: s.summary.attention,
                attention_tab_position: s.summary.attention_tab_position,
                attention_pane_id: attention_pane(s.summary),
                is_current: s.is_current,
                selected: selected_name == Some(s.summary.session_name.as_str()),
                // The current session's own tree never renders (its tabs are
                // the local cards). Peer trees only when shown, agent panes
                // only — the filter sits here, upstream of the tab glyph and
                // the line budget in `render_peer_children`.
                tabs: if s.is_current || !self.show_trees {
                    Vec::new()
                } else {
                    s.summary
                        .tabs
                        .iter()
                        .map(|tab| SummaryTab {
                            panes: tab
                                .panes
                                .iter()
                                .filter(|p| crate::kind::Kind::from_source(&p.kind).is_agent())
                                .cloned()
                                .collect(),
                            ..tab.clone()
                        })
                        .collect()
                },
            })
            .collect()
    }

    /// Whether a cycle selection is pending — the runtime keeps the Fast
    /// timer cadence armed while true, so the idle-commit in `tick()` fires
    /// promptly rather than waiting for the next Slow tick. Only the wasm
    /// runtime reads this in production; pinned by a host test
    /// (`wants_fast_cadence_tracks_pending_selection_through_commit_and_cancel`).
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub(crate) fn wants_fast_cadence(&self) -> bool {
        self.selection.is_some()
    }

    /// The shared ordering: the current session first (at most one — `own`,
    /// once known), then attention>0 peers by name, then the rest of the
    /// peers by name. Both `badge()` and `cycle()` go through this so
    /// "what's shown" and "what Alt+[/] steps through" can never disagree.
    fn ordered(&self) -> Vec<OrderedEntry<'_>> {
        let own_name = self.own.as_ref().map(|p| p.session_name.as_str());
        // `own`'s counts come from set_own's direct knowledge — it must
        // always win a name collision, so a peer sharing our own session's
        // name is dropped outright rather than shown as a second line.
        let (mut attention, mut rest): (Vec<_>, Vec<_>) = self
            .peers
            .iter()
            .filter(|p| own_name != Some(p.session_name.as_str()))
            .map(|summary| OrderedEntry { summary, is_current: false })
            .partition(|e| e.summary.attention > 0);
        fn by_name(a: &OrderedEntry<'_>, b: &OrderedEntry<'_>) -> std::cmp::Ordering {
            a.summary.session_name.cmp(&b.summary.session_name)
        }
        attention.sort_by(by_name);
        rest.sort_by(by_name);
        let current = self.own.as_ref().map(|p| OrderedEntry { summary: p, is_current: true });
        current.into_iter().chain(attention).chain(rest).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::radar_state::Direction;

    fn peer_with_tree(label: &str) -> SessionSummary {
        let json = serde_json::json!({
            "session_name": "alpha", "running": 1, "attention": 0,
            "tabs": [{"position": 0, "name": "t", "panes": [
                {"pane_id": 3, "kind": "claude", "status": "running", "label": label},
                {"pane_id": 4, "kind": "test", "status": "running", "label": "cargo test"}
            ]}]
        });
        serde_json::from_value(json).unwrap()
    }

    fn global_peer(name: &str, needs_you_pane: Option<u32>) -> SessionSummary {
        let panes = needs_you_pane
            .map(|id| SummaryPane {
                pane_id: Some(id),
                kind: "claude".into(),
                status: "pending".into(),
                ..Default::default()
            })
            .into_iter()
            .collect();
        SessionSummary {
            session_name: name.into(),
            attention: needs_you_pane.map_or(0, |_| 1),
            tabs: vec![SummaryTab { panes, ..Default::default() }],
            ..Default::default()
        }
    }

    #[test]
    fn global_peers_focus_the_waiting_pane() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        assert!(s.update_global(vec![global_peer("alpha", Some(42))]));
        let badge = s.badge();
        let names: Vec<&str> = badge.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, vec!["work", "alpha"]);
        assert_eq!(badge[1].attention_pane_id, Some(42));
        s.cycle(Direction::Next);
        s.tick();
        let t = s.tick().unwrap();
        assert_eq!((t.name.as_str(), t.attention_tab_position, t.attention_pane_id), ("alpha", None, Some(42)));
    }

    #[test]
    fn a_known_tab_position_wins_over_the_attention_pane() {
        let mut p = global_peer("alpha", Some(42));
        p.attention_tab_position = Some(1);
        assert_eq!(attention_pane(&p), None);
        assert_eq!(attention_pane(&global_peer("beta", None)), None);
    }

    #[test]
    fn peer_tree_edges_do_not_change_the_badge_with_trees_off() {
        let mut s = Sessions::default();
        s.update_global(vec![peer_with_tree("one")]);
        assert!(!s.update_global(vec![peer_with_tree("two")]), "a label edge in a tree nobody shows must not repaint");
        assert!(s.badge().iter().all(|e| e.tabs.is_empty()));
    }

    #[test]
    fn shown_peer_trees_list_agent_panes_only() {
        let mut s = Sessions::default();
        s.update_global(vec![peer_with_tree("one")]);
        assert!(s.set_show_trees(true), "turning trees on changes the badge");
        let entry = s.badge().into_iter().find(|e| e.name == "alpha").unwrap();
        assert_eq!(entry.tabs[0].panes.len(), 1);
        assert_eq!(entry.tabs[0].panes[0].kind, "claude");
        assert!(s.update_global(vec![peer_with_tree("two")]), "with trees on, label edges repaint");
    }

    fn own(name: &str) -> SessionSummary {
        SessionSummary {
            session_name: name.into(),
            running: 0,
            attention: 0,
            attention_tab_position: None,
            tabs: vec![],
        }
    }
    fn summary(name: &str, running: usize, attention: usize) -> SessionSummary {
        SessionSummary { session_name: name.into(), running, attention, ..Default::default() }
    }

    #[test]
    fn badge_orders_current_then_attention_then_rest() {
        let mut s = Sessions::default();
        s.update_global(vec![summary("zeta", 1, 2), summary("alpha", 1, 0)]);
        s.set_own(SessionSummary {
            session_name: "work".into(),
            running: 3,
            attention: 0,
            attention_tab_position: None,
            tabs: vec![],
        });
        // (Bound rather than chained straight off `s.badge()`: the literal
        // brief snippet borrows from a temporary `Vec<BadgeEntry>` that would
        // be dropped at the end of the `let` statement — a compile-mechanics
        // issue, not a semantic change; the assertion is unchanged.)
        let badge = s.badge();
        let names: Vec<&str> = badge.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, vec!["work", "zeta", "alpha"]);
    }

    #[test]
    fn cycle_advances_and_wraps_and_tick_commits_after_idle() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        s.update_global(vec![summary("alpha", 0, 1), summary("beta", 1, 0)]);
        // Order: work(current), alpha(attention), beta.
        s.cycle(Direction::Next); // select alpha
        assert_eq!(s.tick(), None, "the fire covering the tap must not commit — it clears the flag");
        s.cycle(Direction::Next); // skip to beta
        assert_eq!(s.tick(), None, "the fire covering THIS tap must not commit either");
        let t = s.tick().expect("a fully quiet fire commits");
        assert_eq!(t.name, "beta");
        assert_eq!(s.tick(), None, "committed selection is cleared");
    }

    #[test]
    fn committing_on_current_session_is_a_noop_cancel() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        s.update_global(vec![summary("alpha", 0, 0)]);
        s.cycle(Direction::Next); // alpha
        s.cycle(Direction::Next); // wraps to work (current)
        s.tick(); // the fire covering that tap: skip, clears the flag
        assert_eq!(s.tick(), None, "a quiet fire landing on the current session cancels");
    }

    #[test]
    fn commit_carries_attention_tab_position() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        s.update_global(vec![SessionSummary { attention_tab_position: Some(2), ..summary("alpha", 0, 1) }]);
        s.cycle(Direction::Next);
        s.tick(); // the fire covering the tap: skip, clears the flag
        assert_eq!(s.tick().unwrap().attention_tab_position, Some(2));
    }

    // -- Pinning: field bugs in the old tick-counter commit check ------------
    // The old `now_tick > last_tap_tick` comparison committed on the very
    // next fire after a tap (a tap landing just before an already-scheduled
    // fire compared as "idle" and committed instantly) and never extended
    // the deadline for taps faster than the 1s tick grain (they all shared
    // one tick number, so the commit landed ~1s after the FIRST tap, not
    // the last). The `taps_since_last_fire` flag fixes both.

    #[test]
    fn tap_then_immediate_fire_does_not_commit() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        s.update_global(vec![summary("alpha", 1, 0)]);
        s.cycle(Direction::Next); // selects alpha
                                  // A fire landing right after the tap must not commit instantly —
                                  // it covers the tap's interval and must reset the deadline instead.
        assert_eq!(s.tick(), None, "a fire landing right after the tap must not commit instantly");
        assert!(s.wants_fast_cadence(), "the selection survives that fire, still pending");
        let t = s.tick().expect("the next, fully quiet fire commits");
        assert_eq!(t.name, "alpha");
    }

    #[test]
    fn rapid_multi_tap_across_several_fires_commits_only_after_first_quiet_fire() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        s.update_global(vec![summary("alpha", 1, 0), summary("beta", 1, 0)]);
        // Order: work(current), alpha, beta. Several taps land, each one
        // arriving before the next fire — faster than the old 1-tick grain
        // could distinguish. Every fire covering a tap must skip, not just
        // the first.
        s.cycle(Direction::Next); // alpha
        assert_eq!(s.tick(), None);
        s.cycle(Direction::Next); // beta
        assert_eq!(s.tick(), None);
        s.cycle(Direction::Next); // wraps to work (current) — still just cycling
        assert_eq!(s.tick(), None, "a fire covering a tap must skip even on the 3rd consecutive tap");
        s.cycle(Direction::Prev); // back to beta — the LAST tap in this burst
        assert_eq!(s.tick(), None, "the fire covering the last tap still must not commit");
        let t = s.tick().expect("the first fully quiet fire after the burst commits");
        assert_eq!(t.name, "beta", "must commit to whatever the LAST tap selected, not an earlier one");
    }

    // -- Pinning: wants_fast_cadence() --------------------------------------
    // No wasm caller exercises this from a host test, so a refactor that
    // breaks the "armed while a selection is pending" contract would
    // otherwise go unnoticed until manual testing in Zellij.

    #[test]
    fn wants_fast_cadence_tracks_pending_selection_through_commit_and_cancel() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        s.update_global(vec![summary("alpha", 1, 0), summary("beta", 1, 0)]);
        assert!(!s.wants_fast_cadence(), "nothing selected initially");

        s.cycle(Direction::Next); // selects alpha
        assert!(s.wants_fast_cadence(), "a cycle tap arms a pending selection");

        s.tick(); // the fire covering the tap: skip, still pending
        assert!(s.wants_fast_cadence(), "a tap-covering fire must not clear the pending selection");
        s.tick(); // idle commit to alpha
        assert!(!s.wants_fast_cadence(), "a commit clears the pending selection");

        s.cycle(Direction::Next); // alpha
        s.cycle(Direction::Next); // beta
        s.cycle(Direction::Next); // wraps to work (current)
        assert!(s.wants_fast_cadence(), "still pending until the idle tick fires");

        s.tick(); // the fire covering the last tap: skip, still pending
        assert!(s.wants_fast_cadence());
        s.tick(); // cancel-commit (lands on current)
        assert!(!s.wants_fast_cadence(), "a cancel-commit also clears the pending selection");
    }

    // -- Pinning: content-compare returns ------------------------------------
    // Each mutator must report `true` only when the derived badge actually
    // differs, not merely because it was called. A refactor that starts
    // returning `true` unconditionally (or `false` unconditionally) would
    // make the runtime over- or under-repaint without any other test
    // noticing, since none of the tests above assert on the return value.

    #[test]
    fn update_global_reports_change_only_on_actual_content_change() {
        let mut s = Sessions::default();
        assert!(s.update_global(vec![summary("alpha", 1, 0)]), "first peer report changes the badge");
        assert!(!s.update_global(vec![summary("alpha", 1, 0)]), "identical peer report is not a change");
    }

    #[test]
    fn set_own_reports_change_only_on_actual_content_change() {
        let mut s = Sessions::default();
        let p = SessionSummary {
            session_name: "work".into(),
            running: 3,
            attention: 0,
            attention_tab_position: None,
            tabs: vec![],
        };
        assert!(s.set_own(p.clone()), "first own-count report changes the badge");
        assert!(!s.set_own(p), "an identical report is not a change");
    }

    #[test]
    fn cycle_reports_change_when_the_highlight_moves() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        s.update_global(vec![summary("alpha", 1, 0), summary("beta", 1, 0)]);
        assert!(s.cycle(Direction::Next), "starting a selection flips a `selected` flag in the badge");
        assert!(s.cycle(Direction::Next), "advancing the selection moves the `selected` flag");
    }

    // -- Selection identity (not positional index) ---------------------------
    // `SelectionState` must track the selected session by name, not by its
    // position in the derived order — a reorder between the cycle() tap and
    // the commit tick() must never silently retarget a different session.

    #[test]
    fn cycle_selection_survives_reordering_by_name_not_index() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        s.update_global(vec![summary("alpha", 1, 0), summary("beta", 1, 0)]);
        // Order: work (current), alpha, beta — both zero-attention, by name.
        s.cycle(Direction::Next); // selects alpha (first non-current)

        // "aardvark" joins and sorts ahead of "alpha" in the rest bucket. A
        // positional-index selection (old index 1) would now point at
        // "aardvark" instead of "alpha". `update_global` REPLACES the
        // peer set wholesale, so this is the same "membership changed under
        // the selection" shape.
        s.update_global(vec![summary("aardvark", 1, 0), summary("alpha", 1, 0), summary("beta", 1, 0)]);

        s.tick(); // the fire covering the tap: skip, clears the flag
        let t = s.tick().expect("a quiet fire commits");
        assert_eq!(t.name, "alpha", "selection must follow the named session, not the reordered index");
    }

    #[test]
    fn selected_session_vanishing_before_tick_clears_selection_and_commits_nothing() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        s.update_global(vec![summary("alpha", 0, 0)]);
        s.cycle(Direction::Next); // selects alpha
        s.update_global(vec![]); // alpha's summary vanished before the idle tick

        s.tick(); // the fire covering the tap: skip, clears the flag (selection still pending)
        assert_eq!(s.tick(), None, "a vanished selection must not commit");
        assert!(!s.wants_fast_cadence(), "the cleared selection must not keep fast cadence armed");
    }

    // -- Pinning: cycle restart after vanish and no-op change report -----------
    // Cycle selection must restart fresh when the previously-selected session
    // disappears and a new cycle() call advances the selection. A no-op cycle
    // (nothing to select) must not set up a pending selection or report a
    // badge change.

    #[test]
    fn cycle_restarts_fresh_after_selected_session_vanishes() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        s.update_global(vec![summary("alpha", 1, 0), summary("beta", 1, 0), summary("gamma", 1, 0)]);
        // No attention: order is work (current), then alpha, beta, gamma by name.

        s.cycle(Direction::Next); // selects alpha (first non-current)

        // alpha vanishes, leaving a fresh order with *two* non-current entries
        // (beta, gamma) — order: work, beta, gamma.
        s.update_global(vec![summary("beta", 1, 0), summary("gamma", 1, 0)]);

        // Cycling with Prev is what actually discriminates restart-fresh from
        // an index-0 fallback: restart-fresh ignores the stale direction and
        // always selects the first non-current entry (beta). A regression
        // that resolved the vanished name to index 0 ("as if `work`/current
        // were selected") would instead advance *backward* from there and
        // land on the last entry (gamma) — the same bug that, under
        // Direction::Next, would coincidentally land on beta too and pass
        // silently.
        s.cycle(Direction::Prev);
        let badge = s.badge();
        let selected: Vec<&str> = badge.iter().filter(|b| b.selected).map(|b| b.name.as_str()).collect();
        assert_eq!(
            selected,
            vec!["beta"],
            "restart-fresh must ignore the stale direction and pick the first \
             non-current entry, not fall back to advancing from index 0"
        );

        s.tick(); // the fire covering the last tap: skip, clears the flag
        let t = s.tick().expect("a quiet fire commits");
        assert_eq!(
            t.name, "beta",
            "after vanished selection, cycle must restart and select the first non-current entry"
        );
    }

    // -- Pinning: a peer claiming our own name -------------------------------

    #[test]
    fn peer_claiming_own_name_does_not_duplicate_own_entry() {
        // `own` is this session's own direct knowledge (set_own's doc
        // comment) and must win outright — the peer here claims larger
        // counts, and must still lose.
        let mut s = Sessions::default();
        s.set_own(SessionSummary { running: 3, ..own("work") });
        s.update_global(vec![summary("work", 9, 9)]);
        let badge = s.badge();
        let work_entries: Vec<&BadgeEntry> = badge.iter().filter(|b| b.name == "work").collect();
        assert_eq!(work_entries.len(), 1, "a peer claiming our own name must not add a second badge line");
        assert!(work_entries[0].is_current, "the surviving line must be the own entry");
        assert_eq!(work_entries[0].running, 3, "own's own-known counts must win over a peer claiming the same name");
        assert_eq!(work_entries[0].attention, 0, "own's own-known counts must win over a peer claiming the same name");
    }

    #[test]
    fn cycle_returns_false_on_noop_with_only_current_session() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        // No summaries, no peers — work is the only session.

        let changed = s.cycle(Direction::Next);
        assert!(!changed, "cycle with nothing to select must return false (no badge change)");
        assert!(!s.wants_fast_cadence(), "cycle with nothing to select must not set up a pending selection");
    }
}
