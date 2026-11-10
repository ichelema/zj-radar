//! `zj-radar run` — turnkey: own a Zellij config dir and launch it.
//!
//! Side effects live in `run()`; every decision (session name, launch args,
//! which advisories to print) is pure and lives in `plan_run`, mirroring the
//! pure-editor / thin-IO split that `setup.rs` uses.

use super::fsutil::atomic_write;
use std::path::{Path, PathBuf};

/// Shown when the wasm is still ungranted at launch: `--print-cmd` (which
/// writes nothing) or a pre-seed that didn't land (see `preseed_grant`).
/// Zellij then shows its own prompt in place of the rail. Also shown when the
/// pre-seed just wrote the grant for an existing session: its rails may be
/// parked at a prompt the running server already shows, which the file can't
/// resolve.
const GRANT_HINT: &str = "First run: Zellij asks for the sidebar's permissions inside the rail — \
    focus the rail and press y.";

// ── Pure helpers ─────────────────────────────────────────────────────────────

/// Session name from the cwd basename or an explicit name argument
/// (`zj-radar run <name>`) — both sanitized. Zellij session names allow
/// `[A-Za-z0-9_-]`; other chars fold to `-`. The override is NOT taken
/// verbatim because the name round-trips through
/// `zellij list-sessions --short` output (`session_is_live` trims each line),
/// so a name with edge spaces could never match its own session again. If nothing alphanumeric survives
/// (empty or all-symbol input), falls back to `"radar"` rather than emitting a
/// degenerate all-dashes name.
pub(crate) fn session_name(cwd: &Path, name_override: Option<&str>) -> String {
    let base = name_override.unwrap_or_else(|| cwd.file_name().and_then(|s| s.to_str()).unwrap_or(""));
    let sanitized: String =
        base.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' }).collect();
    // Trim the dash a trailing folded char leaves ("My Proj!" → "My-Proj",
    // not "My-Proj-") — pure cosmetics for the session list.
    let sanitized = sanitized.trim_end_matches('-');
    if sanitized.chars().any(|c| c.is_ascii_alphanumeric()) {
        sanitized.to_string()
    } else {
        "radar".to_string()
    }
}

/// The shared `--config <dir>/config.kdl --config-dir <dir>` prefix of every
/// launch. The explicit `--config` is load-bearing, not redundant: Zellij's
/// precedence is `--config` flag > `ZELLIJ_CONFIG_FILE` env > `<config-dir>/
/// config.kdl` (zellij-utils `cli.rs` binds the flag and the env var to the
/// same field; `Config::try_from(&CliArgs)` returns it before ever consulting
/// the config dir). With `--config-dir` alone, a user who exports
/// `ZELLIJ_CONFIG_FILE` launches "owned" sessions on THEIR config — radar
/// alias and tab-jump keybinds vanish with no diagnostic. The flag outranks the
/// env var, and — unlike an env strip — survives into `--print-cmd` output,
/// which can't carry environment changes. `--config-dir` stays alongside it:
/// layouts still resolve from the owned dir's `layouts/`.
fn owned_config_args(config_dir: &Path) -> Vec<String> {
    vec![
        "--config".into(),
        config_dir.join("config.kdl").to_string_lossy().into_owned(),
        "--config-dir".into(),
        config_dir.to_string_lossy().into_owned(),
    ]
}

/// Args to CREATE a new session with the rail layout:
/// `zellij --config <dir>/config.kdl --config-dir <dir> --session <name>
/// --new-session-with-layout radar`.
///
/// `--new-session-with-layout` (NOT `--layout`) is required here: when combined
/// with `--session`, a plain `--layout` is interpreted by Zellij as "add a tab to
/// the EXISTING session <name>", which fails ("session not found") when it
/// doesn't exist yet. `--new-session-with-layout` always starts a new session —
/// even when invoked from inside Zellij — using the named layout resolved from
/// `--config-dir`'s `layouts/`.
pub(crate) fn create_session_args(config_dir: &Path, session: &str, layout: &str) -> Vec<String> {
    let mut args = owned_config_args(config_dir);
    args.extend(["--session".into(), session.into(), "--new-session-with-layout".into(), layout.into()]);
    args
}

/// Args to attach to (or resurrect) an existing session:
/// `zellij --config <dir>/config.kdl --config-dir <dir> attach <name>`.
///
/// The layout was applied at creation, but the owned config is still
/// load-bearing here: Zellij config (keybinds included) comes from the
/// ATTACHING client, and a resurrected session is a brand-new server started
/// by that client. Without it, attach reads the user's default
/// `~/.config/zellij`, where the `radar` alias and the Alt-1..9 binds don't
/// exist. Passing the owned dir makes the baked-in binds real for every
/// client, not just the creating one.
pub(crate) fn attach_session_args(config_dir: &Path, session: &str) -> Vec<String> {
    let mut args = owned_config_args(config_dir);
    args.extend(["attach".into(), session.into()]);
    args
}

/// Every permission the plugin requests on load — keep in lockstep with the
/// `Effect::RequestPermission` list in `crates/plugin/src/lib.rs`.
pub(crate) const REQUIRED_PLUGIN_PERMISSIONS: [&str; 5] =
    ["ReadApplicationState", "ReadCliPipes", "ChangeApplicationState", "RunCommands", "ReadPaneContents"];

