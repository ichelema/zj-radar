# AGENTS.md

Entry point for AI agents (and humans skimming) working in zj-radar. It points
at the real docs rather than duplicating them.

zj-radar is a [Zellij](https://zellij.dev) sidebar (Rust → `wasm32-wasip1`)
plus a host-side `zj-radar` CLI. Zellij 0.46 computes each pane's attention
from the OSC 7501 and OSC 133 sequences programs print, and the sidebar reads
it, so any agent that emits OSC 7501 shows up with no adapter.

## Read first

- [`CONTEXT.md`](CONTEXT.md): domain glossary and the seams. Read before
  changing the core.
- [`CONTRIBUTING.md`](CONTRIBUTING.md): project shape, build/test/lint, welded
  files, PR rules, docs ownership.
- [`docs/design.md`](docs/design.md): architecture and mechanisms.
- [`docs/activity-model.md`](docs/activity-model.md): what each status and
  kind means and how it renders.

## Commands

```sh
cargo build                                    # host library + CLI checks
cargo build --release --target wasm32-wasip1 -p zj-radar-plugin   # the wasm plugin Zellij loads
just test        # L1–L4 deterministic host suite (unit, insta, proptest, vt100)
just test-bash   # shellcheck + bats for install.sh, funnel.sh, demo/ (needs bats + shellcheck)
just test-e2e    # L5 live: builds wasm, drives a real Zellij in a PTY (needs zellij)
just fmt         # cargo fmt --all (rustfmt.toml)
just ci          # what every PR must pass: fmt check + test + clippy + wasm build + test-bash
just review      # accept intentional insta snapshot changes (cargo insta review)
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

No wasm build is needed for typical work: the plugin's domain modules
are host-testable (`zellij-tile` is wasm-only).

## Non-negotiable rules

- **Format with `just fmt`** (`cargo fmt --all`, config in `rustfmt.toml`);
  `just ci` and CI fail on unformatted code.
- **Push-driven, never poll-driven.** No polling, no blocking host queries on
  any per-event or per-tick path. Two exceptions, both for tab naming: the
  once-per-pane `Effect::ResolveCwd` bootstrap, and the once-per-cwd async
  `git remote get-url origin` read (`Effect::ResolveRepo`). See
  [`docs/smart-tabs-postmortem.md`](docs/smart-tabs-postmortem.md).
- **Rail lockstep.** Emitted ANSI and the click-target map stay in 1:1 line
  correspondence, structurally (`CONTEXT.md` → *Lockstep*).
- **Welded files.** Several docs and assets are test inputs; edit them
  through their test. The table is in
  [`CONTRIBUTING.md`](CONTRIBUTING.md#welded-files).

## Adding a producer or agent

The producer interface is OSC 7501, the Program Status Protocol Zellij reads
([`docs/producers.md`](docs/producers.md#writing-your-own-producer)). The
plugin's other external contracts are the `zj_radar.cmd.v1` and
`zj_radar.config.v1` pipes ([`docs/configuration.md`](docs/configuration.md));
it reads OSC 7501 through Zellij's `PaneInfo.attention` and `program_status`.
A new agent needs no code; at most it needs an `app` token →
`Kind` entry in the `kinds!` table in `crates/plugin/src/kind.rs`
(`attention::kind_for_app` routes the token). Shell commands like `cargo test`
come from Zellij's OSC 133 tracking, not from a producer.
