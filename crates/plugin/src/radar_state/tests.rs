use super::*;
use crate::kind::Kind;
use crate::test_fixtures::{self, pane, set_status};

/// Unlike the shared `test_fixtures::tab` (id derived from position), this
/// suite pins ids explicitly: its tests exercise TabId-vs-position decoupling
/// (ids 10/20/30 shuffled across positions 0/1/2).
fn tab(id: usize, position: usize, name: &str, active: bool) -> RadarTab {
    test_fixtures::tab_with_id(id, position, name, active)
}

fn focused_pane(id: u32) -> TerminalPane {
    TerminalPane { focused_in_tab: true, ..pane(id) }
}

fn own_tab(radar: &mut RadarState, position: usize) {
    radar.own_plugin_tab_changed(Some(position));
}

#[test]
fn pending_wait_drives_the_slow_cadence_until_saturation() {
    // The `· Nm` wait tag advances at minute granularity, so an unsaturated
    // pending row must keep the Slow heartbeat armed — and release it once
    // the tag freezes at 1h+ (same window the ledger uses).
    let mut radar = RadarState::default();
    // `set_status` stamps the wait edge at 100 s.
    set_status(&mut radar, 7, Status::Pending, "pinky", 1);
    assert!(radar.pending_wait_unsaturated(100));
    assert!(radar.pending_wait_unsaturated(100 + crate::ledger::SATURATE_S - 1));
    assert!(!radar.pending_wait_unsaturated(100 + crate::ledger::SATURATE_S));
    // Answering the question (running again) releases it immediately.
    set_status(&mut radar, 7, Status::Running, "pinky", 2);
    assert!(!radar.pending_wait_unsaturated(100));
}

#[test]
fn rows_sort_tabs_by_position_and_aggregate_panes() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(30, 2, "c", false), tab(10, 0, "a", true), tab(20, 1, "b", false)]);
    radar.set_tab_panes_for_position(0, vec![pane(42)]);
    set_status(&mut radar, 42, Status::Running, "working", 1);

    let rows = radar.rows(1);

    assert_eq!(rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["a", "b", "c"]);
    assert_eq!(rows[0].display.status, Status::Running);
}

#[test]
fn tabs_changed_sanitizes_host_names_before_they_reach_the_grid() {
    // Tab names are the one externally-writable string that reaches the render
    // grid (`zellij rename-tab`, layouts). A newline would desync the
    // line↔click-target lockstep; an ESC would inject ANSI; bidi controls are
    // the Trojan-Source class. The intake scrubs all three, like pane titles.
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "evil\x1b[31m\nname\u{202e}", true)]);
    assert_eq!(radar.rows(0)[0].name, "evil name");
}

#[test]
fn a_shown_floating_pane_holds_focus_for_the_highlight() {
    // Zellij reports focus per layer: tiled pane 1 AND floating pane 2 both
    // say focused. Which one the user is in depends on the tab's layer.
    let floating = TerminalPane { floating: true, ..focused_pane(2) };
    for (shown, want) in [(true, 2), (false, 1)] {
        let mut radar = RadarState::default();
        radar.tabs_changed(vec![RadarTab { floating_visible: shown, ..tab(10, 0, "a", true) }]);
        radar.set_tab_panes_for_position(0, vec![focused_pane(1), floating.clone()]);
        assert_eq!(radar.rows(0)[0].display.focused_pane, Some(want), "floating shown={shown}");
    }
}

#[test]
fn rename_ownership_follows_stable_tab_id_across_reorder() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "Tab #1", true), tab(20, 1, "custom", false)]);
    own_tab(&mut radar, 0);
    radar.set_tab_panes_for_position(0, vec![focused_pane(1)]);
    let rename = radar.cwd_changed(1, "/work/alpha".into(), config::NamingMode::Managed);
    assert_eq!(rename.renames, vec![TabRename { id: TabId::new(10), name: "alpha".into() }]);
    assert_eq!(radar.applied_name(TabId::new(10)), Some("alpha"));

    radar.tabs_changed(vec![tab(20, 0, "custom", false), tab(10, 1, "alpha", true)]);
    radar.set_tab_panes_for_position(1, vec![focused_pane(1)]);
    let rename = radar.cwd_changed(1, "/work/beta".into(), config::NamingMode::Managed);

    assert_eq!(rename.renames, vec![TabRename { id: TabId::new(10), name: "beta".into() }]);
    assert_eq!(radar.applied_name(TabId::new(10)), Some("beta"));
}

#[test]
fn managed_naming_skips_manual_names_but_force_overrides() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "manual", true)]);
    own_tab(&mut radar, 0);
    radar.set_tab_panes_for_position(0, vec![focused_pane(1)]);
    radar.pane_cwd.insert(1, "/work/repo".into());

    let managed = radar.recompute_renames(config::NamingMode::Managed);
    assert!(managed.is_empty());

    let forced = radar.recompute_renames(config::NamingMode::Force);
    assert_eq!(forced, vec![TabRename { id: TabId::new(10), name: "repo".into() }]);
}