/// True iff `permissions.kdl`'s EFFECTIVE grant for `wasm_abs_path` covers the
/// full permission set the plugin requests. Zellij keys grants by the literal
/// path string, so an exact match (closing quote included) is correct; the `{`
/// guard skips a bare quoted string that isn't a block header.
///
/// Duplicate blocks follow Zellij's own semantics: the file loads into a map
/// keyed by path, so the LAST block for a path is the one Zellij consults —
/// a stale partial block before a later full one is granted (the preseed
/// appends exactly that shape), and a trailing partial block overrides an
/// earlier full one. An inline-closed block (`"path" { A; B }`) carries its
/// grant set on the header line; reading it any other way would swallow the
/// NEXT entry's lines as this block's permissions.
///
/// The full-set requirement is load-bearing: when an upgraded plugin requests a
/// permission its old grant lacks, Zellij re-prompts — and that prompt is
/// illegible inside the 32-col rail (zellij#4749), so a partial grant presents
/// as a silently blank rail. Treating it as ungranted makes `run` and
/// `setup zellij` pre-seed the full set again.
pub(crate) fn wasm_is_granted(permissions_kdl: &str, wasm_abs_path: &str) -> bool {
    // The key as it appears in the file: a KDL string, escaped the same way
    // the preseed (and Zellij's own KDL writer) spells it.
    let needle = format!("\"{}\"", crate::setup::kdl_string(wasm_abs_path));
    let mut lines = permissions_kdl.lines().map(str::trim_start);
    let mut last: Option<Vec<String>> = None;
    while let Some(header) = lines.by_ref().find(|l| l.starts_with(&needle) && l.contains('{')) {
        let body = &header[header.find('{').map_or(0, |i| i + 1)..];
        last = Some(match body.find('}') {
            Some(end) => body[..end].split([';', ' ']).filter(|t| !t.is_empty()).map(str::to_string).collect(),
            None => lines.by_ref().take_while(|l| !l.starts_with('}')).map(|l| l.trim_end().to_string()).collect(),
        });
    }
    last.is_some_and(|granted| REQUIRED_PLUGIN_PERMISSIONS.iter().all(|required| granted.iter().any(|t| t == required)))
}

/// Join argv into a copy-pasteable shell command. Every printed command hint
/// (`--print-cmd`, "try running: zellij …") must go through this: on macOS the
/// owned config dir lives under `~/Library/Application Support/…`, so a bare
/// `join(" ")` produces a command that breaks at the space for every user who
/// pastes it. Conservatively single-quotes anything beyond the unambiguous
/// safe set (with embedded `'` as `'\''`, the POSIX idiom).
pub(crate) fn shell_join(args: &[String]) -> String {
    let quoted: Vec<String> = args.iter().map(|a| shell_quote(a)).collect();
    quoted.join(" ")
}

fn shell_quote(arg: &str) -> String {
    let safe =
        |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/' | b':' | b'=' | b',' | b'@' | b'+');
    if !arg.is_empty() && arg.bytes().all(safe) {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

// ── Path locators ────────────────────────────────────────────────────────────

/// The zj-radar–owned Zellij config directory rooted under `data_dir`.
pub(crate) fn owned_config_dir_in(data_dir: &Path) -> PathBuf {
    data_dir.join("zj-radar").join("zellij")
}

/// Zellij's cache root under `cache_dir` — `permissions.kdl` and every
/// plugin's `/cache` mount live below it. The folder differs between macOS
/// (`org.Zellij-Contributors.Zellij`) and Linux (`zellij`).
pub(crate) fn zellij_cache_root_in(cache_dir: &Path, is_macos: bool) -> PathBuf {
    cache_dir.join(if is_macos { "org.Zellij-Contributors.Zellij" } else { "zellij" })
}

/// Zellij's `permissions.kdl` rooted under `cache_dir`.
pub(crate) fn permissions_path_in(cache_dir: &Path, is_macos: bool) -> PathBuf {
    zellij_cache_root_in(cache_dir, is_macos).join("permissions.kdl")
}

/// Platform-resolved owned config dir, or `None` if the data dir is unknown.
/// `ZJ_RADAR_DATA_DIR` overrides the platform data dir — the isolation knob
/// `just dev` sets so a locally-built CLI materializes into its own sandbox
/// and can never touch the installed zj-radar's assets.
pub(crate) fn owned_config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("ZJ_RADAR_DATA_DIR").filter(|d| !d.is_empty()) {
        return Some(owned_config_dir_in(Path::new(&dir)));
    }
    dirs::data_dir().map(|d| owned_config_dir_in(&d))
}

/// Platform-resolved path to Zellij's `permissions.kdl`, or `None` if the cache
/// dir is unknown.
pub(crate) fn zellij_permissions_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|c| permissions_path_in(&c, cfg!(target_os = "macos")))
}

/// `permissions.kdl` contents when the cache dir resolves and the file reads —
/// the shared read behind every grant probe (`run`, the installer, the doctor).
pub(crate) fn zellij_permissions_text() -> Option<String> {
    zellij_permissions_path().and_then(|p| std::fs::read_to_string(p).ok())
}

/// The per-session ownership marker `run` stamps at create time:
/// `<owned config dir>/sessions/<name>`. Attach consults it so `run` never
/// silently attaches a session it didn't create — Zellij takes config from the
/// attaching client (see `attach_session_args`), so attaching a user's own
/// session via `run` would swap their keybinds/theme for the bundled config.
/// Session names are pre-sanitized to `[A-Za-z0-9_-]` (`session_name`), so the
/// name is path-safe by construction. `materialize` never touches this
/// directory, so markers survive version re-materialization.
fn session_marker_path(config_dir: &Path, session: &str) -> PathBuf {
    config_dir.join("sessions").join(session)
}

/// Stamp the ownership marker. Best-effort: a failed stamp costs one consent
/// prompt on a later `run`, never the launch itself.
fn stamp_session_marker(config_dir: &Path, session: &str) {
    let path = session_marker_path(config_dir, session);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, "created by `zj-radar run`\n");
}

// ── Materializer ─────────────────────────────────────────────────────────────

pub(crate) struct Assets {
    pub config_template: &'static str,
    pub layout: &'static str,
    /// `Some` when the wasm is baked into the binary (every prebuilt install);
    /// `None` for a from-crates.io `cargo install`, where `run` downloads it.
    pub wasm: Option<&'static [u8]>,
}

pub(crate) struct Materialized {
    pub config_dir: PathBuf,
    pub wasm_path: PathBuf,
}

/// The `.zj-radar-version` marker contents: crate version plus a hash of the
/// embedded text assets. Folding the content in makes staleness structural —
/// an edit to config.kdl/radar.kdl shipped WITHOUT a
/// version bump still re-materializes every existing install, instead of
/// relying on release discipline to remember one. The wasm is deliberately
/// excluded: it's `None` on from-crates.io installs, so hashing it would make
/// the marker disagree between an embedded and a downloaded install of the
/// same version. `DefaultHasher::new()` uses fixed keys, so the hash is
/// deterministic across processes; it is not promised stable across Rust
/// releases, but a toolchain bump costs one harmless re-materialization.
pub(crate) fn asset_marker(version: &str, assets: &Assets) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    assets.config_template.hash(&mut h);
    assets.layout.hash(&mut h);
    format!("{version}-{:x}", h.finish())
}

