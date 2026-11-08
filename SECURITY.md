# Security policy

Please report vulnerabilities privately via
[GitHub Security Advisories](https://github.com/marktoda/zj-radar/security/advisories/new)
rather than a public issue. You should hear back within a few days.

The main supply-chain surface is distribution: the `curl | sh` installer and its
per-artifact `.sha256` checksum sidecars, and the CLI's checksum verification of
the downloaded sidebar wasm — the same code path behind
`zj-radar setup zellij --download` *and* `zj-radar run`'s first-use wasm fetch
on a crates.io-installed CLI. Be aware the verification **fails open** by
design: a missing `.sha256` sidecar, or no local sha256 tool
(`sha256sum`/`shasum`), downgrades to a warning and installs anyway, with TLS +
GitHub release storage as the floor; only an actual digest mismatch aborts
(`crates/cli/src/setup/download.rs`, `scripts/install.sh`). Reports about
weaknesses in that path are especially welcome. There is no bug bounty.

Dependency advisories are monitored by a nightly
`cargo deny check advisories licenses sources` CI job (config in `deny.toml`);
a failure automatically files a tracking issue.

## Trust boundaries

The plugin reads three inputs it does not control. It treats all of them as
untrusted display data.

- **OSC 7501 status from programs in panes.** Any program that writes to a
  pane can report a status for it, including a remote host reached over
  `ssh`, and Zellij passes that status to the plugin. Every text field is sanitized (control, ANSI, and bidi characters
  stripped) and truncated before it reaches the rail.
- **The `zj_radar.config.v1` and `zj_radar.cmd.v1` pipes.** Any process in the
  session (or another plugin, via `MessagePlugin`) can send them. `config.v1`
  payloads over 64 KB are dropped whole; unknown keys, values, and verbs are
  ignored. At worst a sender changes display or naming options, or moves focus
  to another tab or session.
- **The git remote read.** For tab naming the plugin runs
  `git -C <cwd> remote get-url origin` in each pane's working directory, the
  only command it ever runs. Whoever controls that directory controls the
  answer. The plugin keeps only the last path segment of the URL as a tab
  name, sanitized like any other text.

What this cannot prevent: a local writer can always paint misleading status or
a misleading tab name. That is inherent to the boundary, not a vulnerability.
