//! The Zellij sidebar wasm plugin: per-tab agent-status rail.
//!
//! # Shape
//! - [`State`] is the per-tab plugin instance Zellij loads. It owns a
//!   [`runtime::PluginRuntime`] (the host-testable state machine — pure apart
//!   from wall-clock reads via `clock::now_epoch_s`).
//! - The `#[cfg(target_arch = "wasm32")] impl ZellijPlugin for State` block is the
//!   only code that touches the Zellij host API; everything it calls is pure logic
//!   in the modules below. Host tests exercise that logic without a wasm build.
//! - The `#[cfg(test)] impl State` block holds test-only fixtures (`build_rows`,
//!   …) that are never linked into the shipped wasm.

// On a plain host build (`cargo build`, not wasm, not test) the only consumers
// of the pure modules are the wasm glue (cfg'd out) and the unit tests (cfg'd
// out), so every public item appears dead. One crate-level allow covers all
// modules, replacing the 13 per-module #[cfg_attr] annotations from the
// previous monolithic layout. Dead-code detection still runs under
// `cargo test` (not(test) is false) and the wasm build.
#![cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]

mod attention;
mod clock;
mod config;
mod control;
mod kind;
mod ledger;
mod mode;
mod observation;
mod payload;
mod radar_state;
#[cfg(test)]
mod reference_tests;
mod render;
mod rollup;
mod runtime;
mod sessions;
mod status;
mod tab_namer;
mod task;
#[cfg(test)]
mod test_fixtures;
mod theme;

// RadarTab/TabId are used only by the wasm glue (tests build tabs through
// `test_fixtures`); on any host build the glue is cfg'd out, so the import is
// allowed to go unused there.
#[cfg_attr(not(target_arch = "wasm32"), allow(unused_imports))]
use radar_state::{RadarTab, TabId};
#[cfg(test)]
use rollup::TabRow;
use runtime::PluginRuntime;

#[cfg(target_arch = "wasm32")]
use runtime::Effect;
#[cfg(target_arch = "wasm32")]
use std::collections::BTreeMap;
#[cfg(target_arch = "wasm32")]
use zellij_tile::prelude::*;

// Pipe names live with their vocabularies (host-testable, so the guard tests
// in control.rs/config.rs can pin them against the docs); these aliases just
// keep the match arms below short.
#[cfg(target_arch = "wasm32")]
const CONFIG_PIPE: &str = config::CONFIG_PIPE;
#[cfg(target_arch = "wasm32")]
const CMD_PIPE: &str = control::CMD_PIPE;

/// Zellij's `InputMode` → the host-testable `mode::Mode` mirror. Exhaustive
/// on purpose: a mode added upstream fails this build instead of rendering
/// as something else.
#[cfg(target_arch = "wasm32")]
fn input_mode(mode: InputMode) -> mode::Mode {
    match mode {
        InputMode::Normal => mode::Mode::Normal,
        InputMode::Locked => mode::Mode::Locked,
        InputMode::Resize => mode::Mode::Resize,
        InputMode::Pane => mode::Mode::Pane,
        InputMode::Tab => mode::Mode::Tab,
        InputMode::Scroll => mode::Mode::Scroll,
        InputMode::EnterSearch => mode::Mode::EnterSearch,
        InputMode::Search => mode::Mode::Search,
        InputMode::RenameTab => mode::Mode::RenameTab,
        InputMode::RenamePane => mode::Mode::RenamePane,
        InputMode::RenameSession => mode::Mode::RenameSession,
        InputMode::Session => mode::Mode::Session,
        InputMode::Move => mode::Mode::Move,
        InputMode::Prompt => mode::Mode::Prompt,
        InputMode::Tmux => mode::Mode::Tmux,
    }
}

/// The Zellij theme palette's color source: `text_unselected` background/base
/// when both are truecolor. Pure field copy; the contrast policy lives in the
/// host-tested `theme::DerivedColors::from_theme_palette`. 8-bit entries (the
/// built-in default theme, `ansi`) yield `None`: their RGB is the terminal's
/// own palette, which the plugin cannot see.
#[cfg(target_arch = "wasm32")]
fn theme_palette(colors: &Styling) -> Option<theme::DerivedColors> {
    let rgb = |c: PaletteColor| match c {
        PaletteColor::Rgb(rgb) => Some(rgb),
        PaletteColor::EightBit(_) => None,
    };
    let text = &colors.text_unselected;
    theme::DerivedColors::from_theme_palette(rgb(text.background)?, rgb(text.base)?)
}

#[derive(Default)]
pub struct State {
    runtime: PluginRuntime,
    #[cfg(target_arch = "wasm32")]
    plugin_id: Option<u32>,
}

// ── Pure helper wrappers (no host calls) — compiled only for host tests ──

#[cfg(test)]
impl State {
    fn build_rows(&self) -> Vec<TabRow> {
        // Test-only convenience: clone out of the runtime's shared Rc so
        // assertions keep plain-Vec ergonomics.
        (*self.runtime.build_rows()).clone()
    }

    fn record_permission_request_started(&mut self) {
        self.runtime.record_permission_request_started();
    }

    fn record_permission_result(&mut self, granted: bool) {
        self.runtime.record_permission_result(granted);
    }

    /// Render at an explicit width so `last_rendered` is populated.
    /// Click tests historically asserted the width-80 layout; keep that explicit.
    /// When no height has been set yet (last_render_height == 0), use this
    /// session's natural content height (see `PluginRuntime::natural_height`)
    /// so folding/overflow never discards rows unexpectedly — and so the
    /// bottom region's pinned footer doesn't pad the render out to
    /// an unboundedly large height (`usize::MAX / 2` used to be a safe "big
    /// enough" sentinel; now it would land in the footer's unbounded-filler
    /// branch and blow the allocator).
    ///
    /// # Contract — LIVE, permission-granted rail only
    ///
    /// This helper unconditionally sets the permission state to
    /// `PermissionState::Resolved { granted: true }` so that
    /// `runtime.render` produces a real tab rail rather than the onboarding
    /// screen. It is intentionally a LIVE-RAIL fixture and MUST NOT be used
    /// to test the no-permission / onboarding case. Onboarding tests must
    /// drive `runtime.mouse_click` directly (as `mouse_click_without_permission_is_inert`
    /// does) — they must never call `render_at`, `tab_position_at_line`, or
    /// `target_at_line`, or they will silently force-grant permission and
    /// produce misleading results.
    #[cfg(test)]
    fn render_at(&mut self, width: usize) {
        self.runtime.permission = crate::runtime::PermissionState::Resolved { granted: true };
        let height = if self.runtime.last_render_height == 0 {
            self.runtime.natural_height(width)
        } else {
            self.runtime.last_render_height
        };
        let _ = self.runtime.render(height, width);
    }