/// Write the owned config dir idempotently. A no-op when the version+content
/// marker matches (see [`asset_marker`]) AND all generated files are present
/// (so a deleted file forces a rewrite). Each file is written atomically; the
/// marker is written last, so an interrupted run is re-materialized rather
/// than served half-written.
pub(crate) fn materialize(dir: &Path, version: &str, assets: &Assets) -> std::io::Result<Materialized> {
    let wasm_path = dir.join("plugins").join(crate::WASM_FILE_NAME);
    let config_path = dir.join("config.kdl");
    let layout_path = dir.join("layouts").join("radar.kdl");
    let marker = dir.join(".zj-radar-version");
    let marker_value = asset_marker(version, assets);

    let up_to_date = std::fs::read_to_string(&marker).is_ok_and(|v| v == marker_value)
        && wasm_path.exists()
        && config_path.exists()
        && layout_path.exists();
    if up_to_date {
        return Ok(Materialized { config_dir: dir.to_path_buf(), wasm_path });
    }

    // The token sits inside a KDL string (`location="file:@WASM@"`), so the
    // path is escaped — a `"` in a data-dir path must not end the string.
    let config = assets.config_template.replace("@WASM@", &crate::setup::kdl_string(&wasm_path.to_string_lossy()));
    // Write the embedded wasm if we have it; otherwise leave wasm_path for the
    // caller to populate (download). The `up_to_date` check above already gates
    // on wasm_path.exists(), so a not-yet-downloaded wasm never short-circuits.
    if let Some(bytes) = assets.wasm {
        atomic_write(&wasm_path, bytes)?;
    }
    atomic_write(&config_path, config.as_bytes())?;
    atomic_write(&layout_path, assets.layout.as_bytes())?;
    atomic_write(&marker, marker_value.as_bytes())?;
    Ok(Materialized { config_dir: dir.to_path_buf(), wasm_path })
}

// ── Embedded assets ──────────────────────────────────────────────────────────

const CONFIG_TEMPLATE: &str = include_str!("run_assets/config.kdl");
const LAYOUT: &str = include_str!("run_assets/radar.kdl");

// build.rs sets `embedded_wasm` (+ ZJ_RADAR_WASM_PATH) when it has a wasm to
// bake in — true for every prebuilt binary (curl|sh, binstall, nix) and any
// in-workspace build. A from-crates.io `cargo install` has no wasm to embed, so
// WASM is None and `run` downloads the matching release on first use.
#[cfg(embedded_wasm)]
const WASM: Option<&[u8]> = Some(include_bytes!(env!("ZJ_RADAR_WASM_PATH")));
#[cfg(not(embedded_wasm))]
const WASM: Option<&[u8]> = None;

fn embedded_assets() -> Assets {
    Assets { config_template: CONFIG_TEMPLATE, layout: LAYOUT, wasm: WASM }
}

// ── Orchestration: pure plan + thin IO ───────────────────────────────────────

/// Inputs gathered from the environment, separated from the decision so that
/// `plan_run` is pure and unit-testable.
struct RunFacts {
    /// Resolved session name (cwd basename or override).
    session: String,
    config_dir: PathBuf,
    wasm_path: PathBuf,
    /// Whether a Zellij session of this name already exists (attach vs create).
    session_exists: bool,
    /// Whether the existing session carries `run`'s ownership marker (stamped at
    /// create time). Unowned + existing = a session the user made themselves;
    /// attaching would swap their client config, so it needs explicit consent.
    session_owned: bool,
    /// Whether we're invoked from inside a Zellij session (`run` can't nest).
    inside_zellij: bool,
    permissions_kdl: Option<String>,
    /// Whether this run's `preseed_grant` just wrote the grant (false before it).
    grant_just_written: bool,
}

/// What to launch and what to advise — the pure result of `plan_run`.
struct RunPlan {
    args: Vec<String>,
    advisories: Vec<String>,
    /// When set, the caller must NOT launch (we're nested) — show guidance.
    nested: bool,
    /// When set, the target session exists but wasn't created by `run`:
    /// the caller must get explicit consent before attaching (the attach
    /// swaps that client onto the bundled config).
    foreign_session: bool,
}

/// Pure decision: attach if the session exists, otherwise create with the rail
/// layout; advise the grant prompt when the wasm is still ungranted, or when
/// the grant just landed under an existing session (see `GRANT_HINT`); surface
/// whether we're nested.
fn plan_run(facts: &RunFacts) -> RunPlan {
    let granted =
        facts.permissions_kdl.as_deref().is_some_and(|kdl| wasm_is_granted(kdl, &facts.wasm_path.to_string_lossy()));
    let args = if facts.session_exists {
        attach_session_args(&facts.config_dir, &facts.session)
    } else {
        create_session_args(&facts.config_dir, &facts.session, "radar")
    };
    let prompt_may_be_pending = !granted || (facts.session_exists && facts.grant_just_written);
    let advisories = if prompt_may_be_pending { vec![GRANT_HINT.to_string()] } else { Vec::new() };
    RunPlan {
        args,
        advisories,
        nested: facts.inside_zellij,
        foreign_session: facts.session_exists && !facts.session_owned,
    }
}

pub struct RunOptions {
    pub name: Option<String>,
    pub print_cmd: bool,
}

/// Every spawned `zellij` child starts here: `ZELLIJ_CONFIG_FILE` is stripped
/// because it outranks `--config-dir` (Zellij precedence: `--config` flag >
/// env > config dir — see `owned_config_args`). Launch args already carry an
/// explicit `--config`, which outranks the env var on its own; the strip is
/// belt-and-braces for the launches and keeps the `list-sessions` probe from
/// choking on a user-exported config file we never asked for.
fn zellij_command() -> std::process::Command {
    let mut cmd = std::process::Command::new("zellij");
    cmd.env_remove("ZELLIJ_CONFIG_FILE");
    cmd
}

