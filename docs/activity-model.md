# The activity model: states, classes, and presentation

`design.md` documents the pipeline (how state moves); this documents the
semantics (what a state means and how it looks). Zellij decides each pane's
status; the plugin decides how it looks. Everything here is shipped.

## 1. The core principle

> **A spinner means bounded work in progress.** Animation is a promise:
> "this will complete, and you will want to know when."

The user-meaningful axis is **attention direction**, who is waiting on whom:

- You wait on **it** → activity. Show progress.
- **It** waits on you → attention. Show urgency (needs you, error).
- Neither (unending by design, or interactive) → context. Show identity
  quietly, or nothing. Never a spinner.

An open editor is not activity. A dev server is not "still running" in any
sense the user needs animated. A `cargo build` is exactly what the spinner was
made for.

Zellij applies half of this rule before the plugin sees anything: it lists a
shell command only once it has run for `done_threshold_secs` (default 10), and
leaves out full-screen commands and commands you type into. The plugin applies
the other half with the steady classes (§2).

## 2. The three orthogonal axes

Every tracked pane's presentation is a function of three independent facts:

| Axis | Type | Values | Who owns it |
|---|---|---|---|
| **Origin** | `ObservationOrigin` | `Program` \| `Command` | Zellij's attention source: an OSC 7501 record or any other non-command signal (`Program`), or a shell command, command pane, or foreground process (`Command`) |
| **Kind** | `Kind` | Claude, Codex, Opencode, Pi, Omp, Gemini, Test, Build, Deploy, Server, Remote, Command, Other | `attention::kind_for_app` on the OSC 7501 `app`; a command-origin row is `Server` when its text has a service word, else `Command` (`attention::command_kind`) |
| **Class** | semantic vocabulary, **not** a stored Rust type | `Job` \| `Service` \| `Remote` | this document |

