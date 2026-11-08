# Troubleshooting

The sidebar lives in your tab templates and runs once per tab, which meets a
few sharp edges in Zellij itself. Each entry is symptom, cause, fix.

## Sidebar shows "needs permission" (or looks blank)

**Symptom:** the rail shows ` RADAR` over an orange `⚠ needs permission` line,
or one rail pane is blank, and tab naming does not work either.

**Why:** the plugin is parked at Zellij's permission prompt. It paints that
face but receives no events, so everything looks dead. The blank case is
Zellij drawing its own y/n prompt into one plugin pane, where at rail width it
is unreadable ([zellij #4749](https://github.com/zellij-org/zellij/issues/4749)).
You get here when the install-time grant was skipped or went stale: a declined
consent prompt, a `permissions.kdl` the installer refused to edit, a grant
from an older zj-radar that lacks a permission this release requests
([Permissions](install.md#permissions)), an alias pointing at a `/nix/store/`
path, or no resolvable Zellij cache directory.

**Fix:** `zj-radar setup zellij --check`; a non-`ok` `grant` item confirms it.
Then pick the fix for how the rail was installed:

- **With `setup zellij` (or `update`):** run `zj-radar setup zellij -y` to
  pre-authorize, then open a new tab or restart Zellij.
- **In a `zj-radar run` session:** quit and re-run `zj-radar run`; it
  pre-authorizes its own wasm before launching.
- **By hand, or when the two above cannot write the grant:** from inside the
  session, run `zj-radar setup zellij --grant` and press `y` in the floating
  pane it opens. Without the CLI, focus the blank rail pane and press `y`
  blind.

A rail on Zellij older than 0.46.0 does not work at all; no grant fixes it.

## An agent pane shows no status

**Symptom:** an agent works in a pane, but its row never appears or never
leaves idle.

The rail shows what Zellij reports for the pane, and Zellij reports what the
agent sends it over OSC 7501. Nothing needs installing, so check these in
order:

1. **`zj-radar setup zellij --check`.** It flags a Zellij older than 0.46.0
   and a missing grant.
2. **Rule out the rail.** Report a status by hand from a shell in the session,
   as in [Writing your own producer](producers.md#writing-your-own-producer).
   If that row lights up, the rail is fine and the agent sends nothing.
3. **The agent's version.** Each agent reports only from a release with
   OSC 7501 support ([supported agents](producers.md#supported-agents)). omp
   needs a release that contains
   [can1357/oh-my-pi#17147](https://github.com/can1357/oh-my-pi/pull/17147);
   before it, omp's own shell-integration output cleared its working state.
4. **Claude Code's environment.** Claude Code reports nothing when
   `CLAUDE_CODE_DISABLE_TERMINAL_TITLE` is set in its environment, for
   example in a Claude started from another Claude's Bash tool
   ([Claude Code](producers.md#claude-code)). Start it without the variable:
   `env -u CLAUDE_CODE_DISABLE_TERMINAL_TITLE claude`.

**Opencode never shows done.** Opencode reports only working and needs-you,
with no done or error
([anomalyco/opencode#60411](https://github.com/anomalyco/opencode/issues/60411)).
A finished turn reads idle, and it never reaches `─ earlier ─`.

## Command rows show no command text

**Symptom:** shell commands get rows, but each one shows only its status.

**Why:** Zellij withholds command text from a plugin without the
`ReadPaneContents` permission. A grant written before this release lacks it.

**Fix:** re-grant as in
[Sidebar shows "needs permission"](#sidebar-shows-needs-permission-or-looks-blank).

## Can't open a new tab (the two-template rule)

**Symptom:** tabs created at runtime (`Ctrl+t n`) contain only the sidebar and
status bar, so keystrokes have nowhere to land.

**Why:** without an explicit `new_tab_template`, Zellij derives one from
`default_tab_template` and drops a `children` placeholder nested inside a
split ([zellij#3247](https://github.com/zellij-org/zellij/issues/3247)). A
left column is exactly that shape.

**Fix:** declare `new_tab_template` with a concrete `pane focus=true`, as in
the [layout snippet](install.md#add-the-sidebar-to-a-layout).

## Tab opens with only the rail (or the session exits at once)

**Symptom:** a tab declared in the layout file opens with the sidebar and
status bar but no terminal. Before the fix for #46 the session instead died
the moment the plugins loaded — `Bye from Zellij!` within a few hundred
milliseconds, every attach — and `zellij --debug`'s log showed
`Failed to resize horizontally/vertically` at `ApplyLayout`, then
`failed to send message to screen` panics in `wasm_bridge.rs`
([#46](https://github.com/marktoda/zj-radar/issues/46)).

**Why:** the layout has a `tab` whose body declares no pane — `tab focus=true`
with nothing inside, or a plain `tab` — beneath a `default_tab_template` that
nests `children` inside the rail's vertical split. A top-level `children`
fills such a tab with a default pane; a nested one gets **nothing** — Zellij
logs `spawn_terminals_for_layout: 0 tiled + 0 floating panes` and emits the
resize warnings ([zellij#5618](https://github.com/zellij-org/zellij/issues/5618)).
The tab's only panes are plugins. Zellij closes any tab with no *selectable*
pane, so when the rail made itself non-selectable at load (as every status-bar
style plugin does), the tab closed — and, as the last tab, the session with
it. The `wasm_bridge` panics in the log are shutdown noise, not the cause.

Since that fix the rail stays selectable until a terminal pane shares its tab,
so the tab survives with the rail focused; open a pane (`Ctrl+p n`) and the
rail hands focus to it and goes passive as usual. The rail is also the
*first* pane in the split, so an empty tab lands with it focused: keystrokes
go nowhere until that pane exists.

**Fix:** give every declared `tab` a pane in its body:

```kdl
tab focus=true {
    pane
}
```

`zj-radar setup zellij --check` reports such tabs as a `tab bodies` warning,
and `--inject` notes them after writing. A layout with **no** `tab` nodes at
all is fine as long as `new_tab_template` is declared (Zellij builds the
first tab from it) — which is why the [two-template rule](#cant-open-a-new-tab-the-two-template-rule)
above matters here too.

## Alt+] hides the rail (or stops cycling)

**Symptom:** `Alt+[` / `Alt+]` (cycle swap layouts) either makes the sidebar
vanish from the current tab or does nothing.

**Why:** any custom layout discards Zellij's built-in swap layouts, so a layout
that declares none has no cycling. And a swap layout replaces the tab's
arrangement wholesale, so an entry that does not include the rail swaps it
away.

**Fix:** the injected rail and
[`examples/radar-sidebar.kdl`](../examples/radar-sidebar.kdl) redeclare
`swap_tiled_layout` with every entry routed through a rail-carrying
`tab_template name="ui"`. Two cases need your hand:

- **A hand-written layout without swap blocks:** copy them from the example,
  or re-run `zj-radar setup zellij --inject`, which adds them when none exist.
- **A layout with its own `swap_tiled_layout` blocks:** `--inject` leaves them
  alone and adds none of its own. Route each entry through the `ui` template:

  ```kdl
  swap_tiled_layout name="vertical" {
      ui max_panes=5 {          // ← was: tab_template { … } or a bare pane tree
          pane split_direction="vertical" {
              pane
              pane { children; }
          }
      }
  }
  ```

## Session crashes with "too many open files" (EMFILE)

**Symptom:** in a long-lived session, `zellij pipe` calls hang, stuck `zellij
pipe` processes pile up, and the Zellij server dies with `Too many open files`.

**Why:** Zellij holds each `zellij pipe` client until every plugin instance has
consumed the message. An instance parked at its permission prompt never does,
so each broadcast leaves one blocked client pinning two server file
descriptors.

**Fix:** grant the prompt so no instance stays wedged
([above](#sidebar-shows-needs-permission-or-looks-blank)), and give scripted
`zellij pipe` sends a deadline (for example `timeout 5 zellij pipe …`).

## Cards look flat (no colored row backgrounds)

**Symptom:** status glyphs are colored but the per-row card tints are missing.

**Why:** the tints are truecolor (24-bit) escapes. A terminal without truecolor
(macOS Terminal.app, the Linux console, a `tmux` without `RGB`) ignores them.
Layout and click targeting are unaffected.

**Fix:** use a truecolor terminal (Alacritty, Kitty, WezTerm, iTerm2, foot,
Ghostty). Inside `tmux`: `set -as terminal-features ',*:RGB'`.

## Rail glyphs spill past the sidebar edge

**Symptom:** a status glyph or the `═` rule pushes a row one column too wide.

**Why:** the rail budgets each glyph as one column. Several are East-Asian-Width
Ambiguous codepoints, which a terminal set to ambiguous width = double renders
as two columns. An emoji with a presentation selector (`⚠️`) can measure one
column narrower than it draws.

**Fix:** set ambiguous-character width to narrow (Kitty: default; WezTerm:
`treat_east_asian_ambiguous_width_as_narrow = true`; iTerm2: Profiles → Text →
"Treat ambiguous-width characters as double width" off).

## Focused card's highlight stops short of the pane edge (Ghostty)

**Symptom:** in Ghostty the focused card's background band appears to end one
or two columns before the rail's right edge.

**Why:** the rail paints every column Zellij gives it (verified with real-PTY
tests). Ghostty's window padding is drawn over the trailing columns.

**Fix:** in Ghostty's config, `window-padding-color = extend`.

## Sidebar is still the old version after `zj-radar update`

A running session keeps the plugin it loaded. Restart Zellij, or open a new
session. `zj-radar update --check` confirms the installed wasm matches the
release. If it reports the wasm as a symlink managed by Nix or home-manager,
`update` left it alone; update the flake input instead.

## Zellij plugin-reload quirks

**Symptom:** during development, reloading the plugin opens an extra tiled
plugin pane.

**Why:** Zellij's reload actions misbehave for a plugin created by a layout
that has made itself non-selectable, as the sidebar does once a terminal pane
shares its tab.

**Fix:** `just dev` never reloads in place; each iteration is a fresh
`zj-radar-dev-<hhmmss>` session. See [Dev loop in
CONTRIBUTING](../CONTRIBUTING.md#dev-loop).