/// True iff a Zellij session named `name` exists (alive or resurrectable), per
/// `zellij list-sessions --short` (plain names, one per line). Any error
/// (zellij missing, no server) is treated as "does not exist" → create path.
fn session_is_live(name: &str) -> bool {
    zellij_command()
        .args(["list-sessions", "--short"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| String::from_utf8_lossy(&o.stdout).lines().any(|l| l.trim() == name))
}

/// Write the sidebar's full grant into Zellij's `permissions.kdl` before the
/// launch, so every rail's `request_permission` resolves from Zellij's cache
/// and the native prompt (barely legible at rail width, zellij#4749) never
/// shows. `run` grants only the wasm path it owns. Merge-safe with a `.bak`
/// (`merge_grant` + `backup_then_write`, as `setup zellij`), and best-effort:
/// any refusal leaves the prompt in the rail, which `GRANT_HINT` explains.
/// Returns true iff it wrote the grant (already granted or refused → false).
fn preseed_grant(perms_path: &Path, wasm_path: &Path) -> bool {
    use crate::setup::preseed::{merge_grant, Preseed};
    // Only a missing file starts from scratch: an unreadable one is left
    // alone, never replaced by a fresh file that would drop its grants.
    let existing = match std::fs::read_to_string(perms_path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            eprintln!("zj-radar: not pre-authorizing the sidebar ({}: {e})", perms_path.display());
            return false;
        }
    };
    let merged = match merge_grant(existing.as_deref(), &wasm_path.to_string_lossy()) {
        Ok(Preseed::AlreadyGranted) => return false,
        Ok(Preseed::Merged(text)) => text,
        Err(e) => {
            eprintln!("zj-radar: not pre-authorizing the sidebar ({e})");
            return false;
        }
    };
    let written = perms_path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| crate::setup::backup_then_write(perms_path, &merged));
    match written {
        Ok(()) => {
            eprintln!(
                "zj-radar: pre-authorized the sidebar's permissions ({}) in {}",
                REQUIRED_PLUGIN_PERMISSIONS.join(", "),
                perms_path.display()
            );
            true
        }
        Err(e) => {
            eprintln!("zj-radar: not pre-authorizing the sidebar (write failed: {e})");
            false
        }
    }
}

