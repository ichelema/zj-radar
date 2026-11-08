//! The completion ledger: a small ring of "what finished earlier" entries that
//! cards hand off to when a Done/Error leaves the rail (spec §4). Pure data +
//! policy, memory only: `RadarState` feeds it the native completion edges
//! (wired in `radar_state.rs`); the renderer consumes prepared lines.
//!
//! Consumers, by seam: `push` via `RadarState::ledger_native` (every edge
//! that retires a card); `entries`/`format_age` via
//! the rail's bottom region (`render::render_bottom` through
//! `RadarState::ledger_lines`); `any_unsaturated`/`SATURATE_S` via
//! `PluginRuntime::desired_cadence` (the Slow-vs-disarm cadence pick); and
//! `is_empty` via `PluginRuntime::render`'s onboarding-vs-rail choice.

use crate::payload::{sanitize, MAX_MSG_CHARS, MAX_TAB_NAME_CHARS};
use crate::radar_state::TabId;
use std::collections::VecDeque;

/// Storage cap for the ring — bounds memory, NOT what
/// the rail shows (`render::LEDGER_DISPLAY_CAP` caps that at 10 rows).
pub(crate) const LEDGER_CAP: usize = 32;
/// Two entries within this many seconds and equal (pane, outcome, label) are
/// the same event reported twice (spec §4.3).
pub(crate) const MERGE_WINDOW_S: u64 = 4;
/// Ages ≥ this render as the frozen "1h+" — the display never changes again,
/// which is what lets the idle timer fully disarm (spec §4.4/§10).
pub(crate) const SATURATE_S: u64 = 3600;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LedgerOutcome {
    Done,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LedgerEntry {
    pub at_epoch_s: u64,
    pub outcome: LedgerOutcome,
    pub tab_id: TabId,
    pub tab_name: String,
    pub label: String,
    pub pane_id: u32,
}

impl LedgerEntry {
    /// Scrub the free-text fields (strip controls/ANSI, cap), never leaving
    /// one blank: an all-control-chars value falls back to a fixed token.
    pub(crate) fn sanitized(mut self) -> LedgerEntry {
        self.tab_name = sanitized_or(&self.tab_name, MAX_TAB_NAME_CHARS, "tab");
        self.label = sanitized_or(&self.label, MAX_MSG_CHARS, "done");
        self
    }
}

/// THE never-blank scrub both free-text ledger columns share: sanitize
/// (strip controls/ANSI, cap at `max_chars`), trim, and fall back to
/// `fallback` for an empty result — a ledger row never renders a blank
/// column.
fn sanitized_or(text: &str, max_chars: usize, fallback: &str) -> String {
    let clean = sanitize(text, max_chars);
    let trimmed = clean.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

/// Same (pane, outcome, label) within `MERGE_WINDOW_S` — the match `push`
/// dedups on (spec §4.3).
fn is_same_event(a: &LedgerEntry, b: &LedgerEntry) -> bool {
    a.pane_id == b.pane_id
        && a.outcome == b.outcome
        && a.label == b.label
        && a.at_epoch_s.abs_diff(b.at_epoch_s) <= MERGE_WINDOW_S
}

#[derive(Default)]
pub(crate) struct Ledger {
    /// Newest at front.
    entries: VecDeque<LedgerEntry>,
}

impl Ledger {
    /// Append unless a matching entry (same pane/outcome/label within
    /// MERGE_WINDOW_S) is already present — the dedup guard that stops the
    /// two edges of one completion (`native_changed`) double-appending. The
    /// EXISTING entry's stamp is kept: first-seen is the honest completion time.
    pub(crate) fn push(&mut self, entry: LedgerEntry) -> bool {
        if self.entries.iter().any(|existing| is_same_event(existing, &entry)) {
            return false;
        }
        self.entries.push_front(entry);
        self.entries.truncate(LEDGER_CAP);
        true
    }

    /// newest first
    pub(crate) fn entries(&self) -> impl Iterator<Item = &LedgerEntry> {
        self.entries.iter()
    }

    /// Consumed (via `RadarState::ledger_is_empty`) by `PluginRuntime::render`:
    /// zero tracked tabs AND an empty ledger is the only state that shows the
    /// onboarding face instead of the rail.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Any entry still younger than SATURATE_S? (Drives the Slow cadence.)
    pub(crate) fn any_unsaturated(&self, now_epoch_s: u64) -> bool {
        self.entries.iter().any(|e| now_epoch_s.saturating_sub(e.at_epoch_s) < SATURATE_S)
    }
}

/// Relative age per the spec §4.4 table. Negative (clock skew) → "<1m".
/// The ledger presents the sub-minute band (a fresh completion still reads as
/// an age); the rail's time tags render nothing there — one band, two faces.
pub(crate) fn format_age(at_epoch_s: u64, now_epoch_s: u64) -> String {
    minute_tag(now_epoch_s.saturating_sub(at_epoch_s)).unwrap_or_else(|| "<1m".to_string())
}

/// THE minute band every age display shares — the ledger's `format_age` and
/// the rail's wait/run tags (`render::{wait_tag, run_tag}`): `None` under a
/// minute, whole minutes below the saturate window, frozen at `1h+` past it.
/// `age` is in seconds — or Fast-cadence ticks, which advance at ~1 Hz (see
/// `render::run_tag`).
/// The freeze is load-bearing for cadence disarm — the rendered age stops
/// changing exactly when `any_unsaturated` goes false and the Slow timer
/// disarms — so the band must not exist twice.
pub(crate) fn minute_tag(age: u64) -> Option<String> {
    if age < 60 {
        None
    } else if age < SATURATE_S {
        Some(format!("{}m", age / 60))
    } else {
        Some("1h+".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pane_id: u32, outcome: LedgerOutcome, label: &str, at_epoch_s: u64) -> LedgerEntry {
        LedgerEntry {
            at_epoch_s,
            outcome,
            tab_id: TabId::new(0),
            tab_name: "tab".into(),
            label: label.to_string(),
            pane_id,
        }
    }

    #[test]
    fn sanitized_scrubs_disk_loaded_free_text() {
        // Entries loaded off disk may predate intake sanitization; the scrub
        // strips ANSI/controls and folds newlines like every other intake.
        let e = entry(1, LedgerOutcome::Done, "did\nthings\x1b[31m", 5);
        let e = LedgerEntry { tab_name: "ev\x1b[2Jil\ntab".into(), ..e }.sanitized();
        assert_eq!(e.tab_name, "evil tab");
        assert_eq!(e.label, "did things");

        // A label that is NOTHING BUT control chars scrubs to empty — fall
        // back to an origin-NEUTRAL "done": origin is not retained on a
        // disk-loaded entry, so this seam must not guess "turn done" (an
        // agent-turn claim) for what may have been a command completion.
        let e = entry(1, LedgerOutcome::Done, "\x1b[2J\x07", 5).sanitized();
        assert_eq!(e.label, "done");
    }

    #[test]
    fn push_dedups_within_window_and_caps_at_32() {
        let mut ledger = Ledger::default();
        assert!(ledger.is_empty());

        assert!(ledger.push(entry(1, LedgerOutcome::Done, "cargo test", 100)));
        // Same (pane, outcome, label) 2s later is the same event — rejected.
        assert!(!ledger.push(entry(1, LedgerOutcome::Done, "cargo test", 102)));
        assert_eq!(ledger.entries().count(), 1);
        assert_eq!(ledger.entries().next().unwrap().at_epoch_s, 100, "the original stamp is kept, not the dup's");

        // A different label on the same pane is a distinct event.
        assert!(ledger.push(entry(1, LedgerOutcome::Done, "cargo build", 103)));
        assert_eq!(ledger.entries().count(), 2);

        // 40 distinct pushes (far enough apart, or on distinct panes, to all
        // survive dedup) cap the ring at 32, keeping the newest.
        let mut ledger = Ledger::default();
        for i in 0..40u32 {
            assert!(ledger.push(entry(i, LedgerOutcome::Done, "cargo test", 1000 + i as u64 * 10)));
        }
        assert_eq!(ledger.entries().count(), LEDGER_CAP);
        let newest = ledger.entries().next().unwrap();
        assert_eq!(newest.pane_id, 39, "newest push stays at the front");
        assert!(ledger.entries().all(|e| e.pane_id >= 8), "the oldest 8 were evicted");
    }

    #[test]
    fn any_unsaturated_reflects_age() {
        let ledger = Ledger::default();
        assert!(!ledger.any_unsaturated(1_000_000), "an empty ledger has nothing left to age out");

        let mut ledger = Ledger::default();
        ledger.push(entry(1, LedgerOutcome::Done, "x", 1000));
        assert!(ledger.any_unsaturated(1000 + SATURATE_S - 1));
        assert!(!ledger.any_unsaturated(1000 + SATURATE_S));
    }

    #[test]
    fn age_format_table() {
        assert_eq!(format_age(100, 100), "<1m");
        assert_eq!(format_age(100, 159), "<1m");
        assert_eq!(format_age(100, 160), "1m");
        assert_eq!(format_age(100, 100 + 59 * 60), "59m");
        assert_eq!(format_age(100, 100 + 3600), "1h+");
        assert_eq!(format_age(200, 100), "<1m"); // negative skew
    }
}
