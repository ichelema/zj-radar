# Producers: getting status onto the rail

The sidebar only displays. A **producer** is any program that reports its
state to Zellij with the Program Status Protocol: an `OSC 7501` escape
sequence printed to its own terminal. Zellij turns those reports, and its own
shell-command tracking, into per-pane attention, and the rail draws it. There
is no hook, plugin, or setup step on the zj-radar side.

This needs Zellij 0.46.0 or newer and the [sidebar](install.md). Desktop
notifications come from Zellij itself (`attention` → `desktop_notifications`
in its config), not from zj-radar.

## Supported agents

These agents speak OSC 7501 natively. Run them in a Zellij pane and their row
appears.

| Agent | What it reports | Limits |
|---|---|---|
| [Claude Code](#claude-code) | working, needs you, done; subagents as child records; clears its status on exit | Reports only when its OSC 7501 probe gets an answer |
| [Codex](#codex) | its turn state, `app=codex` | none known |
| [pi](#pi) | working, needs you, done, error, idle | Interactive mode only |
| [Opencode](#opencode) | working and needs you, one child record per session | No done or error: a finished turn reads idle |
| [omp](#omp) | as pi | Needs a release containing can1357/oh-my-pi#17147 |

### Claude Code

Verified with Claude Code 2.1.318. It reports `state`, `kind`, `progress`,
`title`, and `msg` with `app=claude-code`, and sends `state=clear` when it
exits. Each subagent is a child record, drawn as a background-task line; a
subagent's permission prompt arrives on the root record.

Claude Code reports only after its `OSC 7501 ; ?` probe gets an answer, and
Zellij 0.46 answers it. Two environments turn reporting off:

- `CLAUDE_CODE_DISABLE_TERMINAL_TITLE` set. Claude Code sets it for the
  processes it starts, so a Claude launched from Claude's own Bash tool
  reports nothing.
- A background session (`CLAUDE_CODE_SESSION_KIND=bg`).

### Codex

Codex 0.167.0 emits OSC 7501 with `app=codex`.

### pi

pi reports from its interactive TUI only. Its `msg` is the session name while
working or done, the dialog title while blocked, and the first line of the
error on failure. Prompts and assistant output are never reported. "Needs you"
appears only when an extension opens a dialog, since pi has no built-in
permission prompts.

### Opencode

Opencode reports `working` and `blocked` as one child record per session, with
the session ID as `id` (anomalyco/opencode#55172). It has no root record, so
the rail takes the pane's status from its most severe session. Opencode sends
no `done` or `error` (anomalyco/opencode#60411): at the end of a turn the
record goes away and the row reads idle, so the finished turn never reaches
the `earlier` ledger.

### omp

omp (oh-my-pi, a fork of pi) needs a release that contains
can1357/oh-my-pi#17147. Before that fix, omp's own OSC 133 prompt marks made
Zellij drop its `working` record.

### Upgrading from agent adapters

Older zj-radar releases wired each agent with hooks, plugins, or extensions
that called `zj-radar notify`. Remove them as described in
[Upgrading from a release with agent adapters](install.md#upgrading-from-a-release-with-agent-adapters).

## Writing your own producer

Any script can put a row on the radar: print OSC 7501 to the terminal of the
pane it runs in. Zellij attributes the sequence to the pane whose terminal
receives it, so print to `/dev/tty` when your stdout may be piped.

The sequence is:

```text
ESC ] 7501 ; key=value:key=value ESC \
```

- Pairs are separated by `:`, not `;`.
- `BEL` (`\a`) also terminates the sequence.
- Keys are lowercase ASCII. A key over 16 bytes drops the whole sequence.
- Values use only `A-Z a-z 0-9 _ . , + / = -`, so free text travels as base64
  (`msg`, `title`).
- A pair with an unknown or non-lowercase key, or a value with other
  characters, is skipped.
- A sequence over 4096 bytes is dropped.

A deploy script that reports its progress:

```sh
#!/usr/bin/env bash
# One OSC 7501 record for this pane.
status() { printf '\033]7501;%s\033\\' "$1" > /dev/tty; }
b64() { printf %s "$1" | base64 | tr -d '\n'; }

status "state=working:app=deploy:progress=40:msg=$(b64 'uploading assets')"
# … do the work …
status "state=blocked:app=deploy:kind=permission:msg=$(b64 'promote to production?')"
read -r answer
status "state=done:app=deploy:msg=$(b64 'deployed v1.4')"
```

[`demo/agent.sh`](../demo/agent.sh) is a longer example: it scripts whole
agent turns with `printf`.

To check that the terminal understands the protocol, send the query
`ESC ] 7501 ; ? ESC \`. Zellij answers with the states and kinds it accepts:

```text
ESC ] 7501 ; ?:states=idle,working,done,blocked,error:kinds=permission,question,auth ESC \
```

### Field reference

| Key | Values | Effect |
|---|---|---|
| `state` | `idle`, `working`, `blocked`, `done`, `error`, `clear` | Required. Any other value drops the sequence. |
| `app` | `A-Z a-z 0-9 _ . + -`, at most 32 bytes | The program's name, which picks the row's mark (below). Longer drops the sequence; other characters drop the key. |
| `kind` | `permission`, `question`, `auth` | With `blocked` only: the label is `needs approval`, `has a question`, or `needs login`. Without it the label is `needs you`. |
| `progress` | `0` to `100` | With `working` or `blocked` only: the text reads `working 40%: …`. |
| `msg` | base64 of UTF-8, at most 2048 bytes decoded (2732 encoded) | The activity text. |
| `title` | base64 of UTF-8, at most 192 bytes decoded (256 encoded) | A short name. On the root record it shows only when there is no `msg`. |
| `id` | a path of `/`-separated segments | Makes the record a child record (below). |

A `msg` or `title` that is not valid base64, exceeds its cap, or decodes to
control characters drops the whole sequence.

How each `state` renders on the rail (the full matrix is in
[the activity model](activity-model.md#3-the-state--class-presentation-matrix)):

| `state` | Rail |
|---|---|
| `working` | Spinner. `app=server` and `app=remote` hold a steady `▸` instead. |
| `blocked` | `◆`, needs you. A `· 12m` wait tag after a minute, and a `✓` to mark it seen. |
| `done` | `●`. Once you see the pane, the row goes idle and the completion moves to `earlier`. |
| `error` | `✗`. Same as `done` for seeing and `earlier`. |
| `idle` | A muted `○` row, kept while the pane has a record. |
| `clear` | Removes the records: the row disappears. |

The row's text is Zellij's attention text, which already carries the label
and the progress: `working 40%: uploading assets`, `needs approval: promote to
production?`, `done: deployed v1.4`. A `working` record without `progress`
shows its `msg` alone.

`app` picks the mark. Tokens are case-sensitive.

| `app` | Mark |
|---|---|
| `claude-code`, `claude` | `✳` |
| `codex` | `❉` |
| `opencode` | `✺` |
| `pi` | `✴` |
| `omp` | `✷` |
| `gemini` | `✦` |
| `test` | `⚗` |
| `build` | `⚙` |
| `deploy` | `⇡` |
| `server` | `❯`, steady |
| `remote`, `zellij` | `⇄`, steady |
| `command` | `$` |
| anything else, or none | `⦿` |

### Child records

A record without `id` is the pane's root record. A record with `id` is a child
record: a subagent, a background job, a session. Each child is drawn as a `┊`
background-task line under the pane's row.

| Child `state` | Task line |
|---|---|
| `working`, `blocked` | Spinner and its age, `· 4m` |
| `done` | `●` |
| `error` | `✗` |
| `idle` | Muted `·` |

- A pane shows at most three task lines: two, then `┊ +N more`.
- `task_lines` in the [configuration](configuration.md#options) switches to a
  `+N` count or hides them.
- A task line's label is the child's `title`, else its `msg`, else its `id`.
- Task lines themselves never feed the tab's status or counts.
- Zellij lets a child raise the pane's status only when it is `blocked` or
  `error`.
- A pane with children and no root record takes its status from its most
  severe child, which becomes the pane's row.
- A child without `app` inherits the nearest ancestor's.
- Zellij 0.46 reports a child that appears or goes away under a root that
  stays `working` only at the pane's next update, such as a new root `msg`
  or a tab switch, so its task line can show up or stop late
  ([design](design.md#5-plugin--zellij-wiring)).

`id` rules: at most 8 segments, each at most 32 bytes of
`A-Z a-z 0-9 _ . + -`, at most 128 bytes in all. An invalid `id` drops the
sequence. A pane holds at most 256 records; a new one past that evicts the
oldest.

### Lifecycle

- Each record is replaced by the next one with the same `id` (or none, for the
  root).
- `state=clear:id=job` removes `job` and every record under `job/`.
  `state=clear` with no `id` removes all of the pane's records.
- `done` and `error` stay until you see the pane. A focused pane counts as
  seen shortly after.
- When the shell prompt returns, the pane's foreground program exits, or the
  pane closes, Zellij drops the `idle`, `working`, and `blocked` records. A
  script that dies mid-run leaves no stale spinner.
- Send `done` or `error` when your script finishes, so the outcome stays on
  the rail.

Shell commands need no producer: Zellij tracks them and the rail shows them as
`$` rows ([activity model](activity-model.md#4-where-the-status-comes-from)).

## Remote sessions over ssh

A remote Zellij reports its session to the terminal it runs in. With
`attention.host_status` (default `true`) it sends an aggregated OSC 7501 with
`app=zellij`, once that terminal answers its `OSC 7501 ; ?` query. Your local
Zellij answers it.

So a local pane that runs `ssh host` and then `zellij attach` gets the remote
session's status, with no setup on either side:

- The row carries the remote mark `⇄` and Zellij's text, which names the
  remote tab and pane (`Tab #1 › Pane #1: …`).
- A working remote session holds the steady `▸`, and the tab line shows `⇄`.
- Needs you, done, and error render as for a local agent.
- When the remote client detaches or quits, it sends `state=clear` and the row
  goes away.

A plain `ssh` session with no remote Zellij reports nothing: it is a command
you type into, which Zellij does not list.