pub fn run(opts: RunOptions) {
    let Some(dir) = owned_config_dir() else {
        crate::exit::fail_report("zj-radar", "could not resolve a data directory");
        return;
    };
    let materialized = match materialize(&dir, env!("CARGO_PKG_VERSION"), &embedded_assets()) {
        Ok(m) => m,
        Err(e) => {
            crate::exit::fail_report("zj-radar", format!("failed to set up config dir {}: {e}", dir.display()));
            return;
        }
    };

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let session = session_name(&cwd, opts.name.as_deref());
    let session_exists = session_is_live(&session);
    let inside_zellij = std::env::var_os("ZELLIJ").is_some();

    // Local-wasm override (`just dev`): copy the freshly-built artifact over
    // the materialized path on every run, so the session always loads the
    // build under test — never the embedded or downloaded release wasm.
    // Atomic (read + temp-file rename via `atomic_write`), not `fs::copy` onto
    // the destination: a live session may load the wasm mid-copy otherwise.
    if let Some(wasm) = std::env::var_os("ZJ_RADAR_WASM").filter(|w| !w.is_empty()) {
        let copied = std::fs::read(&wasm).and_then(|bytes| atomic_write(&materialized.wasm_path, &bytes));
        if let Err(e) = copied {
            crate::exit::fail_report(
                "zj-radar",
                format!("copying ZJ_RADAR_WASM ({}) failed — {e}", Path::new(&wasm).display()),
            );
            return;
        }
    }

    // No embedded wasm (a from-crates.io `cargo install`) and none cached yet —
    // fetch the matching release once. Prebuilt binaries embed the wasm, so this
    // path is inert for them.
    if !materialized.wasm_path.exists() {
        let version = super::setup::wasm_download_version();
        if let Err(e) = super::setup::download_wasm_to(&version, &materialized.wasm_path) {
            crate::exit::fail_report(
                "zj-radar",
                format!(
                    "no embedded wasm and the download failed — {e}\n\
                     Fetch it once while online with: zj-radar setup zellij --download"
                ),
            );
            return;
        }
    }

    let mut facts = RunFacts {
        session_owned: session_marker_path(&materialized.config_dir, &session).exists(),
        session,
        config_dir: materialized.config_dir,
        wasm_path: materialized.wasm_path,
        session_exists,
        inside_zellij,
        permissions_kdl: zellij_permissions_text(),
        grant_just_written: false,
    };
    let plan = plan_run(&facts);

    // The nested refusal comes before the advisories: they advise the launch,
    // and nested means there is no launch — "press y at the prompt" followed
    // by "detach first" reads as advice for a run that never happens.
    // (`--print-cmd` still prints below: the user can run the printed command
    // after detaching, so its advisories still apply.)
    if plan.nested && !opts.print_cmd {
        crate::exit::fail_report(
            "zj-radar",
            "you're already inside Zellij. `run` starts a NEW session — detach first \
             (Ctrl-o d by default) and re-run, or use `zj-radar setup` to add the rail to your \
             current Zellij config.",
        );
        return;
    }
    // Advisories are guidance, which belongs on stderr (see the `exit` module
    // doc) — and `--print-cmd`'s stdout must stay machine-readable: a shell
    // doing `$(zj-radar run --print-cmd)` must capture the command, not prose.
    if opts.print_cmd {
        for advisory in &plan.advisories {
            eprintln!("{advisory}");
        }
        if plan.foreign_session {
            eprintln!(
                "note: session '{}' wasn't created by zj-radar — attaching with this command \
                 switches that client to zj-radar's bundled config (keybinds, theme)",
                facts.session
            );
        }
        println!("zellij {}", shell_join(&plan.args));
        return;
    }
    // A name-matching session the user created themselves: attaching would swap
    // that client's keybinds/theme for the bundled config, so it takes explicit
    // consent. Consent is remembered (the marker is stamped) — including for
    // radar sessions created by versions that predate the marker.
    if plan.foreign_session {
        use std::io::IsTerminal;
        // One boundary probe, reused by `confirm` below (its parameter, per
        // the `inject_mode` pattern).
        let is_tty = std::io::stdin().is_terminal();
        let escape = format!(
            "attach with your own config via `zellij attach {}`, or pick a different \
             name with `zj-radar run <name>`",
            facts.session
        );
        if !is_tty {
            crate::exit::fail_report(
                "zj-radar",
                format!(
                    "session '{}' exists but wasn't created by zj-radar; refusing to attach \
                     non-interactively (it would switch that client to zj-radar's bundled \
                     config) — {escape}",
                    facts.session
                ),
            );
            return;
        }
        let question = format!(
            "Session '{}' exists but wasn't created by zj-radar. Attach anyway with \
             zj-radar's bundled config (keybinds, theme)?",
            facts.session
        );
        if !crate::setup::confirm(&question, false, is_tty) {
            eprintln!("zj-radar: not attaching — {escape}");
            return;
        }
        stamp_session_marker(&facts.config_dir, &facts.session);
    }
    // Only a run that will launch pre-seeds: a refused one (nested, declined,
    // non-interactive foreign session) and `--print-cmd` have returned above,
    // so they never touch `permissions.kdl`. The grant is then read back so
    // the advisories reflect whether the pre-seed landed.
    if let Some(perms_path) = zellij_permissions_path() {
        facts.grant_just_written = preseed_grant(&perms_path, &facts.wasm_path);
    }
    facts.permissions_kdl = zellij_permissions_text();
    let plan = plan_run(&facts);
    for advisory in &plan.advisories {
        eprintln!("{advisory}");
    }
    // Stamp the create path before launching: the marker records "this name is
    // radar's" from the moment we ask Zellij to create it.
    if !facts.session_exists {
        stamp_session_marker(&facts.config_dir, &facts.session);
    }
    // A failed launch on the create path must roll the marker stamped above
    // back: it claims "this name is radar's" for a session Zellij never
    // created. Left behind, a session the user later creates under the same
    // name would read as owned and be silently attached onto the bundled
    // config — the exact swap the foreign-session consent flow prevents.
    let unstamp_on_failure = || {
        if !facts.session_exists {
            let _ = std::fs::remove_file(session_marker_path(&facts.config_dir, &facts.session));
        }
    };
    match zellij_command().args(&plan.args).status() {
        // Attach/detach exits 0; a non-zero status is a real failure (old
        // zellij without --new-session-with-layout, bad config, server crash)
        // that `run && next` must be able to gate on.
        Ok(status) if !status.success() => {
            unstamp_on_failure();
            crate::exit::fail_report(
                "zj-radar",
                format!(
                    "zellij exited with {status}; \
                     try running: zellij {}",
                    shell_join(&plan.args)
                ),
            );
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            unstamp_on_failure();
            crate::exit::fail_report("zj-radar", crate::setup::zellij_missing_message());
        }
        Err(e) => {
            unstamp_on_failure();
            crate::exit::fail_report("zj-radar", format!("failed to launch zellij: {e}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn session_name_sanitizes_falls_back_and_overrides() {
        assert_eq!(session_name(Path::new("/Users/m/dev/zj-radar"), None), "zj-radar");
        assert_eq!(session_name(Path::new("/Users/m/dev/My Proj!"), None), "My-Proj");
        assert_eq!(session_name(Path::new("/"), None), "radar");
        // All-symbol basename: nothing alphanumeric survives -> fall back, not "---".
        assert_eq!(session_name(Path::new("/Users/m/%%%"), None), "radar");
        assert_eq!(session_name(Path::new("/Users/m/dev/foo"), Some("bar")), "bar");
    }

    #[test]
    fn session_name_sanitizes_explicit_override_too() {
        // An explicit name argument round-trips through `zellij list-sessions`,
        // which splits on whitespace — a verbatim "my proj" would never match
        // its own session again (always the create path, which then collides).
        assert_eq!(session_name(Path::new("/x"), Some("my proj")), "my-proj");
        assert_eq!(session_name(Path::new("/x"), Some("a/b:c")), "a-b-c");
        // All-symbol override takes the same fallback as an all-symbol basename.
        assert_eq!(session_name(Path::new("/x"), Some("%%%")), "radar");
    }

    #[test]
    fn create_and_attach_args_are_exact() {
        // The explicit `--config` is load-bearing on BOTH: Zellij's precedence
        // is `--config` flag > ZELLIJ_CONFIG_FILE env > `<config-dir>/config.kdl`,
        // so without it a user-exported ZELLIJ_CONFIG_FILE silently swaps the
        // owned config out (blank rail, dead keybinds) — including in
        // `--print-cmd` output, which can't carry env changes.
        assert_eq!(
            create_session_args(Path::new("/cfg"), "foo", "radar"),
            vec![
                "--config", "/cfg/config.kdl", "--config-dir", "/cfg", "--session", "foo", "--new-session-with-layout",
                "radar"
            ]
        );
        // The owned config on attach is load-bearing: a resurrected session is
        // a NEW server whose config (radar alias, Alt-1..9 binds) comes from
        // the attaching client, not from the session's creation.
        assert_eq!(
            attach_session_args(Path::new("/cfg"), "foo"),
            vec!["--config", "/cfg/config.kdl", "--config-dir", "/cfg", "attach", "foo"]
        );
    }

    const SAMPLE: &str = r#"
"/nix/store/abc-room.wasm" {
    ReadApplicationState
}
"/Users/m/Library/Application Support/zj-radar/zellij/plugins/zj_radar.wasm" {
    ReadApplicationState
    ReadCliPipes
    ChangeApplicationState
    RunCommands
    ReadPaneContents
}
"#;

    // A grant from an older plugin version: entry exists, RunCommands missing.
    const STALE_SAMPLE: &str = r#"
"/Users/m/Library/Application Support/zj-radar/zellij/plugins/zj_radar.wasm" {
    ReadApplicationState
    ReadCliPipes
    ChangeApplicationState
}
"#;

    #[test]
    fn shell_join_quotes_what_a_shell_would_split_or_expand() {
        let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // Plain flags and paths pass through bare.
        assert_eq!(shell_join(&s(&["attach", "--config-dir", "/cfg", "foo"])), "attach --config-dir /cfg foo");
        // The macOS owned-config path — the case this helper exists for.
        assert_eq!(
            shell_join(&s(&["--config-dir", "/Users/m/Library/Application Support/zj-radar/zellij"])),
            "--config-dir '/Users/m/Library/Application Support/zj-radar/zellij'"
        );
        // Embedded single quote uses the POSIX '\'' idiom; empty arg stays visible.
        assert_eq!(shell_join(&s(&["it's", ""])), r"'it'\''s' ''");
        // Shell metacharacters (globs, vars, semicolons) get quoted, not trusted.
        assert_eq!(shell_join(&s(&["a b", "$HOME", "*;rm"])), "'a b' '$HOME' '*;rm'");
    }

    #[test]
    fn grant_detection_matches_block_headers_only() {
        let p = "/Users/m/Library/Application Support/zj-radar/zellij/plugins/zj_radar.wasm";
        assert!(wasm_is_granted(SAMPLE, p));
        assert!(!wasm_is_granted(SAMPLE, "/some/other/zj_radar.wasm"));
        assert!(!wasm_is_granted("", p));
        // A quoted path with no opening brace is not a grant block.
        assert!(!wasm_is_granted("\"/x/zj_radar.wasm\"\n", "/x/zj_radar.wasm"));
        // The closing quote in the needle prevents matching a longer path it prefixes.
        assert!(!wasm_is_granted("\"/x/zj_radar.wasm.bak\" {\n}\n", "/x/zj_radar.wasm"));
    }

    #[test]
    fn grant_detection_compares_against_the_kdl_escaped_key() {
        // The file holds KDL strings: a path with `"` or `\` is keyed in its
        // escaped spelling, so the raw path must never be what's matched.
        let p = r#"/x/we"ird\zj_radar.wasm"#;
        let perms = "ReadApplicationState; ReadCliPipes; ChangeApplicationState; RunCommands; ReadPaneContents";
        let escaped = format!(r#""/x/we\"ird\\zj_radar.wasm" {{ {perms} }}"#);
        assert!(escaped.parse::<kdl::KdlDocument>().is_ok(), "fixture must be valid KDL: {escaped}");
        assert!(wasm_is_granted(&escaped, p));
        assert!(!wasm_is_granted(&format!("\"{p}\" {{ {perms} }}"), p), "the raw spelling is not the key");
    }

    #[test]
    fn grant_detection_requires_the_full_permission_set() {
        let p = "/Users/m/Library/Application Support/zj-radar/zellij/plugins/zj_radar.wasm";
        // An entry from an older plugin version (RunCommands missing) is NOT a
        // grant: zellij will re-prompt for the new permission, illegibly, in
        // the rail — the exact blank-rail failure this check exists to catch.
        assert!(!wasm_is_granted(STALE_SAMPLE, p));
        // An empty grant block is not a grant either.
        assert!(!wasm_is_granted(&format!("\"{p}\" {{\n}}\n"), p));
    }

    #[test]
    fn grant_detection_follows_zellijs_later_block_wins() {
        // Zellij loads permissions.kdl into a map keyed by path: with
        // duplicate blocks the LATER one is the effective grant. The preseed
        // appends full blocks on that assumption, so this probe must agree —
        // an any-block match would report AlreadyGranted while Zellij
        // re-prompts on the trailing partial entry.
        let p = "/x/zj_radar.wasm";
        let full = format!(
            "\"{p}\" {{\n    ReadApplicationState\n    ReadCliPipes\n    ChangeApplicationState\n    RunCommands\n    ReadPaneContents\n}}\n"
        );
        let partial = format!("\"{p}\" {{\n    ReadApplicationState\n}}\n");
        assert!(wasm_is_granted(&format!("{partial}{full}"), p), "stale partial + later full = granted");
        assert!(!wasm_is_granted(&format!("{full}{partial}"), p), "later partial overrides the full block");
    }

    #[test]
    fn grant_detection_reads_inline_closed_blocks() {
        // A hand-tightened one-line entry is valid KDL; its body sits on the
        // header line. Misreading it used to swallow the NEXT entry's lines
        // as this block's grant set.
        let p = "/x/zj_radar.wasm";
        let inline_full = format!(
            "\"{p}\" {{ ReadApplicationState; ReadCliPipes; ChangeApplicationState; RunCommands; ReadPaneContents }}\n"
        );
        assert!(wasm_is_granted(&inline_full, p), "inline full block is a grant");
        let inline_partial_then_foreign = format!(
            "\"{p}\" {{ ReadApplicationState }}\n\
             \"/other.wasm\" {{\n    ReadCliPipes\n    ChangeApplicationState\n    RunCommands\n    ReadApplicationState\n}}\n"
        );
        assert!(
            !wasm_is_granted(&inline_partial_then_foreign, p),
            "a foreign block's permissions must not bleed into an inline-closed block's grant set"
        );
    }

    #[test]
    fn locators_compose_expected_paths() {
        assert_eq!(owned_config_dir_in(Path::new("/data")), Path::new("/data/zj-radar/zellij"));
        assert_eq!(
            permissions_path_in(Path::new("/cache"), true),
            Path::new("/cache/org.Zellij-Contributors.Zellij/permissions.kdl")
        );
        assert_eq!(permissions_path_in(Path::new("/cache"), false), Path::new("/cache/zellij/permissions.kdl"));
    }

    fn test_assets() -> Assets {
        Assets {
            config_template: "plugins { radar location=\"file:@WASM@\" {} }\n",
            layout: "layout { default_tab_template { children } tab { pane } }\n",
            wasm: Some(b"\0asm-dummy"),
        }
    }

    #[test]
    fn materialize_without_embedded_wasm_writes_config_but_not_wasm() {
        let d = tempdir().unwrap();
        let dir = d.path().join("c");
        let assets = Assets {
            config_template: "plugins { radar location=\"file:@WASM@\" {} }\n",
            layout: "layout {}\n",
            wasm: None,
        };
        let m = materialize(&dir, "0.1.0", &assets).unwrap();
        // No embedded wasm → materialize must not fabricate the file; `run`
        // downloads it. Config/layout/marker are still written.
        assert!(!m.wasm_path.exists(), "must not create a wasm file when none is embedded");
        assert!(dir.join("config.kdl").exists(), "config still written");
        assert!(dir.join("layouts/radar.kdl").exists(), "layout still written");
        assert!(dir.join(".zj-radar-version").exists(), "marker still written");
    }

    #[test]
    fn materialize_writes_all_files_and_substitutes_wasm_path() {
        let d = tempdir().unwrap();
        let dir = d.path().join("zj-radar/zellij");
        let m = materialize(&dir, "0.1.0", &test_assets()).unwrap();
        assert_eq!(m.config_dir, dir);
        assert_eq!(m.wasm_path, dir.join("plugins/zj_radar.wasm"));
        assert_eq!(std::fs::read(&m.wasm_path).unwrap(), b"\0asm-dummy");
        let cfg = std::fs::read_to_string(dir.join("config.kdl")).unwrap();
        assert!(cfg.contains(&format!("file:{}", m.wasm_path.display())));
        assert!(!cfg.contains("@WASM@"));
        assert!(dir.join("layouts/radar.kdl").exists());
        assert_eq!(
            std::fs::read_to_string(dir.join(".zj-radar-version")).unwrap(),
            asset_marker("0.1.0", &test_assets())
        );
    }

    #[test]
    fn materialize_escapes_the_wasm_path_inside_the_kdl_string() {
        // A `"` in the data dir must not terminate `location="file:…"` early:
        // the written config must parse, and decode back to the real path.
        let d = tempdir().unwrap();
        let dir = d.path().join("we\"ird").join("zellij");
        let m = materialize(&dir, "0.1.0", &test_assets()).unwrap();
        let cfg = std::fs::read_to_string(dir.join("config.kdl")).unwrap();
        let doc: kdl::KdlDocument = cfg.parse().unwrap_or_else(|e| panic!("config must parse ({e}):\n{cfg}"));
        let radar = doc.get("plugins").and_then(|p| p.children()).and_then(|c| c.get("radar")).expect("radar alias");
        let location = radar.get("location").and_then(|v| v.as_string()).expect("location string");
        assert_eq!(location, format!("file:{}", m.wasm_path.display()));
    }

    #[test]
    fn materialize_is_noop_on_matching_version_and_assets() {
        // The up-to-date probe is the marker (version + asset hash) plus file
        // existence — it never re-reads file CONTENTS. So a sentinel scribbled
        // into a generated file survives a matching re-materialize, proving
        // the second call wrote nothing. (A changed asset flips the marker and
        // rewrites — covered by the without-version-bump test below.)
        let d = tempdir().unwrap();
        let dir = d.path().join("c");
        materialize(&dir, "0.1.0", &test_assets()).unwrap();
        std::fs::write(dir.join("layouts/radar.kdl"), "SENTINEL-LOCAL-EDIT\n").unwrap();
        materialize(&dir, "0.1.0", &test_assets()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("layouts/radar.kdl")).unwrap(),
            "SENTINEL-LOCAL-EDIT\n",
            "matching version + assets must be a no-op"
        );
    }

    #[test]
    fn materialize_rewrites_when_a_file_is_missing_despite_matching_marker() {
        let d = tempdir().unwrap();
        let dir = d.path().join("c");
        materialize(&dir, "0.1.0", &test_assets()).unwrap();
        std::fs::remove_file(dir.join("config.kdl")).unwrap();
        materialize(&dir, "0.1.0", &test_assets()).unwrap();
        assert!(dir.join("config.kdl").exists(), "deleted file must be restored");
    }

    #[test]
    fn materialize_rewrites_on_version_change() {
        let d = tempdir().unwrap();
        let dir = d.path().join("c");
        materialize(&dir, "0.1.0", &test_assets()).unwrap();
        materialize(&dir, "0.2.0", &test_assets()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join(".zj-radar-version")).unwrap(),
            asset_marker("0.2.0", &test_assets())
        );
    }

    #[test]
    fn asset_marker_is_deterministic_and_covers_every_embedded_asset() {
        // Deterministic: two builds of the same inputs agree, so a matching
        // marker really means "same version, same assets".
        let base = asset_marker("0.1.0", &test_assets());
        assert_eq!(base, asset_marker("0.1.0", &test_assets()));
        assert!(base.starts_with("0.1.0-"), "marker leads with the crate version: {base}");
        // Structural staleness: editing ANY of the embedded text assets
        // changes the marker — no release-discipline version bump required.
        let mut config_edit = test_assets();
        config_edit.config_template = "plugins { radar location=\"file:@WASM@\" { edited } }\n";
        assert_ne!(base, asset_marker("0.1.0", &config_edit), "config.kdl edit must change the marker");
        let mut layout_edit = test_assets();
        layout_edit.layout = "layout { tab { pane; pane } }\n";
        assert_ne!(base, asset_marker("0.1.0", &layout_edit), "radar.kdl edit must change the marker");
        // And a plain version bump still re-materializes, as before.
        assert_ne!(base, asset_marker("0.2.0", &test_assets()));
    }

    #[test]
    fn materialize_rewrites_when_asset_content_changes_without_version_bump() {
        // The gap the content hash closes: a shipped asset edit that forgot
        // the version bump must still reach existing installs.
        let d = tempdir().unwrap();
        let dir = d.path().join("c");
        materialize(&dir, "0.1.0", &test_assets()).unwrap();
        let mut edited = test_assets();
        edited.layout = "layout { tab { pane; pane } }\n";
        materialize(&dir, "0.1.0", &edited).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("layouts/radar.kdl")).unwrap(),
            edited.layout,
            "same version + edited asset must re-materialize"
        );
    }

    #[test]
    fn bundled_layout_has_swaps_and_alias() {
        assert!(LAYOUT.contains("swap_tiled_layout"), "rail layout must declare swaps");
        assert!(LAYOUT.contains("location=\"radar\""), "rail must use the radar alias");
        assert!(CONFIG_TEMPLATE.contains("@WASM@"), "config template needs the @WASM@ token");
    }

    #[test]
    fn bundled_config_has_no_grant_float() {
        // The grant is pre-seeded (`preseed_grant`), so the owned config no
        // longer carries the onboarding float or its Ctrl-y keybind.
        assert!(!CONFIG_TEMPLATE.contains("Ctrl y"), "no grant keybind left");
        assert!(!CONFIG_TEMPLATE.contains("onboarding"), "no onboarding role left");
        assert!(!CONFIG_TEMPLATE.contains("grant_hint"), "the plugin has no grant_hint key");
    }

    #[test]
    fn bundled_config_binds_alt_n_tab_jumps() {
        // The rail numbers its rows after tabs, so the owned config supplies
        // Alt-1..9 → GoToTab as an unadvertised nicety (Zellij owns keybinds,
        // not the plugin).
        for n in 1..=9 {
            assert!(
                CONFIG_TEMPLATE.contains(&format!("bind \"Alt {n}\"")),
                "config must bind Alt {n} → GoToTab (the rail numbers rows after tabs)"
            );
        }
        assert!(CONFIG_TEMPLATE.contains("GoToTab 1"));
        // Deliberately NOT advertised: no `jump_hint` on the alias, so the
        // footer never renders ` alt-[n] jump`. Alt+digit is commonly claimed
        // upstream of Zellij (window-manager workspace hotkeys, macOS Option
        // typing `¡`), and the rail can't detect interception — a promise the
        // binds alone can't keep. `JumpHint` stays config-gated (default
        // hidden) for users whose setups do deliver the chord.
        assert!(
            CONFIG_TEMPLATE.lines().all(|l| !l.trim_start().starts_with("jump_hint")),
            "the alias must not advertise the alt-[n] chord — interception \
             upstream of Zellij makes the hint machine-dependent"
        );
    }

    #[test]
    fn stamped_marker_is_removed_by_the_rollback_path() {
        // The failed-create rollback in run() removes session_marker_path();
        // this pins that stamp and rollback resolve the SAME file, so the
        // rollback can never silently orphan a marker under a diverged path.
        // (The run()-level trigger itself — zellij exiting non-zero — spawns a
        // real zellij, so it's manually verified, not unit-tested.)
        let d = tempdir().unwrap();
        stamp_session_marker(d.path(), "proj");
        let marker = session_marker_path(d.path(), "proj");
        assert!(marker.exists(), "stamp must land at session_marker_path");
        let _ = std::fs::remove_file(&marker);
        assert!(!marker.exists(), "rollback removal must clear the stamp");
    }

    // ── preseed_grant ──
    const WASM: &str = "/data/zj-radar/zellij/plugins/zj_radar.wasm";

    #[test]
    fn preseed_grant_writes_a_full_grant_when_the_file_is_missing() {
        let d = tempdir().unwrap();
        let perms = d.path().join("zellij/permissions.kdl");
        assert!(preseed_grant(&perms, Path::new(WASM)), "a fresh write reports true");
        assert!(wasm_is_granted(&std::fs::read_to_string(&perms).unwrap(), WASM));
        assert!(!preseed_grant(&perms, Path::new(WASM)), "an existing grant is no new write");
    }

    #[test]
    fn preseed_grant_appends_and_backs_up_an_existing_file() {
        let d = tempdir().unwrap();
        let perms = d.path().join("permissions.kdl");
        let foreign = "\"/other.wasm\" {\n    ReadApplicationState\n}\n";
        std::fs::write(&perms, foreign).unwrap();
        preseed_grant(&perms, Path::new(WASM));
        let text = std::fs::read_to_string(&perms).unwrap();
        assert!(text.starts_with(foreign), "existing grants are kept byte-for-byte:\n{text}");
        assert!(wasm_is_granted(&text, WASM));
        let backup = crate::setup::path_with_suffix(&perms, crate::setup::BACKUP_SUFFIX);
        assert_eq!(std::fs::read_to_string(backup).unwrap(), foreign);
    }

    #[test]
    fn preseed_grant_leaves_an_unreadable_path_alone() {
        // A directory at the path reads as an error other than NotFound: the
        // preseed must not replace it with a fresh file.
        let d = tempdir().unwrap();
        let perms = d.path().join("permissions.kdl");
        std::fs::create_dir(&perms).unwrap();
        assert!(!preseed_grant(&perms, Path::new(WASM)), "a refusal is no write");
        assert!(perms.is_dir(), "the unreadable path must be untouched");
    }

    // ── plan_run decision matrix ──
    // Defaults: session "proj", does not exist (create path), owned, not nested.
    fn facts(granted: bool) -> RunFacts {
        RunFacts {
            session: "proj".to_string(),
            config_dir: PathBuf::from("/data/zj-radar/zellij"),
            wasm_path: PathBuf::from(WASM),
            session_exists: false,
            // Existing-session tests model sessions `run` itself created, so
            // the marker is present; `plan_run_flags_foreign_sessions` covers
            // the unowned case.
            session_owned: true,
            inside_zellij: false,
            permissions_kdl: granted
                .then(|| format!("\"{WASM}\" {{\n    {}\n}}\n", REQUIRED_PLUGIN_PERMISSIONS.join("\n    "))),
            grant_just_written: false,
        }
    }

    #[test]
    fn plan_run_creates_the_rail_layout_granted_or_not() {
        for granted in [true, false] {
            let p = plan_run(&facts(granted));
            assert_eq!(p.args, create_session_args(Path::new("/data/zj-radar/zellij"), "proj", "radar"));
            assert!(!p.nested);
        }
    }

    #[test]
    fn plan_run_attaches_when_session_exists() {
        let mut f = facts(true);
        f.session_exists = true;
        let p = plan_run(&f);
        assert_eq!(p.args, attach_session_args(Path::new("/data/zj-radar/zellij"), "proj"));
    }

    #[test]
    fn plan_run_flags_nested_when_inside_zellij() {
        let mut f = facts(true);
        f.inside_zellij = true;
        assert!(plan_run(&f).nested);
    }

    #[test]
    fn plan_run_flags_foreign_sessions_but_never_fresh_creates() {
        // An existing session without run's ownership marker is the user's own:
        // attaching swaps their client onto the bundled config, so the plan
        // must demand consent. A fresh create (no session) is never foreign,
        // and neither is an existing marker-stamped session.
        let mut f = facts(true);
        f.session_exists = true;
        f.session_owned = false;
        assert!(plan_run(&f).foreign_session);

        f.session_owned = true;
        assert!(!plan_run(&f).foreign_session);

        f.session_exists = false;
        f.session_owned = false;
        assert!(!plan_run(&f).foreign_session, "create path is never foreign");
    }

    #[test]
    fn plan_run_advises_the_rail_prompt_only_when_ungranted() {
        assert!(plan_run(&facts(true)).advisories.is_empty());
        let p = plan_run(&facts(false));
        assert_eq!(p.advisories, vec![GRANT_HINT.to_string()]);
        assert!(p.advisories[0].contains("press y"), "names the grant key");
        assert!(!p.advisories[0].contains("Ctrl-y"), "no keybind fallback left");
    }

    #[test]
    fn plan_run_advises_the_prompt_when_the_grant_just_landed_under_a_live_session() {
        // ICH-437: the file now reads granted, but an existing session's rails
        // may already sit at Zellij's prompt, which the file can't resolve.
        let mut f = facts(true);
        f.grant_just_written = true;
        assert!(plan_run(&f).advisories.is_empty(), "a fresh create loads the grant, no prompt");
        f.session_exists = true;
        assert_eq!(plan_run(&f).advisories, vec![GRANT_HINT.to_string()]);
        f.grant_just_written = false;
        assert!(plan_run(&f).advisories.is_empty(), "a grant older than this run means no pending prompt");
    }
}
