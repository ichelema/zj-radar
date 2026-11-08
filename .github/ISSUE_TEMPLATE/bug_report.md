---
name: Bug report
about: Something in the sidebar or the CLI isn't behaving
title: ""
labels: bug
assignees: ""
---

## What happened

A clear description of the bug.

## Expected

What you expected instead.

## Repro

Steps to reproduce. A minimal layout snippet, or the OSC 7501 `printf` or
`zellij pipe` command that triggers it, is ideal:

```sh
# e.g. the status you reported, the pipe message you sent, or the layout you loaded
```

## Environment

- zj-radar version / commit:
- Zellij version (`zellij --version`):
- OS:
- Pane program: Claude Code / Codex / Opencode / pi / omp / other OSC 7501 program / shell command (with its version)
- Installed via: `install.sh` / `cargo install` / Nix / release artifact / build from source
- `zj-radar setup --check` output:

## Notes

Anything else — screenshots of the rail, relevant config (`density`/`naming`/
`glyphs`), whether it's reproducible or intermittent.