fn pane_update(tab_panes: HashMap<usize, Vec<TerminalPane>>) -> PaneUpdate {
    let live = tab_panes.values().flat_map(|panes| panes.iter().map(|p| p.id)).collect();
    PaneUpdate { tab_panes, live, theme: None, native: HashMap::new() }
}

#[test]
fn panes_changed_requests_cwd_bootstrap_for_new_pane_without_cwd() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "Tab #1", true)]);

    let change =
        radar.panes_changed(pane_update(HashMap::from([(0, vec![focused_pane(7)])])), 1, config::NamingMode::Managed);

    assert_eq!(change.cwd_bootstrap, vec![7]);
}

#[test]
fn panes_changed_requests_no_cwd_bootstrap_when_naming_off() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "Tab #1", true)]);

    let change =
        radar.panes_changed(pane_update(HashMap::from([(0, vec![focused_pane(7)])])), 1, config::NamingMode::Off);

    assert!(change.cwd_bootstrap.is_empty());
}

#[test]
fn panes_changed_skips_cwd_bootstrap_when_cwd_already_known() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "Tab #1", true)]);
    radar.cwd_changed(7, "/work/repo".into(), config::NamingMode::Managed);

    let change =
        radar.panes_changed(pane_update(HashMap::from([(0, vec![focused_pane(7)])])), 1, config::NamingMode::Managed);

    assert!(change.cwd_bootstrap.is_empty());
}

#[test]
fn panes_changed_requests_each_pane_cwd_only_once_even_if_unresolved() {
    // The host call may come back empty (a fresh pane with no cwd yet); we
    // must never re-issue it, or we rebuild the meltdown re-poll loop.
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "Tab #1", true)]);
    let update = || pane_update(HashMap::from([(0, vec![focused_pane(7)])]));

    let first = radar.panes_changed(update(), 1, config::NamingMode::Managed);
    let second = radar.panes_changed(update(), 2, config::NamingMode::Managed);

    assert_eq!(first.cwd_bootstrap, vec![7]);
    assert!(second.cwd_bootstrap.is_empty());
}

#[test]
fn cwd_bootstrap_attempt_resets_when_pane_id_is_recycled() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "Tab #1", true)]);

    let first =
        radar.panes_changed(pane_update(HashMap::from([(0, vec![focused_pane(7)])])), 1, config::NamingMode::Managed);
    assert_eq!(first.cwd_bootstrap, vec![7]);

    // Pane 7 closes (no longer live), then a new pane reuses id 7.
    radar.panes_changed(pane_update(HashMap::new()), 2, config::NamingMode::Managed);
    let reborn =
        radar.panes_changed(pane_update(HashMap::from([(0, vec![focused_pane(7)])])), 3, config::NamingMode::Managed);

    assert_eq!(reborn.cwd_bootstrap, vec![7]);
}

#[test]
fn cwd_bootstrap_prioritizes_focused_panes_and_caps_volume_per_update() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "Tab #1", true)]);
    // Eight unfocused panes (ids 1..=8) plus one focused pane (id 9): more
    // candidates than the per-update cap.
    let mut panes: Vec<TerminalPane> = (1..=MAX_CWD_BOOTSTRAP_PER_UPDATE as u32).map(pane).collect();
    panes.push(focused_pane(9));

    let first = radar.panes_changed(pane_update(HashMap::from([(0, panes)])), 1, config::NamingMode::Managed);

    assert_eq!(first.cwd_bootstrap.len(), MAX_CWD_BOOTSTRAP_PER_UPDATE);
    // The focused pane is resolved this round even though its id sorts last;
    // the lowest-id unfocused pane (8) spills to the next round.
    assert!(first.cwd_bootstrap.contains(&9));
    assert!(!first.cwd_bootstrap.contains(&8));

    let second = radar.panes_changed(
        pane_update(HashMap::from([(
            0,
            (1..=MAX_CWD_BOOTSTRAP_PER_UPDATE as u32).map(pane).chain(std::iter::once(focused_pane(9))).collect(),
        )])),
        2,
        config::NamingMode::Managed,
    );
    assert_eq!(second.cwd_bootstrap, vec![8]);
}