    /// Map a clicked line back to a tab position through the renderer-owned
    /// target map. Thin wrapper over `target_at_line` that drops the pane id;
    /// used by host unit tests that only assert tab membership.
    #[cfg(test)]
    fn tab_position_at_line(&mut self, line: isize) -> Option<usize> {
        self.render_at(80);
        self.runtime.tab_position_at_line(line)
    }

    /// Map a clicked line to `(tab_position, Option<pane_id>)` through the
    /// render module's `RenderedRail` target map. Runtime tests use this helper
    /// to exercise the same render-owned targeting seam as the wasm mouse path.
    #[cfg(test)]
    fn target_at_line(&mut self, line: isize) -> Option<(usize, Option<u32>)> {
        self.render_at(80);
        self.runtime.target_at_line(line)
    }
}

// ── Wasm-only glue — each item gated so host `cargo test` never links these.
// `register_plugin!` lives in the BINARY crate (`src/main.rs`) so the `fn main`
// it generates becomes the wasm `_start` Zellij requires; here we only provide
// the `ZellijPlugin` impl + host-fn helpers it drives.

#[cfg(target_arch = "wasm32")]
impl State {
    fn handle_outcome(&mut self, outcome: runtime::Outcome) -> bool {
        self.handle_effects(outcome.effects);
        outcome.render
    }

    /// Effects are fire-and-forget host calls with nothing to report;
    /// `ResolveCwd` deliberately drops its nested render flag (see
    /// `resolve_cwd`'s doc comment).
    fn handle_effects(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                // Keep in lockstep with REQUIRED_PLUGIN_PERMISSIONS in
                // crates/cli/src/run.rs: adding a permission here makes every
                // existing grant partial, and the CLI must detect that (a
                // partial grant re-prompts illegibly inside the rail).
                Effect::RequestPermission => request_permission(&[
                    PermissionType::ReadApplicationState,
                    PermissionType::ReadCliPipes,
                    PermissionType::ChangeApplicationState,
                    PermissionType::RunCommands,
                    // Without it Zellij strips the command text from attention.
                    PermissionType::ReadPaneContents,
                ]),
                Effect::SetSelectable(selectable) => set_selectable(selectable),
                Effect::SetTimeout(cadence) => set_timeout(cadence.seconds()),
                Effect::RenameTab { tab_id, name } => rename_tab_with_id(tab_id.raw() as u64, &name),
                Effect::SwitchTab { position } => switch_tab_to(position as u32 + 1),
                Effect::ShowPane { pane_id } => {
                    show_pane_with_id(PaneId::Terminal(pane_id), false, true);
                }
                Effect::ResolveCwd { pane_ids } => self.resolve_cwd(pane_ids),
                Effect::ResolveRepo { cwd } => {
                    let context = BTreeMap::from([
                        ("zj_radar".to_string(), "repo".to_string()),
                        ("cwd".to_string(), cwd.clone()),
                    ]);
                    run_command(&["git", "-C", &cwd, "remote", "get-url", "origin"], context);
                }
                Effect::SwitchSession { name, tab_position, pane_id } => match (tab_position, pane_id) {
                    (None, None) => switch_session(Some(&name)),
                    _ => switch_session_with_focus(&name, tab_position, pane_id.map(|id| (id, false))),
                },
                Effect::MarkSeen { session, pane_id } => {
                    mark_session_pane_seen(&session, PaneId::Terminal(pane_id));
                }
            }
        }
    }

    /// Bootstrap tab names for freshly-opened panes by reading each pane's cwd
    /// once. `get_pane_cwd` is a blocking host round-trip, but the runtime has
    /// already gated this to at most once per pane id (capped per update), so it
    /// runs at pane-creation rate — never the per-output re-poll that melted the
    /// predecessor plugin. A successful read feeds the existing `cwd_changed`
    /// path, which performs the rename; a failure is simply dropped (the id is
    /// already marked attempted and a later `cd` will still name the tab).
    /// Each nested outcome's `render: true` flag is intentionally dropped
    /// (only its effects are replayed): the one emitter of `Effect::ResolveCwd`
    /// — `panes_changed` — already returns `render: true` in the same update
    /// cycle, so the carrying update repaints.
    fn resolve_cwd(&mut self, pane_ids: Vec<u32>) {
        for id in pane_ids {
            if let Ok(path) = get_pane_cwd(PaneId::Terminal(id)) {
                let outcome = self.runtime.cwd_changed(id, path.to_string_lossy().to_string());
                self.handle_effects(outcome.effects);
            }
        }
    }
}

/// Zellij's attention status in the rail's vocabulary. Muted panes read Idle.
#[cfg(target_arch = "wasm32")]
fn native_status(s: AttentionStatus) -> status::Status {
    use status::Status;
    match s {
        AttentionStatus::Working => Status::Running,
        AttentionStatus::NeedsYou => Status::Pending,
        AttentionStatus::Done => Status::Done,
        AttentionStatus::Failed => Status::Error,
        AttentionStatus::Idle | AttentionStatus::Quiet | AttentionStatus::NewOutput => Status::Idle,
    }
}

#[cfg(target_arch = "wasm32")]
fn native_attention(a: PaneAttention) -> attention::PaneAttention {
    attention::PaneAttention {
        status: native_status(a.effective_status()),
        source: match a.source {
            Some(AttentionSource::ProgramStatus) => attention::Source::Program,
            Some(AttentionSource::ShellCommand | AttentionSource::CommandPane | AttentionSource::ForegroundProcess) => {
                attention::Source::Command
            }
            _ => attention::Source::Other,
        },
        text: a.text,
        app: a.detail.app,
        exit_code: a.detail.exit_code,
        since_ms: a.since_ms,
        remembered: a.remembered.map(|r| attention::Remembered {
            status: native_status(r.status),
            text: r.text,
            since_ms: r.since_ms,
        }),
    }
}

