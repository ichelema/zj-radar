# Piano: stato degli agenti da OSC 7501 nativo (Zellij 0.46)

Branch: `feat/native-program-status`, da `main` (`5ba183c`). Il branch
`feat/remote-peer-attach` resta com'è e non entra in questo lavoro.

## 0. Obiettivo e decisioni

Il rail mostra lo stato che Zellij già calcola per ogni pane. Il plugin non
riceve più stato da pipe, hook o file. Ogni agente che emette OSC 7501
funziona senza adapter: Claude Code, Codex, pi, Opencode, omp (sezione 1.1).

Decisioni dell'utente:

1. Rimuovi tutti gli adapter: plugin Claude, hook Codex, bridge Opencode/pi/omp,
   `zj-radar notify`, pipe `zj_radar.status.v1`.
2. Per ogni agente il rail usa solo i campi OSC 7501: `state`, `kind`,
   `progress`, `title`, `msg`, record figli `id`. Spariscono branch, task label,
   background task con `holds`, ack via pipe.
3. Nome del tab: nome manuale (se l'utente ha rinominato il tab) → nome
   progetto dal git remote → nome della cwd.
4. Funzioni che restano: badge cross-session, ledger `earlier`, righe dei
   comandi osservati, tab naming automatico. Tutte con fonti native.

Decisioni prese nel piano (da confermare in review):

- Le notifiche desktop del plugin spariscono: Zellij ha
  `attention.desktop_notifications` nativo.
- Il pane flottante di onboarding e `defer_permission` spariscono: `setup` e
  `run` pre-scrivono già il grant in `permissions.kdl` (`setup/preseed.rs`), e
  Zellij risponde `Granted` da cache senza prompt.
- `tools/wasm-fuel` sparisce: il proto vendorizzato è della 0.44 e lo
  strumento non serve al prodotto.
- Il crate `crates/core` sparisce del tutto: dopo la rimozione nessuno lo usa.

## 1. Fatti controllati

Sorgente Zellij al commit installato (`zellij-git 0.44.1.r203.g43c8710`,
versione workspace `0.46.0`, `43c8710b3a9845f60d5a567c7410fc1197fabb66`).

| Fatto | Dove |
|---|---|
| `PaneInfo` ha `attention: PaneAttention`, `command_state: CommandState`, `program_status: Vec<ProgramStatusRecord>` | `zellij-utils/src/data.rs:3572` |
| `TabInfo` ha `attention: TabAttention` (status + counts) | `zellij-utils/src/data.rs:3497` |
| Ogni cambio di attention invia `PaneUpdate` (manifest di tutti i tab) e `TabUpdate` | `zellij-server/src/screen/attention.rs:236` (`flush_attention`) |
| `PaneUpdate` arriva solo ai plugin del tab attivo (più i background plugin) | `zellij-server/src/screen.rs:8481` |
| `PaneAttentionChanged`, `CommandStarted`, `CommandFinished`, `GlobalAttentionUpdate` vanno a tutti i plugin iscritti | `screen/attention.rs:271`, `plugins/mod.rs:1621` |
| Tutti questi eventi richiedono solo `ReadApplicationState` | `plugins/wasm_bridge.rs:3489` |
| Senza `ReadPaneContents` Zellij toglie il testo dei comandi da attention/command_state | `plugins/wasm_bridge.rs:3562` |
| `GlobalAttention` porta tutte le sessioni: nome, status, counts, item per pane | `zellij-utils/src/attention.rs` (`GlobalAttention`) |
| `mark_session_pane_seen`, `focus_session_pane`, `mark_all_seen` esistono; permesso `ChangeApplicationState` | `zellij-tile/src/shim.rs:1049,1125`; `zellij_exports.rs:6764` |
| Uno Zellij remoto con `attention.host_status` (default `true`) emette verso il suo terminale un OSC 7501 aggregato della sessione | `screen/attention.rs:318`, `host_program_status` |
| Un permesso già concesso viene risposto da `PermissionCache` senza prompt | `zellij_exports.rs:1687` |
| `zellij-tile` 0.46 non è su crates.io (ultima 0.45.1) | API crates.io |
| Claude Code 2.1.318 emette `state`, `app=claude-code`, `id`, `kind`, `progress`, `title`, `msg` e manda `state=clear` all'uscita | `/opt/claude-code/bin/claude` (funzione `rto`) |
| Codex 0.167.0 emette OSC 7501 | binario `codex` |

### 1.1 Supporto OSC 7501 per agente

| Agente | Supporto | Fonte | Limite |
|---|---|---|---|
| Claude Code | sì | binario 2.1.318 | — |
| Codex | sì | binario 0.167.0 | — |
| pi | sì | test dell'utente | — |
| Opencode | `working`, `blocked` (un record per sessione, id = session ID) | PR anomalyco/opencode#55172, unita il 2026-10-13 | Non emette `done`/`error`: a fine turno il record sparisce e il rail mostra `idle`. Issue aperta anomalyco/opencode#60411 |
| omp | sì | issue can1357/oh-my-pi#17145 | Prima della fix #17147 (unita il 2026-10-27) l'OSC 133 di omp cancellava `working`. Serve una release di omp con quella fix |

Conseguenza ssh: il pane locale che esegue `ssh host` + `zellij attach` riceve
l'OSC 7501 aggregato dello Zellij remoto. Il pane locale ha quindi
`attention`/`program_status` come un agente locale. Non serve codice per il
remoto.

## 2. Architettura prima e dopo

```figure
PRIMA
 agente ─hook─► zj-radar notify ─► zellij pipe status.v1 ─┐
 shell  ─CommandChanged─► core::command::classify ─────────┤
                                                           ▼
 plugin: StatusStore + CommandStore + snapshot/presence/lock su /cache
         + notify claims + focus condiviso + exit grace
                                                           │
 altre sessioni ◄── presence.<pid>.json (mtime, reap) ─────┘

DOPO
 agente ─OSC 7501─► Zellij (attention) ─► PaneUpdate / TabUpdate
 shell  ─OSC 133──► Zellij (attention) ─► PaneAttentionChanged
 altre sessioni ──► Zellij (peers)     ─► GlobalAttentionUpdate
                                              │
                                              ▼
 plugin: vista pura dell'ultimo manifest + ledger in memoria
```

## 3. Mappa dei file

Legenda: **R** = rimuovi il file, **C** = cambia, **T** = tieni invariato.

### 3.1 Plugin (`crates/plugin/src/`)

| File | Azione | Motivo |
|---|---|---|
| `session_files.rs` (1563) | R | Snapshot, marker/lock permessi, presence, claim notifiche, focus condiviso: tutto file-based |
| `presence.rs` | R | Presence file |
| `radar_state/snapshot.rs` | R | Lo stato vive in Zellij |
| `status_store.rs` (632) | R | Niente più pipe status |
| `notify_rules.rs` (436) | R | Notifiche native di Zellij |
| `permission.rs` (383) | R | Sostituito da una richiesta diretta a `load` |
| `hooks_manifest_tests.rs` | R | Il plugin Claude sparisce |
| `sessions.rs` (1035) | C | Alimentato da `GlobalAttention`; via liveness/mtime/reap/dismiss |
| `radar_state.rs` (1451) | C | Stato = tab + pane dell'ultimo manifest; via stores, exit grace, snapshot |
| `rollup.rs` | C | Input = stato pane nativo; via distinzione job/service/remote |
| `ledger.rs` | C | Alimentato da `PaneAttentionChanged`; solo memoria |
| `runtime.rs` (1982) | C | Meno effect, meno input (sezione 4.4) |
| `render.rs` + `render/` | C | Sotto-righe da record figli; via branch e ✕ |
| `tab_namer.rs` | C | Candidato repo dal git remote (sezione 4.6) |
| `config.rs` | C | Via opzioni di notifiche, comandi, permessi deferiti |
| `control.rs` | C | Verbi `cmd.v1` invariati salvo verifica (sezione 4.7) |
| `lib.rs` | C | Adapter dei nuovi eventi e campi |
| `clock.rs`, `theme.rs`, `mode.rs` | T | Nessuna dipendenza dai file |
| `reference_tests.rs`, `test_fixtures.rs` | C | Fixture nuove |
| nuovo `attention.rs` | nuovo | Specchio host-testabile dei tipi Zellij (sezione 4.1) |

### 3.2 Core (`crates/core/`)

Rimuovi il crate intero: `payload.rs`, `pipe.rs`, `presence.rs`, `task.rs`,
`observation.rs`, `wire.rs`, `kind.rs`, `command.rs` (+ `command/tests.rs`).
Sposta in `crates/plugin/src/` solo `status.rs` (il vocabolario
`Status`/severità che render, rollup e theme già usano), ridotto al necessario.
Se il renderer usa `kind.rs` per l'icona dell'agente, sposta solo la mappa
`app → glyph` (sezione 4.3).

### 3.3 CLI (`crates/cli/`)

| File | Azione |
|---|---|
| `notify.rs`, `state.rs`, `agents.rs`, `agents/**`, `codex_daemon.rs`, `dedup.rs`, `git.rs`, `producers.rs` | R |
| `setup/claude.rs`, `codex.rs`, `opencode.rs`, `pi.rs`, `omp.rs`, `vendored.rs`, `*.js` | R |
| `tests/cli_notify.rs`, `cli_state.rs`, `*.test.mjs` | R |
| `setup/mod.rs`, `analyze.rs`, `check.rs`, `detect.rs`, `edit.rs` | C: solo target `zellij`; floor versione 0.46.0 |
| `setup/preseed.rs` | C: aggiungi `ReadPaneContents` al grant |
| `lib.rs`, `main.rs` | C: via sottocomandi `notify`, `state`, target agenti di `setup` |
| `run.rs`, `update.rs`, `layout.rs`, `fsutil.rs`, `setup/zellij.rs`, `setup/download.rs` | T (salvo i chiamanti rimossi) |
| `run_assets/radar-onboarding.kdl` | R |
| `tests/cli_setup.rs`, `cli_run.rs` | C |

### 3.4 Repository

| Percorso | Azione |
|---|---|
| `plugins/zj-radar-claude/**`, `.claude-plugin/marketplace.json` | R |
| `tools/wasm-fuel/**` | R (+ ricetta in `justfile`) |
| `justfile` | C: via `test-bash` per gli hook, `test-js`; `ci` aggiornato |
| `.github/workflows/ci.yml`, `e2e.yml`, `funnel.yml` | C: via job bats/node degli hook; Zellij 0.46 da sorgente per e2e |
| `flake.nix` | C: filtro sorgenti senza i file rimossi |
| `demo/agent.sh` | C: emette OSC 7501 con `printf` |
| `Cargo.toml` workspace | C: via membro `core` e pin `=X.Y.Z` |
| `RELEASING.md` | C: via publish di `zj-radar-core` |
| `scripts/tests/install.bats`, `scripts/install.sh` | C se citano gli adapter |

## 4. Implementazione

### 4.1 Dipendenza Zellij

```toml
# crates/plugin/Cargo.toml
[target.'cfg(target_arch = "wasm32")'.dependencies]
zellij-tile = { git = "https://github.com/zellij-org/zellij", rev = "43c8710b3a9845f60d5a567c7410fc1197fabb66" }
```

- Cambia il floor in `setup/analyze.rs` a `0.46.0` e il test guardia
  `doctor_version_gate_matches_the_zellij_tile_pin` per leggere il nuovo pin.
- Il plugin ha `publish = false`, quindi la dipendenza git non blocca il
  publish della CLI. La CLI incorpora il wasm già compilato.
- Quando Zellij pubblica 0.46 su crates.io, torna a `=0.46.x`.

Nuovo `crates/plugin/src/attention.rs`, senza `zellij-tile`, perché i moduli di
dominio restano host-testabili:

```rust
pub(crate) struct PaneAttention {
    pub status: Status,            // Running | Pending | Done | Error | Idle
    pub from_program: bool,        // source == ProgramStatus
    pub text: String,              // attention.text
    pub kind: Option<BlockedKind>, // permission | question | auth
    pub progress: Option<u8>,
    pub app: Option<String>,
    pub exit_code: Option<i32>,
    pub since_ms: u64,
    pub seen: bool,
}

pub(crate) struct Record {        // ProgramStatusRecord figlio (id != None)
    pub id: String,
    pub status: Status,
    pub progress: Option<u8>,
    pub label: String,             // title, altrimenti msg, altrimenti id
}
```

Mappa degli stati, una sola funzione in `lib.rs`:

| Zellij `AttentionStatus` | Plugin `Status` |
|---|---|
| `Working` | `Running` |
| `NeedsYou` | `Pending` |
| `Done` | `Done` |
| `Failed` | `Error` |
| `Idle`, `Quiet`, `NewOutput`, muted | `Idle` |

Eccezione (passo 0, Opencode): se `attention.source` non è `ProgramStatus` e
`program_status` non è vuoto, lo stato del pane è il record più grave
(`blocked` → `Pending`, `error` → `Error`, `working` → `Running`, `done` →
`Done`). Zellij alza il pane solo per figli `blocked`/`error`; un pane con
soli figli `working` resterebbe `Idle`. Test unitario per i due casi.

### 4.2 Stato del plugin

`RadarState` diventa una vista dell'ultimo manifest:

- `panes_changed(update)` sostituisce tutti i pane con quelli del manifest,
  ognuno con `PaneAttention`, `Vec<Record>`, cwd, title, focus.
- Rimuovi `StatusStore`, `CommandStore`, `ObservationStore`, `resolve` a due
  store, exit grace (`absent_once` resta solo se serve al naming), snapshot,
  `persists_edges_for`.
- Zellij già sceglie la fonte vincente per pane (OSC 7501 batte comando
  shell), quindi la regola "status wins over command" sparisce.
- Il render gate resta: `generation` cresce solo se il manifest cambia i
  campi che il rail mostra.

Le istanze nascoste non ricevono `PaneUpdate`, ma non disegnano (gate
`Visible`). Quando il tab diventa attivo, Zellij invia un manifest nuovo. Il
passo 0 controlla questo comportamento.

### 4.3 Rendering

- Riga pane: glyph dello stato, `text` di attention, `progress` come `40%` se
  presente, label di `kind` (`needs approval`, `has a question`, `needs login`)
  per `Pending`.
- Icona agente da `app`: `claude-code`, `codex`, `pi`; `zellij` = pane remoto
  (Zellij annidato o via ssh), icona remota; altrimenti nessuna.
- Sotto-righe `┊`: i `Record` figli, al posto dei background task. Non
  contano nel roll-up (regola attuale).
- Righe comando: attention con fonte comando (`ShellCommand`, `CommandPane`,
  `ForegroundProcess`); `exit_code` dà l'esito.
- Rimuovi: branch, task label, hotspot `✕`, righe "steady" per
  service/remote.
- Il lockstep resta strutturale (`RenderedRail::from_lines`), nessun cambio.
- Aggiorna `docs/rail-reference.md` dal test (`reference_tests.rs`) e accetta
  gli snapshot insta con `just review`, uno per uno.

### 4.4 Runtime: effect e input

Effect che restano: `RequestPermission`, `SetSelectable`, `SetTimeout`,
`RenameTab`, `SwitchTab`, `ShowPane`, `SwitchSession`, `CloseSelf` (se resta un
uso), più due nuovi:

- `MarkSeen { pane_id }` → `mark_session_pane_seen(session, PaneId::Terminal(id))`
  (gesto `✓`, sostituisce `BroadcastStatus`).
- `ResolveRepo { cwd }` → `run_command` git (sezione 4.6).
- `FocusPeerPane { session, pane_id }` → `focus_session_pane` (click su item
  di un'altra sessione).

Effect da rimuovere: `PersistSnapshot`, `PersistPermissionMarker`,
`HeartbeatPermissionLock`, `Notify`, `PublishFocus`, `PersistPresence`,
`ClaimPresenceWriter`, `ReadPresences`, `DismissPresence`, `BroadcastStatus`.
`ResolveCwd` resta: il passo 0 mostra che senza OSC 7 il manifest non ha la cwd.

Input che restano: `tabs_changed`, `panes_changed`, `timer`, mouse,
`control`, `config_pipe`, `session_name_changed`, `mode_changed`,
`visibility_changed`, `permission_result`. Nuovi: `global_attention(…)`,
`attention_changed(pane, before→after)` per il ledger, `repo_resolved(cwd, name)`.
Da rimuovere: `status_pipe`, `command_changed`, `presences_changed`,
`presence_writer_acquired`, `snapshot_json`, `presence_json`,
`wants_permission_probe`.

Eventi a cui iscriversi in `lib.rs`: `TabUpdate`, `PaneUpdate`, `ModeUpdate`,
`Timer`, `Mouse`, `Visible`, `PermissionRequestResult`, `SessionUpdate` (solo
per il nome sessione, se serve), `PaneAttentionChanged`,
`GlobalAttentionUpdate`, `RunCommandResult`, `CwdChanged` (resta, passo 0
punto 5). Via `CommandChanged`.

Permessi: `ReadApplicationState`, `ChangeApplicationState`, `ReadCliPipes`
(pipe `cmd.v1`/`config.v1`), `RunCommands` (git), `ReadPaneContents` (testo
dei comandi; senza, la riga comando mostra solo lo stato). Aggiorna
`setup/preseed.rs` e il text pin del grant in `lib.rs`.

Cadence: Fast solo per lo spinner di righe `Running` visibili; Slow per le età
del ledger; disarmato altrimenti. Via i trigger di heartbeat, permission lock
e presence.

### 4.5 Cross-session e ledger

`Sessions` riceve `GlobalAttention` intero a ogni `GlobalAttentionUpdate`:

- badge = `sessions` diverse dalla corrente con `status`/`counts`;
- albero del peer = `attention.items` (tab, pane, testo);
- click su un item → `FocusPeerPane`; click su un peer senza item →
  `SwitchSession`;
- Alt+[/] (`SessionNext`/`SessionPrev`) usa lo stesso `ordered()`.

Rimuovi la liveness per mtime (fresh/stale/dead), il reap, il dismiss `✕`, il
writer claim. Zellij gestisce l'elenco delle sessioni vive.

Ledger: `attention_changed` aggiunge un'entry quando un pane passa da
`Done`/`Error` a un altro stato, o quando il pane sparisce dal manifest con
`Done`/`Error`. Solo memoria, cap 32 invariato.

```rust
// ponytail: un'istanza nata dopo (tab nuovo) parte con ledger vuoto, upgrade se serve storico condiviso
```

### 4.6 Nome del tab

Ordine dei candidati in `tab_namer::name_candidates`:

1. Nome manuale: regola esistente (`Managed` non sovrascrive un rename
   manuale). Nessun cambio.
2. Nome progetto del git remote della cwd del pane con focus, poi di
   qualsiasi pane.
3. `worktree_repo_dir(cwd)` (basename, con risoluzione dei worktree
   `.claude/worktrees` e `.Codex/worktrees` esistente).
4. Title del pane.

Lettura del remote, una volta per cwd distinta e per istanza:

```rust
run_command(
    &["git", "-C", cwd, "remote", "get-url", "origin"],
    BTreeMap::from([("zj_radar".into(), "repo".into()), ("cwd".into(), cwd.into())]),
);
```

- `RunCommandResult` con exit 0 → nome = ultimo segmento dopo `/` o `:`,
  senza `.git` finale e senza `/` finale. Altrimenti `None`.
- Cache `HashMap<String, Option<String>>` in `RadarState`; nessun retry.
- Pane remoti (`ReportedCwd.is_local == false`): niente git, usa il basename.
- Test unitario per il parser dell'URL: `git@github.com:o/r.git`,
  `https://h/o/r`, `https://h/o/r/`, `ssh://h:22/o/r.git`, stringa vuota.
- Aggiorna la regola push-driven in `CLAUDE.md`/`AGENTS.md`: seconda
  eccezione, un `run_command` asincrono per cwd nuova.

### 4.7 Pipe `cmd.v1` e `config.v1`

Restano (keybinding e config runtime). Rimuovi solo i verbi che dipendono dai
presence file, se presenti su `main` (`grep -n` in `control.rs`). Aggiorna
`docs/configuration.md` nello stesso commit (welded).

### 4.8 CLI

- `zj-radar setup` accetta solo `zellij` (o nessun target = zellij).
- `setup --check`: via controlli producer; resta Zellij ≥ 0.46.0, layout,
  wasm, grant.
- Rimuovi la dipendenza `zj-radar-core` da `crates/cli/Cargo.toml`.

## 5. Ordine dei commit

Ogni passo lascia `just ci` verde.

| # | Commit | Controllo |
|---|---|---|
| 0 | Spike (non committato): plugin minimo che logga `program_status`, `attention`, `command_state.cwd` | Sezione 6 |
| 1 | `build(plugin): zellij-tile da git 0.46` + floor 0.46.0 | `cargo build --release --target wasm32-wasip1 -p zj-radar-plugin` |
| 2 | `feat(plugin): attention nativa` (nuovo `attention.rs`, `RadarState` dal manifest, render) | `just test`, insta review |
| 3 | `feat(plugin): badge cross-session da GlobalAttentionUpdate` | test `sessions.rs` |
| 4 | `feat(plugin): ledger da PaneAttentionChanged` | test `ledger.rs` |
| 5 | `feat(plugin): nome tab dal git remote` | test parser + `tab_namer` |
| 6 | `refactor(plugin)!: rimuovi pipe status, session files, notifiche, permission lock` | `cargo clippy -D warnings` (codice morto) |
| 7 | `refactor(cli)!: rimuovi notify, state e adapter` | `cargo test -p zj-radar` |
| 8 | `refactor!: rimuovi crates/core, plugin Claude, wasm-fuel` | `just ci` |
| 9 | `docs: modello nativo OSC 7501` | link e welded test |

Dopo il passo 8 controlla che non resti nulla:

```sh
rg -n 'status\.v1|zj_radar\.status|presence|session_files|notify_rules|PermissionMarker|zj_radar_core|StatusStore|CommandStore' --glob '!docs/plans/**'
```

## 6. Passo 0: controlli sul vivo

Prima di cambiare il plugin, controlla con uno spike che logga su stderr
(`eprintln!` finisce nel log di Zellij):

1. Un pane con `claude` mostra `program_status` root con `app=claude-code` e
   `attention.status` `Working` durante un turno, `NeedsYou` su un prompt di
   permesso.
2. I record figli (`id`) compaiono con subagent o task in background.
3. `PaneAttentionChanged` arriva anche alle istanze di tab nascosti.
4. Quando un tab diventa attivo, la sua istanza riceve subito un `PaneUpdate`.
5. `command_state.cwd` è popolata senza OSC 7 (fonte `Poll`). Se no, tieni
   `CwdChanged` + `ResolveCwd`.
6. Pane `ssh host` con Zellij remoto: `attention` locale segue l'agente remoto.
7. `GlobalAttentionUpdate` elenca le altre sessioni locali con i loro item.
8. Opencode e omp: ripeti il punto 1 con le versioni installate (non sono su
   questa macchina al momento del piano).

Se un punto fallisce, il piano cambia solo nella sezione collegata.

### 6.1 Esito (2026-11-08, Zellij 0.46.0 @ 43c8710b, Claude Code 2.1.318)

Spike in sessione Zellij isolata (socket, cache e config separati), pilotata
da tmux. Sequenze emesse a mano e un turno reale di Claude Code.

| # | Esito | Osservato |
|---|---|---|
| 1 | ok | `Working` con `app=claude-code`; prompt di permesso → `NeedsYou`, `kind=Permission`, testo `needs approval: approve Bash: …` |
| 2 | ok | subagent = record figlio `id=a4cc…`, `Working`, `title="Elenca file in docs/plans"`; i permessi del subagent arrivano sul record root |
| 3 | ok | `PaneAttentionChanged` arriva a tutte le istanze, anche di tab nascosti |
| 4 | ok | al cambio tab: `Visible(true)` e subito `PaneUpdate` |
| 5 | **no** | il server non imposta mai `CwdSource::Poll`; senza OSC 7 la cwd resta vuota o vecchia |
| 6 | ok, con limite | Zellij annidato inoltra stato, `kind` e testo, ma `app=zellij` e testo `Tab #1 › Pane #1: <msg>` |
| 7 | ok | le istanze della sessione B ricevono la sessione A con stato, conteggi e item |
| 8 | non fatto | Opencode e omp non installati; da sorgente: pi `app="pi"` (`config.js` `APP_NAME`), Codex `app=codex` |

Altri fatti emersi:

- Un record figlio alza lo stato del pane solo se `blocked` o `error`
  (`deciding_record` in `zellij-server/src/attention/program_status.rs`). Un
  figlio `working` senza root lascia il pane `Idle`: è il caso di Opencode.
- `Done`/`Failed` restano dopo l'uscita del processo finché il pane non è
  visto. Un pane con focus diventa "visto" circa 2 s dopo e torna `Idle`; il
  record resta in `program_status`.
- Claude Code emette `done` a fine turno e `state=clear` all'uscita
  (`program_status` vuoto).
- Claude Code emette OSC 7501 solo se il probe `OSC 7501 ; ?` riceve risposta
  e se `CLAUDE_CODE_DISABLE_TERMINAL_TITLE` non è impostata. Claude Code la
  imposta per i propri processi figli: un Claude lanciato dal tool Bash di
  Claude non emette nulla. L'e2e deve togliere `CLAUDE*`, `TMUX*` e
  `TERM_PROGRAM` dall'ambiente.
- Il separatore delle coppie è `:` (`state=working:app=x`), non `;`.

Conseguenze sul piano, già applicate nelle sezioni 4.1, 4.3 e 4.4 (punto 4 era già in 4.5):

1. Stato del pane: se `attention.source != ProgramStatus` e ci sono record,
   lo stato viene dal record più grave (`blocked` > `error` > `working` >
   `done`). Copre Opencode senza codice specifico.
2. Cwd: restano `CwdChanged` e `ResolveCwd`.
3. Icona: `app=zellij` = pane remoto, icona remota.
4. `Done` sparisce quando l'utente guarda il pane: il ledger "earlier" lo
   registra sulla transizione `Done → Idle`, come già previsto in 4.5.

## 7. Documentazione

- Riscrivi: `docs/design.md` (§2 diventa "Perché OSC 7501", via §5 pipe, §7
  adapter, §10 exit grace, §13 presence), `docs/activity-model.md`,
  `docs/producers.md` (diventa: "emetti OSC 7501", con l'esempio `printf`
  della spec), `docs/configuration.md`, `docs/install.md`, `docs/using.md`,
  `docs/troubleshooting.md`, `README.md`, `CONTEXT.md` (via Status contract,
  Background task, Information source pushed/observed, Cross-session presence),
  `CONTRIBUTING.md` (tabella welded), `AGENTS.md`/`CLAUDE.md`.
- Rimuovi le righe della tabella welded per `hooks.json`, `notify.sh`,
  pin core, `Notify::agent`.

## 8. Migrazione della macchina

Prima di installare la nuova CLI, rimuovi gli adapter con la CLI attuale:

```sh
for a in claude codex opencode pi omp; do zj-radar setup "$a" --uninstall; done
```

Se un hook resta, Claude/Codex/pi mostrano un errore a ogni evento perché
`zj-radar notify` non esiste più. Documenta lo stesso comando in
`docs/install.md` per gli altri utenti.

## 9. Rischi e limiti

- Zellij richiesto ≥ 0.46.0, oggi solo da git. Chi usa 0.44/0.45 perde il rail.
- CI: `e2e.yml` compila Zellij al `ZELLIJ_REV` (stesso rev del pin, test
  guardia in `lib.rs`), con cache. `scripts/funnel.sh` resta su v0.44.3 perché
  prova le release già pubblicate: passalo a v0.46.0 nella prima release del
  nuovo plugin, quando Zellij 0.46.0 esiste come binario.
- Con Zellij 0.46 falliscono 5 test e2e già su `main` (click tab, comandi
  nei tab nascosti, rename manuale, pittura colonne, Alt+[/]): da sistemare
  nei commit 2-6, che riscrivono quei percorsi.
- Opencode non emette `done`/`error` (anomalyco/opencode#60411): il rail e il
  ledger non mostrano la fine di un turno di Opencode. Nessun workaround nel
  plugin; il limite sparisce quando Opencode unisce la fix.
- omp mostra `working` solo con una versione che contiene can1357/oh-my-pi#17147.
- `ReadPaneContents` è un permesso più ampio: serve solo per il testo dei
  comandi. Senza, le righe comando mostrano solo lo stato.
- Il ledger non è condiviso tra istanze: un tab nuovo parte vuoto.
- Il nome repo per pane remoti è il basename della cwd remota, non il remote.
- Branch git e task label spariscono dal rail (decisione 2).

## 10. Criteri di accettazione

1. Un turno di Claude Code, Codex, pi e omp in pane locali mostra
   working → needs-you → done nel rail, senza hook installati. Opencode mostra
   working → needs-you → idle.
2. Lo stesso via `ssh` + Zellij remoto.
3. `✓` marca il pane come visto in Zellij (sparisce anche dal tab marker
   nativo).
4. Il badge mostra le altre sessioni; click e Alt+[/] cambiano sessione.
5. Nessun file scritto in `/cache` dal plugin.
6. Il nome tab segue l'ordine della sezione 4.6.
7. `just ci` verde; il comando `rg` della sezione 5 non trova nulla.