The kind table (`kinds!` in `kind.rs`) maps each `app` token to a kind and its
mark. `kind_for_app` adds two names: `claude-code` is Claude, and `zellij` (a
nested or remote Zellij, see
[remote sessions](producers.md#remote-sessions-over-ssh)) is Remote. An
unknown or missing `app` is Other. The tokens and marks are listed in
[producers](producers.md#field-reference).

The classes are predicates on `Kind`, not an enum:

- **`Job`**: bounded work with an end. Every kind that is not steady: the
  agent kinds, `Test`, `Build`, `Deploy`, `Command`, `Other`.
- **`Service`**: unending by design. Kind `Server` (`Kind::is_service`), from
  `app=server` or a shell command with a service word (§4).
- **`Remote`**: a live connection to another machine. Kind `Remote`
  (`Kind::is_remote`), from `app=remote` or `app=zellij`. Steady like
  `Service`, and it also marks the tab line while connected.

`Kind::is_steady()` (`is_service() || is_remote()`) sits behind four readers:
the glyph split in `render::running_glyph`, the cadence term
`TrackedObservation::animating`, the run-tag exclusion in `render::run_tag`,
and the roll-up tie-break. `Kind::is_remote()` alone drives the tab `⇄`
marker.

Agents (`Kind::is_agent()`) are Jobs that label their own turns: they spin,
but wear no run tag.

Origin decides three things on its own: only program-origin rows can be
needs-you and carry the `✓` gesture, and only command-origin rows carry an
exit code.

There is no class for interactive programs. Zellij never lists an editor,
pager, or REPL as working, so it never reaches the rail (§4).

## 3. The state × class presentation matrix

`Status` (Idle < Done < Running < Pending < Error, severity-ordered) is the
lifecycle vocabulary. It comes from Zellij's attention status:

| Zellij attention | OSC 7501 `state` | `Status` |
|---|---|---|
| `Working` | `working` | Running |
| `NeedsYou` | `blocked` | Pending |
| `Done` | `done` | Done |
| `Failed` | `error` | Error |
| `Idle`, `Quiet`, `NewOutput` | `idle` | Idle |

A muted pane reads Idle (`PaneAttention::effective_status`).

Presentation is `f(Status, class)`. The executable examples live in
[`rail-reference.md`](rail-reference.md) (scenarios AE, AF, AG).

| | `Job` | `Service` | `Remote` |
|---|---|---|---|
| **Running** | `⠋` spinner + Zellij's text; non-agent rows add a `· 4m` run tag once ≥ 1 minute (whole minutes, frozen at `1h+`); the spinner eases to a slow blink for long runs | steady `▸` + text, **no spinner, no run tag, no fast cadence** | steady `▸`, same as `Service`; a right-aligned `⇄` also marks the tab line |
| **Pending** | `◆` + Zellij's label (`needs approval: …`); a `· 12m` wait tag once ≥ 1 minute; a `✓` hotspot; the tab flashes on entry | same | same |
| **Done** | `●` + text; stays until Zellij marks the pane seen, then the row goes Idle and the completion enters the `earlier` ledger | same | same; the `⇄` marker clears |
| **Error** | `✗`; a command row adds `exit N` when Zellij reports the code; ledgered like Done | same | same; the `⇄` marker clears |
| **Idle** | muted `○` row while the pane still has an OSC 7501 record; no row otherwise | same | same; the `⇄` marker clears (it tracks *connected*, not *was remote*) |

The text is Zellij's attention text. It already carries the label and the
progress: `working 40%: …`, `needs approval: …`, `done: …`. The ledger drops
the `done: ` and `failed: ` heads, which its glyph already says.

Seen is Zellij's state. A focused pane's Done is seen almost at once; the
plugin still ledgers it from the pane's remembered record. The `✓` gesture
asks Zellij to mark the pane seen (`mark_session_pane_seen`) and changes no
local state.

**Background tasks** are the pane's OSC 7501 child records (`id`). They render
as `┊` sub-lines under the pane line, in this matrix's vocabulary: a running or
blocked child spins with a `· 4m` age, `●` done, `✗` failed, muted `·` idle,
with their frozen duration once finished. The task lines never feed the
roll-up or the counts. Two rules come from Zellij and the plugin's reading of
it:

- A child raises the pane's own status only when it is `blocked` or `error`.
- A pane with children and no root record takes its status from its most
  severe child, which becomes the pane line (Opencode reports this way).

At most three task lines per pane: two plus `┊ +N more` beyond that, running
work sorted first, then failures. A fold always keeps the first failure on a
line, and `+N more` is error-coloured when it hides another. Below 20 columns,
under height pressure, or with `task_lines count`, a card drops them for a
`+N` count on the pane line (running and failed tasks only; error-coloured
when one failed, absent when all finished cleanly). The height squeeze order
is: inter-card gaps go, idle tabs fold into the strip, the strip line goes,
then task lines fold, and only then do pane rows compress.

Ties in the tab-level roll-up: on equal severity a `Job` outranks a steady row
(`Service` or `Remote`) as the tab's primary detail. A spinning build
summarizes the tab better than a server or an ssh session that is merely up.

Cadence rule per class: only *animating* work (`TrackedObservation::animating`
= Running minus steady, so agent rows animate too) and a tab's Pending flash
keep the 1 Hz timer armed. `Service` and `Remote` rows never pin the fast
cadence: a dev server or a live ssh session left overnight costs zero ticks.
The minute-granular ages (ledger, wait tags) run on the Slow heartbeat and
freeze at `1h+`, so the timer can disarm. The long-run *easing* does not save
cadence (an eased spinner still repaints every 4th tick), so steady-row relief
comes only from the steady mark plus the `animating` term.

Desktop notifications are Zellij's (`attention` → `desktop_notifications`);
the plugin sends none.

## 4. Where the status comes from

The plugin reads one source, Zellij's native per-pane attention, from the pane
manifest (`PaneInfo::attention` and `program_status`) and from
`PaneAttentionChanged`. `attention::NativePane::observation` turns it into the
row. The plugin classifies a command only by the words of its text (below)
and never polls.

**Programs (OSC 7501).** A program reports records; Zellij picks the deciding
one (the root record, raised by a `blocked` or `error` child). Zellij's text
and `app` decide the row's text and kind. The plugin overrides Zellij in one
case only: when the pane has records but no root and Zellij's source is not the
program, the most severe record decides (§3). The wire format is in
[producers](producers.md#writing-your-own-producer).

**Shell commands.** Zellij tracks commands through OSC 133 prompt marks,
command panes, and the foreground process. Its rules, all on the Zellij side:

- A command is working only after `done_threshold_secs` (default 10).
- Full-screen commands and commands you type into are left out
  (`track_commands`): editors, pagers, `htop`, an `ssh` shell.
- A successful command reports Done only if it ran at least
  `done_threshold_secs`; a failure always reports Error, with its exit code
  when the shell reports one.
- A pane whose program reports OSC 7501 is not command-tracked.
- Without the `ReadPaneContents` permission Zellij strips the command text;
  the plugin requests it.

A command row is `Kind::Server` (`❯`), a `Service`, when Zellij's text has
one of the whole words `dev`, `serve`, `server`, `start`, or `watch`
(`attention::command_kind`): `npm run dev`, `cargo watch`, `python -m
http.server`. The text splits on spaces; a token with `/` or `=` (a path,
URL, redirect, or `KEY=VAL`: `2>/dev/null`, `tests/server/`) never counts,
and the rest lose leading dashes and split on `:` and `.`. So `start:dev`,
`http.server`, and `--watch` match, while `api-server`, `restart`, and
`watch_mode` do not. Every other command row is `Kind::Command` (`$`) and a
`Job`. A bounded command with one of these words (`start.sh`, `npm run
build:dev`, `cargo test -- start`) holds the steady mark too. Without the
command text (`running`) the row is `Command`.

**Other attention.** Zellij's other signals (a bell, a terminal notification,
a watch) arrive as program-origin rows with no `app`: kind Other (`⦿`),
usually needs-you.

**Other sessions.** Zellij's `GlobalAttentionUpdate` feeds the cross-session
badge with the same status vocabulary (`attention::PeerSession`).

## 5. Extension guide

- **A program needs its own mark** → one `kinds!` row in `kind.rs` (token +
  marks), plus a `kind_for_app` arm when its `app` differs from the token.
- **A new kind** → declare its side in `is_agent` and `is_service` /
  `is_remote`; the matches are exhaustive, so it does not compile until you
  do.
- **A new presentation-relevant distinction** (the `Remote` class is the
  worked example) → a row in the §3 matrix first, then a `Kind` predicate added
  *with its first consumer*, never a speculative enum ahead of a call site
  that matches it.
- **A command shows wrong** → Zellij's `attention` settings decide whether
  it shows; the plugin's only command list is `SERVICE_WORDS` in
  `attention.rs`, which picks steady over spinning.
- **A new agent** → nothing in zj-radar if it emits OSC 7501; add a `kinds!`
  row only for a dedicated mark.

## 6. Known limits

- Opencode sends no `done` or `error` (anomalyco/opencode#60411): a finished
  turn reads Idle and never reaches the ledger.
- A plain `ssh` session with no remote Zellij shows nothing: it is a command
  you type into.
- A command that succeeds in under `done_threshold_secs` leaves no Done.
- The run tag counts from when this rail first saw the status; a rail loaded
  mid-run starts from zero.
- A task line's age counts from the child record's last update, not its
  start: OSC 7501 carries no start time.