#[test]
fn bootstrapped_cwd_names_the_tab_and_later_cd_still_renames() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "Tab #1", true)]);
    own_tab(&mut radar, 0);

    // 1. New tab → bootstrap requests pane 7's cwd.
    let opened =
        radar.panes_changed(pane_update(HashMap::from([(0, vec![focused_pane(7)])])), 1, config::NamingMode::Managed);
    assert_eq!(opened.cwd_bootstrap, vec![7]);

    // 2. Host resolves it; the tab is named from the spawn directory.
    let bootstrapped = radar.cwd_changed(7, "/work/alpha".into(), config::NamingMode::Managed);
    assert_eq!(bootstrapped.renames, vec![TabRename { id: TabId::new(10), name: "alpha".into() }]);

    // 3. A later PaneUpdate does not re-request the cwd (we already have it).
    let refreshed =
        radar.panes_changed(pane_update(HashMap::from([(0, vec![focused_pane(7)])])), 2, config::NamingMode::Managed);
    assert!(refreshed.cwd_bootstrap.is_empty());

    // 4. A real `cd` (CwdChanged) renames the tab as before.
    let moved = radar.cwd_changed(7, "/work/beta".into(), config::NamingMode::Managed);
    assert_eq!(moved.renames, vec![TabRename { id: TabId::new(10), name: "beta".into() }]);
}

// The pure roll-up behaviours — untracked panes shown-but-not-counted,
// severity precedence, tie-break, done/total/pending counts, and the
// command-only outcome mapping — are tested directly against `roll_up` in
// `rollup.rs`. The tests below cover what the roll-up seam can't see: the
// status>command resolution precedence and the full command-pipeline path.

// ── End-result outcome (full pipeline) ──

#[test]
fn applied_tab_name_sticks_when_focus_moves_to_a_different_repo_pane() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "Tab #1", true)]);
    own_tab(&mut radar, 0);
    radar.set_tab_panes_for_position(0, vec![focused_pane(1), pane(2)]);
    radar.cwd_changed(1, "/work/alpha".into(), config::NamingMode::Managed);
    radar.cwd_changed(2, "/work/beta".into(), config::NamingMode::Managed);
    // Focused pane 1 named the tab "alpha".
    assert_eq!(radar.applied_name(TabId::new(10)), Some("alpha"));
    // Zellij echoes our rename back as a TabUpdate.
    radar.tabs_changed(vec![tab(10, 0, "alpha", true)]);

    // Focus shifts to pane 2 (a different repo). "alpha" is still justified by
    // pane 1, so the tab name must NOT flip — no rename emitted.
    let change = radar.panes_changed(
        pane_update(HashMap::from([(0, vec![pane(1), focused_pane(2)])])),
        2,
        config::NamingMode::Managed,
    );
    assert!(change.renames.is_empty());
    assert_eq!(radar.applied_name(TabId::new(10)), Some("alpha"));
}

#[test]
fn manual_rename_is_preserved_through_focus_and_cwd_changes() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "Tab #1", true)]);
    own_tab(&mut radar, 0);
    radar.set_tab_panes_for_position(0, vec![focused_pane(1)]);
    radar.cwd_changed(1, "/work/alpha".into(), config::NamingMode::Managed);
    // Zellij echoes our auto-name, then the user renames the tab by hand.
    radar.tabs_changed(vec![tab(10, 0, "alpha", true)]);
    radar.tabs_changed(vec![tab(10, 0, "my-thing", true)]);

    // A focus change must not reclaim the manual name.
    let focus = radar.panes_changed(
        pane_update(HashMap::from([(0, vec![focused_pane(1), pane(2)])])),
        2,
        config::NamingMode::Managed,
    );
    assert!(focus.renames.is_empty());

    // Neither may a later `cd` in the pane.
    let cd = radar.cwd_changed(1, "/work/gamma".into(), config::NamingMode::Managed);
    assert!(cd.renames.is_empty());
}

#[test]
fn applied_tab_name_repicks_when_the_naming_pane_closes() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "Tab #1", true)]);
    own_tab(&mut radar, 0);
    radar.set_tab_panes_for_position(0, vec![focused_pane(1), pane(2)]);
    radar.cwd_changed(1, "/work/alpha".into(), config::NamingMode::Managed);
    radar.cwd_changed(2, "/work/beta".into(), config::NamingMode::Managed);
    assert_eq!(radar.applied_name(TabId::new(10)), Some("alpha"));
    // Zellij echoes our rename back as a TabUpdate.
    radar.tabs_changed(vec![tab(10, 0, "alpha", true)]);

    // Pane 1 (which justified "alpha") closes. "alpha" is no longer supported,
    // so the tab re-picks from the surviving pane → "beta".
    let change =
        radar.panes_changed(pane_update(HashMap::from([(0, vec![focused_pane(2)])])), 2, config::NamingMode::Managed);
    assert_eq!(change.renames, vec![TabRename { id: TabId::new(10), name: "beta".into() }]);
    assert_eq!(radar.applied_name(TabId::new(10)), Some("beta"));
}

