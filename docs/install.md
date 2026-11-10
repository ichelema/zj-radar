# Installing the sidebar

This page installs the sidebar: the wasm plugin, its `radar` alias, the rail in
your layout, and the permission grant. Agents need no install: the rail shows
any program that reports its status to Zellij (see
[supported agents](producers.md#supported-agents)). The copy-paste path is the
README's [Quick start](../README.md#quick-start).

**Requirements:** Zellij 0.46.0 or newer (`zellij --version`). Older releases
lack the pane attention API the rail reads status from. `--download` needs
`curl` or `wget`.

## Recommended: install the CLI, then `setup zellij --download`

Tagged releases ship a prebuilt `zj-radar` CLI for Linux (x86_64 and aarch64,
static musl) and Apple Silicon macOS. Intel macOS has no prebuilt binary; the
installer detects it and points you at `cargo install zj-radar` (see
[Build from source](#build-from-source)).

```sh
# Installs to ~/.local/bin by default.
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/marktoda/zj-radar/releases/latest/download/install.sh | sh

zj-radar setup zellij --download
```

`setup zellij --download` does six things:

1. Downloads the wasm built for this CLI's version, so the two never drift
   apart. `ZJ_RADAR_VERSION` pins a different tag.
2. Verifies it against the release's `.sha256` checksum. A mismatch aborts; a
   missing checksum or no `sha256sum`/`shasum` on `PATH` falls back to TLS-only
   with a warning.
3. Copies it to `~/.config/zellij/plugins/zj_radar.wasm`.
4. Adds or updates a managed `radar` alias in `~/.config/zellij/config.kdl`.
5. Pre-writes the sidebar's permission grant in Zellij's `permissions.kdl`
   (see [Permissions](#permissions)), so Zellij doesn't prompt.
6. Asks `Inject the rail into <layout>? [y/N]`. `y` splices the rail in place
   (backup at `<layout>.zj-radar.bak`); `N` prints the snippet to paste.

Flags:

| Flag | Effect |
|---|---|
| `--inject` | Non-interactive yes to the layout prompt. |
| `--layout <name>` | Target `~/.config/zellij/layouts/<name>.kdl` instead of the default layout. |
| `--dry-run` | Preview every change; write nothing. |
| `--yes` / `-y` | Fully non-interactive. Takes the safe default at each prompt: pre-seeds the grant, prints the layout snippet instead of injecting. |
| `--force` | Replace an existing unmanaged `radar` alias. |

The installer script honors `ZJ_RADAR_VERSION` (release tag) and
`ZJ_RADAR_BIN_DIR` (install directory).

## Try it without touching your config (`zj-radar run`)

`zj-radar run` launches a throwaway Zellij session with the rail wired in. It
owns that session's config, so nothing under `~/.config/zellij` is read or
edited. Consequences:

- Your Zellij keybinds and theme do not apply inside `run` sessions.
- Attaching to a session `run` did not create asks first.
- Before launching, it pre-authorizes its own wasm in Zellij's
  `permissions.kdl` (backup at `permissions.kdl.zj-radar.bak`), so Zellij
  never prompts. If that write is refused, Zellij asks inside the rail: focus
  it and press `y`. A session already waiting at that prompt keeps it: the
  `run` that writes the grant while attaching warns about it, later runs do
  not. If an earlier `run` already wrote the grant (for example while
  creating another session), no `run` warns; the rail itself still shows the
  prompt.
- On top of Zellij's defaults it binds `Alt 1`–`Alt 9` (tab jumps), so those
  chords do not reach programs in the panes.
- It uses the wasm bundled into the binary. A `cargo install` build has no
  bundled wasm and downloads the matching one on first use.

## Upgrading from a release with agent adapters

Releases up to 0.12 wired each agent to the sidebar with hooks, plugins, or
extensions that call `zj-radar notify`. The current CLI has no `notify` and
no agent targets: agents that emit OSC 7501 need no setup, and
`zj-radar setup <agent>` refuses. Remove the old adapters with the OLD CLI,
before you install the new one:

```sh
for a in claude codex opencode pi omp; do zj-radar setup "$a" --uninstall; done
claude plugin marketplace remove zj-radar
```

The second line drops the `zj-radar` marketplace entry, which
`setup claude --uninstall` leaves in Claude Code. If you already upgraded,
remove the adapters by hand:

| Agent | What to remove |
|---|---|
| Claude Code | `claude plugin uninstall zj-radar-claude@zj-radar`, then `claude plugin marketplace remove zj-radar`. |
| Codex | In `$CODEX_HOME/hooks.json` (default `~/.codex/hooks.json`), every hook whose command contains `ZJ_RADAR_CODEX_HOOK=v1`. In `$CODEX_HOME/config.toml`, a top-level `notify = ["zj-radar", "notify", "codex"]` if you used `--legacy-notify`. |
| Opencode | `plugins/zj-radar.js` and the `plugins/zj-radar/` directory under `$XDG_CONFIG_HOME/opencode/` (default `~/.config/opencode/`). |
| pi | `extensions/zj-radar.js` under `$PI_CODING_AGENT_DIR` (default `~/.pi/agent/`). |
| omp | `extensions/zj-radar-omp.js` under omp's agent dir: `~/.omp/agent/` by default, `~/.omp/profiles/<name>/agent/` under a named profile (`$OMP_PROFILE`), or `$PI_CODING_AGENT_DIR` when set. `$PI_CONFIG_DIR` replaces `.omp`. |

Restart each agent afterwards. A `.zj-radar.bak` beside a file is that file as
it was before setup edited or replaced it.

An adapter left behind does no harm while the new CLI is on `PATH`: it keeps
a hidden `zj-radar notify` that only prints a reminder to remove the adapter
and exits 0, so it never blocks its agent.

Older releases also left runtime state you can delete at any time: a
`zj-radar-dedup-<uid>/` directory under `$XDG_RUNTIME_DIR` (else `$TMPDIR` or
`/tmp`), and the rail's per-session files under Zellij's plugin cache
(fallback `$TMPDIR/zellij-<uid>/zj-radar`).

The upgrade also changes the permission set (see [Permissions](#permissions)).
`zj-radar update` and `setup zellij` rewrite the grant; a hand-written grant
needs the new permission added.

## Build from source

```sh
git clone https://github.com/marktoda/zj-radar
cd zj-radar

# rust-toolchain.toml requests the wasm32-wasip1 target; rustup installs it.
cargo build --release --target wasm32-wasip1 -p zj-radar-plugin
cargo install --path crates/cli

zj-radar setup zellij --wasm target/wasm32-wasip1/release/zj_radar.wasm
```

See [`TOOLCHAIN.md`](TOOLCHAIN.md) if your toolchain lacks the wasm target.

## Manual setup

Without the CLI, copy the wasm to the stable path yourself, from a source build
or a release download:

```sh
mkdir -p ~/.config/zellij/plugins
cp target/wasm32-wasip1/release/zj_radar.wasm ~/.config/zellij/plugins/
```

Define the alias once in `~/.config/zellij/config.kdl`:

```kdl
plugins {
    radar location="file:~/.config/zellij/plugins/zj_radar.wasm" {
        naming "managed"
    }
}
```

The path must be stable. Zellij ties a plugin's permission grant to its
location string, so a path that changes on every rebuild re-prompts every time.

## Add the sidebar to a layout

The sidebar is a pinned, borderless left column in every tab. Zellij's only
mechanism for that is the tab templates, where its own tab bar and status bar
live, so zj-radar integrates the way [zjstatus](https://github.com/dj95/zjstatus)
does: one pane in your templates, the rest of the layout stays yours.

With the CLI, inject or re-inject at any time:

```sh
zj-radar setup zellij --inject              # into the default layout
zj-radar setup zellij --inject --layout my  # into layouts/my.kdl
zj-radar setup zellij --uninstall           # strip the injected rail
```

If no layout file exists (stock Zellij ships none), `--inject` creates one with
the full rail layout.

By hand, add this to any layout file:

```kdl
// Tabs defined in the layout file get their panes via `children`.
default_tab_template {
    pane split_direction="vertical" {
        pane size=32 borderless=true {
            plugin location="radar"   // ← alias
        }
        children
    }
    pane size=2 borderless=true {
        plugin location="zellij:status-bar"
    }
}

// Tabs created at runtime (Ctrl+t n) get a CONCRETE focused pane, not `children`.
new_tab_template {
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

Both templates are required. Zellij derives `new_tab_template` from
`default_tab_template` when you omit it and drops a `children` nested inside a
split, leaving new tabs with no focusable pane. See
[Can't open a new tab](troubleshooting.md#cant-open-a-new-tab-the-two-template-rule).

If the layout also declares `tab` nodes, give each one a pane in its body
(`tab focus=true { pane }`): a nested `children` is filled by the tab body, and
an empty body yields a tab with nothing but the rail — see
[Tab opens with only the rail](troubleshooting.md#tab-opens-with-only-the-rail-or-the-session-exits-at-once).
Keep the plugin nodes on their own lines as above: Zellij 0.44's KDL parser
rejects the one-line form `{ plugin location="radar" }` unless it ends in `;`.

Any custom layout also discards Zellij's built-in swap layouts, so `Alt+[` /
`Alt+]` cycling stops working. The injected rail and the example layout below
redeclare `swap_tiled_layout` through a rail-carrying `tab_template name="ui"`.
If you write the layout by hand, copy those blocks; if your layout already has
its own swaps, see
[Alt+] hides the rail](troubleshooting.md#alt-hides-the-rail-or-stops-cycling).

With [`show_mode true`](configuration.md#options) the rail's footer
shows the current Zellij mode, so you can delete every `zellij:status-bar` pane
(one per template, including `tab_template name="ui"` if your layout has it)
and get those rows back. You lose the status bar's keybinding hints.

For a complete starting layout, copy
[`examples/radar-sidebar.kdl`](../examples/radar-sidebar.kdl) to
`~/.config/zellij/layouts/` and run `zellij --layout radar-sidebar`.

To put the column on the right, move `children` (and the runtime
`pane focus=true`) before the radar pane in each split. Change `size` for a
different width.

## Permissions

Zellij asks for an explicit grant the first time a plugin loads from a given
path. The sidebar requests five:

- `ReadApplicationState`: read tabs, panes, and Zellij's attention state for
  this session and the others.
- `ReadCliPipes`: receive the `zj_radar.config.v1` and `zj_radar.cmd.v1` pipes
  ([configuration](configuration.md)).
- `ChangeApplicationState`: switch tabs, panes, and sessions on click, apply
  managed tab names, and mark a pane seen when you click `✓`.
- `RunCommands`: run `git remote get-url origin` in a pane's directory to name
  its tab. Nothing else is ever run.
- `ReadPaneContents`: see the text of shell commands. Without it Zellij
  withholds that text, and command rows show only their state.

Releases up to 0.12 requested only the first four. Zellij prompts again when
a grant lacks a permission the plugin requests, so a grant written for those
releases prompts again.

You normally never see the prompt. `setup zellij` asks for consent at install
time and writes the grant into Zellij's `permissions.kdl` itself: merge-safe,
other plugins' entries untouched, a `.zj-radar.bak` left beside the file, and
an unparseable file refused rather than edited. Zellij re-reads the file on
every plugin load, so the sidebar comes up live on the next launch, or on the
next new tab in a running session. To revoke, delete the `zj_radar.wasm`
block from `permissions.kdl`.

If you declined, the sidebar shows Zellij's own y/n prompt on first load, and
at rail width that prompt looks blank. Press `y` in the rail pane. Details in
[Sidebar shows "needs permission"](troubleshooting.md#sidebar-shows-needs-permission-or-looks-blank).

## Check your setup (`--check`)

`zj-radar setup zellij --check` is a read-only diagnostic:

```
zj-radar setup zellij --check
zellij:
  ok zellij binary: found on PATH (zellij 0.46.0)
  ok alias: radar plugin alias present in config.kdl
  ok wasm: wasm plugin file present
  missing layout: `default` layout does not have the radar rail — run `zj-radar setup zellij` or paste the snippet
  missing grant: wasm not granted — run `zj-radar setup zellij -y` to pre-authorize (or focus the rail and press y)
```

Items are `ok`, `warn`, or `missing`. Any `missing` item makes the command exit
nonzero.

- **zellij binary**: on `PATH`; warns below 0.46.0.
- **alias**: `radar` alias in `config.kdl`; warns if it points at a
  `/nix/store/` path, since that grant dies on the next rebuild.
- **wasm**: file present at the stable path.
- **layout**: the default layout (or `--layout <name>`) contains the rail.
- **tab bodies** (only when the layout has the rail): `tab` nodes with no pane
  in their body; see
  [Tab opens with only the rail](troubleshooting.md#tab-opens-with-only-the-rail-or-the-session-exits-at-once).
- **grant**: `permissions.kdl` grants the wasm path every permission the
  sidebar requests.
- **managed config** (only when `config.kdl` is a symlink, as under
  home-manager): direct edits may be overwritten.
- **config env** (only when `$ZELLIJ_CONFIG_FILE` points elsewhere): Zellij
  reads that file, not the one setup edits.

Bare `zj-radar setup --check` is the same check.

## Upgrade (`zj-radar update`)

```sh
zj-radar update            # move the CLI and the sidebar to the latest release
zj-radar update --check    # report only; exit 1 when an update is available
```

`update` does three things:

1. Finds the latest release (`ZJ_RADAR_VERSION` pins a tag) and compares it
   to this CLI. If newer, it downloads the binary for this platform, verifies
   the release's `.sha256` checksum, and swaps it in place. The running
   process is unaffected; the next `zj-radar` is the new one.
2. Runs `setup zellij --download` through the new binary, so the wasm at
   `~/.config/zellij/plugins/zj_radar.wasm` comes from the same release. The
   file is rewritten only when its bytes differ.
3. Runs the doctor (`setup --check`).

Restart Zellij, or open a new session, to load the new sidebar. A running
session keeps the old plugin.

From a release with agent adapters, remove them first, as in
[Upgrading from a release with agent adapters](#upgrading-from-a-release-with-agent-adapters).
The old `update` re-runs `setup <agent>` through the new binary, which refuses
it, so `update` ends with an error. The new CLI and sidebar are installed all
the same.

`--check` compares both halves: the CLI version against the release tag, and
the installed wasm's checksum against the release's published `.sha256`. A
wasm built from source reads as differing, since it is not byte-identical to
the release artifact.

What `update` does with each kind of install:

| Install | Behavior |
|---|---|
| `curl \| sh` installer (`~/.local/bin`) | Replaced in place. |
| `cargo install` (`~/.cargo/bin` or `$CARGO_HOME/bin`) | Left alone. Run `cargo install zj-radar` (or `cargo binstall zj-radar`), then `zj-radar setup zellij --download`. |
| Nix / home-manager (`/nix/store` binary, or a symlinked wasm) | Left alone. Update the flake input. |
| No sidebar installed | Reported; run `zj-radar setup zellij --download` to add one. |
| No prebuilt binary (Intel macOS) | Build from source, then `zj-radar setup zellij --download`. |
| `ZJ_RADAR_VERSION` older than this CLI | Refused. Downgrade with the installer and that tag. |

Re-running `zj-radar setup zellij --download` by hand refreshes the wasm alone.

## Loading straight from a release URL

Zellij can load a plugin from an `https://` URL and cache it:

```kdl
plugin location="https://github.com/marktoda/zj-radar/releases/latest/download/zj_radar.wasm"
```

Not recommended here. The sidebar loads once per tab, and Zellij has a known
bug where several tabs fetching the same remote plugin at once corrupt the
download. Use the `file:` path or the Nix package; keep the URL form for a
single-tab try.

## Nix / home-manager

The flake exposes the wasm as `packages.default` and the CLI as
`packages.zj-radar-cli`:

```nix
# flake.nix
inputs.zj-radar.url = "github:marktoda/zj-radar";
```

The wasm is all Zellij needs. Install the CLI from the same pin if you want
`setup --check` and `run`:

```nix
# home-manager module
home.packages = [inputs.zj-radar.packages.${pkgs.system}.zj-radar-cli];

# Symlink the wasm to a STABLE path instead of pointing the alias at the
# /nix/store path: Zellij keys grants by the location string, so a per-build
# store path re-prompts after every rebuild (`--check` warns about this).
home.file.".config/zellij/plugins/zj_radar.wasm".source =
  "${inputs.zj-radar.packages.${pkgs.system}.default}/bin/zj_radar.wasm";
```

Then point the alias in your generated `config.kdl` at that path:

```kdl
plugins {
    radar location="file:~/.config/zellij/plugins/zj_radar.wasm" {
        naming "managed"
    }
}
```

To pin the release wasm without a Rust toolchain:

```nix
zjRadarWasm = pkgs.fetchurl {
  url = "https://github.com/marktoda/zj-radar/releases/latest/download/zj_radar.wasm";
  hash = "sha256-..."; # nix-prefetch-url the asset to fill this in
};
```

`setup` treats a symlinked `config.kdl`, layout, or wasm as yours to manage:
it warns and leaves the file alone rather than replace or remove the link.

## Files zj-radar creates (and how to remove them)

Paths are the defaults; `ZELLIJ_CONFIG_DIR` / `XDG_CONFIG_HOME` move the
config-dir entries with them.

| File | Created by | `--uninstall` |
|---|---|---|
| `~/.config/zellij/config.kdl`: managed `radar` alias between `// zj-radar:` markers | `setup zellij` | **reversed**: strips only the fenced block |
| `~/.config/zellij/layouts/<name>.kdl`: rail spliced between markers | `setup zellij --inject` into an existing layout | **reversed**: exact inverse of the splice |
| `~/.config/zellij/layouts/<name>.kdl`: whole file | `setup zellij --inject` when no layout existed | left in place; delete the file |
| `<edited file>.zj-radar.bak` | every config/layout edit, and `run`'s grant write (`permissions.kdl.zj-radar.bak`) | left in place; they are your restore points |
| `~/.config/zellij/plugins/zj_radar.wasm` | `setup zellij --wasm/--download` | left in place; `rm` it |
| `permissions.kdl` grant entry (macOS `~/Library/Caches/org.Zellij-Contributors.Zellij/`, Linux `~/.cache/zellij/`) | `setup zellij` with your consent, `zj-radar run`, or Zellij when you answer `y` | left in place (Zellij also writes this file); delete the `zj_radar.wasm` block |
| `run`'s config dir (macOS `~/Library/Application Support/zj-radar/`, Linux `~/.local/share/zj-radar/`) | `zj-radar run` | not touched by `setup`; `rm -r` it, it holds only re-materializable assets and session markers |

The plugin itself writes no files.

Complete removal:

```sh
zj-radar setup zellij --uninstall -y
```

then delete the wasm, the `run` data dir, the grant block, and the binary.
Agent adapters from releases up to 0.12 are not in this table; remove them as
in [Upgrading from a release with agent adapters](#upgrading-from-a-release-with-agent-adapters).
