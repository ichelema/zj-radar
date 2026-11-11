# Using the rail

What the sidebar shows, what the glyphs mean, and what clicking does. The exact
grid for most scenarios is in [`rail-reference.md`](rail-reference.md)
(background-task lines are pinned by render snapshots instead); this page is
the reader's version.

The rail shows the status Zellij already tracks for every pane. Programs
report it to Zellij themselves ([supported agents](producers.md#supported-agents)),
and Zellij tracks shell commands on its own.

## Anatomy

```
 RADAR                     ·3 1!   ← header: tab count, needs-you badge
════════════════════════════════   ← rule (sweeps a ◆ while anything is working)
▌⠋ 1 pinky                         ← focused tab: spine, status, number, name
 ◆ 2 review                    ✓   ← tab that needs you; ✓ marks it seen
 ├ ◆ ✳ needs approval: Bash    ✓   ← pane row: status, kind mark, text
 └ ⠋ ❉ working: write tests        ← last pane in the tab
 ○ 3 notes                         ← plain terminal, nothing tracked
─ earlier ──────────────────────   ← recent completions, newest first
2m ● pinky cargo test
                                   ← spacer
────────────────────────────────
2 working · 1 need you             ← footer tally
```

**Header.** ` RADAR` plus `·N` tabs. When a tab needs you, `N!` appears in
bold. While anything is working, a `◆` marches along the rule once a second
(compact and comfortable densities only).

**Tab rows.** Column 0 is the spine: `▌` marks the focused tab. The glyph is
the tab's dominant status, most urgent first: `✗` error, `◆` needs you, `⠋`
working, `●` done, `○` idle. Then the tab number and name.

**Pane rows.** Every tracked pane in a tab gets a line under it, joined by
`├` / `└`. Each line is status glyph, kind mark, then the text Zellij shows
for the pane: the program's state and message, with its progress when it
reports one (`working 40%: …`). A tab shows at most six pane lines; the rest
fold into `+N more`.

**Focused panes.** Zellij marks a pane that finishes while focused as seen at
once, so its row goes straight to idle. The finish still lands in
`─ earlier ─`.

**Background tasks.** Work an agent reports separately from its own turn (a
subagent, a backgrounded test run) gets its own lines under the agent's pane
row, on a dashed `┊` guide:

```
▌└ ⠋ ✳ working: fix the e2e retries
▌  ┊ ⠋ Run the test suite · 4m     ← still running
▌  ┊ ✗ lint · 2m                   ← failed
▌  ┊ ● explore the repo            ← completed
```

A spinner means the task is still running. Finished tasks read `●` completed,
`✗` failed, or a muted `·` when the task went idle with no outcome. A line
stays as long as the agent keeps reporting the task. At most three lines show
per pane (the rest fold into `┊ +N more`); on a narrow rail they fold into a
`+N` count on the pane row, error-coloured when one failed. Set
`task_lines count` to always show just that count, or `task_lines off` to hide
background tasks ([options](configuration.md#options)). Only agents that
report sub-tasks get these lines
([supported agents](producers.md#supported-agents)).

**Kind marks.** `✳` Claude Code · `❉` Codex · `✺` Opencode · `✴` pi · `✷` omp ·
`✦` Gemini · `$` shell command · `⇄` remote Zellij session · `⦿` any other
program. The mark comes from the program name in its status report; a script
can also pick `⚗` test, `⚙` build, `⇡` deploy, or `❯` server
([full table](producers.md#field-reference)).

**Shell commands.** Zellij tracks the commands you run in a shell, and each
one gets a plain `$` row: a spinner while it runs, then `●` or `✗` with its
exit code (`exit 1`) when it fails. The command text needs the
`ReadPaneContents` permission ([permissions](install.md#permissions));
without it the row shows only the state.

**Remote sessions.** `ssh` into a host whose Zellij reports its status (its
`attention.host_status` setting, on by default), attach there, and the local
pane shows that session's status with the `⇄` mark. While the remote session
is working, its row holds a steady `▸` instead of a spinner and its tab line
carries a right-aligned `⇄`.

**Time tags.** Time appears only where it costs you something: a pane that
needs you shows how long it has waited (`· 4m`), and a shell command or other
non-agent program shows how long it has run. Both are whole minutes, from one
minute up, frozen at `1h+`.

**Overflow.** When tabs outnumber the lines available, trailing idle tabs fold
into a `+N idle ▾` strip and the header count gains `▲`.

**Footer.** With two or more spare lines the rail pins a rule and a tally to the
bottom: `N working`, plus `· M need you` when M is nonzero. With
[`show_mode true`](configuration.md#options) the tally line also carries the
current Zellij mode (`NORMAL`, `LOCKED`, …) flush right.

**Earlier.** Above the footer, an `─ earlier ─` section lists recent
completions (newest first, up to ten) with a relative age. Click one to jump
to its tab if the tab still exists. The list lives in the memory of each
tab's rail, so a new tab starts with an empty list. Set `ledger false` to
hide the section; history is still recorded.

**Densities.** `cards` (the default) paints each tab as a tinted band using
your theme's colors; `comfortable` separates tabs with a blank line;
`compact` is flush. Card tints need a truecolor terminal.

**Tab names.** With `naming "managed"` (the default) the rail names each tab
after the project its panes are in. A name you set by hand always wins. Next
comes the project name from the directory's git remote (`origin`), then the
directory's name, then the pane title. For a remote (`ssh`) pane the name is
the remote directory's name.

## Mouse

- **Click a tab row** to switch to that tab. **Click a pane row** to switch to
  the tab and focus that pane.
- **Click `✓`** on a row that needs you to mark it seen in Zellij. Zellij then
  stops reporting it as needing you, in every tab's rail and in Zellij's own
  tab marker. Use it when an agent ends a finished turn with a courtesy
  question ("want me to also…?"). A question that actually blocks the agent
  clears on its own when you answer it.
- Right-click does nothing yet: Zellij does not deliver right-clicks to
  plugins ([zellij#5350](https://github.com/zellij-org/zellij/issues/5350)).

Desktop notifications come from Zellij itself, through its `attention`
settings; the rail sends none.

## Keybinds

Nothing is bound by default. Four verbs are available for your own bindings,
delivered over the `zj_radar.cmd.v1` pipe:

- `attention-next` / `attention-prev` cycle focus through the tabs that need
  you, errored, or finished, in tab order.
- `session-next` / `session-prev` step the cross-session badge selection (below).

Bindings for all four, and for changing options from a key, are in
[`configuration.md`](configuration.md#binding-keys-to-commands).

## Cross-session badge

Run more than one Zellij session on the same machine and each rail grows one
line per session: your session first, then sessions that need attention, then
the rest. Each line shows the session name with its working and needs-you
pane counts. Every session counts the same way: an agent that works or waits,
and a shell command that failed, count; a shell command that is still running
(`vim`, `cargo test`, a dev server) does not. With a single session the badge
is invisible.

- **Click a line** to switch to that session, landing on a pane that needs
  you if it has one.
- **`session_tree true`** ([configuration](configuration.md#options)) expands
  the badge into a tree. Your session's line heads your own tabs; each other
  session follows with the tabs and panes Zellij reports activity for, except
  running shell commands, so every count has its row. Click
  a peer pane row to switch there and focus that pane; a peer tab row focuses
  its first listed pane. Peer rows only use the height left after your local
  tabs, the ledger, and the footer, so a short rail shows fewer of them (or
  none); session lines always remain visible. `session_tree` controls whether
  *your* rail draws peers' trees.
- **`session-next` / `session-prev`** move a highlight through the same order.
  The switch happens about a second after your last tap; landing back on your
  own session cancels.

The sessions come from Zellij's own attention tracking: a session Zellij stops
reporting leaves the badge.