#[test]
fn mutating_events_request_a_render() {
    // Renders are requested exactly where something rows-visible changed —
    // Zellij delivers each of these events to EVERY tab's instance, so a
    // blanket render=true multiplied session-wide repaints by the event rate.
    let mut radar = RadarState::default();
    assert!(radar.tabs_changed(vec![tab(1, 0, "a", true)]).render, "a genuine tab change must request a render");
    assert_eq!(
        radar.tabs_changed(vec![tab(1, 0, "a", true)]),
        RadarChange::default(),
        "an identical TabUpdate is a strict no-op (Zellij re-reports on focus churn)"
    );
    assert!(
        !radar.cwd_changed(1, "/tmp".into(), config::NamingMode::Off).render,
        "a cwd feeds naming only; the repaint (if any) rides the RenameTab \
         effect's own TabUpdate echo"
    );
    // An attention move is rows-visible.
    assert!(set_status(&mut radar, 1, Status::Done, "done", 2), "a new native status must repaint");
}

// ── next_attention_tab integration tests ─────────────────────────────────

#[test]
fn next_attention_tab_skips_running_and_idle() {
    let mut st = RadarState::default();
    // 3 tabs at positions 0,1,2; tab 0 active.
    st.tabs_changed(vec![
        RadarTab {
            floating_visible: false,
            id: TabId::new(1),
            position: 0,
            name: "a".into(),
            active: true,
            has_bell: false,
        },
        RadarTab {
            floating_visible: false,
            id: TabId::new(2),
            position: 1,
            name: "b".into(),
            active: false,
            has_bell: false,
        },
        RadarTab {
            floating_visible: false,
            id: TabId::new(3),
            position: 2,
            name: "c".into(),
            active: false,
            has_bell: false,
        },
    ]);
    // tab 0: running (not attention); tab 1: pending (attention); tab 2: idle.
    st.set_tab_panes_for_position(0, vec![pane(10)]);
    st.set_tab_panes_for_position(1, vec![pane(11)]);
    set_status(&mut st, 10, Status::Running, "", 1);
    set_status(&mut st, 11, Status::Pending, "", 1);

    assert_eq!(st.next_attention_tab(Direction::Next), Some(1));
    assert_eq!(st.next_attention_tab(Direction::Prev), Some(1));
}

#[test]
fn next_attention_tab_none_when_no_attention() {
    let mut st = RadarState::default();
    st.tabs_changed(vec![RadarTab {
        floating_visible: false,
        id: TabId::new(1),
        position: 0,
        name: "a".into(),
        active: true,
        has_bell: false,
    }]);
    st.set_tab_panes_for_position(0, vec![pane(10)]);
    set_status(&mut st, 10, Status::Running, "", 1);
    assert_eq!(st.next_attention_tab(Direction::Next), None);
    assert_eq!(st.next_attention_tab(Direction::Prev), None);
}

// ── cycle_attention unit tests ────────────────────────────────────────────

#[test]
fn cycle_attention_empty_set_is_none() {
    let tabs = [(0usize, Status::Idle), (1, Status::Running)];
    assert_eq!(cycle_attention(&tabs, Some(0), Direction::Next), None);
    assert_eq!(cycle_attention(&tabs, Some(0), Direction::Prev), None);
}

#[test]
fn cycle_attention_sole_member_equal_to_active_is_none() {
    let tabs = [(0usize, Status::Pending), (1, Status::Running)];
    assert_eq!(cycle_attention(&tabs, Some(0), Direction::Next), None);
    assert_eq!(cycle_attention(&tabs, Some(0), Direction::Prev), None);
}

#[test]
fn cycle_attention_next_and_prev_wrap_around() {
    // attention at positions 2 and 5
    let tabs = [(2usize, Status::Pending), (5, Status::Error)];
    // active = 2 → next is 5, prev wraps to 5
    assert_eq!(cycle_attention(&tabs, Some(2), Direction::Next), Some(5));
    assert_eq!(cycle_attention(&tabs, Some(2), Direction::Prev), Some(5));
    // active = 5 → next wraps to 2, prev is 2
    assert_eq!(cycle_attention(&tabs, Some(5), Direction::Next), Some(2));
    assert_eq!(cycle_attention(&tabs, Some(5), Direction::Prev), Some(2));
}

#[test]
fn cycle_attention_active_outside_set_enters_set() {
    let tabs = [(2usize, Status::Pending), (5, Status::Done)];
    // active = 3 (not an attention tab) → next 5, prev 2
    assert_eq!(cycle_attention(&tabs, Some(3), Direction::Next), Some(5));
    assert_eq!(cycle_attention(&tabs, Some(3), Direction::Prev), Some(2));
    // active = None → next = smallest, prev = largest
    assert_eq!(cycle_attention(&tabs, None, Direction::Next), Some(2));
    assert_eq!(cycle_attention(&tabs, None, Direction::Prev), Some(5));
}

// ── Stateful property test ────────────────────────────────────────────────
//
// `RadarState` is an event aggregator: the host feeds it interleaved tab,
// pane, attention, timer and cwd events, and a bug usually hides
// in some *ordering* of them rather than any single event. The example tests
// above cover specific sequences; this drives random sequences and asserts
// the structural invariants that must hold after ANY history.
use proptest::prelude::*;

