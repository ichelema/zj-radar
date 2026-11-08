# zj-radar: domain glossary

Names for the seams in zj-radar. Each entry says what the term means, where it
lives, and which seam it crosses. The mechanisms behind them are in
[`docs/design.md`](docs/design.md); this file stays a glossary.

## Rail

The rendered sidebar: a pinned left column listing every tab with per-tab
agent status. The rail seam is `render_rail(rows, ledger, opts) ->
RenderedRail` in `crates/plugin/src/render.rs`, with `onboarding(opts)` and
`needs_permission(opts, grant_hint)` as sibling faces for the no-agents-yet and
not-yet-granted states. Layout planning (overflow folding, card spacing, tree
expansion) is implementation behind the seam. Color is additive: stripping SGR
from the rail yields the same character grid, so layout and color test apart.
The exact grid is [`docs/rail-reference.md`](docs/rail-reference.md).

## RenderedRail

The rail seam's output: `ansi` paired with a same-height target map and a
same-height hotspot map. `target_at_line(line)` resolves a physical line to a
`RailTarget`; `hotspot_at(line, col)` resolves a glyph cell to a
`HotspotAction` (`Acknowledge` for `✓`). The runtime
caches the last `RenderedRail` and scores mouse clicks against it, so the rail
the user sees is the rail clicks resolve on.

## RailTarget

What a clickable line resolves to: a tab (`tab_position`), a pane (`pane_id`),
or a peer session (`session`, carrying its attention tab). Pane targets cover a
multi-pane tab's tree lines and a single-pane tab's pane and detail lines.
Header, gap, and idle-strip lines have none. The runtime turns a `RailTarget`
into a `SwitchTab` / `ShowPane` / `SwitchSession` effect.

## Lockstep

The rail's load-bearing invariant: `ansi`, `targets`, and `hotspots` stay in
exact 1:1 line correspondence. It is structural, not discipline-held:
`render_rail` builds one `Vec<Line>`, each line carrying its own target and
optional hotspot, and `RenderedRail::from_lines` derives all three vectors from
it. There is no separate height predictor to drift.

## RadarState

The plugin's session-state module (`crates/plugin/src/radar_state.rs`): the
current view of tabs, live panes, and Zellij's native per-pane attention
(`attention.rs`, from the manifest and `PaneAttentionChanged`), plus `TabNamer`
and the memory-only ledger. `RadarState::resolve` is the one place a pane's
observation is read; `tab_display` reads through it.

Focus never drives rail state: a pane's status is whatever Zellij reports, so
every tab's instance converges. Focus may shape paint, never state:
in Cards density the active tab's focused pane line takes the bright
`surface_active` band (`TabDisplay::focused_pane`, from the same
`PaneUpdate` every instance sees); see design.md → *Per-pane to per-tab
aggregation*.

## Status contract

The seam between Zellij and the plugin is Zellij's native attention (OSC 7501
program status, shell-command tracking): `PaneInfo::attention` and
`program_status` in the manifest, and `PaneAttentionChanged` for one pane,
mirrored by `attention::NativePane` and turned into a `TrackedObservation` by
`NativePane::observation`. Zellij decides which signal wins for a pane. The
plugin is a caller too: the `✓` gesture asks Zellij to mark the pane seen
(`Effect::MarkSeen` → `mark_session_pane_seen`) instead of mutating local
state, and the next manifest carries the result.

## Background task

Work an agent started that runs without its attention, reported as an OSC 7501
child record (`id`). Rendered as `┊` sub-lines under the pane line
(`render::emit_task_lines`); never feeds the roll-up, counts, or
notifications. Not the activity model's *Job*, which is a class of command.

## Information source

Anything that produces a per-pane observation. Today there is one: Zellij's
native attention, read from the pane manifest. An agent that emits OSC 7501
(`app`, `state`, `kind`, `progress`, `title`, `msg`) needs no adapter; a shell
command shows as an attention row with the command source (`Kind::Command`,
exit code from the attention).

## Tab roll-up

Per-pane to per-tab aggregation: `rollup::roll_up(panes, resolve) ->
TabDisplay` in `crates/plugin/src/rollup.rs`. Severity is `error > pending >
running > done > idle`; the highest-severity pane supplies the detail line,
with a bounded job outranking a steady row (service or remote) on ties. `roll_up` owns its output
vocabulary (`TabDisplay`, `PaneDisplay`, `PrimaryDetail`, `ProgressCounts`,
`ExitOutcome`); `resolve` is the closure `RadarState` hands it. Tab status is never derived from tab
names.

## Cadence

How often the one-shot timer re-fires (`PluginRuntime::desired_cadence`):
Fast (1 Hz) while there is tick-windowed work, subdivided into spinner frames
(`spinner_fps`) while a visible rail has full-speed work spinning — frames
repaint, but the tick still runs once a second — Slow (once a minute) while only
minute-granular ages change, and disarmed
otherwise. Service rows and remote rows never pin Fast.
Trigger lists: design.md → *Timer and cadence*.

## Render gate

Why an event repaints, or does not. Three layers keep repaints proportional to
change: intake no-ops (an attention report that changes no observation
reports a default `RadarChange`), the rows-diff gate (`project` drops a render
whose content key equals the last one drawn), and the visibility gate (a rail
whose tab Zellij reported hidden paints nothing while its state keeps
ticking). The first two rest on
`RadarState::generation` and the `rows()` memo; the last on
`Event::Visible`. A missed `touch()` on a new mutator is a stale-rail bug.
Detail: design.md → *Render gate*.

## Ledger

The completion history: a fixed-cap ring (`LEDGER_CAP` = 32) rendered as the
rail's trailing `─ earlier ─` region. `crates/plugin/src/ledger.rs` is pure
data and policy; `RadarState` wires every edge that retires a card into it
(`ledger_native`, from the manifest and `PaneAttentionChanged`) and prepares
`LedgerLine`s; the renderer only consumes them. The ring lives in memory only.
An observation enters when it stops being a card fact, never before. Entries
stamp epoch seconds, and age saturates at `1h+` so the timer can disarm.
Detail: design.md → *Ledger*.

## Tab naming

The policy that names tabs and remembers what it applied: `TabNamer::rename(tabs,
mode) -> Vec<TabRename>` in `crates/plugin/src/tab_namer.rs`, fed resolved
`TabFacts` by `RadarState::name_facts`. Candidates are one ordered list
(focused pane's repo, any repo, worktree-resolved cwd, title); an applied name
sticks while any pane still justifies it, and `Managed` never clobbers a manual
rename. Ownership is per tab: each instance names only the tab holding its own
plugin pane, resolved to a stable `TabId`. The once-per-pane cwd bootstrap
(`Effect::ResolveCwd`) feeds naming but is not naming policy.

## Cross-session presence

How one session's rail learns another's counts and tab tree: from Zellij's
`GlobalAttentionUpdate`, converted by `attention::PeerSession::presence` into
the `Presence` data carrier and fed to `Sessions` (`sessions.rs`), pure state
that derives the badge on demand. Zellij owns peer liveness; this session's own
row comes from its rail rows. Badge order and cycling order come from one
`ordered()`. Detail: design.md → *Cross-session badge*.

## Setup analysis

How `zj-radar setup` learns the state of the world: pure `analyze_*(&Env) ->
Facts` functions per target in `crates/cli/src/setup/analyze.rs`, fed
already-read values by the IO shell. `--check` and the install orchestrators
both project from `Facts`; the pure mutators (`edit_*`) share only the
low-level detectors in `setup/detect.rs`.
