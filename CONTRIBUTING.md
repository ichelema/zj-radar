# Contributing to zj-radar

zj-radar is a [Zellij](https://zellij.dev) sidebar (Rust → `wasm32-wasip1`)
plus a host-side CLI. Zellij computes each pane's attention from the OSC 7501
and OSC 133 sequences programs print, and the sidebar reads it, so any agent
that emits OSC 7501 shows up with no adapter. This guide covers building,
testing, and proposing changes.

## Project shape

A two-crate Cargo workspace:

| Path | What it is |
|------|------------|
| `crates/cli/` | The `zj-radar` binary (`setup`, `run`, `update`; a hidden `notify` no-op keeps hooks left by older releases exiting 0). `build.rs` embeds the wasm with `include_bytes!`. Published to crates.io. |
| `crates/plugin/` | The wasm sidebar. `lib.rs`/`main.rs` are the only files that touch the Zellij host API and are wasm-only; everything else (native attention, runtime, roll-up, renderer, naming, ledger, sessions) is host-testable. |
| `docs/` | User docs, the design doc, the executable rail spec. |

Two rules are load-bearing. Read [`CONTEXT.md`](CONTEXT.md) before changing
the plugin.

- **Push-driven, never poll-driven.** Zellij pushes each pane's attention
  (`PaneUpdate`, `PaneAttentionChanged`, `GlobalAttentionUpdate`); the plugin
  makes no blocking host queries on any per-event or per-tick path. Two
  exceptions, both for tab naming: a single `get_pane_cwd` per new pane
  (`Effect::ResolveCwd`), and one async `git remote get-url origin` per new
  cwd (`Effect::ResolveRepo`).
  Polling melted the predecessor ([postmortem](docs/smart-tabs-postmortem.md));
  a PR that reintroduces it will not be accepted.
- **Rail lockstep.** The emitted ANSI and the click-target map stay in 1:1
  line correspondence, structurally (one `Vec<Line>` derives both). See
  `CONTEXT.md` → *Lockstep*.

## Prerequisites

- A stable Rust toolchain. `rust-toolchain.toml` requests the `wasm32-wasip1`
  target and `rustup` installs it on first build ([`docs/TOOLCHAIN.md`](docs/TOOLCHAIN.md)).
- **MSRV is Rust 1.95** (`rust-version` in the root `Cargo.toml`). CI's `msrv`
  job builds with exactly that toolchain.
- For the full suite: `just`, `bats`, `shellcheck`, and `zellij` ≥ 0.46.0 for
  the live E2E layer (CI builds the Zellij rev the plugin pins, `ZELLIJ_REV`
  in `e2e.yml`).
- Optional: Nix. `nix develop` pins all of the above; `nix flake check` runs
  what the `hermetic` CI job runs.

## Build

```sh
cargo build                                                        # host library + CLI
cargo build --release --target wasm32-wasip1 -p zj-radar-plugin    # the wasm Zellij loads
```

## Test

```sh
just test        # L1–L4: unit, insta snapshots, proptest, vt100 (deterministic, host-only)
just test-bash   # shellcheck + bats for install.sh, funnel.sh, and the demo/ scripts
just test-e2e    # L5: builds the wasm and drives a real Zellij in a PTY
just ci          # what every PR must pass: fmt check + test + clippy + wasm build + test-bash
just review      # accept intentional insta snapshot changes
```

Run one test with `cargo test <name>`, scoped with `-p zj-radar-plugin` and so
on. Most work needs no wasm build: the plugin's modules have no `zellij-tile`
dependency on the native target.

- **Snapshots** use [`insta`](https://insta.rs). After an intentional render
  change, accept with `just review`. CI fails on unreviewed drift.
- **E2E is serial** (`--test-threads=1`); parallel Zellij sessions contend at
  startup. It runs nightly on both OSes, on PRs that touch `crates/plugin/`, the
  lockfile, `justfile`, the flake, or `e2e.yml` (ubuntu only), as a release
  gate, and on `workflow_dispatch`.

### Welded files

Some docs and assets are test inputs. Edit them through their test.

| File | Pinned by |
|---|---|
| `docs/rail-reference.md` (`rail-input`/`rail-expect` blocks) | `crates/plugin/src/reference_tests.rs` |
| `docs/configuration.md` (both pipe names; every `cmd.v1` verb) | `config.rs`, `control.rs` |
| `examples/radar-sidebar.kdl` | `crates/cli/src/layout.rs` |
| Grant probe (`REQUIRED_PLUGIN_PERMISSIONS`) and Zellij version floor (`MIN_SUPPORTED_ZELLIJ`) | `cli_grant_probe_covers_the_exact_requested_permission_set` and `doctor_version_gate_matches_the_zellij_tile_pin`, text pins in `crates/plugin/src/lib.rs` |
| `flake.nix` source filter | the inventory of every non-Rust file the hermetic build reads; a new `include_str!` outside crate sources needs an entry |

The README quick start is run verbatim by `scripts/funnel.sh` on every release.
Change the commands there and in the script together.

## Lint and formatting

```sh
just fmt        # cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Rust code is formatted by rustfmt with the repo's `rustfmt.toml` (width 120,
dense heuristics). `just ci` and the CI `checks` job run
`cargo fmt --all --check`, so run `just fmt` before pushing. The one-time
reformat commit is listed in `.git-blame-ignore-revs`; point
`git config blame.ignoreRevsFile .git-blame-ignore-revs` at it to skip it in
`git blame` (GitHub's blame view does this automatically).

`cargo deny check` runs nightly in CI (advisories, licenses, sources; config
in `deny.toml`), not on PRs, because the advisory DB changes daily.

## Dev loop

```sh
just dev          # build wasm + CLI, launch a fresh sandboxed zj-radar-dev-<hhmmss> session
just dev-build    # build the dev artifacts without launching
```

`just dev` drives the real `zj-radar run` flow, grant pre-seed included,
under `target/dev/data` (`ZJ_RADAR_DATA_DIR` + `ZJ_RADAR_WASM`). Every run is
a new uniquely named session: Zellij does not safely hot-reload layout-created
plugins, and attaching to a leftover would keep running the old wasm. Exited
dev sessions are swept; live ones are never killed. Run it from a plain
terminal; `zj-radar run` refuses to nest inside Zellij. In the Nix shell,
`nix develop -c just dev`.

## Pull requests

1. Open an issue first for anything non-trivial.
2. One logical change per PR.
3. `just ci` must pass.
4. Add tests at the right layer: render behavior → a snapshot or a
   `rail-reference.md` scenario; attention/parse behavior → a unit or proptest.
5. Update the docs when behavior or interfaces change (see the ownership map
   below).
6. Any new `include_str!`/`include_bytes!` of a non-Rust file needs an entry
   in `flake.nix`'s source filter, or the hermetic job cannot see it.
7. Don't commit `target/` or editor state.

## Docs

Each topic has one home; everything else links to it.

| Topic | Home |
|---|---|
| Pitch, quick start, doc index | `README.md` |
| Install, permissions, layouts, removal | `docs/install.md` |
| Reading the rail, mouse, badge | `docs/using.md` |
| Options and pipes | `docs/configuration.md` |
| Producers (OSC 7501) | `docs/producers.md` |
| Symptom → fix | `docs/troubleshooting.md` |
| Status × class semantics | `docs/activity-model.md` |
| Architecture and mechanisms | `docs/design.md` |
| Glossary | `CONTEXT.md` |
| Exact rendered grid | `docs/rail-reference.md` |

Style: present tense, describe what ships. State a rule once and link to it.
Rationale lives in `design.md`; user docs give the rule. One idea per
sentence. Code identifiers only in contributor docs.

## Adding a producer or an agent

The producer interface is OSC 7501, the Program Status Protocol Zellij reads
([`docs/producers.md`](docs/producers.md#writing-your-own-producer)). A new
agent needs no code. The one optional entry is its `app` token: map it to a
`Kind` in the `kinds!` table in `crates/plugin/src/kind.rs`
(`attention::kind_for_app` routes the token); an unknown `app` renders as
`Kind::Other`. Shell commands like `cargo test` come from Zellij's OSC 133
tracking and show as `Kind::Command`, or `Kind::Server` when the text has a
service word (`attention::command_kind`).

## License

By contributing, you agree that your contributions are licensed under the
project's [MIT License](LICENSE).