fn arb_status() -> impl Strategy<Value = Status> {
    prop_oneof![
        Just(Status::Idle),
        Just(Status::Done),
        Just(Status::Running),
        Just(Status::Pending),
        Just(Status::Error),
    ]
}

/// One host event. Pane ids and tab positions are drawn from small ranges so
/// they recur across events — exercising prune, pane recycling, and tab
/// reordering rather than a stream of never-seen ids.
#[derive(Clone, Debug)]
enum Op {
    /// Replace the tab set with tabs at these positions (deduped; first active).
    Tabs(Vec<usize>),
    /// Set the live pane layout (position → pane ids), the native attention
    /// the manifest carries, and an optional focused pane id (marks that pane
    /// `focused_in_tab`, driving real focus transitions — including bounces to
    /// a None reading when the focus id isn't present).
    Panes(Vec<(usize, Vec<u32>)>, Vec<(u32, Status)>, Option<u32>),
    /// Deliver a `PaneAttentionChanged` for a pane.
    Status(u32, Status),
    /// Fire the timer.
    Timer,
    /// Report a pane's cwd.
    Cwd(u32),
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        proptest::collection::vec(0usize..4, 0..4).prop_map(Op::Tabs),
        (
            proptest::collection::vec((0usize..4, proptest::collection::vec(1u32..6, 0..3)), 0..4),
            proptest::collection::vec((1u32..6, arb_status()), 0..3),
            proptest::option::of(1u32..6),
        )
            .prop_map(|(layout, native, focus)| Op::Panes(layout, native, focus)),
        (1u32..6, arb_status()).prop_map(|(p, s)| Op::Status(p, s)),
        Just(Op::Timer),
        (1u32..6).prop_map(Op::Cwd),
    ]
}

proptest! {
    #[test]
    fn radar_state_invariants_hold_after_any_event_sequence(
        ops in proptest::collection::vec(arb_op(), 0..40)
    ) {
        let mut st = RadarState::default();

        for (i, op) in ops.iter().enumerate() {
            let tick = i as u64;
            match op {
                Op::Tabs(positions) => {
                    let mut seen = HashSet::new();
                    let tabs: Vec<RadarTab> = positions
                        .iter()
                        .filter(|p| seen.insert(**p))
                        .enumerate()
                        .map(|(idx, &p)| RadarTab { floating_visible: false,
                            id: TabId::new(p + 1),
                            position: p,
                            name: format!("t{p}"),
                            active: idx == 0,
                            has_bell: false,
                        })
                        .collect();
                    st.tabs_changed(tabs);
                }
                Op::Panes(layout, natives, focus) => {
                    let mut tab_panes: HashMap<usize, Vec<TerminalPane>> = HashMap::new();
                    for (pos, ids) in layout {
                        let panes = ids
                            .iter()
                            .map(|&id| TerminalPane { floating: false,
                                id,
                                title: format!("p{id}"),
                                // Mark the drawn focus pane focused_in_tab. When the
                                // focus id isn't present the reading is None — so the
                                // sequence also exercises focus bounces off a terminal.
                                focused_in_tab: Some(id) == *focus,
                            })
                            .collect();
                        tab_panes.insert(*pos, panes);
                    }
                    let live: HashSet<u32> = tab_panes
                        .values()
                        .flat_map(|v| v.iter().map(|p| p.id))
                        .collect();
                    let native: HashMap<u32, crate::attention::NativePane> = natives
                        .iter()
                        .map(|&(id, status)| {
                            let attention = test_fixtures::program_attention(status, "m");
                            (id, crate::attention::NativePane { attention, ..Default::default() })
                        })
                        .collect();
                    let update = PaneUpdate { tab_panes, live: live.clone(), theme: None, native: native.clone() };
                    st.panes_changed(update, tick, config::NamingMode::Off);

                    // The manifest is the whole truth: after panes_changed every
                    // observation belongs to a pane it carries attention for.
                    for id in st.native.keys() {
                        prop_assert!(native.contains_key(id), "native observation kept for pane {id} the manifest dropped");
                    }
                }
                Op::Status(pane, status) => {
                    set_status(&mut st, *pane, *status, "m", tick);
                }
                Op::Timer => {
                    st.timer(tick);
                }
                Op::Cwd(pane) => {
                    st.cwd_changed(*pane, "/home/u/proj".into(), config::NamingMode::Off);
                }
            }

            // `rows()` is total (never panics) and well-formed after every op.
            let rows = st.rows(tick);
            for w in rows.windows(2) {
                prop_assert!(
                    w[0].number < w[1].number,
                    "rows must be strictly ordered by tab position"
                );
            }
            for r in rows.iter() {
                // 1:1 pane mapping and count sanity flow through the pipeline.
                prop_assert!(r.display.progress.done <= r.display.progress.total);
                prop_assert!(r.display.progress.total <= r.display.panes.len());
            }
        }

    }

    #[test]
    fn attention_next_visits_every_member_and_returns_to_start(
        members in proptest::collection::btree_set(0usize..64, 1..8),
        start_pick in 0usize..8,
    ) {
        let members: Vec<usize> = members.into_iter().collect();
        let m = members.len();
        let start = members[start_pick % m];
        let tabs: Vec<(usize, Status)> =
            members.iter().map(|&p| (p, Status::Pending)).collect();

        let mut active = Some(start);
        let mut visited = Vec::new();
        for _ in 0..m {
            match cycle_attention(&tabs, active, Direction::Next) {
                None => {
                    // Only legal when the set has a single member equal to active.
                    prop_assert_eq!(m, 1);
                    visited.push(start);
                }
                Some(n) => {
                    prop_assert_ne!(Some(n), active);
                    visited.push(n);
                    active = Some(n);
                }
            }
        }
        // Returned to the origin after a full lap, having touched every member once.
        prop_assert_eq!(active, Some(start));
        let mut seen = visited.clone();
        seen.sort_unstable();
        seen.dedup();
        prop_assert_eq!(seen, members);
    }
}