#[cfg(target_arch = "wasm32")]
impl ZellijPlugin for State {
    fn load(&mut self, config: BTreeMap<String, String>) {
        subscribe(&[
            EventType::TabUpdate,
            EventType::PaneUpdate,
            EventType::CwdChanged,
            EventType::Timer,
            EventType::Mouse,
            EventType::PermissionRequestResult,
            // Every other session on this server, with counts and the panes
            // that want attention: the cross-session badge's peers.
            EventType::GlobalAttentionUpdate,
            // One pane's attention moved; unlike `PaneUpdate` it reaches the
            // instances of hidden tabs too (ledger edges, counts).
            EventType::PaneAttentionChanged,
            // The git remote reads behind tab names (`Effect::ResolveRepo`).
            EventType::RunCommandResult,
            // Replaces `SessionUpdate`: stock zj-radar never calls the
            // blocking `get_session_list()` host fn that's the ONLY thing
            // that repopulates `SessionUpdate`'s peer list (checked on zellij 0.44.3)
            // (`docs/design.md`, "Why not SessionUpdate") — only Zellij's own
            // session-manager plugin does, so a user without it open would
            // see an empty cross-session badge forever. `ModeUpdate` fires
            // automatically right after this plugin loads (Zellij's
            // `RequestStateUpdateForPlugins` → `Tab::update_input_modes`,
            // unconditional on every successful plugin load — no polling,
            // no other plugin required) and carries `ModeInfo.session_name`,
            // which is all this plugin ever needed from `SessionUpdate` in
            // the first place.
            EventType::ModeUpdate,
            // Never handled (falls to `update`'s `_ => false`), subscribed
            // purely as an opt-out: Zellij ≥0.44 treats a plugin subscribed
            // to `InitialKeybinds` as one that caches its keybinds, and
            // sends every later `ModeUpdate` with `keybinds` EMPTY
            // (`wasm_bridge.rs::apply_event_to_plugin`). Without it each
            // `ModeUpdate` — every mode change, in every tab's instance —
            // carries the whole keybinding table (141 binds in the stock
            // config) through protobuf decode under the interpreter, for a
            // plugin that only reads `session_name` off it.
            EventType::InitialKeybinds,
            // Tab shown / hidden (tab switch, close, layout apply, client
            // detach; never at load). A hidden rail skips its paints only;
            // state and ticks carry on —
            // `PluginRuntime::hidden` has the contract.
            EventType::Visible,
        ]);
        let ids = get_plugin_ids();
        self.plugin_id = Some(ids.plugin_id);
        let outcome = self.runtime.load(config::Config::from_map(&config));
        self.handle_outcome(outcome);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::TabUpdate(tabs) => {
                let tabs = tabs
                    .into_iter()
                    .map(|t| RadarTab {
                        id: TabId::new(t.tab_id),
                        position: t.position,
                        name: t.name,
                        active: t.active,
                        has_bell: t.has_bell_notification,
                        floating_visible: t.are_floating_panes_visible,
                    })
                    .collect();
                let outcome = self.runtime.tabs_changed(tabs);
                self.handle_outcome(outcome)
            }
            Event::PaneUpdate(manifest) => {
                let own_tab_position = self.plugin_id.and_then(|plugin_id| {
                    manifest.panes.iter().find_map(|(tab_pos, panes)| {
                        panes.iter().any(|pane| pane.is_plugin && pane.id == plugin_id).then_some(*tab_pos)
                    })
                });
                self.runtime.own_plugin_tab_changed(own_tab_position);
                // Pure adapter: copy the host's `PaneInfo` fields into `RawPane`s.
                // All policy — plugin-skip, title sanitize, theme-color precedence —
                // lives in the host-tested `PaneUpdate::from_raw`.
                let raw: Vec<radar_state::RawPane> = manifest
                    .panes
                    .into_iter()
                    .flat_map(|(tab_pos, panes)| {
                        panes.into_iter().map(move |p| radar_state::RawPane {
                            tab_pos,
                            id: p.id,
                            title: p.title,
                            is_plugin: p.is_plugin,
                            is_focused: p.is_focused,
                            is_floating: p.is_floating,
                            default_bg: p.default_bg,
                            default_fg: p.default_fg,
                            attention: Some(attention::NativePane {
                                records: p
                                    .program_status
                                    .into_iter()
                                    .map(|r| attention::Record {
                                        status: native_status(r.state().attention_status()),
                                        id: r.id,
                                        app: r.app,
                                        title: r.title,
                                        msg: r.msg,
                                        updated_ms: r.updated_ms,
                                    })
                                    .collect(),
                                attention: native_attention(p.attention),
                            }),
                        })
                    })
                    .collect();
                let outcome = self.runtime.panes_changed(radar_state::PaneUpdate::from_raw(raw));
                self.handle_outcome(outcome)
            }
            Event::GlobalAttentionUpdate(global) => {
                let peers = global
                    .sessions
                    .into_iter()
                    .filter(|s| !s.is_current)
                    .map(|s| {
                        let counts = s.attention.counts;
                        attention::PeerSession {
                            working: counts.working,
                            needs_you: counts.needs_you + counts.failed,
                            items: s
                                .attention
                                .items
                                .into_iter()
                                .filter_map(|item| match item.pane_id {
                                    PaneId::Terminal(pane_id) => Some(attention::PeerItem {
                                        pane_id,
                                        tab_id: item.tab_id,
                                        tab_name: item.tab_name,
                                        attention: native_attention(item.attention),
                                    }),
                                    PaneId::Plugin(_) => None,
                                })
                                .collect(),
                            name: s.name,
                        }
                        .summary()
                    })
                    .collect();
                let outcome = self.runtime.global_attention_changed(peers);
                self.handle_outcome(outcome)
            }
            Event::Timer(elapsed_s) => {
                // Upstream documents `elapsed_s` only as the timer-expiry
                // payload: elapsed seconds since the `set_timeout` call, which
                // for a fired one-shot is ~= the duration it was armed with —
                // not a guaranteed-exact echo of it. That's all the runtime
                // needs: its 5s stale-fire threshold discriminates a ~1s (Fast)
                // arm from a ~60s (Slow) one with wide margin either way, so
                // scheduler jitter can't flip the classification.
                let outcome = self.runtime.timer(elapsed_s, crate::clock::now_epoch_s());
                self.handle_outcome(outcome)
            }
            Event::Mouse(Mouse::LeftClick(line, col)) => {
                // Zellij reports zero-based terminal display-cell offsets; the
                // e2e SGR helper deliberately converts its one-based screen
                // coordinates before this reaches the runtime.
                let outcome = self.runtime.mouse_click(line, col as usize);
                self.handle_outcome(outcome)
            }
            Event::Mouse(Mouse::RightClick(line, col)) => {
                let outcome = self.runtime.mouse_right_click(line, col as usize);
                self.handle_outcome(outcome)
            }
            Event::PermissionRequestResult(status) => {
                let granted = status == PermissionStatus::Granted;
                let outcome = self.runtime.permission_result(granted);
                self.handle_outcome(outcome)
            }
            Event::ModeUpdate(mode_info) => {
                let palette = theme_palette(&mode_info.style.colors);
                let outcome = self.runtime.mode_changed(mode_info.session_name, input_mode(mode_info.mode), palette);
                self.handle_outcome(outcome)
            }
            Event::Visible(visible) => {
                let outcome = self.runtime.visibility_changed(visible);
                self.handle_outcome(outcome)
            }
            Event::CwdChanged(pane_id, path, _clients) => {
                if let PaneId::Terminal(id) = pane_id {
                    let outcome = self.runtime.cwd_changed(id, path.to_string_lossy().to_string());
                    return self.handle_outcome(outcome);
                }
                false // plugin panes: nothing observed, nothing to repaint
            }
            Event::RunCommandResult(exit_code, stdout, _stderr, mut context) => {
                if context.get("zj_radar").map(String::as_str) != Some("repo") {
                    return false; // not a repo lookup: no result to read
                }
                let Some(cwd) = context.remove("cwd") else { return false };
                let url = String::from_utf8_lossy(&stdout);
                let outcome = self.runtime.repo_resolved(cwd, (exit_code == Some(0)).then_some(&*url));
                self.handle_outcome(outcome)
            }
            Event::PaneAttentionChanged(PaneId::Terminal(id), attention) => {
                let outcome = self.runtime.attention_changed(id, native_attention(attention));
                self.handle_outcome(outcome)
            }
            _ => false,
        }
    }

    fn pipe(&mut self, message: PipeMessage) -> bool {
        let Some(raw) = &message.payload else { return false };
        let outcome = match message.name.as_str() {
            CONFIG_PIPE => self.runtime.config_pipe(raw),
            CMD_PIPE => self.runtime.control_pipe(raw),
            _ => return false,
        };
        self.handle_outcome(outcome)
    }

    fn render(&mut self, rows: usize, cols: usize) {
        print!("{}", self.runtime.render(rows, cols));
    }
}

