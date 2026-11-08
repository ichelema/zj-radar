//! Shared fixtures for the plugin's unit-test modules (the `lib.rs` tests,
//! `runtime/tests.rs`, and `radar_state/tests.rs`).
//!
//! These pin the canonical test vocabulary — position-derived tab ids, bare
//! panes, and native program attention — in one place so the suites can't
//! drift apart. Fixtures that
//! encode a *suite-specific* concern (State wiring in lib.rs, focus flags and
//! explicit tab ids in radar_state) stay as thin local wrappers over these,
//! so the difference reads as intentional at the definition site.

use crate::attention::{PaneAttention, Source};
use crate::radar_state::{RadarState, RadarTab, TabId};
use crate::rollup::TerminalPane;
use crate::status::Status;

/// A tab whose `TabId` is decoupled from its position — for suites that
/// exercise id-vs-position semantics. Everyone else wants [`tab`].
pub(crate) fn tab_with_id(id: usize, position: usize, name: &str, active: bool) -> RadarTab {
    RadarTab { floating_visible: false, id: TabId::new(id), position, name: name.into(), active, has_bell: false }
}

/// The common-case tab: `TabId` derived from its position (`position + 1`),
/// no bell.
pub(crate) fn tab(position: usize, name: &str, active: bool) -> RadarTab {
    tab_with_id(position + 1, position, name, active)
}

/// A bare terminal pane: only the id set — unfocused, no exit, no title.
pub(crate) fn pane(id: u32) -> TerminalPane {
    TerminalPane { id, ..TerminalPane::default() }
}

/// The native attention OSC 7501 reports for a pane: `status` from a program
/// (`app` claude-code), `msg` as Zellij's display text. `since_ms` stamps the
/// completion / wait epoch (100 s).
pub(crate) fn program_attention(status: Status, msg: &str) -> PaneAttention {
    PaneAttention {
        status,
        source: Source::Program,
        text: msg.into(),
        app: Some("claude-code".into()),
        since_ms: 100_000,
        ..PaneAttention::default()
    }
}

/// Report `pane_id`'s native attention the way `PaneAttentionChanged` does.
/// Returns whether the pane's observation changed.
pub(crate) fn set_status(radar: &mut RadarState, pane_id: u32, status: Status, msg: &str, tick: u64) -> bool {
    radar.attention_changed(pane_id, program_attention(status, msg), tick)
}