// `PaneUpdate::from_raw` — the manifest-folding policy that used to live in the
// wasm-only `Event::PaneUpdate` arm (host-untestable). These pin the parts that
// decide observable behavior: plugin-skip, title sanitize, and the
// focused-else-any terminal-color precedence.

fn raw_pane(id: u32, tab_pos: usize) -> RawPane {
    RawPane {
        is_floating: false,
        tab_pos,
        id,
        title: String::new(),
        is_plugin: false,
        is_focused: false,
        default_bg: None,
        default_fg: None,
        attention: None,
    }
}

#[test]
fn from_raw_skips_plugin_panes() {
    let update = PaneUpdate::from_raw(vec![RawPane { is_plugin: true, ..raw_pane(1, 0) }, raw_pane(2, 0)]);
    assert!(!update.live.contains(&1), "the plugin (rail) pane is not a live terminal");
    assert_eq!(update.live, HashSet::from([2]));
    assert_eq!(update.tab_panes[&0].len(), 1);
    assert_eq!(update.tab_panes[&0][0].id, 2);
}

#[test]
fn from_raw_sanitizes_and_truncates_titles() {
    let update = PaneUpdate::from_raw(vec![RawPane { title: "x".repeat(100), ..raw_pane(1, 0) }]);
    assert!(update.tab_panes[&0][0].title.chars().count() <= 40);
}

#[test]
fn from_raw_collects_live_ids_and_groups_by_tab() {
    let update = PaneUpdate::from_raw(vec![raw_pane(1, 0), raw_pane(2, 0), raw_pane(3, 1)]);
    assert_eq!(update.live, HashSet::from([1, 2, 3]));
    assert_eq!(update.tab_panes[&0].len(), 2);
    assert_eq!(update.tab_panes[&1].len(), 1);
}

#[test]
fn from_raw_theme_prefers_focused_pane_over_any_regardless_of_order() {
    // An earlier *unfocused* pane reports colors, a later *focused* pane reports
    // different ones — the focused pane must win even though it appears second.
    let update = PaneUpdate::from_raw(vec![
        RawPane { default_bg: Some("#101010".into()), default_fg: Some("#aaaaaa".into()), ..raw_pane(1, 0) },
        RawPane {
            is_focused: true,
            default_bg: Some("#202020".into()),
            default_fg: Some("#bbbbbb".into()),
            ..raw_pane(2, 0)
        },
    ]);
    // DerivedColors isn't PartialEq; rail_bg uniquely distinguishes the two inputs.
    let focused = theme::DerivedColors::from_bg_fg((0x20, 0x20, 0x20), (0xbb, 0xbb, 0xbb));
    let any = theme::DerivedColors::from_bg_fg((0x10, 0x10, 0x10), (0xaa, 0xaa, 0xaa));
    let theme = update.theme.expect("a terminal pane reported colors");
    assert_eq!(theme.rail_bg, focused.rail_bg);
    assert_ne!(theme.rail_bg, any.rail_bg);
}

#[test]
fn from_raw_theme_falls_back_to_any_terminal_pane_when_none_focused() {
    let update = PaneUpdate::from_raw(vec![RawPane {
        default_bg: Some("#101010".into()),
        default_fg: Some("#aaaaaa".into()),
        ..raw_pane(1, 0)
    }]);
    let expected = theme::DerivedColors::from_bg_fg((0x10, 0x10, 0x10), (0xaa, 0xaa, 0xaa));
    assert_eq!(update.theme.expect("colors present").rail_bg, expected.rail_bg);
}

#[test]
fn from_raw_theme_is_none_without_color_reports() {
    assert!(PaneUpdate::from_raw(vec![raw_pane(1, 0)]).theme.is_none());
}

// ── Ledger recede edges ──

