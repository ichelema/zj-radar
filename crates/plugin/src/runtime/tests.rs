use super::*;
use crate::attention::{PaneAttention, Source};
use crate::config::{Density, NamingMode};
use crate::radar_state::TabId;
use crate::rollup::TerminalPane;
use crate::status::{GlyphSet, Status};
use crate::test_fixtures::{pane, program_attention, set_status, tab};
use std::collections::{HashMap, HashSet};

/// The shared test config pins `spinner_fps` to 1: the cadence/timer-chain
/// suite is about the 1 Hz chain (`Cadence::Fast`), so sub-second frames are
/// opted into only by the frame tests (`frame_*`) that exercise them.
fn config() -> config::Config {
    config::Config { naming: NamingMode::Off, density: Density::Compact, spinner_fps: 1, ..config::Config::default() }
}

fn runtime_with_config(config: config::Config) -> PluginRuntime {
    PluginRuntime { config, ..Default::default() }
}

/// A granted runtime with the default test `config()` and NO session name —
/// the "permission resolved, nameless" shape many render/effect tests hand-roll
/// inline. `runtime_with_granted_permission` differs by also setting a session
/// name, which only the session-name tests need.
fn granted_runtime() -> PluginRuntime {
    PluginRuntime { permission: PermissionState::Resolved { granted: true }, config: config(), ..Default::default() }
}

/// Push the canonical Done ledger entry (tab 1 "work", "cargo test", pane 5) at
/// `at_epoch_s`. The cadence tests differ only in that timestamp — a fresh
/// `now` vs a saturated `0` — so a helper makes that the visible payload
/// instead of burying it in a repeated eight-line literal.
fn seed_done_ledger(rt: &mut PluginRuntime, at_epoch_s: u64) {
    rt.radar.ledger_mut().push(crate::ledger::LedgerEntry {
        at_epoch_s,
        outcome: crate::ledger::LedgerOutcome::Done,
        tab_id: TabId::new(1),
        tab_name: "work".into(),
        label: "cargo test".into(),
        pane_id: 5,
    });
}

impl PluginRuntime {
    /// Test shorthand: deliver a timer fire "now" (a real wall-clock
    /// capture) with the given elapsed. The sites that still pass an
    /// explicit epoch to `timer` directly are exactly the ones where the
    /// epoch value is load-bearing (virtual time, or a deliberately shifted
    /// clock) — they're meant to stand out.
    fn timer_now(&mut self, elapsed: f64) -> Outcome {
        self.timer(elapsed, crate::clock::now_epoch_s())
    }

    /// Test shorthand: deliver a live Fast fire (elapsed ~1s) — how every
    /// test that isn't about the stale-fire dedup drives the tick entry
    /// point. Dedup tests pass explicit elapsed values via `timer_now`.
    fn timer_fast(&mut self) -> Outcome {
        self.timer_now(Cadence::Fast.seconds())
    }

    /// Test shorthand: deliver a live Slow fire (elapsed ~60s).
    fn timer_slow(&mut self) -> Outcome {
        self.timer_now(Cadence::Slow.seconds())
    }

    /// Test shorthand: `pane_id`'s native attention moved to `status`.
    fn push(&mut self, pane_id: u32, status: Status) -> Outcome {
        self.attention_changed(pane_id, program_attention(status, "working"))
    }
}

#[test]
fn load_requests_permission() {
    let mut runtime = PluginRuntime::default();
    let outcome = runtime.load(config());

    assert_eq!(runtime.permission, PermissionState::Requesting);
    assert_eq!(
        outcome,
        Outcome {
            render: false,
            // SetTimeout keeps a paint trigger alive so the needs_permission
            // screen reaches the rail before the user grants (pre-grant
            // Zellij sends no state events to trigger a render).
            effects: vec![Effect::RequestPermission, Effect::SetTimeout(Cadence::Fast), Effect::SetSelectable(true),],
        }
    );
}

#[test]
fn denied_rail_with_running_work_stops_ticking() {
    // A denied rail receives none of the state events that clear domain work
    // (they need ReadApplicationState), so a `Running` row can never finish —
    // re-arming Fast for it would pin 1 Hz ticks and repaints forever behind
    // the static needs-permission face.
    let mut runtime = PluginRuntime::default();
    set_status(&mut runtime.radar, 9, Status::Running, "working", 7);
    let _ = runtime.load(config());
    let _ = runtime.permission_result(false);

    assert!(runtime.permission.denied());
    assert!(runtime.radar.needs_fast_ticks());
    let tick = runtime.timer_fast();
    assert!(
        !tick.effects.iter().any(|e| matches!(e, Effect::SetTimeout(_))),
        "denied rail must not tick for work it can never observe finishing"
    );
}

#[test]
fn rail_paints_needs_permission_while_request_in_flight() {
    // Pre-grant, Zellij delivers no state events (they need
    // ReadApplicationState), so without the load-armed timer the
    // needs_permission screen never gets a render trigger and the rail sits
    // blank — the bug this guards.
    let mut runtime = PluginRuntime::default();
    let _ = runtime.load(config());

    // The tick repaints while still awaiting the user's y/n, with no agent
    // work to report.
    let tick = runtime.timer_fast();
    assert!(tick.render, "rail repaints needs_permission while its request is in-flight",);
    assert!(!runtime.permission.granted());

    // Once the user answers, the heartbeat stops: a granted, idle rail must
    // not spin a timer forever.
    let _ = runtime.permission_result(true);
    let after = runtime.timer_fast();
    assert!(!after.render, "granted idle rail must not keep repainting");
    assert!(!after.effects.contains(&Effect::SetTimeout(Cadence::Fast)));
}

#[test]
fn permission_result_updates_selectability() {
    let mut runtime = PluginRuntime::default();
    runtime.record_permission_request_started();

    let outcome = runtime.permission_result(true);

    assert!(runtime.permission.granted());
    // The first sync declares the state explicitly — and with no manifest
    // yet, that state is selectable. The flip to passive is the manifest's
    // job (`rail_turns_passive_only_once_a_terminal_pane_shares_its_tab`).
    assert_eq!(outcome, Outcome { render: true, effects: vec![Effect::SetSelectable(true)] });
}

#[test]
fn panes_changed_emits_resolve_cwd_effect_for_new_panes() {
    let mut runtime = runtime_with_config(config::Config {
        naming: NamingMode::Managed,
        density: Density::Compact,
        ..config::Config::default()
    });
    runtime.tabs_changed(vec![tab(0, "Tab #1", true)]);
    runtime.own_plugin_tab_changed(Some(0));

    let mut focused = pane(7);
    focused.focused_in_tab = true;
    let outcome = runtime.panes_changed(PaneUpdate {
        tab_panes: HashMap::from([(0, vec![focused])]),
        live: HashSet::from([7]),
        theme: None,
        native: HashMap::new(),
    });

    assert!(outcome.effects.iter().any(|e| matches!(e, Effect::ResolveCwd { pane_ids } if pane_ids == &vec![7])));
}

#[test]
fn the_git_remote_names_the_tab_once_per_cwd() {
    let mut runtime = runtime_with_config(config::Config { naming: NamingMode::Managed, ..config::Config::default() });
    runtime.tabs_changed(vec![tab(0, "Tab #1", true)]);
    runtime.own_plugin_tab_changed(Some(0));
    runtime.radar.set_tab_panes_for_position(0, vec![pane(7)]);
    let lookup = Effect::ResolveRepo { cwd: "/work/wt-3".into() };
    assert!(runtime.cwd_changed(7, "/work/wt-3".into()).effects.contains(&lookup));
    // Zellij echoes the basename rename before the remote read is back.
    runtime.tabs_changed(vec![tab(0, "wt-3", true)]);
    assert!(!runtime.cwd_changed(7, "/work/wt-3".into()).effects.contains(&lookup), "one read per cwd");

    let named = runtime.repo_resolved("/work/wt-3".into(), Some("git@github.com:o/zj-radar.git\n"));
    assert_eq!(
        named.effects,
        vec![Effect::RenameTab { tab_id: TabId::new(1), name: "zj-radar".into() }],
        "the remote's name replaces the applied basename"
    );
    runtime.tabs_changed(vec![tab(0, "zj-radar", true)]);
    // No remote (a plain dir, or git failed): the cwd basename stands.
    let plain = runtime.repo_resolved("/work/wt-3".into(), None);
    assert_eq!(plain.effects, vec![Effect::RenameTab { tab_id: TabId::new(1), name: "wt-3".into() }]);
}

#[test]
fn cwd_change_renames_default_named_tab() {
    let mut runtime = runtime_with_config(config::Config {
        naming: NamingMode::Managed,
        density: Density::Compact,
        ..config::Config::default()
    });
    runtime.tabs_changed(vec![tab(0, "Tab #1", true)]);
    runtime.own_plugin_tab_changed(Some(0));
    runtime.radar.set_tab_panes_for_position(0, vec![pane(7)]);

    let rename = runtime.cwd_changed(7, "/work/myrepo".into());

    assert_eq!(
        rename.effects,
        vec![
            Effect::RenameTab { tab_id: TabId::new(1), name: "myrepo".into() },
            Effect::ResolveRepo { cwd: "/work/myrepo".into() },
        ]
    );
    assert_eq!(runtime.radar.applied_name(TabId::new(1)), Some("myrepo"));
}

#[test]
fn sidebar_instance_never_renames_a_foreign_tab() {
    let mut runtime = runtime_with_config(config::Config {
        naming: NamingMode::Managed,
        density: Density::Compact,
        ..config::Config::default()
    });
    runtime.tabs_changed(vec![tab(0, "Tab #1", true), tab(1, "Tab #2", false)]);
    runtime.own_plugin_tab_changed(Some(0));
    let topology = runtime.panes_changed(PaneUpdate {
        tab_panes: HashMap::from([(0, vec![pane(10)]), (1, vec![pane(11)])]),
        live: HashSet::from([10, 11]),
        theme: None,
        native: HashMap::new(),
    });
    // The cwd bootstrap is deliberately NOT ownership-scoped (the cwd also
    // stamps observed commands' repo); only renames are.
    assert!(topology
        .effects
        .iter()
        .any(|effect| { matches!(effect, Effect::ResolveCwd { pane_ids } if pane_ids == &vec![10, 11]) }));

    let foreign = runtime.cwd_changed(11, "/work/foreign".into());
    assert!(
        foreign
            .effects
            .iter()
            .all(|effect| { !matches!(effect, Effect::RenameTab { tab_id, .. } if *tab_id == TabId::new(2)) }),
        "tab 1's sidebar must not rename tab 2: {:?}",
        foreign.effects
    );

    let owned = runtime.cwd_changed(10, "/work/owned".into());
    assert!(owned.effects.iter().any(|effect| {
        matches!(effect, Effect::RenameTab { tab_id, name } if *tab_id == TabId::new(1) && name == "owned")
    }));
}

