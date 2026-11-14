# zj-radar design

How the sidebar works and why it is built this way. This describes the shipped
system; the history is in git and in
[`smart-tabs-postmortem.md`](smart-tabs-postmortem.md). Vocabulary is defined
in [`CONTEXT.md`](../CONTEXT.md); status and class semantics in
[`activity-model.md`](activity-model.md); the exact rendered grid in
[`rail-reference.md`](rail-reference.md).

## 1. Goal

Bring Cmux-style agent awareness into an existing Zellij setup without
changing keybindings, swap layouts, or config: an always-on left sidebar that
lists every tab and, for tabs running AI agents or commands, shows per-pane
state (working, waiting for you, done, error) and the last message, and jumps
to a tab on click.

The hard constraint: **push-driven, never poll-driven.** No polling, and no
blocking host queries on any per-event or per-tick path. Signals come from
pushed events (`TabUpdate`, `PaneUpdate`, `PaneAttentionChanged`,
`GlobalAttentionUpdate`, `CwdChanged`, `ModeUpdate`). There are two
exceptions, both for tab naming:

- `Effect::ResolveCwd`: one blocking `get_pane_cwd` per freshly opened pane,
  never repeated.
- `Effect::ResolveRepo`: one async `git remote get-url origin` per cwd, never
  repeated.

The predecessor polled every pane on every output event and melted a
many-agent session; the postmortem has the numbers.

## 2. Why OSC 7501

Zellij 0.46 computes per-pane attention itself. It owns the PTY, so it parses
two terminal signals:

- **OSC 7501, the Program Status Protocol.** A program reports its own state
  (`state`, `app`, `kind`, `progress`, `title`, `msg`, child records by `id`).
  The body is `key=value` pairs separated by `:`; `msg` and `title` are
  base64. Claude Code, Codex, pi, Opencode and omp emit it with no setup
  ([`producers.md`](producers.md)).
- **OSC 133 shell-command tracking.** A shell that marks its prompts lets
  Zellij see each command start, run, and exit with a code.

Zellij picks the signal that wins for each pane and hands the plugin the
result as structured data: `PaneInfo.attention` and `PaneInfo.program_status`
in the manifest, plus events for single-pane changes and for other sessions.
Its attention text already carries the kind label and progress
(`needs approval: …`, `working 40%: …`).

The rejected alternative is a private status pipe fed by per-agent adapters
(hooks, plugins, JS bridges). It needed one adapter per agent, a
backpressure-bound `zellij pipe` send on every hook, and shared files to
rehydrate new tabs and reach other sessions. Native attention removes all
three: the plugin is a pure view of what Zellij reports.

What it costs:

- Zellij ≥ 0.46.0 (`MIN_SUPPORTED_ZELLIJ`).
- OSC 7501 has no git branch or task label, so rows show neither.
- Opencode emits only `working` and `blocked`, never `done` or `error`
  ([anomalyco/opencode#60411](https://github.com/anomalyco/opencode/issues/60411)):
  a finished Opencode turn reads idle.
- omp needs a release that contains
  [can1357/oh-my-pi#17147](https://github.com/can1357/oh-my-pi/pull/17147);
  before it, omp's OSC 133 cleared its own `working`.

**Remote sessions come for free.** A remote Zellij with `attention.host_status`
(default `true`) emits an aggregated OSC 7501 for its session to its terminal.
A local pane running `ssh host` + `zellij attach` therefore carries
`app=zellij` attention like a local agent, and the plugin shows it as a remote
row (`Kind::Remote`).

## 3. Architecture

Thin Zellij-host glue around a deep, pure runtime plus pure state and
rendering. The plugin's domain modules have no `zellij-tile` dependency
(`zellij-tile` is `cfg(target_arch = "wasm32")`-scoped): `lib.rs` copies raw
events into repo-owned types and applies the ordered effects the runtime
returns.

The repo is a two-crate workspace: `crates/plugin` (the wasm sidebar) and
`crates/cli` (the host binary: `setup`, `run`, `update`, plus a hidden no-op
`notify` so hooks left by older releases exit 0). Filenames below live under
`crates/plugin/src/`.

```
┌ Zellij 0.46 (owns the PTY) ─────────────────────────────────────────────┐
│ agent  ─OSC 7501─┐                                                      │
│ shell  ─OSC 133──┼─► attention tracker ─► PaneUpdate / TabUpdate        │
│                  │                        (active tab's plugins only)   │
│                  │                     ─► PaneAttentionChanged          │
│                  │                        (every subscribed instance)   │
│ other sessions ──┘                     ─► GlobalAttentionUpdate         │
└───────────────────────────┬─────────────────────────────────────────────┘
                            ▼
┌ zj-radar plugin (Rust → wasm32-wasip1) ────────────────────────────────┐
│  lib.rs: Zellij adapter                                                │
│    Event/PaneInfo/TabInfo/PaneAttention ⇄ repo-owned inputs/effects    │
│                                                                        │
│  runtime.rs: PluginRuntime                                             │
│    lifecycle, permissions, timers, naming, config/cmd pipes, mouse     │
│    output: Outcome { render, effects: Vec<Effect> }                    │
│                                                                        │
│  attention.rs: mirrors Zellij's attention types; builds the            │
│    TrackedObservation per pane, and the badge's SessionSummary per     │
│    peer                                                                │
│                                                                        │
│  radar_state.rs / rollup.rs / tab_namer.rs: state + tab model          │
│    native observations, roll_up(tab), TabNamer                         │
│                                                                        │
│  ledger.rs / sessions.rs: completion ring; cross-session peers         │
│  status.rs / kind.rs / observation.rs / task.rs / payload.rs:          │
│    shared vocabulary and sanitizing                                    │
│  control.rs / config.rs / theme.rs: cmd + config pipes, colors         │
│                                                                        │
│  render.rs: pure rail renderer                                         │
│    render_rail(rows, ledger, opts) -> RenderedRail                     │
│    (onboarding + needs_permission faces)                               │
│    owns layout, overflow, ANSI, click-target + hotspot lockstep        │
└────────────────────────────────────────────────────────────────────────┘
        │ Effects: RequestPermission, SetSelectable, SetTimeout,
        │ RenameTab, SwitchTab, ShowPane, ResolveCwd, ResolveRepo,
        │ SwitchSession, MarkSeen
        ▼
```

Host-coupled code stays thin. Lifecycle decisions live in `PluginRuntime`,
attention policy in `attention.rs`, layout and click decisions in
`RenderedRail`, so the core is unit-testable with `cargo test`. The external
seams are Zellij's attention data and the `zj_radar.cmd.v1` /
`zj_radar.config.v1` pipes ([`configuration.md`](configuration.md)).

## 4. Lifecycle

### 4.1 Attention → status

`lib.rs` maps Zellij's `AttentionStatus` (its effective status, so a muted
pane reads idle) into the rail's `Status`:

| Zellij | Rail |
|---|---|
| `Working` | `Running` |
| `NeedsYou` | `Pending` |
| `Done` | `Done` |
| `Failed` | `Error` |
| `Idle`, `Quiet`, `NewOutput` | `Idle` |

`NativePane::observation` turns one pane's attention and records into a
`TrackedObservation`:

- **Origin and kind.** A shell command, command pane, or foreground process
  is `ObservationOrigin::Command` with `Kind::Command`, or `Kind::Server` when
  its text has a service word (`attention::command_kind`). Anything else is
  `ObservationOrigin::Program`, with the kind from the OSC 7501 `app`
  (`attention::kind_for_app`): `claude-code` is
  Claude, `zellij` is Remote, and any other token goes through
  `Kind::from_source` (`codex`, `pi`, `server`, `build`, …; unknown is
  `Other`).
- **Message.** Zellij's attention text. With no text, the root record's
  `title`, else `msg`, else `id`.
- **Child-only panes.** Zellij raises a pane from a child record only for
  `blocked` or `error` (`deciding_record`), so a pane whose only records are
  `working` children (Opencode) reads idle in Zellij. When the attention
  source is not OSC 7501 and there is no root record, the observation takes
  the most severe record instead.
- **Background tasks.** Child records with an `id` (except one that decided
  the status) become `BgTask`s, rendered as `┊` sub-lines under the pane
  line. They never feed the roll-up or the counts.
- **Nothing to show.** An idle pane with no records has no observation.

An unchanged status keeps its `last_change_tick`, so the spinner's ease and
the rows memo hold across manifests. The pending wait tag and completion
stamp come from Zellij's `since_ms`. Free text is sanitized before it reaches
the grid (`TrackedObservation::sanitized`).

### 4.2 Per-pane to per-tab aggregation

Multi-agent tabs exist (four Claude panes in one tab), so tab state cannot come
from names. Observations key by pane id; `PaneUpdate`'s manifest gives the
pane → tab map. `rollup::roll_up` aggregates:

- **Severity:** `error > pending > running > done > idle`. Error is highest so
  failures never hide behind "working".
- **Counts:** `total` is the panes in the tab with an observation; `done` is
  those currently `done`; `pending` those currently `pending`.
- **Primary detail:** the highest-severity pane. On ties a bounded job
  outranks a steady row (`Kind::is_steady`: a `server` or a remote session),
  then the most recent change wins.
- **Focused pane:** `TabDisplay::focused_pane` is render-only. It paints the
  active tab's focused pane line on a brighter surface and feeds nothing else.

A pane gone from the manifest loses its observation at once. If it showed a
completion, the ledger files it under the tab it was last seen in (§9).

### 4.3 Focus never drives state

The plugin never derives state from focus: focus is per client, and only the
active tab's instances see it. A pane's status is whatever Zellij reports, and
`PaneAttentionChanged` reaches every instance, hidden tabs included, so all
tabs converge.

Zellij itself uses focus: it marks a focused pane's `Done` or `Error` seen
almost at once, so that status never shows as the pane's. Zellij keeps the
deciding record as `PaneAttention::remembered`, and the ledger files the
completion from there.

**Acknowledge is a Zellij call.** The `✓` hotspot (on a tab header or pane
line with a program-origin `Pending`) and a right-click on a pane or tab row
(blocked upstream for now, §5 *Click targeting*) emit `Effect::MarkSeen` for each pending pane, which calls
`mark_session_pane_seen`. The plugin mutates nothing: every instance
converges on the attention change Zellij sends back. `MarkSeen` addresses
panes by session, so it is inert until `ModeUpdate` has supplied the session
name.

## 5. Plugin ↔ Zellij wiring

**Permissions.** `load()` requests them directly:

- `ReadApplicationState`: every state event.
- `ReadCliPipes`: the `cmd.v1` and `config.v1` pipes.
- `ChangeApplicationState`: switch tab, pane, and session; rename tabs; mark
  panes seen.
- `RunCommands`: the git remote read behind tab names. Without it,
  `run_command` is a silent no-op.
- `ReadPaneContents`: without it Zellij strips the command text from
  attention, and command rows show only their status.

`zj-radar setup zellij` and `zj-radar run` pre-write the full grant into
`permissions.kdl` (`setup/preseed.rs`), so Zellij answers `Granted` from its
cache and no prompt appears ([`install.md`](install.md#permissions)). Without
the grant, each tab's rail shows Zellij's prompt. The CLI's
`REQUIRED_PLUGIN_PERMISSIONS` must equal the plugin's request list; a text-pin
test in `lib.rs` keeps them together, because a partial grant re-prompts
illegibly inside the rail.

**Selectability.** One level rule, `PluginRuntime::desired_selectable`: the
rail pane is selectable while its own permission request is in flight
(Zellij's y/n prompt is tied to the requesting pane) and until a `PaneUpdate`
shows a terminal pane sharing its tab; then `set_selectable(false)` so it never
steals focus. `SetSelectable` is emitted on change only, from the permission
transitions and `panes_changed`. The terminal-neighbor half exists because
Zellij closes any tab with no selectable tiled pane: a rail that went passive
at load in a tab holding only plugins closed that tab — and, as the last tab,
the session ([#46](https://github.com/marktoda/zj-radar/issues/46); see
[`troubleshooting.md`](troubleshooting.md#tab-opens-with-only-the-rail-or-the-session-exits-at-once)).
A rail that never sees a manifest (denied permission, a tab never activated)
simply stays selectable. The terminal-neighbor evidence is a **latch**, not a
level: once a terminal has shared the tab the rail stays passive even when the
manifest later shows none. Zellij's `ClosePane` arm reports that manifest to
plugins synchronously and defers the tab-closing render, so a rail that flipped
back to selectable on "no terminals left" would beat the render and keep a dead
tab — and the session — alive. With the latch, `exit` in a tab's last shell
closes the tab exactly as before.

**Subscriptions.**

- `TabUpdate`, `PaneUpdate`: tabs, the pane manifest, and each pane's
  `attention` and `program_status`.
- `PaneAttentionChanged`: one pane's attention.
- `GlobalAttentionUpdate`: every session on the server (§10).
- `CwdChanged`, `RunCommandResult`: tab naming (§8).
- `Timer`, `Mouse`, `PermissionRequestResult`, `Visible`.
- `ModeUpdate`: `ModeInfo.session_name` (the source §10 depends on),
  `ModeInfo.mode` for the `show_mode` footer label, and the theme palette.
  Zellij re-sends it to the newly active tab on every tab switch, so a
  revealed rail's label is current.
- `InitialKeybinds`, which is never handled. Subscribing to it is Zellij's
  opt-out that strips the full keybinding table from every `ModeUpdate`, so
  the per-instance protobuf decode of each mode change stays small.

**Event routing.** Zellij delivers `TabUpdate`, `PaneUpdate`, and `ModeUpdate`
only to the plugins in each client's *active* tab. `PaneAttentionChanged`,
`GlobalAttentionUpdate`, `CwdChanged`, and pipe messages reach every
subscribed instance. A hidden rail therefore keeps each pane's attention
current, but its tabs, topology, and child records wait for the manifest its
reveal brings. That includes a child-only pane (Opencode): its row takes its
status from the records (§4.1, *Child-only panes*), so the row waits too,
unless a `blocked` or `error` child makes Zellij raise the pane, which
`PaneAttentionChanged` does carry.

**Child records arrive late (Zellij 0.46).** Of the events the rail
subscribes to, only `PaneUpdate` carries `program_status`. `SessionUpdate`
carries the same manifest to every instance, but Zellij does not send it on a
record change, and the rail does not subscribe to it (§10). Storing a record
(`attention/tracker.rs::on_signal`, `ProgramStatus` arm) does not mark the
pane changed: `PaneUpdate` follows a change in any pane's resolved attention
or frame marker (`resolve`), a title change, or a focus, layout, or tab
change, never the record itself. So a `working`, `done`, or `idle` child
added, changed, or cleared under a root that stays `working` produces no
event, and its task line appears, or its spinner stops, only at the next
unrelated `PaneUpdate`. A `blocked` or `error` child is not delayed: it raises
the pane, so the resolved attention changes and both events follow at once.
Claude Code 2.1.325 hits it on every subagent: it rewrites only the records
whose text changed, and under a multiplexer its title does not animate. No
plugin-side fix exists: reading `get_pane_info` on a tick is the polling the
push-driven rule forbids. The fix belongs in Zellij (§13).

**Tab index footgun.** `TabInfo.position` is 0-indexed; `switch_tab_to` is
1-indexed. `display_tab_number = position + 1` is used for both rendering and
clicks.

**Click targeting.** `render_rail()` emits ANSI plus same-height target and
hotspot maps (`CONTEXT.md` → *Lockstep*). Header, folded-idle strip, and gap
rows map to nothing; a tab's header line maps to `SwitchTab`; pane lines map
to `ShowPane`; peer-session lines map to `SwitchSession`. The runtime caches
the latest `RenderedRail` and resolves clicks against it rather than replaying
layout math in the host glue. Hotspots ride the same `Line` records that
derive ANSI and targets, so they cannot desynchronize; only the glyph's own
cells trigger. Right-click is blocked upstream
([zellij#5350](https://github.com/zellij-org/zellij/issues/5350)).

**Command verbs.** `cmd.v1` verbs (`PluginRuntime::control`) are inert on a
hidden rail. A keybind's `MessagePlugin` reaches every tab's rail, and a
hidden rail's frozen tabs still mark its own tab active, so letting it act
would race the visible rail's `SwitchTab` with a wrong one
([#89](https://github.com/marktoda/zj-radar/issues/89)).

**Layout is the integration seam.** The sidebar is a borderless left column
inside a vertical split, outside `children`, so `swap_tiled_layout` cycling
never disturbs it. The tab templates are the only native place Zellij pins a
pane into every tab, so radar integrates like zjstatus: the user adds one pane
to their templates. `setup zellij` installs the wasm at a stable path and
manages a `radar` alias in `config.kdl`, so the per-layout snippet is
path-free.

```kdl
default_tab_template {                       // layout-defined tabs fill `children`
    pane split_direction="vertical" {
        pane size=32 borderless=true {
            plugin location="radar"
        }
        children
    }
    pane size=2 borderless=true {
        plugin location="zellij:status-bar"
    }
}
new_tab_template {                           // runtime tabs need a CONCRETE pane
    pane split_direction="vertical" {
        pane size=32 borderless=true {
            plugin location="radar"
        }
        pane focus=true
    }
    pane size=2 borderless=true {
        plugin location="zellij:status-bar"
    }
}
```

`new_tab_template` is mandatory. When omitted, Zellij derives one from
`default_tab_template` and drops a `children` nested inside a split
([zellij#3247](https://github.com/zellij-org/zellij/issues/3247)); the new tab
then has no focusable terminal. A top-level `children` (the stock compact
layout) is unaffected; only the nested case is. The same nesting has a second
edge: a `tab` node whose body declares no pane fills the nested `children`
with nothing, so Zellij spawns no terminal for it
([zellij#5618](https://github.com/zellij-org/zellij/issues/5618)) — the tab
opens with the rail alone (*Selectability* above is what keeps such a tab,
and the session, alive). `layout::empty_tab_bodies` names those tabs for the
doctor and `--inject`. Plugin nodes stay on their own lines: Zellij's KDL
parser rejected a one-line `{ plugin location="radar" }` without a trailing
`;` (seen on 0.44).

## 6. Timer and cadence

Zellij's timer is one-shot, so the plugin re-arms it each fire at one of three
speeds or not at all (`PluginRuntime::desired_cadence`):

- **Fast (1 Hz)** while there is tick-windowed work: the permission request
  still in flight; a spinning glyph (`TrackedObservation::animating`: Running
  and not steady); an active ping flash (two ticks after a pane flips to
  Pending); and a pending cross-session cycle selection awaiting its
  idle-commit.
- **Frame (`spinner_fps`, default 4 Hz)** is Fast subdivided for the
  spinner, used instead of Fast only while this rail is visible, granted, and
  something spins at full speed (animating and not yet eased,
  `RadarState::has_full_speed_spinner`). Each fire advances the render-only
  `frame` clock and repaints; the full tick runs only once the fires'
  reported elapsed adds up to a second, so `tick` keeps meaning seconds.
  Only a Slow arm is ever topped up (to Fast or Frame): two sub-second fires
  in flight could not be told apart by elapsed, so a Fast↔Frame switch waits
  for the in-flight fire. Hidden rails, eased long-runners, and
  `spinner_fps 1` stay on plain Fast.
- **Slow (once a minute)** once none of that holds but a minute-granular age
  is still changing (an unsaturated ledger entry or pending wait tag).
- **Disarmed** in two shapes: idle (every age has saturated) and denied (a
  permission-denied rail disarms unconditionally, since no clearing event will
  ever arrive).

Steady rows never pin Fast: a `server` row or a remote session costs zero
ticks. A shell command with a service word (`npm run dev`) is `Kind::Server`
and steady too; any other long-running shell command is `Kind::Command`, so
it spins and keeps Fast armed while it runs. A `done`, `error`, or
`pending` row is terminal and does not keep Fast alive.

## 7. Render gate

Zellij delivers every `PaneAttentionChanged`, `CwdChanged`, and pipe message
to every tab's instance, each running under a wasm interpreter, so one busy
pane multiplies across N tabs at interpreter prices. Three layers keep
repaints proportional to change:

1. **Intake no-ops.** An intake that changed nothing rows-visible reports a
   default `RadarChange`: an identical `TabUpdate`, a `PaneAttentionChanged`
   that leaves the observation unchanged, a `CwdChanged` (naming rides the
   `RenameTab` effect's own `TabUpdate` echo).
2. **Rows-diff gate.** `project` drops a requested render whose content key
   (rows, ledger lines, badge, theme, shown mode) equals what the last
   `render()` drew (`last_render_key`). Every `PaneUpdate` requests a render;
   this gate drops the identical ones. `force_render` bypasses it for timer
   frames and config overrides, which change the drawing without changing the
   key.
3. **Visibility gate.** `Event::Visible(false)` (tab switch, tab close, layout
   apply, client detach) marks the instance hidden: it paints nothing, ticks
   included. Nothing else changes: state keeps ticking at full cadence, so a
   hidden instance never drifts from its siblings. `Visible(true)`
   force-renders the skipped frame. Zellij never sends `Visible` at load, so
   an instance starts visible, and any manifest it receives (which only reach
   active-tab plugins) also clears the flag — the bias is toward painting,
   since a wrongly hidden rail would be a stale screen. Under the interpreter
   the paint is over 95 % of a tick's cost, so this is what makes N tabs cost
   about one.

Underneath all three: `RadarState::generation` (bumped by every mutator of
anything `rows()` reads) and the `rows()` memo keyed on `(generation, the
tabs mid-flash)` — the tick reaches a row only through the ping flash.
One roll-up per event, shared by the own-counts derive, the gate compare, and
the render. A missed `touch()` is a stale-rail bug, not a slow one.

## 8. Tab naming

zj-radar names tabs, push-sourced. `TabNamer` takes the top of one ordered
candidate list (`name_candidates`):

1. The focused pane's project name, then any pane's.
2. The focused pane's cwd basename, then any pane's. A cwd inside an agent
   worktree (`/.claude/worktrees/`, `/.Codex/worktrees/`) resolves to the
   parent repo's basename.
3. The focused pane's title, then any pane's (`PaneInfo.title`, with a
   leading braille spinner and Zellij's `Pane #N` placeholder dropped).

**Manual names win.** Under `naming managed` (the default) the namer
overwrites only a default `Tab #N` or a name it applied itself; `force`
overwrites manual names too; `off` renames nothing. An applied name sticks
while any pane still justifies it, so moving focus between panes in different
repos does not flip the name.

**Project name.** `RadarState::rename_tabs` queues each new cwd of the
instance's own tab once (`repo_names`), and the runtime emits
`Effect::ResolveRepo` for it: an async `run_command` of
`git -C <cwd> remote get-url origin`. The `RunCommandResult` comes back as
`repo_resolved`; `repo_from_remote` takes the URL's last path segment without
`.git`. A found name makes the tab re-pick once (`TabNamer::repick`), so a cwd
basename already applied does not stick over it. A cwd with no remote, or
whose path does not exist locally, keeps the basename. For a remote pane
that is the basename of the remote cwd the pane reports.

**Cwd.** Cwds arrive from `CwdChanged`. A freshly opened pane of the
instance's own tab that has not sent one yet gets one `Effect::ResolveCwd`
read (`get_pane_cwd`), focused panes first, at most
`MAX_CWD_BOOTSTRAP_PER_UPDATE` (8) per manifest, never repeated for the same
pane id. Other tabs' panes are left to their own instances, the only ones
that name those tabs. The bootstrap is skipped under `naming off`.

**Write authority is per tab.** Renames are `rename_tab_with_id` (stable
`TabId`, never position) and fire only on change. Each instance learns its own
plugin pane's position from `PaneUpdate`, correlates it once with `TabUpdate`,
and retains the `TabId`; it emits no renames until that resolves. This stops a
stale background instance from applying its private history to another tab
after navigation, and keeps close/reorder events from redirecting ownership.

**In-flight guard.** `TabNamer` remembers the name a tab showed when a rename
was sent (`in_flight`). Until that name changes (the echo, a manual rename),
it does not send the same rename again: a duplicate sent before the echo could
land after a manual rename and overwrite it.

## 9. Ledger

The completion history: a fixed-cap ring (`LEDGER_CAP` = 32, newest first)
rendered as the rail's `─ earlier ─` region, at most ten entries shown.
`ledger.rs` is pure data and policy; `RadarState::ledger_native` files
entries; the renderer consumes prepared `LedgerLine`s.

**Entry rule.** A completion enters when it leaves the card, never before
(`RadarState::native_changed`):

- a `Done` or `Error` observation whose status changes (including Zellij
  marking it seen, which reads idle);
- a pane that leaves the manifest while showing one, filed under the tab it
  was last seen in;
- a `remembered` completion that never showed as the status (a focused pane,
  §4.3).

`Pending` and `Running` never enter. The label is Zellij's text without its
`done: ` / `failed: ` head. `Ledger::push` drops an entry equal to one
already present (same pane, outcome, and label within `MERGE_WINDOW_S`, 4 s),
which is what merges two edges reporting the same completion.

**Unfiled completions.** A hidden rail can hear `PaneAttentionChanged`
before it has any manifest, so it cannot name the pane's tab. Such entries
wait in `RadarState::unfiled` and are filed once a manifest and the tabs seat
the pane.

**Memory only, per instance.** Nothing is persisted or shared. Every instance
receives the same `PaneAttentionChanged` edges, so their ledgers converge in
practice. A new tab's instance starts empty, apart from the completions
Zellij still reports as `remembered`.

**Timestamps.** Entries stamp the completion's epoch seconds from Zellij's
`since_ms`, not ticks (ticks are per-instance). Rendered age is relative and
freezes at `1h+`. That saturation is what lets the idle timer fully disarm
instead of ticking once a minute forever to redraw an age that will never
change. A line's tab position is a live lookup, so a closed tab's line is
click-inert rather than forgotten.

**Display toggle (`ledger`, display-only).** `ledger false` empties what
`PluginRuntime::shown_ledger` hands the render key and `render_rail`;
recording is untouched, so `true` brings the history back. Hidden history is
not content: with no tabs either, the rail shows the onboarding face.

## 10. Cross-session badge

One session's rail shows counts for every other session on the Zellij server,
with click or cycle to switch. Peers are pushed by Zellij's
`GlobalAttentionUpdate`; Zellij also sends the current set when a plugin
subscribes. `lib.rs` drops the current session and copies each peer into an
`attention::PeerSession`:

- Items are the terminal panes with attention, with their tab id, tab name,
  and attention. Zellij lists every pane that is not idle, plus watched or
  remembered idle ones, without a cap.
- Zellij's own counts are dropped: they include running shell commands.

**One counting rule.** `sessions::reported` decides for this session's row and
for every peer: a pane counts, and has a peer-tree row, unless it is Idle
(muted, quiet, and new-output panes arrive Idle) or a running shell command
(a dev server or a long build can run for hours). `sessions::counts` turns a counted
pane into `running` while Running from a program, and into `attention` while
it needs you, a failed command included. Done panes keep their peer-tree row
and count in neither. Zellij reports outward by nearly the same rule
(`host_program_status`); there a running `zellij run` pane still counts.

The rule reads the pane's status, and the two sides read it from different
places. Peers carry Zellij's status only: `GlobalAttentionUpdate` has no OSC
7501 child records. This session's row reads its rail rows, where a pane with
child records and no root takes its most severe child (Opencode,
`NativePane::observation`). Such a pane can count in its own session and not
in the others, where Zellij reports it Idle: a working child counts as
running, and a child error already seen or a child blocked dismissed with `✓`
still counts as attention.

`PeerSession::summary` folds the items into the badge's `SessionSummary`:
counts over every reported item, tabs in first-seen item order, the tree
bounded by `MAX_TABS` and `MAX_PANES_PER_TAB`, labels sanitized.
`PluginRuntime::global_attention_changed` hands the set to
`Sessions::update_global`. Zellij owns liveness: a session it reports is live,
one it stops reporting is gone, so there is no file IO and no staleness
grading.

This session's own row is `Sessions::set_own`, fed with counts derived from
the rail rows (`summary_counts`, by the same rule) whenever the radar
generation moves. The local rail's rows, header
badge, and footer stay tab-level summaries. Pure state in `sessions.rs`,
wiring in `runtime.rs`, render in `render.rs::render_session_badge`
(headings) and `render_peer_children` (the `session_tree` tree).

**Session tree (`session_tree`, display-only).** The option only decides
whether this rail draws peers' trees. Off, `Sessions::set_show_trees` drops
peers' trees on read, so their tree-only edges never repaint this rail. On,
`Sessions::badge()` passes the summary's tree through. On screen, the current session's
heading sits above the local cards and each peer's heading and tree follow the
cards and the idle strip. Peer rows are the lowest-priority lines on the rail:
they get only the height left after the planned cards and the bottom region
(footer, plus the ledger when present), so a large peer can never fold a local
tab or push the ledger off.

**Why not `SessionUpdate`.** At the pinned Zellij rev, `SessionUpdate`'s peer
list is refreshed only on request: a plugin calling the blocking
`get_session_list()`, or a client asking for the session list
(`Screen::update_session_infos`). Nothing here asks, and asking would break
§1. `GlobalAttentionUpdate` is pushed and already carries each peer's counts
and attention panes. The session's own name arrives push-style via
`Event::ModeUpdate`, and may legitimately be `None` before Zellij assigns one.

**Badge.** `Sessions::badge()` is re-derived on every call. It renders zero
lines while only the current session is known; from two entries on, one line
per session in the order shared with cycling: current first, then peers with
attention by name, then the rest by name. Each line shows the name plus
running and attention counts when nonzero. The current line carries an accent
`•` and no click target. A pending cycle selection renders bold.

**Switching.** A click on a peer line, or a cycle commit, emits
`Effect::SwitchSession`. Peer tab positions are ordinals, not Zellij
positions, so the arrival focus is the first pane waiting on the user, by
pane id (`switch_session_with_focus`).

**Cycling.** `session-next` / `session-prev` on the `cmd.v1` pipe advance a
highlight through the shared order, wrapping. A tap
only moves the highlight and sets a "tap landed" flag; `Sessions::tick` runs
on every timer fire while a selection is pending, and a fire whose interval
saw a tap clears the flag and skips. Only a fire whose whole interval was
tap-free commits, so the commit lands at least one quiet interval after the
last tap. The target is re-resolved by session name, never list position, so
peers joining or leaving mid-cycle cannot retarget it; a selection whose
session vanished is dropped. Landing back on the current session cancels.

## 11. Setup analysis (CLI)

`zj-radar setup` learns the world through a pure
`analyze_zellij(&ZellijEnv) -> ZellijFacts` in
`crates/cli/src/setup/analyze.rs`, fed already-read values by the IO shell.
`ZellijFacts` is the single home for derived facts: alias present (managed vs
unmanaged, store path), wasm present, rail injected, body-less tabs, grant
present, config managed, and the Zellij version checked against
`MIN_SUPPORTED_ZELLIJ`. Both consumers project from it: `zellij_check_items`
renders `--check`, and the install orchestrator gates on it. The pure mutator
(`edit_zellij` → `Outcome`) shares only the low-level detectors in
`setup/detect.rs`.

`zellij` is the only target. Agents need no setup, and naming a former agent
target (`claude`, `codex`, `opencode`, `pi`, `omp`) fails with a hint on how
to remove an old adapter.

## 12. Build and packaging

- Rust, `zellij-tile` pinned to a Zellij git rev at `=0.46.0` (0.46 is not on
  crates.io yet), target `wasm32-wasip1`. Text-pin tests in `lib.rs` keep
  `MIN_SUPPORTED_ZELLIJ` and the e2e and funnel workflows' `ZELLIJ_REV` on that pin. The
  artifact is a binary crate, not `cdylib`: Zellij loads plugins as WASI
  command modules and calls `_start`, which `register_plugin!`'s generated
  `main` provides.
- Zellij runs one instance per tab per attached client (a detached client's
  instances linger hidden), each under the wasmi interpreter with its own
  linear memory, so per-instance fixed cost is paid N times. The
  release profile optimizes for size, `crates/plugin/build.rs` shrinks the
  WASI shadow stack (the bulk of initial memory) to 256 KiB, and the flake
  runs binaryen's `wasm-opt -Oz` over the release wasm, which executes
  measurably fewer instructions than rustc's output alone.
- The dev loop never reloads in place (Zellij does not safely hot-reload
  layout-created plugins); every `just dev` is a fresh sandboxed session
  ([`CONTRIBUTING.md`](../CONTRIBUTING.md#dev-loop)).
- The flake builds the wasm hermetically with `crane`; `nix flake check` runs
  in CI. Consuming from Nix: [`install.md`](install.md#nix--home-manager).
- Releases: one tag ships the wasm and static CLI tarballs; the CLI crate is
  published to crates.io by hand ([`RELEASING.md`](../RELEASING.md)).
- `zj-radar update` moves the CLI and the wasm as one, because they share the
  permission set and the setup expectations, and a hand-run upgrade of only
  one half is the drift that broke installs before. It replaces the binary
  first, then re-executes the *new* binary for `setup zellij --download`, so
  the wasm it fetches is its own version; the resolved tag travels with the
  re-exec as `ZJ_RADAR_VERSION` so a release landing mid-run cannot split
  them. Nix and cargo installs are handed back to their tool rather than
  overwritten behind its record; `--check` grades the wasm by sha256 against
  the release sidecar, the only version signal a wasm file carries. Pins move
  forward only: refreshing the wasm to an older pin while the binary stays
  put would recreate the split.

## 13. Non-goals and follow-ups

- **Floating dashboard overlay.** The inline badge (§10) is a few rail lines,
  not a panel; the floating dashboard stays a non-goal.
- **Launchable floating mode** (`LaunchOrFocusPlugin`). A different product:
  an on-demand peek, not an ambient column, and it would turn the passive
  renderer into an interactive panel with `Key` handling and focus grabs.
- **Horizontal bar mode.** Would need a from-scratch compact renderer.
- **Collapse-to-strip toggle.**
- **Agent-specific adapters.** An agent that does not emit OSC 7501 shows only
  as a shell command; the fix belongs in the agent.
- **Upstream: report record changes.** Zellij should mark a pane changed
  when its record set changes (`records.apply` already returns whether it
  did), so a child record reaches the rail at once (§5, *Child records
  arrive late*). Proposed in
  [zellij#5906](https://github.com/zellij-org/zellij/pull/5906), open as of
  2026-11-14.