/// A tab "work" holding pane 1, which then closes carrying `done`.
fn radar_with_a_closed_done_pane() -> RadarState {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "work", true)]);
    radar.panes_changed(native_update(vec![(1, native_done("claude-code", "shipped"))]), 1, config::NamingMode::Off);
    radar.panes_changed(native_update(vec![]), 2, config::NamingMode::Off);
    radar
}

#[test]
fn ledger_any_unsaturated_reflects_the_saturate_window() {
    let mut radar = RadarState::default();
    assert!(!radar.ledger_any_unsaturated(1_000_000), "an empty ledger has nothing left to age out");

    radar = radar_with_a_closed_done_pane(); // ledgers at its completion epoch, 50
    assert!(radar.ledger_any_unsaturated(50 + crate::ledger::SATURATE_S - 1));
    assert!(!radar.ledger_any_unsaturated(50 + crate::ledger::SATURATE_S));
}

#[test]
fn pending_and_running_never_ledger() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "work", true)]);
    let mut pending = native_working("claude-code", "queued");
    pending.attention.status = Status::Pending;
    radar.panes_changed(
        native_update(vec![(1, native_working("codex", "building")), (2, pending)]),
        1,
        config::NamingMode::Off,
    );
    assert_eq!(radar.rows(1)[0].display.progress.pending, 1);

    // Both panes close.
    radar.panes_changed(native_update(vec![]), 5, config::NamingMode::Off);

    assert!(radar.ledger().is_empty(), "Running/Pending carry nothing to ledger — only Done/Error recede");
}

#[test]
fn ledger_lines_resolve_live_tab_position_or_none() {
    let mut radar = radar_with_a_closed_done_pane();

    let lines = radar.ledger_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].tab_position, Some(0), "tab 10 is still live at position 0");

    // The tab closes; a later `tabs_changed` no longer carries it.
    radar.tabs_changed(vec![]);

    let lines = radar.ledger_lines();
    assert_eq!(lines.len(), 1, "the ledger itself never forgets an entry");
    assert_eq!(lines[0].tab_position, None, "a gone tab resolves to a click-inert row");
}

// ── Ping flash on flip-to-pending ──

// ── Native attention (Zellij ≥ 0.46) ──

fn native_working(app: &str, text: &str) -> crate::attention::NativePane {
    use crate::attention::{NativePane, PaneAttention, Record, Source};
    NativePane {
        attention: PaneAttention {
            status: Status::Running,
            source: Source::Program,
            text: text.into(),
            app: Some(app.into()),
            ..PaneAttention::default()
        },
        records: vec![Record { status: Status::Running, app: Some(app.into()), ..Record::default() }],
    }
}

fn native_update(panes: Vec<(u32, crate::attention::NativePane)>) -> PaneUpdate {
    let mut update = pane_update(HashMap::from([(0, panes.iter().map(|(id, _)| pane(*id)).collect())]));
    update.native = panes.into_iter().collect();
    update
}

#[test]
fn native_attention_drives_the_row() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "Tab #1", true)]);
    radar.panes_changed(
        native_update(vec![(7, native_working("claude-code", "Reading files"))]),
        2,
        config::NamingMode::Off,
    );
    let rows = radar.rows(2);
    let display = &rows[0].display;
    assert_eq!(display.status, Status::Running);
    assert_eq!(display.panes[0].kind(), Kind::Claude);
    assert_eq!(display.panes[0].msg(), "Reading files");
    assert!(radar.needs_fast_ticks(), "a native Running row spins");
}

#[test]
fn native_flip_to_pending_flashes_once() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "Tab #1", true)]);
    radar.panes_changed(native_update(vec![(7, native_working("codex", "x"))]), 1, config::NamingMode::Off);
    let mut pending = native_working("codex", "needs approval");
    pending.attention.status = Status::Pending;
    radar.panes_changed(native_update(vec![(7, pending.clone())]), 5, config::NamingMode::Off);
    assert!(radar.rows(5)[0].flash);
    assert!(radar.pending_wait_unsaturated(100));
    radar.timer(7);
    // A re-reported Pending is not a flip.
    radar.panes_changed(native_update(vec![(7, pending)]), 8, config::NamingMode::Off);
    assert!(!radar.rows(8)[0].flash);
}

fn native_done(app: &str, text: &str) -> crate::attention::NativePane {
    let mut pane = native_working(app, text);
    pane.attention.status = Status::Done;
    pane.attention.since_ms = 50_000;
    pane.records[0].status = Status::Done;
    pane
}