#[test]
fn config_pipe_accepts_json_scalars() {
    let mut runtime = PluginRuntime::default();

    let outcome = runtime.config_pipe(r#"{"header":false,"density":"compact","glyphs":"nerd","naming":"off"}"#);

    assert!(outcome.render);
    assert_eq!(runtime.config.naming, NamingMode::Off);
    assert_eq!(runtime.config.density, Density::Compact);
    assert_eq!(runtime.config.glyphs, GlyphSet::Nerd);
    assert!(!runtime.config.header);
}

#[test]
fn render_records_targets_and_mouse_click_returns_host_effect() {
    // 3 tracked panes → multi-pane mode (line-per-pane).
    // Line 2 = tab header, line 3 = pane 20, line 4 = pane 21, line 5 = pane 22.
    let mut runtime = granted_runtime();
    runtime.tabs_changed(vec![tab(0, "team", false), tab(1, "plain", false)]);
    runtime.radar.set_tab_panes_for_position(0, vec![pane(20), pane(21), pane(22)]);
    set_status(&mut runtime.radar, 20, Status::Pending, "working", 1);
    set_status(&mut runtime.radar, 21, Status::Running, "working", 1);
    set_status(&mut runtime.radar, 22, Status::Running, "working", 1);

    let ansi = runtime.render(100, 80);
    assert!(ansi.contains("team"));

    let tab_click = runtime.mouse_click(2, 0);
    let pane20_click = runtime.mouse_click(3, 0);
    let pane21_click = runtime.mouse_click(4, 0);

    assert_eq!(tab_click.effects, vec![Effect::SwitchTab { position: 0 }]);
    assert_eq!(pane20_click.effects, vec![Effect::ShowPane { pane_id: 20 }]);
    assert_eq!(pane21_click.effects, vec![Effect::ShowPane { pane_id: 21 }]);
}

#[test]
fn single_pane_detail_line_click_shows_the_pane() {
    // One tab, one tracked pane with a msg → single-pane path: header
    // (line 2) + detail line (line 3). The detail line describes that one
    // pane, so it must click-target the pane (ShowPane), not the tab
    // (SwitchTab) — mirroring the multi-pane tree rows.
    let mut runtime = granted_runtime();
    runtime.tabs_changed(vec![tab(0, "team", false)]);
    runtime.radar.set_tab_panes_for_position(0, vec![pane(30)]);
    set_status(&mut runtime.radar, 30, Status::Running, "working", 1);

    let ansi = runtime.render(100, 80);
    assert!(ansi.contains("team"));

    let header_click = runtime.mouse_click(2, 0);
    let detail_click = runtime.mouse_click(3, 0);

    assert_eq!(header_click.effects, vec![Effect::SwitchTab { position: 0 }]);
    assert_eq!(detail_click.effects, vec![Effect::ShowPane { pane_id: 30 }]);
}

#[test]
fn mouse_click_is_ignored_until_permission_granted() {
    let mut runtime = runtime_with_config(config());
    runtime.tabs_changed(vec![tab(0, "team", false)]);
    runtime.render(100, 80);

    assert_eq!(runtime.mouse_click(2, 0), Outcome::default());
}

#[test]
fn no_tabs_with_history_renders_ledger_not_scanning() {
    // Zero tracked tabs alone isn't the onboarding trigger — a
    // session with completion history still has something to show. Seed a
    // Done pane, let it recede into the ledger as its tab closes, then
    // close every tab and confirm `render` picks `render_rail` (header +
    // ledger + footer) over the minimal scanning face.
    let mut runtime = PluginRuntime {
        permission: PermissionState::Resolved { granted: true },
        // `jump_hint` opted in: this test also pins the config → render
        // plumbing for the footer's alt-[n] line (hidden by default —
        // only run-owned configs, which bind the chord, may claim it).
        config: config::Config { jump_hint: config::JumpHint::AltN, ..config() },
        ..Default::default()
    };
    runtime.tabs_changed(vec![tab(0, "web", true)]);
    runtime.radar.set_tab_panes_for_position(0, vec![pane(5)]);
    set_status(&mut runtime.radar, 5, Status::Done, "working", 1);

    // The pane closes with a still-lit Done: the manifest dropping it hands
    // it to the ledger (spec §4.2).
    runtime.panes_changed(PaneUpdate {
        tab_panes: HashMap::new(),
        live: HashSet::new(),
        theme: None,
        native: HashMap::new(),
    });
    assert!(!runtime.radar.ledger().is_empty(), "setup: ledger must be seeded");

    // The tab itself closes too — zero tabs, but history remains.
    runtime.tabs_changed(vec![]);

    let ansi = runtime.render(24, 40);
    assert!(ansi.contains("earlier"), "ledger renders even with no tabs: {ansi:?}");
    assert!(ansi.contains("alt-[n] jump"), "footer still pins to the floor: {ansi:?}");
    assert!(!ansi.to_lowercase().contains("scanning"), "must not fall back to the onboarding scanning face: {ansi:?}");
}

#[test]
fn ledger_false_hides_history_live_without_dropping_it() {
    let mut rt = granted_runtime();
    rt.tabs_changed(vec![tab(0, "web", true)]);
    seed_done_ledger(&mut rt, crate::clock::now_epoch_s());
    assert!(rt.render(24, 40).contains("earlier"), "setup: the ledger shows by default");

    let off = rt.config_pipe(r#"{"ledger":"false"}"#);
    assert!(off.render, "a ledger toggle repaints");
    let ansi = rt.render(24, 40);
    assert!(!ansi.contains("earlier"), "ledger false draws no section: {ansi:?}");
    assert!(!rt.radar.ledger().is_empty(), "off only hides: the ring is still recorded");

    rt.config_pipe(r#"{"ledger":"true"}"#);
    assert!(rt.render(24, 40).contains("earlier"), "flipping back on shows the history");

    // Nothing left to draw with the ledger hidden and no tabs: onboarding, not an empty rail.
    rt.config_pipe(r#"{"ledger":"false"}"#);
    rt.tabs_changed(vec![]);
    assert!(rt.render(24, 40).to_lowercase().contains("scanning"), "hidden history is not content");
}

#[test]
fn command_attention_next_emits_switch_tab() {
    let mut runtime = granted_runtime();
    // tab 0 active (running), tab 1 pending → attention.
    runtime.tabs_changed(vec![tab(0, "a", true), tab(1, "b", false)]);
    runtime.radar.set_tab_panes_for_position(0, vec![pane(10)]);
    runtime.radar.set_tab_panes_for_position(1, vec![pane(11)]);
    set_status(&mut runtime.radar, 10, Status::Running, "working", 1);
    set_status(&mut runtime.radar, 11, Status::Pending, "working", 1);

    let out = runtime.control(Verb::AttentionNext);
    assert_eq!(out.effects, vec![Effect::SwitchTab { position: 1 }]);
}

/// #89: one rail instance per tab, all four tabs needing attention. Zellij
/// sends `TabUpdate` to the active tab's plugins only, so as the user walks
/// tab 0 → 3 each rail's view freezes on the moment it went dark, with its
/// own tab marked active; `Visible(false)` marks it hidden. Every rail then
/// gets each `cmd.v1` press — only the visible one may answer it.
#[test]
fn attention_verbs_are_answered_by_the_visible_rail_only() {
    const TABS: usize = 4;
    let manifest = |active: usize| (0..TABS).map(|p| tab(p, &format!("t{p}"), p == active)).collect::<Vec<_>>();
    let mut rails: Vec<PluginRuntime> = (0..TABS).map(|_| granted_runtime()).collect();
    for (own, rail) in rails.iter_mut().enumerate() {
        for p in 0..TABS {
            rail.radar.set_tab_panes_for_position(p, vec![pane(10 + p as u32)]);
            set_status(&mut rail.radar, 10 + p as u32, Status::Pending, "working", 1);
        }
        // Visit every tab in order: a rail sees manifests only while its
        // own tab is active, then goes dark when the user moves on.
        rail.tabs_changed(manifest(own));
        rail.visibility_changed(false);
    }
    // Back on tab 0: its rail is revealed and gets the fresh manifest; the
    // others stay dark with their frozen views.
    rails[0].visibility_changed(true);
    rails[0].tabs_changed(manifest(0));

    for (verb, want) in [(Verb::AttentionNext, 1), (Verb::AttentionPrev, 3)] {
        let votes: Vec<(usize, Vec<Effect>)> = rails
            .iter_mut()
            .enumerate()
            .map(|(i, rail)| (i, rail.control(verb).effects))
            .filter(|(_, effects)| !effects.is_empty())
            .collect();
        assert_eq!(
            votes,
            vec![(0, vec![Effect::SwitchTab { position: want }])],
            "{verb:?}: exactly one rail (the visible one) may switch tabs"
        );
    }
}

#[test]
fn session_verbs_are_inert_on_a_hidden_rail() {
    let mut rt = runtime_with_granted_permission();
    rt.global_attention_changed(vec![fresh(r#"{"session_name":"alpha","running":0,"attention":1}"#)]);
    rt.visibility_changed(false);
    assert_eq!(rt.control_pipe("session-next"), Outcome::default());
    assert_eq!(rt.control_pipe("session-prev"), Outcome::default());
}

#[test]
fn command_is_inert_without_permission() {
    let mut runtime = PluginRuntime { config: config(), ..Default::default() };
    runtime.tabs_changed(vec![tab(0, "a", true), tab(1, "b", false)]);
    runtime.radar.set_tab_panes_for_position(1, vec![pane(11)]);
    set_status(&mut runtime.radar, 11, Status::Pending, "working", 1);

    assert_eq!(runtime.control(Verb::AttentionNext), Outcome::default());
}

#[test]
fn command_no_op_when_no_attention() {
    let mut runtime = granted_runtime();
    runtime.tabs_changed(vec![tab(0, "a", true)]);
    assert_eq!(runtime.control(Verb::AttentionNext), Outcome::default());
}

#[test]
fn control_pipe_unknown_verb_is_no_op() {
    let mut runtime = granted_runtime();
    assert_eq!(runtime.control_pipe("attention-top"), Outcome::default());
    assert_eq!(runtime.control_pipe(""), Outcome::default());
}

// ── Cross-session badge + cycling ───────────────────────────────────────

/// A granted runtime whose own session name is already known (as it would be
/// in production once Zellij's first `ModeUpdate` lands) — needed for the
/// own badge row in `project` to ever appear.
fn runtime_with_granted_permission() -> PluginRuntime {
    let mut rt = granted_runtime();
    rt.session_name_changed(Some("work".into()));
    rt
}

/// One tab (position 0), one tracked pane (id 7) — the minimal topology a
/// status edge needs to land on a row the own counts can see.
fn drive_tabs_and_panes(rt: &mut PluginRuntime) {
    rt.tabs_changed(vec![tab(0, "a", true)]);
    rt.radar.set_tab_panes_for_position(0, vec![pane(7)]);
}

/// A peer from a JSON literal, in the `SessionSummary` shape `GlobalAttentionUpdate`
/// peers reach the runtime in (`attention::PeerSession::summary`).
fn fresh(json: &str) -> SessionSummary {
    serde_json::from_str(json).expect("test peer JSON must parse")
}

/// Shared setup for the right-click tests: one tab + one "alpha" peer, then
/// a real render so `last_rendered` carries the badge's click targets. The
/// resulting line indices follow `clicking_a_session_line_emits_switch_session`'s
/// bookkeeping — 0/1 header, 2 own "work" line (click-inert), 3 peer "alpha"
/// line, 4 the badge's blank separator, 5 the tab header.
fn render_with_badge(rt: &mut PluginRuntime, peer: SessionSummary) {
    rt.tabs_changed(vec![tab(0, "team", false)]);
    rt.global_attention_changed(vec![peer]);
    let ansi = rt.render(100, 80);
    assert!(ansi.contains("alpha"), "setup: the peer's badge line must actually render");
}

#[test]
fn own_counts_live_status_panes_not_tab_rollups_or_unseated_payloads() {
    let mut rt = runtime_with_granted_permission();
    rt.tabs_changed(vec![tab(0, "pair", true)]);
    rt.radar.set_tab_panes_for_position(0, vec![pane(10), pane(11)]);
    rt.push(10, Status::from_wire("running"));
    rt.push(11, Status::from_wire("running"));
    // Status intake legitimately accepts a payload before topology arrives;
    // the counts must not include that stale/unseated id.
    rt.push(99, Status::from_wire("pending"));

    let own = rt.own_counts();
    assert_eq!(own.running, 2, "two agent panes in one tab count as two");
    assert_eq!(own.attention, 0);
    assert_eq!(own.attention_tab_position, None);
}

#[test]
fn own_counts_exclude_command_attention_and_use_the_first_status_tab() {
    let mut rt = runtime_with_granted_permission();
    rt.tabs_changed(vec![tab(0, "command", false), tab(3, "agent", true)]);
    rt.radar.set_tab_panes_for_position(0, vec![pane(10)]);
    rt.radar.set_tab_panes_for_position(3, vec![pane(20)]);

    // A command-origin Error is urgent in the rail but intentionally absent
    // from the cross-session agent tally.
    let failed = PaneAttention {
        status: Status::Error,
        source: Source::Command,
        text: "cargo test".into(),
        exit_code: Some(1),
        since_ms: 100_000,
        ..PaneAttention::default()
    };
    rt.attention_changed(10, failed);
    rt.push(20, Status::from_wire("pending"));

    let own = rt.own_counts();
    assert_eq!(own.running, 0);
    assert_eq!(own.attention, 1, "only the status-origin Pending counts");
    assert_eq!(own.attention_tab_position, Some(3));
}

#[test]
fn session_cycle_commits_via_switch_session_effect_on_idle_tick() {
    let mut rt = runtime_with_granted_permission(); // own session name is "work"
    rt.global_attention_changed(vec![fresh(
        r#"{"session_name":"alpha","running":0,"attention":1,"attention_tab_position":1}"#,
    )]);

    let out = rt.control_pipe("session-next");
    assert!(out.render, "selection highlight renders");

    // The Fast fire covering the tap itself must not commit (the
    // taps-since-last-fire flag resets the deadline instead) — only the
    // NEXT, fully quiet fire may.
    let covering = rt.timer_fast();
    assert!(
        !covering.effects.iter().any(|e| matches!(e, Effect::SwitchSession { .. })),
        "the fire covering the tap must not commit, got {:?}",
        covering.effects
    );
    let tick = rt.timer_fast(); // the next, quiet Fast tick
    assert!(
        tick.effects.iter().any(|e| matches!(
            e, Effect::SwitchSession { name, tab_position: Some(1), pane_id: None } if name == "alpha"
        )),
        "idle tick commits the pending selection, got {:?}",
        tick.effects
    );
}

#[test]
fn session_cycle_is_inert_without_permission() {
    let mut rt = PluginRuntime::default();
    rt.session_name_changed(Some("work".into()));
    rt.global_attention_changed(vec![fresh(r#"{"session_name":"alpha","running":0,"attention":0}"#)]);
    assert_eq!(rt.control_pipe("session-next"), Outcome::default());
}

#[test]
fn session_cycle_arms_fast_cadence_for_the_idle_commit() {
    let mut rt = runtime_with_granted_permission();
    rt.global_attention_changed(vec![fresh(r#"{"session_name":"alpha","running":0,"attention":0}"#)]);
    let out = rt.control_pipe("session-next");
    assert!(
        out.effects.contains(&Effect::SetTimeout(Cadence::Fast)),
        "a pending cycle selection must arm Fast so the idle-commit fires promptly, got {:?}",
        out.effects
    );
}

#[test]
fn empty_local_session_shows_the_peer_tree_only_with_session_tree_on() {
    let peer =
        r#"{"session_name":"peer","running":0,"attention":0,"tabs":[{"position":0,"name":"review","panes":[]}] }"#;

    // Off (default): no local tabs and no ledger is still onboarding — the
    // compact badge alone never justified a rail.
    let mut rt = runtime_with_granted_permission();
    rt.global_attention_changed(vec![fresh(peer)]);
    assert!(!rt.render(20, 80).contains("review"), "off: no peer tree");

    // On: the peer tree alone is navigation worth a rail.
    let mut rt = runtime_with_granted_permission();
    rt.config.session_tree = true;
    rt.global_attention_changed(vec![fresh(peer)]);
    let ansi = rt.render(20, 80);
    assert!(ansi.contains("peer"), "badge={:?} ansi={ansi:?}", rt.sessions.badge());
    assert!(ansi.contains("review"));
}

#[test]
fn a_global_attention_peer_click_focuses_its_waiting_pane() {
    // Same layout as `session_tree_moves_peer_lines_below_the_local_cards`,
    // with the peer built from a native `PeerSession`.
    let mut rt = runtime_with_granted_permission();
    rt.config.session_tree = true;
    rt.tabs_changed(vec![tab(0, "team", false)]);
    let peer = crate::attention::PeerSession {
        name: "alpha".into(),
        needs_you: 1,
        items: vec![crate::attention::PeerItem {
            pane_id: 7,
            tab_id: 3,
            tab_name: "api".into(),
            attention: crate::attention::PaneAttention {
                status: Status::Pending,
                source: crate::attention::Source::Program,
                app: Some("claude-code".into()),
                ..Default::default()
            },
        }],
        ..Default::default()
    };
    assert!(rt.global_attention_changed(vec![peer.summary()]).render);
    rt.render(100, 80);
    assert_eq!(
        rt.mouse_click(4, 0).effects,
        vec![Effect::SwitchSession { name: "alpha".into(), tab_position: None, pane_id: Some(7) }]
    );
}

#[test]
fn session_tree_moves_peer_lines_below_the_local_cards() {
    // Same bookkeeping as `clicking_a_session_line_emits_switch_session`, tree
    // on: line 0 = title, 1 = rule, 2 = own "work" heading, 3 = local tab,
    // 4 = peer "alpha" heading (clickable).
    let mut rt = runtime_with_granted_permission();
    rt.config.session_tree = true;
    rt.tabs_changed(vec![tab(0, "team", false)]);
    rt.global_attention_changed(vec![fresh(
        r#"{"session_name":"alpha","running":0,"attention":1,"attention_tab_position":2}"#,
    )]);
    rt.render(100, 80);

    assert_eq!(rt.mouse_click(2, 0), Outcome::default(), "own heading is click-inert");
    let peer_click = rt.mouse_click(4, 0);
    assert_eq!(
        peer_click.effects,
        vec![Effect::SwitchSession { name: "alpha".into(), tab_position: Some(2), pane_id: None }]
    );
}

#[test]
fn clicking_a_session_line_emits_switch_session() {
    // Two live sessions (own "work" + peer "alpha") put two badge lines
    // between the header and the first tab card (`render::render_session_badge`,
    // wired into the body in Task 7). Mirrors
    // `render_records_targets_and_mouse_click_returns_host_effect`'s line-index
    // bookkeeping: Compact density + header:true is a 2-line header (title,
    // rule — "line 2 = tab header" there), so with the badge inserted here:
    // line 0 = title, 1 = rule, 2 = own "work" badge line (click-inert, no
    // cross-session target), 3 = peer "alpha" badge line (clickable).
    let mut rt = runtime_with_granted_permission(); // own session name "work"
    rt.tabs_changed(vec![tab(0, "team", false)]);
    rt.global_attention_changed(vec![fresh(
        r#"{"session_name":"alpha","running":0,"attention":1,"attention_tab_position":2}"#,
    )]);

    let ansi = rt.render(100, 80);
    assert!(ansi.contains("alpha"), "setup: the peer's badge line must actually render");

    let own_click = rt.mouse_click(2, 0);
    assert_eq!(own_click, Outcome::default(), "the own-session badge line has no click target, got {:?}", own_click);

    let peer_click = rt.mouse_click(3, 0);
    assert_eq!(
        peer_click.effects,
        vec![Effect::SwitchSession { name: "alpha".into(), tab_position: Some(2), pane_id: None }]
    );
}

#[test]
fn raced_away_acknowledge_glyph_consumes_the_click_instead_of_showing_the_pane() {
    // A hotspot whose action raced away consumes the click: pane 10's
    // Pending completes on its own (another instance's ack echo, or the
    // agent's next broadcast) with no re-render in between, so the cached
    // rail still shows the acknowledge glyph with nothing Pending behind it.
    let mut rt = runtime_with_pending_panes();
    let (start_col, _) =
        rt.last_rendered.hotspot_at_line(3).expect("pending pane 10 must carry its acknowledge hotspot");

    rt.push(10, Status::from_wire("done"));

    let out = rt.mouse_click(3, start_col);
    assert_eq!(out, Outcome::default(), "a raced-away acknowledge must consume the click, not ShowPane");
}

#[test]
fn right_click_on_peer_session_line_is_inert() {
    let mut rt = runtime_with_granted_permission();
    render_with_badge(&mut rt, fresh(r#"{"session_name":"alpha","running":0,"attention":1}"#));

    let out = rt.mouse_right_click(3, 0);

    assert_eq!(out, Outcome::default());
    assert!(rt.sessions.badge().iter().any(|b| b.name == "alpha"), "fresh peer must remain on the badge");
}

#[test]
fn right_click_on_own_session_line_is_inert() {
    let mut rt = runtime_with_granted_permission();
    render_with_badge(&mut rt, fresh(r#"{"session_name":"alpha","running":0,"attention":1}"#));

    assert_eq!(rt.mouse_right_click(2, 0), Outcome::default());
}

#[test]
fn right_click_on_tab_line_is_inert() {
    let mut rt = runtime_with_granted_permission();
    render_with_badge(&mut rt, fresh(r#"{"session_name":"alpha","running":0,"attention":1}"#));

    assert_eq!(rt.mouse_right_click(5, 0), Outcome::default());
}

#[test]
fn right_click_on_empty_line_is_inert() {
    let mut rt = runtime_with_granted_permission();
    render_with_badge(&mut rt, fresh(r#"{"session_name":"alpha","running":0,"attention":1}"#));

    assert_eq!(rt.mouse_right_click(99, 0), Outcome::default());
}

#[test]
fn right_click_without_permission_is_inert_on_a_session_line() {
    let mut rt = runtime_with_granted_permission();
    render_with_badge(&mut rt, fresh(r#"{"session_name":"alpha","running":0,"attention":1}"#));
    rt.permission = PermissionState::default();

    assert_eq!(rt.mouse_right_click(3, 0), Outcome::default());
    assert!(rt.sessions.badge().iter().any(|b| b.name == "alpha"), "ungranted right-click must not mutate the badge");
}

// ── Right-click: pending-pane acknowledge (issue #5) ───────────────────────

/// Two tabs for the pending-pane acknowledge tests: tab 0 has panes 10
/// (Pending), 11 (Running), 12 (Pending); tab 1 has one pane, 20 (Pending).
/// Rendered once so `last_rendered` carries the click-target map. Line
/// layout, pinned by inspection the same way every other click-mapping test
/// in this file hard-codes its lines (Compact density, header:true):
///   0/1 global header
///   2   tab0 header line   → (tab_position: 0, pane_id: None)
///   3   tab0 pane 10 line  → (0, Some(10)) — Pending
///   4   tab0 pane 11 line  → (0, Some(11)) — Running
///   5   tab0 pane 12 line  → (0, Some(12)) — Pending
///   6   tab1 header line   → (1, None)
///   7   tab1 pane 20 line  → (1, Some(20)) — Pending
fn runtime_with_pending_panes() -> PluginRuntime {
    let mut rt = runtime_with_granted_permission(); // own session "work"

    rt.tabs_changed(vec![tab(0, "a", false), tab(1, "b", false)]);
    rt.radar.set_tab_panes_for_position(0, vec![pane(10), pane(11), pane(12)]);
    rt.radar.set_tab_panes_for_position(1, vec![pane(20)]);
    rt.push(10, Status::from_wire("pending"));
    rt.push(11, Status::from_wire("running"));
    rt.push(12, Status::from_wire("pending"));
    rt.push(20, Status::from_wire("pending"));
    let _ = rt.render(100, 80);
    rt
}

#[test]
fn right_click_on_pending_pane_marks_it_seen_and_leaves_local_state_untouched() {
    let mut rt = runtime_with_pending_panes();

    let out = rt.mouse_right_click(3, 0); // tab0 pane 10, Pending
    assert!(!out.render, "the click itself must not mutate local state — see Effect::MarkSeen's doc");
    assert_eq!(out.effects, vec![Effect::MarkSeen { session: "work".into(), pane_id: 10 }]);

    // Pending survives the click alone: the seen mark is Zellij's, and it
    // comes back as the next manifest.
    assert_eq!(rt.radar.observation(10).unwrap().status, Status::Pending, "no local mutation from the click alone");
}

#[test]
fn acknowledge_waits_for_the_session_name() {
    let mut rt = runtime_with_pending_panes();
    rt.own_session_name.clear();

    assert!(rt.mouse_right_click(3, 0).effects.is_empty(), "no session to address yet");
}

#[test]
fn left_click_requires_the_pending_glyph_cells_but_right_click_keeps_row_wide_parity() {
    let mut rt = runtime_with_pending_panes();
    let (start_col, _) =
        rt.last_rendered.hotspot_at_line(2).expect("pending tab header must carry its acknowledge hotspot");

    // The cell before the glyph keeps ordinary navigation semantics; only the
    // glyph itself acknowledges. `start_col` is display-cell zero based.
    assert_eq!(
        rt.mouse_click(2, start_col.saturating_sub(1)),
        Outcome::with_effects(false, vec![Effect::SwitchTab { position: 0 }]),
    );
    let left = rt.mouse_click(2, start_col);
    assert_eq!(left.effects.len(), 2, "tab hotspot acknowledges both pending panes");

    // Right-click remains deliberately row-wide until zellij#5350 is fixed.
    let right = rt.mouse_right_click(2, 0);
    assert_eq!(right.effects.len(), 2, "right-click parity stays row-wide");
}

#[test]
fn right_click_on_tab_row_marks_every_pending_pane_in_that_tab_only() {
    let mut rt = runtime_with_pending_panes();

    let out = rt.mouse_right_click(2, 0); // tab0 header line, pane_id: None
    assert!(!out.render);
    let acked: Vec<u32> = out
        .effects
        .iter()
        .map(|e| {
            let Effect::MarkSeen { session, pane_id } = e else {
                panic!("expected MarkSeen, got {e:?}");
            };
            assert_eq!(session, "work");
            *pane_id
        })
        .collect();
    assert_eq!(acked.len(), 2, "tab0 has exactly 2 pending panes (10 and 12), got {acked:?}");
    assert!(acked.contains(&10) && acked.contains(&12));
    assert!(!acked.contains(&11), "pane 11 is Running, not Pending — untouched");
    assert!(!acked.contains(&20), "tab1's pending pane must be untouched by a tab0 click");
}

#[test]
fn right_click_on_a_running_pane_row_is_a_strict_no_op() {
    let mut rt = runtime_with_pending_panes();
    assert_eq!(rt.mouse_right_click(4, 0), Outcome::default(), "pane 11 is Running, not Pending");
}

#[test]
fn right_click_on_a_tab_row_with_no_tracked_panes_is_a_strict_no_op() {
    let mut rt = granted_runtime();
    rt.tabs_changed(vec![tab(0, "a", false)]);
    let _ = rt.render(100, 80);
    assert_eq!(rt.mouse_right_click(2, 0), Outcome::default(), "no panes at all in this tab");
}

#[test]
fn own_badge_row_updates_live_as_running_and_attention_move() {
    // `Sessions::set_own` was once dead code: nothing called it, so the own
    // row of the cross-session badge never reflected the running/
    // attention counts actually moving underneath it — it would render
    // whatever it started at (0/0) forever. The fix has
    // `project` call it every pass once the name is known; this pins
    // that the own row's rendered counts actually track a later status edge,
    // not just its state at the moment the name became known.
    let mut rt = runtime_with_granted_permission(); // own session name "work"
                                                    // A second session must exist for the badge to render at all
                                                    // (`render_session_badge` renders zero lines for `entries.len() <= 1`).
    rt.global_attention_changed(vec![fresh(r#"{"session_name":"alpha","running":0,"attention":0}"#)]);
    drive_tabs_and_panes(&mut rt); // tab 0, pane 7

    let before = rt.render(100, 80);
    assert!(!before.contains("work 1"), "setup: own row must start at zero running, got {before:?}");

    rt.push(7, Status::from_wire("running"));
    let after = rt.render(100, 80);
    let running_glyph = Status::Running.glyph_for(GlyphSet::Plain);
    assert!(
        after.contains(&format!("work 1{running_glyph}")),
        "own badge row must show the fresh running count, got {after:?}"
    );
}

/// Helper: two tabs; pane 5 focused in active tab 0, pane 7 in background
/// tab 1. Both panes report Running attention.
fn two_tab_runtime_with_running_panes() -> PluginRuntime {
    let mut rt = runtime_with_config(config());
    rt.tabs_changed(vec![tab(0, "active", true), tab(1, "bg", false)]);
    // Place panes in their tabs.
    rt.radar.set_tab_panes_for_position(0, vec![TerminalPane { id: 5, focused_in_tab: true, ..Default::default() }]);
    rt.radar.set_tab_panes_for_position(1, vec![pane(7)]);
    rt.push(5, Status::Running);
    rt.push(7, Status::Running);
    // Arm the timer chain the way a first tick would.
    rt.timer_fast();
    rt
}

#[test]
fn render_gate_vetoes_repaints_whose_drawn_content_is_unchanged() {
    // Zellij re-reports the pane manifest on plenty of non-changes and
    // delivers it to every tab's instance. Once a render has stamped what is
    // actually on screen, an event that re-derives identical content must not
    // request another repaint — while a tick-driven frame (`force_render`)
    // and a genuine content change still do.
    let mut rt = runtime_with_config(config());
    rt.permission = PermissionState::Resolved { granted: true };
    rt.tabs_changed(vec![tab(0, "work", true)]);

    let mut live = HashSet::new();
    live.insert(7);
    let mut tab_panes = HashMap::new();
    tab_panes.insert(0, vec![TerminalPane { focused_in_tab: true, ..pane(7) }]);
    let manifest =
        || PaneUpdate { tab_panes: tab_panes.clone(), live: live.clone(), theme: None, native: HashMap::new() };

    let first = rt.panes_changed(manifest());
    assert!(first.render, "first manifest is un-stamped content: renders");
    let _ = rt.render(30, 40); // Zellij's render call — stamps last_render_key

    let repeat = rt.panes_changed(manifest());
    assert!(!repeat.render, "identical manifest re-report draws nothing new");

    // Tick-driven frames bypass the gate (spinner/ages animate off now_tick).
    set_status(&mut rt.radar, 7, Status::Running, "working", rt.tick);
    let _ = rt.render(30, 40);
    let tick = rt.timer_fast();
    assert!(tick.render, "animation tick renders even with identical rows");

    // A genuine content change still passes the gate.
    let _ = rt.render(30, 40);
    let edge = rt.push(7, Status::Pending);
    assert!(edge.render, "a real status edge repaints");
}

#[test]
fn noop_report_skips_own_counts_work() {
    // An identical re-report advances nothing: no own-counts re-derive, no
    // render. The pane's own counts can't have moved — the radar
    // generation didn't.
    let mut rt = runtime_with_config(config());
    rt.tabs_changed(vec![tab(0, "work", true)]);
    rt.radar.set_tab_panes_for_position(0, vec![pane(7)]);
    let _ = rt.session_name_changed(Some("main".into()));

    rt.push(7, Status::Running);

    let repeat = rt.push(7, Status::Running);
    assert!(!repeat.render);
    assert!(
        repeat.effects.iter().all(|e| matches!(e, Effect::SetTimeout(_))),
        "an identical re-report may at most re-arm the timer, got {:?}",
        repeat.effects
    );
}

#[test]
fn project_emits_effects_in_canonical_order() {
    // Sole home of the order contract: renames → cwd → SetTimeout
    // — exercising all three effect kinds in one change.
    let mut rt = two_tab_runtime_with_running_panes();
    // `TimerChain::arm` self-guards on the armed cadence; the setup helper's
    // timer tick already armed it, so force the disarmed state to let
    // `project`'s unconditional arm call actually produce a `SetTimeout`.
    rt.timer_chain.disarm_for_test();

    let change = RadarChange {
        render: true,
        renames: vec![TabRename { id: TabId::new(1), name: "renamed".into() }],
        cwd_bootstrap: vec![7],
        ..RadarChange::default()
    };
    let outcome = rt.project(vec![], change, 0);

    let kind = |e: &Effect| match e {
        Effect::RenameTab { .. } => 0,
        Effect::ResolveCwd { .. } => 1,
        Effect::SetTimeout(_) => 2,
        other => panic!("unexpected effect in canonical-order test: {other:?}"),
    };
    let kinds: Vec<i32> = outcome.effects.iter().map(kind).collect();
    let mut sorted = kinds.clone();
    sorted.sort_unstable();
    assert_eq!(
        kinds, sorted,
        "effects must appear in canonical order (renames < cwd < timer); got {:?}",
        outcome.effects
    );
    // All three kinds must actually be present, otherwise the ordering
    // assertion above is vacuous.
    for expected in 0..=2 {
        assert!(kinds.contains(&expected), "expected effect kind {expected} to be present; got {:?}", outcome.effects);
    }
}

#[test]
fn cwd_changed_never_bootstraps_cwd() {
    // Guards the bound documented on `Effect::ResolveCwd`: `cwd_changed`'s
    // `RadarChange` must never carry a `cwd_bootstrap`, or the
    // `ResolveCwd` → `cwd_changed` re-entry could recurse.
    let mut runtime = runtime_with_config(config::Config {
        naming: NamingMode::Managed,
        density: Density::Compact,
        ..config::Config::default()
    });
    runtime.tabs_changed(vec![tab(0, "Tab #1", true)]);
    runtime.radar.set_tab_panes_for_position(0, vec![pane(7)]);

    let change = runtime.radar.cwd_changed(7, "/work/myrepo".into(), NamingMode::Managed);

    assert!(change.cwd_bootstrap.is_empty());
}

#[test]
fn backgrounded_done_lets_the_timer_quiesce() {
    // The headline of the timer-arming rule: a finished agent in a background
    // tab must NOT keep the 1 Hz timer alive forever.
    let mut rt = runtime_with_config(config());
    rt.push(7, Status::Done);

    // The timer quiesces within a bounded number of ticks — a backgrounded
    // Done doesn't pin it awake.
    let mut extra = 0;
    while rt.timer_chain.armed().is_some() {
        rt.timer_fast();
        extra += 1;
        assert!(extra < 4, "timer must quiesce for a backgrounded Done, not tick forever");
    }
    assert!(!rt.timer_should_continue(), "quiesced: nothing left to tick for");

    // The Done stays lit.
    assert_eq!(rt.radar.observation(7).unwrap().status, Status::Done);
}

#[test]
fn flash_keeps_fast_timer_until_cleared() {
    // A flip-to-pending edge arms a two-tick ping flash — even with
    // nothing else running, the timer must keep ticking Fast until the flash
    // itself clears. Mirrors
    // `backgrounded_done_lets_the_timer_quiesce`, which
    // quiesces right away; the flash pins the timer open for its own window.
    let mut rt = runtime_with_config(config());
    rt.tabs_changed(vec![tab(0, "work", true)]);
    rt.radar.set_tab_panes_for_position(0, vec![pane(7)]);

    // Waiting since "now", so the `· Nm` wait tag is still counting.
    let waiting = PaneAttention {
        since_ms: crate::clock::now_epoch_s() * 1000,
        ..program_attention(Status::Pending, "approve?")
    };
    let edge = rt.attention_changed(7, waiting);
    assert!(edge.effects.contains(&Effect::SetTimeout(Cadence::Fast)), "the flip-to-pending edge arms the timer");

    // The flash (armed through tick 2) is still active at tick 1, so the
    // timer must not disarm yet.
    rt.timer_fast();
    assert_eq!(rt.tick, 1);
    assert!(rt.timer_chain.armed().is_some(), "flash still active at tick 1 — timer must stay armed");

    // Tick 2: the flash window has just elapsed (`now_tick < flash_until`,
    // and `flash_until == 2`).
    rt.timer_fast();
    assert_eq!(rt.tick, 2);
    assert!(!rt.radar.has_active_flash(rt.tick), "flash window has elapsed by tick 2");

    // With nothing running, the Fast loop has nothing left — but the
    // pending row's `· Nm` wait tag is still counting, so the timer
    // settles to the Slow heartbeat (the same 1h-saturating cadence the
    // ledger uses) rather than disarming outright.
    for _ in 0..3 {
        rt.timer_fast();
    }
    assert!(!rt.timer_should_continue(), "nothing needs the Fast loop");
    assert_eq!(
        rt.timer_chain.armed(),
        Some(Cadence::Slow),
        "an unsaturated pending wait keeps the Slow heartbeat armed"
    );
}

#[test]
fn command_attention_prev_emits_switch_tab() {
    let mut runtime = granted_runtime();
    // tab 0 active (running); tabs 1 and 2 pending → attention.
    // From active 0: Next steps forward to 1, Prev wraps backward to 2.
    runtime.tabs_changed(vec![tab(0, "a", true), tab(1, "b", false), tab(2, "c", false)]);
    runtime.radar.set_tab_panes_for_position(0, vec![pane(10)]);
    runtime.radar.set_tab_panes_for_position(1, vec![pane(11)]);
    runtime.radar.set_tab_panes_for_position(2, vec![pane(12)]);
    set_status(&mut runtime.radar, 10, Status::Running, "working", 1);
    set_status(&mut runtime.radar, 11, Status::Pending, "working", 1);
    set_status(&mut runtime.radar, 12, Status::Pending, "working", 1);

    let out = runtime.control(Verb::AttentionPrev);
    assert_eq!(out.effects, vec![Effect::SwitchTab { position: 2 }]);
}

#[test]
fn control_pipe_dispatches_known_verb() {
    let mut runtime = granted_runtime();
    // tab 0 active (running), tab 1 pending → attention.
    runtime.tabs_changed(vec![tab(0, "a", true), tab(1, "b", false)]);
    runtime.radar.set_tab_panes_for_position(0, vec![pane(10)]);
    runtime.radar.set_tab_panes_for_position(1, vec![pane(11)]);
    set_status(&mut runtime.radar, 10, Status::Running, "working", 1);
    set_status(&mut runtime.radar, 11, Status::Pending, "working", 1);

    // Exercises the full parse → command → effect path through the pipe entry.
    let out = runtime.control_pipe("attention-next");
    assert_eq!(out.effects, vec![Effect::SwitchTab { position: 1 }]);
}

#[test]
fn cadence_seconds_maps_fast_and_slow() {
    // Both cadences are exercised here (rather than only via the wasm-only
    // glue that replays `SetTimeout`) so this pure mapping is host-testable
    // and neither variant reads as dead code under `cargo test`.
    assert_eq!(Cadence::Fast.seconds(), 1.0);
    assert_eq!(Cadence::Slow.seconds(), 60.0);
}

#[test]
fn remote_running_never_arms_fast_cadence() {
    // The regression this feature could most easily cause: a live ssh session
    // must render steady, not spin — and steady means it must never pin the
    // 1 Hz Fast cadence (`AGENTS.md`'s "never poll" rule,
    // `docs/smart-tabs-postmortem.md`).
    let mut rt = runtime_with_config(config());
    let remote = PaneAttention { app: Some("zellij".into()), ..program_attention(Status::Running, "prod-db") };
    let outcome = rt.attention_changed(7, remote);
    assert_eq!(rt.radar.observation(7).unwrap().status, Status::Running);
    assert_eq!(rt.radar.observation(7).unwrap().kind, crate::kind::Kind::Remote);
    assert!(
        !outcome.effects.contains(&Effect::SetTimeout(Cadence::Fast)),
        "a steady remote session must not arm Fast cadence, got {:?}",
        outcome.effects
    );
    assert_ne!(rt.timer_chain.armed(), Some(Cadence::Fast), "steady — nothing here is tick-windowed");
}

#[test]
fn idle_with_fresh_history_arms_slow_and_repaints() {
    let mut rt = granted_runtime();
    let now = crate::clock::now_epoch_s();
    seed_done_ledger(&mut rt, now);
    assert!(rt.timer_chain.armed().is_none(), "setup: nothing has armed a timer yet");

    // Any event that runs `project` (here, a no-op topology update) must
    // arm the Slow heartbeat — nothing is tick-windowed, but the ledger
    // age is still changing.
    let outcome = rt.tabs_changed(vec![]);
    assert!(
        outcome.effects.contains(&Effect::SetTimeout(Cadence::Slow)),
        "idle with unsaturated history must arm Slow, got {:?}",
        outcome.effects
    );
    assert_eq!(rt.timer_chain.armed(), Some(Cadence::Slow));

    // The slow fire itself must render — it exists precisely to repaint
    // the ledger's ages. A genuine Slow fire (elapsed ~60s), because the
    // repaint clause keys on the fire that landed, not on `desired_cadence`.
    let tick = rt.timer_slow();
    assert!(tick.render, "a slow fire renders to repaint ledger ages");
}

#[test]
fn saturated_history_fully_disarms() {
    // The battery property's full-disarm pin, pre-name (the sibling below
    // pins the same once the name is known).
    let mut rt = granted_runtime();
    // Any epoch older than SATURATE_S relative to the real wall clock —
    // 0 trivially qualifies.
    seed_done_ledger(&mut rt, 0);
    assert_eq!(
        rt.desired_cadence(crate::clock::now_epoch_s()),
        None,
        "a saturated ledger has nothing left worth ticking for"
    );

    let outcome = rt.tabs_changed(vec![]);
    assert!(
        !outcome.effects.iter().any(|e| matches!(e, Effect::SetTimeout(_))),
        "a fully-saturated idle rail must not arm any timer, got {:?}",
        outcome.effects
    );
    assert!(rt.timer_chain.armed().is_none());
}

#[test]
fn saturated_history_with_known_name_fully_disarms() {
    // Sibling of `saturated_history_fully_disarms` with a learned own-session
    // name: the name no longer keeps a heartbeat chain alive (peers come from
    // `GlobalAttentionUpdate`, pushed), so a saturated idle rail disarms fully.
    let mut rt = granted_runtime();
    seed_done_ledger(&mut rt, 0);
    // Seed `last_now_epoch_s` with a real wall-clock capture first, so the
    // arm decision inside `session_name_changed`'s project pass evaluates at
    // true "now" — where the 0-stamped ledger entry above reads as saturated.
    rt.tabs_changed(vec![]);

    let named = rt.session_name_changed(Some("work".into()));
    assert!(
        !named.effects.iter().any(|e| matches!(e, Effect::SetTimeout(_))),
        "learning the name must not arm a timer, got {:?}",
        named.effects
    );
    assert_eq!(rt.timer_chain.armed(), None);
}

// ── spinner frames (`spinner_fps`, `Cadence::Frame`) ─────────────────────────

/// A granted, visible rail at `spinner_fps 4` with one Running agent on
/// pane 5 — the state that wants sub-second spinner frames.
fn spinning_runtime() -> PluginRuntime {
    let mut rt = granted_runtime();
    rt.config.spinner_fps = 4;
    rt.tabs_changed(vec![tab(0, "a", true)]);
    rt.radar.set_tab_panes_for_position(0, vec![pane(5)]);
    rt.push(5, Status::Running);
    rt
}

const FRAME_S: f64 = 0.25;

fn fire(rt: &mut PluginRuntime, elapsed_s: f64) -> Outcome {
    rt.timer(elapsed_s, crate::clock::now_epoch_s())
}

#[test]
fn frame_cadence_arms_for_a_visible_full_speed_spinner() {
    let rt = spinning_runtime();
    assert_eq!(rt.timer_chain.armed(), Some(Cadence::Frame(4)));
    assert_eq!(Cadence::Frame(4).seconds(), FRAME_S);
}

#[test]
fn frame_fires_advance_the_spinner_and_every_second_runs_one_tick() {
    let mut rt = spinning_runtime();
    let (tick0, frame0) = (rt.tick, rt.frame);
    for n in 1..=3 {
        let out = fire(&mut rt, FRAME_S);
        assert_eq!(rt.tick, tick0, "frame {n} is frame-only: the seconds clock holds");
        assert_eq!(rt.frame, frame0 + n);
        assert!(out.render, "a frame repaints");
        assert_eq!(out.effects, vec![Effect::SetTimeout(Cadence::Frame(4))], "and only re-arms");
    }
    // The fourth quarter-second completes a second: the full tick runs.
    fire(&mut rt, FRAME_S);
    assert_eq!((rt.tick, rt.frame), (tick0 + 1, frame0 + 4));
    // Over a longer run the tick stays ~1 Hz.
    for _ in 0..40 {
        fire(&mut rt, FRAME_S);
    }
    assert_eq!((rt.tick, rt.frame), (tick0 + 11, frame0 + 44));
}

#[test]
fn a_late_frame_fire_still_ticks_on_real_elapsed_time() {
    let mut rt = spinning_runtime();
    let tick0 = rt.tick;
    fire(&mut rt, FRAME_S);
    fire(&mut rt, 0.8); // a scheduler hiccup: 1.05s have really passed
    assert_eq!(rt.tick, tick0 + 1, "ticks follow real time, not the fire count");
}

#[test]
fn frames_stand_down_when_hidden_eased_or_opted_out() {
    // Hidden: the next fire re-arms plain Fast and paints nothing.
    let mut rt = spinning_runtime();
    rt.visibility_changed(false);
    let out = fire(&mut rt, FRAME_S);
    assert!(!out.render, "a hidden rail paints no frame");
    assert_eq!(out.effects, vec![Effect::SetTimeout(Cadence::Fast)]);

    // Opted out: `spinner_fps 1` is the plain 1 Hz chain.
    let mut rt = granted_runtime();
    rt.tabs_changed(vec![tab(0, "a", true)]);
    rt.radar.set_tab_panes_for_position(0, vec![pane(5)]);
    rt.push(5, Status::Running);
    assert_eq!(rt.timer_chain.armed(), Some(Cadence::Fast));

    // Eased: past EASE_AFTER_TICKS the glyph blinks per tick, so frames between
    // ticks would repaint an identical rail.
    let mut rt = spinning_runtime();
    rt.tick += crate::render::EASE_AFTER_TICKS + 1;
    assert!(!rt.radar.has_full_speed_spinner(rt.tick));
    for _ in 0..4 {
        fire(&mut rt, FRAME_S);
    }
    assert_eq!(rt.timer_chain.armed(), Some(Cadence::Fast));
}

#[test]
fn fast_to_frame_never_tops_up_a_second_sub_second_chain() {
    // Fast-armed (hidden), then revealed: the in-flight 1s fire must be the
    // one that switches to Frame — a top-up would leave two sub-5s fires in
    // flight that `on_fire` could never tell apart.
    let mut rt = spinning_runtime();
    rt.visibility_changed(false);
    fire(&mut rt, FRAME_S); // re-arms Fast
    let reveal = rt.visibility_changed(true);
    assert!(
        !reveal.effects.iter().any(|e| matches!(e, Effect::SetTimeout(_))),
        "no top-up on reveal, got {:?}",
        reveal.effects
    );
    let out = fire(&mut rt, Cadence::Fast.seconds());
    assert!(out.effects.contains(&Effect::SetTimeout(Cadence::Frame(4))), "the live fire switches cadence");
}

#[test]
fn fast_work_arriving_during_slow_rearms_fast() {
    let mut rt = granted_runtime();
    let now = crate::clock::now_epoch_s();
    seed_done_ledger(&mut rt, now);
    rt.tabs_changed(vec![]);
    assert_eq!(rt.timer_chain.armed(), Some(Cadence::Slow), "setup: slow-armed on fresh history");

    // New fast-worthy work (a Running status) arrives while Slow-armed.
    // The earlier-scheduled slow fire is a harmless spurious tick, but a
    // fresh `SetTimeout(Fast)` must also be pushed so the 1s resolution
    // returns promptly.
    let outcome = rt.push(5, Status::Running);
    assert!(
        outcome.effects.contains(&Effect::SetTimeout(Cadence::Fast)),
        "fast work arriving during a slow arm must re-arm Fast, got {:?}",
        outcome.effects
    );
    assert_eq!(rt.timer_chain.armed(), Some(Cadence::Fast));
}

/// Shared setup for the stale-fire dedup tests: Slow-armed on fresh
/// history (one fire in flight), then a Running broadcast tops up Fast
/// (a second, non-cancellable fire in flight).
fn slow_armed_then_fast_topup() -> PluginRuntime {
    let mut rt = granted_runtime();
    let now = crate::clock::now_epoch_s();
    seed_done_ledger(&mut rt, now);
    rt.tabs_changed(vec![]); // arms Slow: one fire in flight
    assert_eq!(rt.timer_chain.armed(), Some(Cadence::Slow), "setup: slow-armed on fresh history");
    let outcome = rt.push(5, Status::Running);
    assert!(
        outcome.effects.contains(&Effect::SetTimeout(Cadence::Fast)),
        "setup: the top-up must arm Fast, got {:?}",
        outcome.effects
    );
    rt
}

#[test]
fn live_fast_fire_processes_then_stale_slow_fire_is_swallowed() {
    // The COMMON arrival order after a Slow→Fast top-up: the fast fire
    // (armed for 1s) lands first; the stale slow fire lands up to 59s
    // later. The fast fire must process normally — swallowing it by count
    // alone would freeze the tick clock (spinner, debounce, TTL, flash)
    // until the slow fire finally landed, while Fast-worthy work runs.
    let mut rt = slow_armed_then_fast_topup();

    // The live fast fire (elapsed ~1s) ticks and re-arms exactly once.
    let tick_before = rt.tick;
    let live = rt.timer_now(1.0);
    assert_eq!(rt.tick, tick_before + 1, "the live fast fire ticks");
    let rearms = live.effects.iter().filter(|e| matches!(e, Effect::SetTimeout(_))).count();
    assert_eq!(rearms, 1, "the live fire re-arms exactly once, got {:?}", live.effects);
    assert!(live.effects.contains(&Effect::SetTimeout(Cadence::Fast)), "running work keeps the Fast cadence");

    // The STALE slow fire (elapsed ~60s) lands second, with the re-armed
    // fast fire still in flight: swallowed whole — no tick advance, no
    // effects, the live arm untouched. Ticking it would re-arm a
    // second persistent chain.
    let tick_before = rt.tick;
    let stale = rt.timer_now(60.0);
    assert_eq!(stale, Outcome::none(), "a stale slow fire must be swallowed whole");
    assert_eq!(rt.tick, tick_before, "a swallowed fire must not advance the tick");
    assert_eq!(rt.timer_chain.armed(), Some(Cadence::Fast), "a swallowed fire must not disturb the live arm");

    // Steady state: exactly one chain remains and keeps ticking.
    let next = rt.timer_now(1.0);
    assert_eq!(rt.tick, tick_before + 1, "the surviving chain keeps ticking");
    assert!(
        next.effects.contains(&Effect::SetTimeout(Cadence::Fast)),
        "the surviving chain re-arms, got {:?}",
        next.effects
    );
}

#[test]
fn stale_slow_fire_landing_first_is_swallowed() {
    // The RARE arrival order (top-up in the slow window's final second):
    // the stale slow fire lands before the fast one. It must be swallowed
    // — another fire is in flight and its elapsed marks it slow-armed —
    // and the fast fire then processes normally.
    let mut rt = slow_armed_then_fast_topup();

    let tick_before = rt.tick;
    let stale = rt.timer_now(60.0);
    assert_eq!(stale, Outcome::none(), "a stale slow fire must be swallowed whole");
    assert_eq!(rt.tick, tick_before, "a swallowed fire must not advance the tick");
    assert_eq!(rt.timer_chain.armed(), Some(Cadence::Fast), "a swallowed fire must not disturb the live arm");

    // The surviving fast fire ticks normally and re-arms exactly once.
    let live = rt.timer_now(1.0);
    assert_eq!(rt.tick, tick_before + 1, "the live fire ticks");
    let rearms = live.effects.iter().filter(|e| matches!(e, Effect::SetTimeout(_))).count();
    assert_eq!(rearms, 1, "the live fire re-arms exactly once, got {:?}", live.effects);
    assert!(live.effects.contains(&Effect::SetTimeout(Cadence::Fast)), "running work keeps the Fast cadence");
}

#[test]
fn lone_slow_fire_processes_as_the_live_chain() {
    // A slow fire with no other fire in flight IS the live chain — its
    // 60s elapsed must not get it swallowed, or the ledger-age repaint it
    // exists for would die.
    let mut rt = granted_runtime();
    let now = crate::clock::now_epoch_s();
    seed_done_ledger(&mut rt, now);
    rt.tabs_changed(vec![]); // arms Slow: the only fire in flight
    assert_eq!(rt.timer_chain.armed(), Some(Cadence::Slow));

    let tick = rt.timer_now(60.0);
    assert!(tick.render, "the lone slow fire processes and repaints ledger ages");
    assert!(
        tick.effects.contains(&Effect::SetTimeout(Cadence::Slow)),
        "the lone slow chain re-arms itself, got {:?}",
        tick.effects
    );
}

// ── Timer-chain simulation ──────────────────────────────────────────────
//
// `FireSim` drives `PluginRuntime::timer` through a real fire-order
// simulation (not hand-picked `elapsed_s` values); `timer_chain_fuzz` uses it
// to search thousands of random event/fire orderings for a stranded chain.

/// A one-shot, non-cancellable `set_timeout` queue, exactly like Zellij's:
/// every `Effect::SetTimeout` schedules a real future fire at `now + dur`,
/// and fires are delivered in absolute-time order (never reordered) —
/// mirroring lib.rs's `set_timeout(cadence.seconds())` and Zellij's own
/// single-threaded event loop. Used to drive `PluginRuntime::timer` the way
/// production actually does, instead of hand-picking `elapsed_s` values.
///
/// Also owns the virtual wall clock: `timer` takes `now_epoch_s` as a plain
/// argument (the module's clock doctrine — no clock to mock), and the
/// harness passes [`FireSim::now_epoch_s`] with each popped fire, so the
/// wall-clock-keyed gates inside `timer` see time advance in lockstep with
/// the fires instead of standing still at test speed.
struct FireSim {
    now_ms: u64,
    /// Real epoch at construction — virtual time counts up from here, so the
    /// epochs handed to `timer` stay monotone with the real-clock captures
    /// domain events take outside the FireSim.
    base_epoch_s: u64,
    queue: std::collections::BinaryHeap<std::cmp::Reverse<(u64, u64)>>,
}
impl FireSim {
    fn new() -> Self {
        Self { now_ms: 0, base_epoch_s: crate::clock::now_epoch_s(), queue: Default::default() }
    }
    /// The virtual "now" a fire popped at this instant should carry into
    /// `timer` — what production's `clock::now_epoch_s()` capture is to a
    /// real fire.
    fn now_epoch_s(&self) -> u64 {
        self.base_epoch_s + self.now_ms / 1000
    }
    fn schedule_from(&mut self, effects: &[Effect]) {
        for e in effects {
            if let Effect::SetTimeout(c) = e {
                let dur_ms = (c.seconds() * 1000.0).round() as u64;
                self.queue.push(std::cmp::Reverse((self.now_ms + dur_ms, dur_ms)));
            }
        }
    }
    /// Pop the earliest scheduled fire and report it as `timer()` wants:
    /// `elapsed_s` = the duration it was armed with (matches lib.rs's
    /// documented approximation). Advances the virtual clock to match —
    /// read it back via [`FireSim::now_epoch_s`].
    fn pop(&mut self) -> Option<f64> {
        let std::cmp::Reverse((at, dur_ms)) = self.queue.pop()?;
        self.now_ms = at;
        Some(dur_ms as f64 / 1000.0)
    }
}

mod timer_chain_fuzz {
    use super::*;
    use proptest::prelude::*;

    #[derive(Clone, Debug)]
    enum Step {
        /// Pop and deliver the earliest scheduled fire (real FIFO order).
        Fire,
        /// A domain edge that can flip `needs_fast_ticks` — the real trigger
        /// for a Slow<->Fast cadence transition.
        SetRunning(bool),
        /// The cross-session cycle's own direct `arm_timer_if_needed` call
        /// site (`control()`), which bypasses `project()` entirely.
        SessionCycle,
    }

    fn arb_step() -> impl Strategy<Value = Step> {
        prop_oneof![
            4 => Just(Step::Fire),
            1 => any::<bool>().prop_map(Step::SetRunning),
            1 => Just(Step::SessionCycle),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]
        #[test]
        fn timer_chain_never_starves_a_session_that_wants_ticks(
            steps in proptest::collection::vec(arb_step(), 0..300)
        ) {
            let mut rt = PluginRuntime {
                permission: PermissionState::Resolved { granted: true },
                config: config(),
                ..Default::default()
            };
            let mut sim = FireSim::new();
            let named = rt.session_name_changed(Some("work".into()));
            sim.schedule_from(&named.effects);
            drive_tabs_and_panes(&mut rt);

            for step in steps {
                match step {
                    Step::Fire => {
                        if let Some(elapsed) = sim.pop() {
                            let out = rt.timer(elapsed, sim.now_epoch_s());
                            sim.schedule_from(&out.effects);
                        }
                    }
                    Step::SetRunning(running) => {
                        let status = if running { "running" } else { "done" };
                        let out = rt.push(7, Status::from_wire(status));
                        sim.schedule_from(&out.effects);
                    }
                    Step::SessionCycle => {
                        let out = rt.control(Verb::SessionNext);
                        sim.schedule_from(&out.effects);
                    }
                }
                // The invariant: whenever the runtime wants a cadence, a
                // physical timer is outstanding — otherwise the chain has
                // permanently stranded itself; no future event will ever
                // revive it, because nothing is left to arm/on_fire from.
                prop_assert!(
                    rt.desired_cadence(rt.last_now_epoch_s).is_none() || !sim.queue.is_empty(),
                    "TimerChain starved: no physical timer outstanding \
                     (armed={:?}) after step, though a cadence is wanted",
                    rt.timer_chain.armed(),
                );
            }

            // Stronger post-condition, independent of the starve check
            // above: after the random churn stops, force any dangling
            // busy edge closed (a *still-running*
            // agent legitimately keeps Fast armed forever — that's correct,
            // not a bug, so it must not be mistaken for one here) and let
            // no more domain events land. A generous drain (well past
            // the flash window and any plausible stacked-stale-fire
            // backlog) must reach full disarm — catching a chain that's
            // stuck "armed" on paper but only ever fires (or only ever gets
            // swallowed as stale) without ever settling.
            let close = rt.push(7, Status::from_wire("done"));
            sim.schedule_from(&close.effects);
            let mut drained = false;
            for _ in 0..5000 {
                let Some(elapsed) = sim.pop() else {
                    drained = true;
                    break;
                };
                let out = rt.timer(elapsed, sim.now_epoch_s());
                sim.schedule_from(&out.effects);
            }
            prop_assert!(
                drained,
                "the chain never fully disarmed within 5000 drained fires after churn stopped \
                 (armed={:?}, queue_len={})",
                rt.timer_chain.armed(),
                sim.queue.len(),
            );
        }
    }
}

// ── Visibility gate ─────────────────────────────────────────────────────

/// A granted, named runtime with one tab, one tracked pane (7) running, and
/// a first frame drawn — the steady state every hidden-rail test starts from.
fn painted_runtime() -> PluginRuntime {
    let mut rt = runtime_with_granted_permission();
    drive_tabs_and_panes(&mut rt);
    rt.push(7, Status::from_wire("running"));
    let _ = rt.render(30, 40);
    rt
}

/// A one-tab, one-pane manifest: pane 7 in tab 0 reporting Running, default theme.
fn manifest_with_pane_7() -> PaneUpdate {
    let running =
        crate::attention::NativePane { attention: program_attention(Status::Running, "working"), ..Default::default() };
    PaneUpdate {
        tab_panes: HashMap::from([(0, vec![pane(7)])]),
        live: HashSet::from([7]),
        theme: None,
        native: HashMap::from([(7, running)]),
    }
}

#[test]
fn hidden_rail_ticks_its_state_but_never_paints() {
    // Every tick still advances the tick clock (flash windows) so a hidden
    // instance stays in lockstep with its visible siblings — it just does not
    // paint the frame nobody can see.
    let mut rt = painted_runtime();
    rt.push(9, Status::Running);

    let hide = rt.visibility_changed(false);
    assert_eq!(hide, Outcome::none(), "going dark is silent: nothing to paint, nothing to write");

    for _ in 0..2 {
        let tick = rt.timer_fast();
        assert!(!tick.render, "a hidden rail never paints a tick frame");
        assert!(
            tick.effects.contains(&Effect::SetTimeout(Cadence::Fast)),
            "…but keeps the Fast chain alive for the animating row, got {:?}",
            tick.effects
        );
    }
    assert_eq!(rt.tick, 2, "the tick clock advanced on schedule while hidden");
}

#[test]
fn reveal_repaints_once_even_when_the_rows_did_not_change() {
    // While hidden the instance painted nothing, so whatever the rows-diff
    // gate remembers as "on screen" is what was there BEFORE the tab went
    // dark. A reveal must paint regardless of the key compare.
    let mut rt = painted_runtime();
    rt.visibility_changed(false);
    assert!(!rt.timer_fast().render);

    let reveal = rt.visibility_changed(true);
    assert!(reveal.render, "reveal repaints the frame that was skipped");

    let again = rt.visibility_changed(true);
    assert_eq!(again, Outcome::none(), "a repeated Visible(true) is a no-op");
    assert!(rt.timer_fast().render, "visible again: tick frames paint as before");
}

#[test]
fn a_manifest_proves_the_rail_visible() {
    // Zellij routes TabUpdate/PaneUpdate to the active tab's plugins only,
    // so receiving one is proof of visibility — the belt-and-braces for a
    // lost Visible(true), whose failure mode would be a rail that never
    // paints again.
    let mut rt = painted_runtime();
    rt.visibility_changed(false);
    assert!(!rt.timer_fast().render);

    rt.tabs_changed(vec![tab(0, "a", true)]);
    assert!(rt.timer_fast().render, "a TabUpdate un-hides the rail");

    rt.visibility_changed(false);
    rt.panes_changed(manifest_with_pane_7());
    assert!(rt.timer_fast().render, "a PaneUpdate un-hides the rail");
}

// ── Selectability: the rail is passive only once it has a terminal neighbor ──
//
// Zellij closes any tab whose tiled panes are all non-selectable
// (`Screen::render` → `tabs_to_close`), and a `children` placeholder nested in
// a split spawns NO terminal when the tab body is empty (zellij#5618, #3247).
// A rail that went `SetSelectable(false)` at load therefore closed the tab —
// and, as the last tab, the whole session ("Bye from Zellij!", issue #46). The
// runtime now flips to passive only after a `PaneUpdate` shows a terminal pane
// sharing its tab — a latch (`own_tab_saw_terminal`), emitted on change only.

fn selectable_effects(outcome: &Outcome) -> Vec<bool> {
    outcome
        .effects
        .iter()
        .filter_map(|e| match e {
            Effect::SetSelectable(v) => Some(*v),
            _ => None,
        })
        .collect()
}

fn manifest(tab_panes: HashMap<usize, Vec<TerminalPane>>) -> PaneUpdate {
    let live = tab_panes.values().flatten().map(|p| p.id).collect();
    PaneUpdate { tab_panes, live, theme: None, native: HashMap::new() }
}

#[test]
fn rail_turns_passive_only_once_a_terminal_pane_shares_its_tab() {
    // Declared selectable at load, then granted — the steady state of every
    // rail after the first run.
    let mut runtime = PluginRuntime::default();
    runtime.load(config());
    runtime.permission = PermissionState::Resolved { granted: true };
    runtime.tabs_changed(vec![tab(0, "Tab #1", true)]);
    runtime.own_plugin_tab_changed(Some(0));

    // Rail-only tab (the issue #46 layout): no terminal in tab 0 — stay
    // selectable so Zellij keeps the tab (and the session) alive. Terminals
    // in OTHER tabs don't count.
    let alone = runtime.panes_changed(manifest(HashMap::from([(0, vec![]), (1, vec![pane(9)])])));
    assert_eq!(selectable_effects(&alone), Vec::<bool>::new(), "already selectable: no edge");

    // A terminal appears in the rail's own tab → passive, so it never steals
    // focus from the shell.
    let joined = runtime.panes_changed(manifest(HashMap::from([(0, vec![pane(7)]), (1, vec![pane(9)])])));
    assert_eq!(selectable_effects(&joined), vec![false]);

    // Steady state: identical manifests emit nothing (emit on change only).
    let again = runtime.panes_changed(manifest(HashMap::from([(0, vec![pane(7)]), (1, vec![pane(9)])])));
    assert_eq!(selectable_effects(&again), Vec::<bool>::new());

    // The tab's terminals vanish (its last shell exited): the rail must STAY
    // passive. Zellij's `ClosePane` arm reports this manifest before its
    // deferred render closes the now-terminal-less tab; a rail that flipped
    // back to selectable here would beat that render and keep a dead tab —
    // and the session — alive (verified live). Latch, not level.
    let emptied = runtime.panes_changed(manifest(HashMap::from([(0, vec![]), (1, vec![pane(9)])])));
    assert_eq!(selectable_effects(&emptied), Vec::<bool>::new());
}

#[test]
fn rail_without_a_known_tab_stays_selectable() {
    // The manifest never located this plugin's pane (`own_plugin_tab_changed(None)`):
    // with no evidence of a terminal neighbor, the safe side is selectable.
    let mut runtime = granted_runtime();
    runtime.own_plugin_tab_changed(None);
    let outcome = runtime.panes_changed(manifest(HashMap::from([(0, vec![pane(7)])])));
    assert_eq!(selectable_effects(&outcome), vec![true], "first sync declares the state explicitly");
}

#[test]
fn requesting_rail_stays_selectable_despite_a_terminal_neighbor() {
    // Zellij's y/n prompt is tied to the requesting pane — it must remain
    // focusable until the answer lands, terminal neighbor or not.
    let mut runtime = PluginRuntime::default();
    let load = runtime.load(config());
    assert!(runtime.permission.is_requesting());
    assert_eq!(selectable_effects(&load), vec![true]);

    runtime.own_plugin_tab_changed(Some(0));
    let with_terminal = runtime.panes_changed(manifest(HashMap::from([(0, vec![pane(7)])])));
    assert_eq!(selectable_effects(&with_terminal), Vec::<bool>::new(), "prompt pending: stay selectable");

    // Grant lands: the prompt is gone AND a terminal neighbor is known → passive.
    let granted = runtime.permission_result(true);
    assert_eq!(selectable_effects(&granted), vec![false]);
}

#[test]
fn granted_rail_in_a_rail_only_tab_stays_selectable_after_the_prompt() {
    // The grant resolves while the rail is still the tab's only pane: the
    // prompt is gone, but there is still nothing else to focus — no flip.
    let mut runtime = PluginRuntime::default();
    runtime.load(config());
    runtime.own_plugin_tab_changed(Some(0));
    runtime.panes_changed(manifest(HashMap::from([(0, vec![])])));

    let granted = runtime.permission_result(true);
    assert_eq!(selectable_effects(&granted), Vec::<bool>::new());
}

// ── show_mode: the Zellij input mode on the footer ──

fn mode_runtime(show_mode: bool) -> PluginRuntime {
    let mut rt = PluginRuntime {
        permission: PermissionState::Resolved { granted: true },
        config: config(),
        ..Default::default()
    };
    rt.config.show_mode = show_mode;
    drive_tabs_and_panes(&mut rt);
    rt.push(7, Status::from_wire("running"));
    // Name already known, so the mode events below carry no news but the mode.
    let _ = rt.session_name_changed(Some("work".into()));
    let _ = rt.render(40, 30);
    rt
}

#[test]
fn mode_change_with_show_mode_off_requests_no_render() {
    let mut rt = mode_runtime(false);
    let out = rt.mode_changed(Some("work".into()), crate::mode::Mode::Locked, None);
    assert!(!out.render, "off: a mode change costs no repaint");
    let out = rt.mode_changed(Some("work".into()), crate::mode::Mode::Normal, None);
    assert!(!out.render);
    assert!(!rt.render(40, 30).contains("NORMAL"), "off: no label drawn");
}

#[test]
fn mode_change_with_show_mode_on_repaints_once_per_distinct_mode() {
    use crate::mode::Mode;
    let mut rt = mode_runtime(true);
    let first = rt.mode_changed(Some("work".into()), Mode::Locked, None);
    assert!(first.render, "the first mode is news");
    assert!(rt.render(40, 30).contains("LOCKED"));
    let repeat = rt.mode_changed(Some("work".into()), Mode::Locked, None);
    assert!(!repeat.render, "an identical mode is not");
    let next = rt.mode_changed(None, Mode::Pane, None);
    assert!(next.render, "a new mode repaints even without a session name");
    assert!(rt.render(40, 30).contains("PANE"));
}

#[test]
fn a_nameless_mode_update_with_show_mode_off_is_a_no_op() {
    // The production path for a `ModeUpdate` that carries no session name:
    // with the label off it must stay the no-op it was before `mode_changed`.
    let mut rt = mode_runtime(false);
    let out = rt.mode_changed(None, crate::mode::Mode::Locked, None);
    assert!(!out.render, "got {:?}", out.effects);
    assert!(out.effects.is_empty(), "got {:?}", out.effects);
}

#[test]
fn the_first_named_mode_update_paints_the_label() {
    let mut rt = PluginRuntime {
        permission: PermissionState::Resolved { granted: true },
        config: config(),
        ..Default::default()
    };
    rt.config.show_mode = true;
    drive_tabs_and_panes(&mut rt);
    let out = rt.mode_changed(Some("work".into()), crate::mode::Mode::Normal, None);
    assert!(out.render, "the first shown mode repaints");
}

#[test]
fn mode_change_on_a_hidden_rail_requests_no_render() {
    let mut rt = mode_runtime(true);
    let _ = rt.visibility_changed(false);
    assert!(!rt.mode_changed(Some("work".into()), crate::mode::Mode::Locked, None).render);
}

#[test]
fn show_mode_live_toggle_repaints_and_shows_the_last_seen_mode() {
    let mut rt = mode_runtime(false);
    let _ = rt.mode_changed(Some("work".into()), crate::mode::Mode::Scroll, None);
    let on = rt.config_pipe(r#"{"show_mode":true}"#);
    assert!(on.render, "the toggle repaints");
    assert!(rt.render(40, 30).contains("SCROLL"), "the mode seen while off is shown once on");
    let off = rt.config_pipe(r#"{"show_mode":false}"#);
    assert!(off.render);
    assert!(!rt.render(40, 30).contains("SCROLL"));
}

// ── theme color sources: pane default_bg > Zellij theme palette > fallback (#85) ──

fn latte() -> theme::DerivedColors {
    theme::DerivedColors::from_theme_palette((220, 224, 232), (76, 79, 105)).unwrap()
}

#[test]
fn theme_palette_is_used_when_no_pane_reports_colors() {
    // Zellij 0.45 leaves every pane's default_bg null (manifests carry
    // `theme: None`), so the ModeUpdate palette must win over the fallback.
    let mut rt = mode_runtime(false);
    assert_eq!(rt.theme, theme::DerivedColors::default());
    let out = rt.mode_changed(Some("work".into()), crate::mode::Mode::Normal, Some(latte()));
    assert!(out.render, "a new palette repaints even with show_mode off");
    assert_eq!(rt.theme, latte());
    let _ = rt.panes_changed(manifest_with_pane_7());
    assert_eq!(rt.theme, latte(), "a colorless manifest keeps the palette");
    let again = rt.mode_changed(Some("work".into()), crate::mode::Mode::Normal, Some(latte()));
    assert!(!again.render, "an unchanged palette costs no repaint");
}

#[test]
fn pane_reported_colors_outrank_the_theme_palette() {
    let mut rt = mode_runtime(false);
    let pane_colors = theme::DerivedColors::from_bg_fg((0, 0, 0), (200, 200, 200));
    let _ = rt.panes_changed(PaneUpdate { theme: Some(pane_colors.clone()), ..manifest_with_pane_7() });
    let _ = rt.mode_changed(Some("work".into()), crate::mode::Mode::Normal, Some(latte()));
    assert_eq!(rt.theme, pane_colors, "OSC 10/11-reported colors are the more specific source");
}

#[test]
fn theme_switch_to_an_unusable_palette_falls_back() {
    // A later ModeUpdate is authoritative: switching to an 8-bit theme (the
    // adapter hands `None`) drops the old palette rather than keeping it.
    let mut rt = mode_runtime(false);
    let _ = rt.mode_changed(Some("work".into()), crate::mode::Mode::Normal, Some(latte()));
    let _ = rt.render(40, 30); // the latte frame is on screen
    let out = rt.mode_changed(Some("work".into()), crate::mode::Mode::Normal, None);
    assert!(out.render);
    assert_eq!(rt.theme, theme::DerivedColors::default());
}

#[test]
fn pane_attention_changed_renders_only_on_a_change() {
    let mut rt = runtime_with_granted_permission();
    rt.tabs_changed(vec![tab(0, "api", true)]);
    let working = crate::attention::PaneAttention {
        status: Status::Running,
        source: crate::attention::Source::Program,
        text: "Thinking".into(),
        app: Some("pi".into()),
        ..Default::default()
    };
    assert!(rt.attention_changed(7, working.clone()).render);
    assert!(!rt.attention_changed(7, working).render);
}
