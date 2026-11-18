# Configuration

zj-radar reads its options from the plugin alias and accepts live updates over
a pipe. For the defaults block, see [Configuration in the
README](../README.md#configuration).

## Options

Options go on the `radar` alias in `~/.config/zellij/config.kdl`. Every key is
optional. This example sets two keys to non-default values:

```kdl
plugins {
    radar location="file:~/.config/zellij/plugins/zj_radar.wasm" {
        density "comfortable"   // default is "cards"
        naming "off"            // default is "managed"
    }
}
```

If `setup zellij` wrote your alias, it sits between `// zj-radar: managed
plugin alias begin`/`end` markers. Add your keys inside that block: a later
`setup zellij` or `zj-radar update` keeps them, and changes only the alias's
`location`. A hand-owned alias whose `location` is the stable wasm path
(`file:~/.config/zellij/plugins/zj_radar.wasm`) is left alone: `setup zellij`
and `zj-radar update` refresh only the wasm behind it.

Unknown keys are ignored and parsing never fails. An unrecognized value keeps
the field as it was, so a typo never resets a setting.

| Key | Values | Default | Effect |
|-----|--------|---------|--------|
| `density` | `cards` · `comfortable` · `compact` | `cards` | Tinted card bands / blank separators / flush rail. |
| `naming` | `off` · `managed` · `force` | `managed` | Auto-name tabs from the project's git remote, else the pane's directory or title ([order](using.md#anatomy)). `managed` never overwrites a name you set by hand; `force` does. See note 1. |
| `header` | `true` · `false` | `true` | Show the ` RADAR` header and tab count. |
| `glyphs` | `plain` · `nerd` | `plain` | Status glyph set. `nerd` needs a Nerd Font. |
| `jump_hint` | `alt-n` · `hidden` | `hidden` | Footer advertises ` alt-[n] jump`. See note 2. |
| `show_mode` | `true` · `false` | `false` | Show the current Zellij input mode (`NORMAL`, `LOCKED`, `PANE`, …) flush right on the footer's tally line. `NORMAL` is muted, other modes use the accent color. The label is dropped when the rail is too narrow to fit it beside the tally, and it lives in the footer, so a rail too short to draw the footer shows no mode. If two clients view the same tab in different modes, the most recent mode change shows. See [`install.md`](install.md#add-the-sidebar-to-a-layout) for dropping the status bar. |
| `spinner_fps` | `1`–`10` | `4` | Frames per second of the working spinner on the visible rail. `1` steps once a second (the pre-0.9 pace). Each frame repaints the rail; hidden tabs and rows running past 10 minutes (whose spinner eases to a slow blink) never animate faster than once a second. |
| `task_lines` | `lines` · `count` · `off` | `lines` | How an agent's background tasks show: a `┊` line per task, only a `+N` count on the agent's row, or nothing. See [`using.md`](using.md). |
| `ledger` | `true` · `false` | `true` | Show the `─ earlier ─` list of recent completions. `false` shows only live panes; history is still recorded, so `true` brings it back. See [`using.md`](using.md). |
| `session_tree` | `true` · `false` | `false` | Draw peer sessions' tab trees on this rail (off keeps one line per session). See [`using.md`](using.md#cross-session-badge). |
| `mini` | `true` · `false` | `false` | This pane is the 4-column mini rail. Set it on the mini rail's layout pane, not on the alias: `pane size=4 borderless=true { plugin location="radar" { mini true } }`. Read once at load; `config.v1` cannot change it. See note 3. |

Notes:

1. The "names I applied" memory lives in plugin memory. After a Zellij server
   restart, previously auto-applied names read as manual, so `managed` leaves
   them alone. Use `force`, or rename the tab back to its default `Tab #N`.
2. Only set `jump_hint` when Alt+digit actually reaches Zellij on your machine.
   Window managers often claim those chords, and macOS terminals type `¡`
   unless option-as-alt is on. `zj-radar run` binds Alt-1..9 but does not set
   this, because it cannot verify the chord arrives.
3. The mini rail sits right of the full rail in the layout
   ([`install.md`](install.md#add-the-sidebar-to-a-layout)). It starts
   collapsed, and `sidebar-toggle` switches every tab between the two (see
   [Binding keys to commands](#binding-keys-to-commands)).

## Runtime config

Change options without editing the layout by broadcasting a flat JSON object
on the `zj_radar.config.v1` pipe:

```sh
zellij pipe --name zj_radar.config.v1 -- '{"density":"compact","header":false}'
```

## Binding keys to runtime config

The same payload can come from a keybind. Zellij's `MessagePlugin` action
delivers a named pipe message straight to the plugin. Add to
`~/.config/zellij/config.kdl`:

```kdl
keybinds {
    shared_except "locked" {
        // Flush/compact rail
        bind "Alt Shift c" {
            MessagePlugin {
                name "zj_radar.config.v1"
                payload "{\"density\":\"compact\"}"
            }
        }
        // Roomy cards
        bind "Alt Shift v" {
            MessagePlugin {
                name "zj_radar.config.v1"
                payload "{\"density\":\"cards\"}"
            }
        }
        // Hide the identity header
        bind "Alt Shift h" {
            MessagePlugin {
                name "zj_radar.config.v1"
                payload "{\"header\":false}"
            }
        }
    }
}
```

- `MessagePlugin` with no plugin named broadcasts the message to every
  running plugin. Each radar instance (one per tab and client) picks it out
  by its pipe name, so the whole session updates at once; other plugins
  ignore it, and in a session without a rail the key does nothing.
- Don't name the plugin (`MessagePlugin "radar" { … }`) unless every session
  loads the rail. Keybinds in `config.kdl` apply to every session, and a
  targeted `MessagePlugin` launches the plugin when no instance is running, so
  the key opens a new rail pane in a session started with another layout.
- `config.v1` sets values; it cannot toggle them. Bind one key per value.

## Binding keys to commands

For imperative actions the plugin accepts `zj_radar.cmd.v1`, whose payload is
a single verb:

```kdl
keybinds {
    shared_except "locked" {
        // Cycle focus to the next tab needing attention (pending / error / done)
        bind "Alt n" {
            MessagePlugin { name "zj_radar.cmd.v1"; payload "attention-next"; }
        }
        bind "Alt p" {
            MessagePlugin { name "zj_radar.cmd.v1"; payload "attention-prev"; }
        }
        // Cycle the badge selection across sessions; commits ~1s after the last tap.
        bind "Alt s" {
            MessagePlugin { name "zj_radar.cmd.v1"; payload "session-next"; }
        }
        bind "Alt Shift s" {
            MessagePlugin { name "zj_radar.cmd.v1"; payload "session-prev"; }
        }
        // Switch every tab between the full rail and the 4-column mini rail.
        bind "Alt b" {
            MessagePlugin { name "zj_radar.cmd.v1"; payload "sidebar-toggle"; }
        }
    }
}
```

- `attention-next` / `attention-prev` walk the tabs that need you, errored,
  or are done, in tab order, wrapping. Running and idle tabs are
  skipped.
- `session-next` / `session-prev` move the cross-session badge's highlight
  through the order the badge renders in (see [Cross-session
  badge](using.md#cross-session-badge)). Nothing switches until about a second
  after the last tap; landing back on your own session cancels. A committed
  switch lands on the target's attention tab if it has one. With only one
  session running these verbs do nothing.
- `sidebar-toggle` collapses the full rail and expands the mini rail in every
  tab, or the reverse. It needs a layout with both rail panes; with only the
  full rail it does nothing. A tab opened while the mini rails show starts
  with the mini rail, and a client that attaches finds every tab as it was.

These binds broadcast too, for the reason above: in a session without a
rail they do nothing.

With two or more clients attached to one session, a key press moves only the
client that pressed it. `sidebar-toggle` is the exception: Zellij collapses
panes for every client, so every client sees the new width. A `zellij pipe` from the command line still moves
every client. A press can also move another client that acted within the
last tenth of a second, for example by typing or clicking
([why](design.md#5-plugin--zellij-wiring)). When another client is looking at
the tab `attention-next` or `attention-prev` sends you to, you can land one
attention tab further.

Unknown verbs are ignored. Both pipes are inert until the sidebar has been
granted permissions, except the internal verbs `grant-ready` and
`grant-ready-now`: a full rail sends one of them once it holds the grant, and
each mini rail then asks for its own (see
[design](design.md#5-plugin--zellij-wiring)). Do not bind them. The `✓` mouse
gesture is described in
[`using.md`](using.md#mouse).