// ── Host unit tests (no host calls — pure helpers only) ──

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::Status;
    use crate::test_fixtures::{pane, set_status, tab};

    /// The CLI's grant probe (`REQUIRED_PLUGIN_PERMISSIONS` in
    /// crates/cli/src/run.rs) must cover exactly the set this plugin requests:
    /// a permission added to the request list but not the probe makes every
    /// existing grant partial, and Zellij's re-prompt is illegible inside the
    /// rail (zellij#4749) — the failure presents as a silently blank rail.
    /// Text-pinned because the two lists live on opposite sides of the wasm
    /// boundary and can't share a const.
    #[test]
    fn cli_grant_probe_covers_the_exact_requested_permission_set() {
        let full_src = include_str!("lib.rs");
        // Scan only the production half of this file (this test would match
        // itself); there, every such token is the request list — the SDK type
        // is referenced nowhere else in the plugin.
        let plugin_src = &full_src[..full_src.find("mod tests").expect("tests module marker")];
        let requested: std::collections::BTreeSet<&str> = plugin_src
            .match_indices("PermissionType::")
            .map(|(at, pat)| {
                let rest = &plugin_src[at + pat.len()..];
                &rest[..rest.find(|c: char| !c.is_alphanumeric()).unwrap_or(rest.len())]
            })
            .collect();
        assert!(!requested.is_empty(), "request list not found in lib.rs");

        let cli_src = include_str!("../../cli/src/run.rs");
        let decl = cli_src.find("const REQUIRED_PLUGIN_PERMISSIONS").expect("cli probe const present");
        let eq = decl + cli_src[decl..].find('=').expect("const has an initializer");
        let open = eq + cli_src[eq..].find('[').expect("initializer is an array");
        let close = open + cli_src[open..].find(']').expect("array closes");
        let probed: std::collections::BTreeSet<&str> = cli_src[open..close].split('"').skip(1).step_by(2).collect();

        assert_eq!(
            requested, probed,
            "the plugin's request_permission list and the CLI's \
             REQUIRED_PLUGIN_PERMISSIONS probe drifted — update both together \
             (a partial grant presents as a silently blank rail)"
        );
    }

    /// The doctor's version floor (`MIN_SUPPORTED_ZELLIJ` in the CLI's
    /// setup/analyze.rs) and this crate's exact `zellij-tile` pin must move
    /// together: the pin is the plugin API this build targets, the floor is
    /// what `--check` tells users about it. Newer Zellij keeps compiled
    /// plugins working, so the gate is a floor rather than a pinned minor.
    /// Text-pinned like the grant-probe guard above, for the same reason.
    #[test]
    fn doctor_version_gate_matches_the_zellij_tile_pin() {
        let manifest = include_str!("../Cargo.toml");
        let pin_line = manifest
            .lines()
            .find(|l| l.trim_start().starts_with("zellij-tile"))
            .expect("plugin Cargo.toml declares zellij-tile");
        let analyze_src = include_str!("../../cli/src/setup/analyze.rs");
        let decl = analyze_src.find("const MIN_SUPPORTED_ZELLIJ:").expect("const present in analyze.rs");
        let eq = decl + analyze_src[decl..].find('=').expect("const has an initializer");
        let triple: Vec<&str> = analyze_src[eq + 1..]
            .split(';')
            .next()
            .unwrap()
            .trim()
            .trim_matches(|c| c == '(' || c == ')')
            .split(',')
            .map(str::trim)
            .collect();
        assert_eq!(triple.len(), 3, "MIN_SUPPORTED_ZELLIJ is a (major, minor, patch) triple");
        let gate = format!("\"={}.{}.{}\"", triple[0], triple[1], triple[2]);
        assert!(
            pin_line.contains(&gate),
            "the doctor's version floor is {gate} but the plugin pins: `{pin_line}` — \
             update MIN_SUPPORTED_ZELLIJ alongside the pin"
        );
    }

    #[test]
    fn e2e_workflow_builds_the_zellij_rev_the_plugin_pins() {
        let manifest = include_str!("../Cargo.toml");
        let pin_line = manifest.lines().find(|l| l.trim_start().starts_with("zellij-tile")).unwrap();
        let rev =
            pin_line.split("rev = \"").nth(1).and_then(|r| r.split('"').next()).expect("zellij-tile pins a git rev");
        let workflow = include_str!("../../../.github/workflows/e2e.yml");
        assert!(
            workflow.contains(&format!("ZELLIJ_REV: {rev}")),
            "e2e.yml's ZELLIJ_REV must match the zellij-tile rev `{rev}`"
        );
    }

    fn make_state_with_tabs(tab_specs: &[(usize, &str, bool)]) -> State {
        // tab_specs: (position, name, active)
        // Uses Compact density so existing click-mapping tests (which hard-code
        // line numbers assuming no gap lines) continue to pass unchanged.
        let tabs = tab_specs.iter().map(|&(pos, name, active)| tab(pos, name, active)).collect();
        let mut state = State::default();
        state.runtime.tabs_changed(tabs);
        state.runtime.config = config::Config { density: config::Density::Compact, ..config::Config::default() };
        state
    }

    fn apply_payload(state: &mut State, pane_id: u32, status: Status, tick: u64) {
        apply_payload_with_msg(state, pane_id, status, tick, "msg");
    }

    fn apply_payload_with_msg(state: &mut State, pane_id: u32, status: Status, tick: u64, msg: &str) {
        set_status(&mut state.runtime.radar, pane_id, status, msg, tick);
    }

    // ── build_rows tests ──

    #[test]
    fn build_rows_empty_state_returns_empty() {
        let state = State::default();
        assert!(state.build_rows().is_empty());
    }

    #[test]
    fn build_rows_returns_one_row_per_tab_in_position_order() {
        let state = make_state_with_tabs(&[(2, "c", false), (0, "a", true), (1, "b", false)]);
        let rows = state.build_rows();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].name, "a");
        assert_eq!(rows[1].name, "b");
        assert_eq!(rows[2].name, "c");
    }

    #[test]
    fn build_rows_number_is_position_plus_one() {
        let state = make_state_with_tabs(&[(0, "first", false), (1, "second", false)]);
        let rows = state.build_rows();
        assert_eq!(rows[0].number, 1);
        assert_eq!(rows[1].number, 2);
    }

    #[test]
    fn build_rows_active_flag_passes_through() {
        let state = make_state_with_tabs(&[(0, "t0", false), (1, "t1", true)]);
        let rows = state.build_rows();
        assert!(!rows[0].active);
        assert!(rows[1].active);
    }

    #[test]
    fn build_rows_display_reflects_pane_status() {
        let mut state = make_state_with_tabs(&[(0, "agent-tab", false)]);
        // Assign pane 42 to tab position 0
        state.runtime.radar.set_tab_panes_for_position(0, vec![pane(42)]);
        apply_payload(&mut state, 42, Status::Running, 1);
        let rows = state.build_rows();
        assert_eq!(rows[0].display.status, Status::Running);
        assert!(rows[0].display.detail.is_some());
    }

    #[test]
    fn build_rows_tab_without_known_panes_is_idle() {
        let state = make_state_with_tabs(&[(0, "plain", false)]);
        // No entry in tab_panes for position 0 — no agent state
        let rows = state.build_rows();
        assert_eq!(rows[0].display.status, Status::Idle);
        assert!(rows[0].display.detail.is_none());
    }

    // ── tab_position_at_line tests ──

    #[test]
    fn click_negative_line_returns_none() {
        let mut state = make_state_with_tabs(&[(0, "t0", false)]);
        assert!(state.tab_position_at_line(-1).is_none());
    }

    #[test]
    fn plain_tabs_each_occupy_one_line() {
        // 3 plain tabs at positions 0, 1, 2 → 2-line header, then lines 2, 3, 4
        let mut state = make_state_with_tabs(&[(0, "a", false), (1, "b", false), (2, "c", false)]);
        assert_eq!(state.tab_position_at_line(0), None); // header
        assert_eq!(state.tab_position_at_line(1), None); // header
        assert_eq!(state.tab_position_at_line(2), Some(0));
        assert_eq!(state.tab_position_at_line(3), Some(1));
        assert_eq!(state.tab_position_at_line(4), Some(2));
    }

    #[test]
    fn click_beyond_last_tab_returns_none() {
        let mut state = make_state_with_tabs(&[(0, "a", false)]);
        // 1 plain tab → header (lines 0,1) + tab (line 2); line 3 is beyond
        assert!(state.tab_position_at_line(3).is_none());
    }

    #[test]
    fn agent_tab_running_occupies_two_lines() {
        let mut state = make_state_with_tabs(&[(0, "agent", false), (1, "plain", false)]);
        state.runtime.radar.set_tab_panes_for_position(0, vec![pane(10)]);
        apply_payload(&mut state, 10, Status::Running, 1); // running → 2 lines
                                                           // rows 0,1 = header
        assert_eq!(state.tab_position_at_line(1), None);
        // rows 2,3 = running agent tab (position 0)
        assert_eq!(state.tab_position_at_line(2), Some(0));
        assert_eq!(state.tab_position_at_line(3), Some(0));
        // row 4 = plain tab (position 1)
        assert_eq!(state.tab_position_at_line(4), Some(1));
        assert!(state.tab_position_at_line(5).is_none());
    }

    #[test]
    fn agent_tab_pending_with_msg_occupies_two_lines() {
        // New line-2 rule: pending + msg → 2 lines (mark + activity). Old 3-line case gone.
        let mut state = make_state_with_tabs(&[(0, "agent", false), (1, "plain", false)]);
        state.runtime.radar.set_tab_panes_for_position(0, vec![pane(10)]);
        apply_payload_with_msg(&mut state, 10, Status::Pending, 1, "approve?"); // pending+msg → 2
        assert_eq!(state.tab_position_at_line(1), None); // header
        assert_eq!(state.tab_position_at_line(2), Some(0)); // line 1
        assert_eq!(state.tab_position_at_line(3), Some(0)); // line 2 (mark + activity)
        assert_eq!(state.tab_position_at_line(4), Some(1)); // plain tab (was line 5 before)
        assert!(state.tab_position_at_line(5).is_none());
    }

    #[test]
    fn switch_tab_to_index_is_position_plus_one() {
        // Confirm that tab_position_at_line returns the 0-based position,
        // so the caller must add 1 before calling switch_tab_to.
        // With the always-on header, tabs start at line 2.
        let mut state = make_state_with_tabs(&[(0, "first", false), (1, "second", false)]);
        // Position 0 → switch_tab_to(0 + 1 = 1)
        assert_eq!(state.tab_position_at_line(2), Some(0));
        // Position 1 → switch_tab_to(1 + 1 = 2)
        assert_eq!(state.tab_position_at_line(3), Some(1));
    }

    #[test]
    fn idle_rail_still_has_header_click_offset_by_two() {
        // All-idle tabs still render the always-on header (2 lines), so the
        // first tab maps to line 2, not line 0.
        let mut state = make_state_with_tabs(&[(0, "a", false), (1, "b", false)]);
        assert_eq!(state.tab_position_at_line(0), None); // header
        assert_eq!(state.tab_position_at_line(1), None); // header
        assert_eq!(state.tab_position_at_line(2), Some(0));
        assert_eq!(state.tab_position_at_line(3), Some(1));
    }

    #[test]
    fn click_mapping_is_fold_aware_when_overflowing() {
        // 6 idle tabs + one pending (urgent) tab at the end; tiny height forces folding.
        let mut state = make_state_with_tabs(&[
            (0, "a", false),
            (1, "b", false),
            (2, "c", false),
            (3, "d", false),
            (4, "e", false),
            (5, "pinky", false),
        ]);
        state.runtime.radar.set_tab_panes_for_position(5, vec![pane(50)]);
        apply_payload(&mut state, 50, Status::Pending, 1); // pending → non-idle, kept
        state.runtime.last_render_height = 6; // body_budget = 4

        // header = lines 0,1. Idle rows fold; only the pending tab (position 5) is kept.
        // It renders right after the header.
        assert_eq!(state.tab_position_at_line(0), None); // header
        assert_eq!(state.tab_position_at_line(1), None); // header
        assert_eq!(state.tab_position_at_line(2), Some(5)); // the kept non-idle tab
    }

    #[test]
    fn click_mapping_unchanged_when_not_overflowing() {
        // Large height → no folding → same as plain position order (offset by 2-line header).
        let mut state = make_state_with_tabs(&[(0, "a", false), (1, "b", false)]);
        state.runtime.last_render_height = 100;
        assert_eq!(state.tab_position_at_line(0), None);
        assert_eq!(state.tab_position_at_line(2), Some(0));
        assert_eq!(state.tab_position_at_line(3), Some(1));
    }

    #[test]
    fn state_defaults_glyphs_to_plain_and_ungranted() {
        let s = State::default();
        assert_eq!(s.runtime.config.glyphs, crate::status::GlyphSet::Plain);
        assert!(!s.runtime.permission.granted());
    }

    #[test]
    fn sidebar_stays_selectable_until_permissions_are_granted() {
        // Zellij's permission prompt for a visible status/sidebar plugin is
        // tied to the pane that called `request_permission` — the pane must
        // stay selectable while the y/n answer is pending (`is_requesting`
        // is one of the two inputs to `PluginRuntime::desired_selectable`;
        // the other is a terminal neighbor in the rail's tab), and peer
        // sidebars that never asked go passive once a manifest shows one.
        let mut s = State::default();
        assert!(!s.runtime.permission.is_requesting(), "peer sidebars that did not request permission stay passive");

        s.record_permission_request_started();
        assert!(
            s.runtime.permission.is_requesting(),
            "the sidebar that owns the first-run prompt must remain focusable"
        );

        s.record_permission_result(true);
        assert!(
            !s.runtime.permission.is_requesting(),
            "after permissions are granted the sidebar returns to passive mode"
        );

        let mut s = State::default();
        s.record_permission_result(false);
        assert!(
            !s.runtime.permission.is_requesting(),
            "after permissions are denied the prompt is gone (the rail stays selectable: no manifest ever shows it a terminal neighbor)"
        );
    }

    #[test]
    fn multi_pane_inactive_collapses_to_header_plus_count() {
        // A tab with 2 panes both running, NOT active → new line-per-pane design:
        // row_lines = 1 header + 2 pane lines = 3 lines.
        let mut state = make_state_with_tabs(&[(0, "team", false), (1, "plain", false)]);
        state.runtime.radar.set_tab_panes_for_position(0, vec![pane(10), pane(11)]);
        apply_payload(&mut state, 10, Status::Running, 1);
        apply_payload(&mut state, 11, Status::Running, 1);
        // header = lines 0,1
        assert_eq!(state.tab_position_at_line(0), None);
        assert_eq!(state.tab_position_at_line(1), None);
        // 3-line multi-pane tab at position 0: header (line 2) + pane10 (line 3) + pane11 (line 4).
        assert_eq!(state.tab_position_at_line(2), Some(0));
        assert_eq!(state.tab_position_at_line(3), Some(0));
        assert_eq!(state.tab_position_at_line(4), Some(0));
        // Header line is tab-only; pane lines each target their pane.
        assert_eq!(state.target_at_line(2), Some((0, None)));
        assert_eq!(state.target_at_line(3), Some((0, Some(10))));
        assert_eq!(state.target_at_line(4), Some((0, Some(11))));
        // plain tab at position 1: line 5
        assert_eq!(state.tab_position_at_line(5), Some(1));
        assert!(state.tab_position_at_line(6).is_none());
    }

    #[test]
    fn multi_pane_child_line_click_targets_that_pane() {
        // 3-pane tab: pane 10 Pending, 11 + 12 Running. New line-per-pane design:
        // header (line 2) + pane 10 (line 3) + pane 11 (line 4) + pane 12 (line 5) = 4 lines.
        let mut state = make_state_with_tabs(&[(0, "monorepo", false), (1, "plain", false)]);
        state.runtime.radar.set_tab_panes_for_position(0, vec![pane(10), pane(11), pane(12)]);
        apply_payload_with_msg(&mut state, 10, Status::Pending, 1, "run migration?");
        apply_payload(&mut state, 11, Status::Running, 1);
        apply_payload(&mut state, 12, Status::Running, 1);
        state.runtime.last_render_height = 100;
        // header = lines 0,1
        assert_eq!(state.target_at_line(2), Some((0, None)), "header → tab only");
        // pane lines each target their pane (in position order)
        assert_eq!(state.target_at_line(3), Some((0, Some(10))), "pane line → pane 10");
        assert_eq!(state.target_at_line(4), Some((0, Some(11))), "pane line → pane 11");
        assert_eq!(state.target_at_line(5), Some((0, Some(12))), "pane line → pane 12");
        // plain tab follows at line 6
        assert_eq!(state.tab_position_at_line(6), Some(1));
        assert!(state.tab_position_at_line(7).is_none());
    }

    #[test]
    fn multi_pane_active_all_children_clickable() {
        // Active tab → ALL panes expand; each child line targets its pane.
        let mut state = make_state_with_tabs(&[(0, "team", true)]);
        state.runtime.radar.set_tab_panes_for_position(0, vec![pane(20), pane(21)]);
        apply_payload(&mut state, 20, Status::Running, 1);
        apply_payload(&mut state, 21, Status::Done, 1);
        state.runtime.last_render_height = 100;
        // header(2) + 2 children, no collapse.
        assert_eq!(state.target_at_line(2), Some((0, None)), "header");
        assert_eq!(state.target_at_line(3), Some((0, Some(20))), "child 0 → pane 20");
        assert_eq!(state.target_at_line(4), Some((0, Some(21))), "child 1 → pane 21");
        assert!(state.tab_position_at_line(5).is_none());
    }

    #[test]
    fn multi_pane_active_tracked_and_untracked_children_clickable() {
        // Only 1 tracked pane (pane 20 Running); pane 21 is untracked.
        // is_multi_pane = false → single-pane path: header + detail line (2 lines total).
        // The untracked pane has no rendered line; the detail line targets the
        // tab's one tracked pane (20), same as the multi-pane tree rows — a
        // click routes to `Effect::ShowPane`, not `SwitchTab`.
        let mut state = make_state_with_tabs(&[(0, "team", true)]);
        state.runtime.radar.set_tab_panes_for_position(0, vec![pane(20), pane(21)]);
        apply_payload(&mut state, 20, Status::Running, 1);
        state.runtime.last_render_height = 100;

        assert_eq!(state.target_at_line(2), Some((0, None)), "header");
        assert_eq!(state.target_at_line(3), Some((0, Some(20))), "detail line targets the single tracked pane");
        assert!(state.tab_position_at_line(4).is_none(), "tab ends after 2 lines");

        let rows = state.build_rows();
        assert_eq!(rows[0].display.progress.total, 1, "untracked pane is not progress");
        assert_eq!(rows[0].display.panes.len(), 2, "both live panes are visible in display");
    }

    /// Click mapping uses PLANNED (compressed) line counts, not uncompressed
    /// `row_lines`. When Running rows are compressed to 1 line each under
    /// pressure, each click mapping span must shrink accordingly.
    #[test]
    fn click_mapping_matches_compressed_layout() {
        // Setup: 3 Running tabs (each normally 2 lines) + 1 Pending-with-msg (now 2 lines).
        // Uncompressed body = 3×2 + 2 = 8 lines. Header = 2.
        // height = 7 → body_budget = 5.
        // plan_overflow compresses Running rows to 1 line; Pending stays at 2.
        // Final plan spans: [1, 1, 1, 2] → total = 5.
        // After header (lines 0-1):
        //   position 0 (Running, 1 line) → line 2
        //   position 1 (Running, 1 line) → line 3
        //   position 2 (Running, 1 line) → line 4
        //   position 3 (Pending, 2 lines) → lines 5-6
        let mut state =
            make_state_with_tabs(&[(0, "r0", false), (1, "r1", false), (2, "r2", false), (3, "urgent", false)]);
        state.runtime.radar.set_tab_panes_for_position(0, vec![pane(10)]);
        state.runtime.radar.set_tab_panes_for_position(1, vec![pane(11)]);
        state.runtime.radar.set_tab_panes_for_position(2, vec![pane(12)]);
        state.runtime.radar.set_tab_panes_for_position(3, vec![pane(13)]);
        apply_payload(&mut state, 10, Status::Running, 1);
        apply_payload(&mut state, 11, Status::Running, 1);
        apply_payload(&mut state, 12, Status::Running, 1);
        apply_payload_with_msg(&mut state, 13, Status::Pending, 1, "please approve");
        state.runtime.last_render_height = 7; // body_budget = 5

        // Header lines
        assert_eq!(state.tab_position_at_line(0), None);
        assert_eq!(state.tab_position_at_line(1), None);
        // Each Running tab compressed to 1 line → one click per tab.
        assert_eq!(state.tab_position_at_line(2), Some(0));
        assert_eq!(state.tab_position_at_line(3), Some(1));
        assert_eq!(state.tab_position_at_line(4), Some(2));
        // Pending tab gets 2 lines (mark + activity; fits without compression).
        assert_eq!(state.tab_position_at_line(5), Some(3));
        assert_eq!(state.tab_position_at_line(6), Some(3));
        // Nothing beyond.
        assert_eq!(state.tab_position_at_line(7), None);
    }

    /// Lockstep: a multi-pane tab's rendered line count must consume exactly
    /// `row_lines()` lines in the click-mapping, so a plain tab immediately
    /// after it maps to the correct line.
    #[test]
    fn multi_pane_tree_click_mapping_lockstep() {
        use crate::status::Status;
        // 3-pane tab (all tracked) → new line-per-pane design:
        // header (line 2) + pane10 (line 3) + pane11 (line 4) + pane12 (line 5) = 4 lines.
        // Followed by a plain tab at line 6.
        let mut state = make_state_with_tabs(&[(0, "team", false), (1, "plain", false)]);
        state.runtime.radar.set_tab_panes_for_position(0, vec![pane(10), pane(11), pane(12)]);
        apply_payload_with_msg(&mut state, 10, Status::Pending, 1, "approve?");
        apply_payload(&mut state, 11, Status::Running, 1);
        apply_payload(&mut state, 12, Status::Running, 1);
        state.runtime.last_render_height = 100;
        // header = lines 0,1. Multi-pane tab at position 0: lines 2-5. Plain tab: line 6.
        assert_eq!(state.tab_position_at_line(0), None, "header line 0");
        assert_eq!(state.tab_position_at_line(1), None, "header line 1");
        assert_eq!(state.tab_position_at_line(2), Some(0), "tab header line");
        assert_eq!(state.tab_position_at_line(3), Some(0), "pane 10 line");
        assert_eq!(state.tab_position_at_line(4), Some(0), "pane 11 line");
        assert_eq!(state.tab_position_at_line(5), Some(0), "pane 12 line");
        // The plain tab must start at line 6, not earlier.
        assert_eq!(state.tab_position_at_line(6), Some(1), "plain tab follows the multi-pane tab");
        assert_eq!(state.tab_position_at_line(7), None, "beyond last tab");
    }

    // ── Density click-mapping tests ──

    #[test]
    fn click_mapping_accounts_for_gaps_comfortable() {
        // Comfortable density, large height → spacing.gap = 1.
        // 2 idle tabs, header=2 lines.
        // Layout: header(2) | tab0 content(1) | tab0 gap(1) | tab1 content(1) | tab1 gap(1)
        // Lines:   0,1      | 2               | 3           | 4               | 5
        //
        // The gap is EXTERNAL separation, so the gap line (3) maps to None — only
        // the owned content rows belong to a tab. Tab 1 starts at line 4.
        let mut state = make_state_with_tabs(&[(0, "a", false), (1, "b", false)]);
        state.runtime.last_render_height = 100; // large → no overflow
        state.runtime.config = config::Config { density: config::Density::Comfortable, ..config::Config::default() };

        // header lines
        assert_eq!(state.tab_position_at_line(0), None, "header line 0");
        assert_eq!(state.tab_position_at_line(1), None, "header line 1");
        // tab 0 content line
        assert_eq!(state.tab_position_at_line(2), Some(0), "tab 0 content line");
        // tab 0 gap line — external separation, maps to None
        assert_eq!(state.tab_position_at_line(3), None, "tab 0 gap line maps to None");
        // tab 1 content line starts at 4
        assert_eq!(state.tab_position_at_line(4), Some(1), "tab 1 content line");
        // tab 1 gap line — external separation, maps to None
        assert_eq!(state.tab_position_at_line(5), None, "tab 1 gap line maps to None");
        // beyond
        assert_eq!(state.tab_position_at_line(6), None, "beyond last tab");
    }

    #[test]
    fn click_mapping_compact_no_gaps() {
        // Compact density → no gaps, tabs are adjacent.
        // 2 idle tabs, header=2 lines.
        // Lines: 0,1 header | 2 tab0 | 3 tab1
        let mut state = make_state_with_tabs(&[(0, "a", false), (1, "b", false)]);
        state.runtime.last_render_height = 100;
        state.runtime.config = config::Config { density: config::Density::Compact, ..config::Config::default() };

        assert_eq!(state.tab_position_at_line(0), None);
        assert_eq!(state.tab_position_at_line(1), None);
        assert_eq!(state.tab_position_at_line(2), Some(0));
        assert_eq!(state.tab_position_at_line(3), Some(1));
        assert_eq!(state.tab_position_at_line(4), None);
    }

    #[test]
    fn click_mapping_cards_one_line_header() {
        // Cards density: the carded hero is a single " RADAR …" title line (no
        // rule), so the header occupies ONE line, not two. Cards now carries
        // gap = 1 (trailing rail_bg row after each card).
        // The gap rows map to None (they are external separation, not owned by
        // a tab). The click mapping must stay in lockstep with render():
        //   line 0  → header (1 line, no rule) → None
        //   line 1  → tab 0 content            → Some(0)
        //   line 2  → tab 0 gap (rail)         → None
        //   line 3  → tab 1 content            → Some(1)
        //   line 4  → tab 1 gap (rail)         → None
        //   line 5  → None (beyond last tab)
        let mut state = make_state_with_tabs(&[(0, "a", false), (1, "b", false)]);
        state.runtime.last_render_height = 100;
        state.runtime.config = config::Config { density: config::Density::Cards, ..config::Config::default() };

        assert_eq!(state.tab_position_at_line(0), None, "1-line header in Cards");
        assert_eq!(state.tab_position_at_line(1), Some(0), "tab 0 content");
        assert_eq!(state.tab_position_at_line(2), None, "tab 0 gap row → None");
        assert_eq!(state.tab_position_at_line(3), Some(1), "tab 1 content");
        assert_eq!(state.tab_position_at_line(4), None, "tab 1 gap row → None");
        assert_eq!(state.tab_position_at_line(5), None, "beyond last tab");
    }

    #[test]
    fn click_mapping_cards_multi_line_card_and_post_content_row() {
        // Exercises the gap semantics explicitly with a multi-line card so the
        // boundary between one card's last content row and the gap row is clear.
        //   header(1) | tab0 content×2 | tab0 gap(1) | tab1 content(1) | tab1 gap(1)
        //   line 0    | lines 1,2      | line 3      | line 4          | line 5
        // The gap row (line 3) maps to None; the tab 1 content (line 4) maps to Some(1).
        // tab 0 is a Running tab WITH detail → 2 content lines.
        let mut state = make_state_with_tabs(&[(0, "work", false), (1, "b", false)]);
        // Make tab 0 a running agent with a detail line (2 content lines).
        state.runtime.radar.set_tab_panes_for_position(0, vec![pane(10)]);
        apply_payload(&mut state, 10, Status::Running, 1);
        state.runtime.last_render_height = 100;
        state.runtime.config = config::Config { density: config::Density::Cards, ..config::Config::default() };

        assert_eq!(state.tab_position_at_line(0), None, "header");
        assert_eq!(state.tab_position_at_line(1), Some(0), "tab 0 content line 1");
        assert_eq!(state.tab_position_at_line(2), Some(0), "tab 0 content line 2");
        assert_eq!(state.tab_position_at_line(3), None, "tab 0 gap row → None");
        assert_eq!(state.tab_position_at_line(4), Some(1), "tab 1 content");
        assert_eq!(state.tab_position_at_line(5), None, "tab 1 gap row → None");
        assert_eq!(state.tab_position_at_line(6), None, "beyond last tab");
    }

    // ── mouse_click → Effect end-to-end tests ──

    #[test]
    fn mouse_click_on_tab_row_emits_switch_tab_effect() {
        use runtime::Effect;
        // 2 idle tabs, permission granted, render at width 80.
        // header=2 lines (Compact), tab 0 at line 2.
        let mut state = make_state_with_tabs(&[(0, "first", false), (1, "second", false)]);
        state.runtime.permission = crate::runtime::PermissionState::Resolved { granted: true };
        let _ = state.runtime.render(100, 80);
        // line 2 is the first tab content row (lines 0-1 are the header)
        let outcome = state.runtime.mouse_click(2, 0);
        assert_eq!(outcome.effects, vec![Effect::SwitchTab { position: 0 }]);
    }

    #[test]
    fn mouse_click_without_permission_is_inert() {
        let mut state = make_state_with_tabs(&[(0, "first", false), (1, "second", false)]);
        state.runtime.permission = crate::runtime::PermissionState::default();
        let _ = state.runtime.render(100, 80);
        assert!(state.runtime.mouse_click(2, 0).effects.is_empty());
    }

    // ── Click round-trip proptest ──

    proptest::proptest! {
        #[test]
        fn click_round_trip_hits_drawn_target(
            n_tabs in 1usize..6,
            active_idx in 0usize..6,
            // Draw statuses straight from the `statuses!` table, so a new variant
            // is covered by this proptest automatically — no ladder to update.
            statuses in proptest::collection::vec(
                proptest::sample::select(Status::ALL.to_vec()),
                1..6,
            ),
            width in proptest::sample::select(vec![24usize, 40, 80]),
        ) {
            // Build a state with n_tabs, each tab one pane with a status.
            let specs: Vec<(usize, &str, bool)> = (0..n_tabs)
                .map(|i| (i, "t", i == active_idx % n_tabs))
                .collect();
            let mut state = make_state_with_tabs(&specs);
            for (i, &st) in statuses.iter().take(n_tabs).enumerate() {
                // one pane per tab; pane id = tab index
                apply_payload(&mut state, i as u32, st, 1);
                state.runtime.radar.set_tab_panes_for_position(i, vec![pane(i as u32)]);
            }
            // Render through the production path at the given width; this populates last_rendered.
            state.runtime.permission = crate::runtime::PermissionState::Resolved { granted: true };
            let ansi = state.runtime.render(200, width);
            let rail_lines = ansi.matches('\n').count();
            // For every drawn line, target_at_line must resolve to a real tab or None.
            // A resolved tab must be in-range.
            let mut resolved = 0usize;
            for line in 0..(rail_lines as isize + 2) {
                if let Some((tab, _pane)) = state.runtime.target_at_line(line) {
                    proptest::prop_assert!(
                        tab < n_tabs,
                        "resolved tab {} out of range {} (width={}, line={})", tab, n_tabs, width, line
                    );
                    resolved += 1;
                }
            }
            proptest::prop_assert!(
                resolved >= 1,
                "test resolved no targets at all — setup may be broken (n_tabs={}, rail_lines={}, width={})",
                n_tabs, rail_lines, width
            );
        }
    }
}