#[test]
fn a_seen_native_done_enters_the_ledger() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "api", true)]);
    radar.panes_changed(
        native_update(vec![(7, native_done("claude-code", "Turn finished"))]),
        1,
        config::NamingMode::Off,
    );
    assert!(radar.ledger().is_empty(), "a lit Done is still a card fact");
    // Zellij marks the focused pane seen: same records, attention Idle.
    let mut seen = native_done("claude-code", "");
    seen.attention.status = Status::Idle;
    seen.attention.source = crate::attention::Source::Other;
    // Zellij also remembers it: the same completion, so still one ledger row.
    seen.attention.remembered = Some(crate::attention::Remembered {
        status: Status::Done,
        text: "done: Turn finished".into(),
        since_ms: 50_000,
    });
    assert!(radar.attention_changed(7, seen.attention.clone(), 2));
    let lines = radar.ledger_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!((lines[0].tab_name.as_str(), lines[0].label.as_str(), lines[0].error), ("api", "Turn finished", false));
    assert_eq!(lines[0].at_epoch_s, 50);
    // Re-reporting the Idle state is not a second edge.
    radar.panes_changed(native_update(vec![(7, seen)]), 3, config::NamingMode::Off);
    assert_eq!(radar.ledger_lines().len(), 1);
}

#[test]
fn a_seen_long_native_done_files_one_ledger_row() {
    // Longer than the 60-char cap: the lit card's `msg` and the remembered
    // text must still yield the same label, or the two edges file twice.
    let text = format!("done: {}", "Refactored the session index and updated every caller ".repeat(2));
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "api", true)]);
    radar.panes_changed(native_update(vec![(7, native_done("claude-code", &text))]), 1, config::NamingMode::Off);
    let mut seen = native_done("claude-code", "");
    seen.attention.status = Status::Idle;
    seen.attention.source = crate::attention::Source::Other;
    seen.attention.remembered = Some(crate::attention::Remembered { status: Status::Done, text, since_ms: 50_000 });
    assert!(radar.attention_changed(7, seen.attention, 2));
    let lines = radar.ledger_lines();
    assert_eq!(lines.len(), 1, "{:?}", lines.iter().map(|l| &l.label).collect::<Vec<_>>());
    assert!(lines[0].label.starts_with("Refactored the session index"));
}

#[test]
fn a_native_error_pane_that_closes_enters_the_ledger() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "api", true)]);
    let mut failed = native_done("codex", "Crashed");
    failed.attention.status = Status::Error;
    radar.panes_changed(native_update(vec![(7, failed)]), 1, config::NamingMode::Off);
    radar.panes_changed(native_update(vec![]), 2, config::NamingMode::Off);
    let lines = radar.ledger_lines();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].error);
    assert_eq!(lines[0].tab_name, "api");
}

#[test]
fn a_done_seen_at_once_enters_the_ledger_from_remembered() {
    // A focused pane's Done is seen before any update shows it: Working →
    // Idle, with the Done kept in `remembered`.
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "api", true)]);
    radar.panes_changed(
        native_update(vec![(7, native_working("claude-code", "Refactor"))]),
        1,
        config::NamingMode::Off,
    );
    let mut seen = native_done("claude-code", "");
    seen.attention.status = Status::Idle;
    seen.attention.remembered = Some(crate::attention::Remembered {
        status: Status::Done,
        text: "done: Refactor complete".into(),
        since_ms: 60_000,
    });
    radar.panes_changed(native_update(vec![(7, seen.clone())]), 2, config::NamingMode::Off);
    radar.panes_changed(native_update(vec![(7, seen)]), 3, config::NamingMode::Off);
    let lines = radar.ledger_lines();
    assert_eq!(lines.len(), 1, "one completion, however many updates repeat it");
    assert_eq!((lines[0].label.as_str(), lines[0].at_epoch_s), ("Refactor complete", 60));
}

#[test]
fn attention_changed_reaches_a_rail_without_a_manifest() {
    // A hidden tab's instance gets no `PaneUpdate`, only `PaneAttentionChanged`.
    let mut radar = RadarState::default();
    let working = native_working("pi", "Thinking");
    assert!(radar.attention_changed(7, working.attention.clone(), 1));
    assert!(radar.needs_fast_ticks());
    assert!(!radar.attention_changed(7, working.attention, 2), "an identical attention changes nothing");
}

#[test]
fn a_hidden_rail_ledgers_on_the_manifest_its_reveal_brings() {
    // Hidden: no tabs, no manifest, only `PaneAttentionChanged`; on reveal
    // the manifest comes before the tabs.
    let mut radar = RadarState::default();
    let mut seen = native_done("claude-code", "");
    seen.attention.status = Status::Idle;
    seen.attention.remembered =
        Some(crate::attention::Remembered { status: Status::Done, text: "done: Ship it".into(), since_ms: 70_000 });
    radar.attention_changed(7, seen.attention.clone(), 1);
    assert!(radar.ledger().is_empty(), "no topology yet: no tab to file it under");
    // By the reveal Zellij has forgotten the remembered Done.
    seen.attention.remembered = None;
    radar.panes_changed(native_update(vec![(7, seen)]), 2, config::NamingMode::Off);
    assert!(radar.ledger().is_empty());
    radar.tabs_changed(vec![tab(10, 0, "api", false), tab(20, 1, "web", true)]);
    let lines = radar.ledger_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!((lines[0].tab_name.as_str(), lines[0].label.as_str()), ("api", "Ship it"));
}
