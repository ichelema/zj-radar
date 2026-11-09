//! `zj-radar setup [zellij]` — idempotent, conflict-aware local wiring of the
//! sidebar: install the wasm at a stable path, manage the `radar` alias in
//! `config.kdl`, inject the rail into a layout, and pre-authorize the plugin's
//! permissions. Agents need no setup: they emit OSC 7501 themselves.

mod analyze;
mod check;
pub(crate) mod detect;
mod download;
mod edit;
pub(crate) mod preseed;
mod zellij;
pub(crate) use analyze::*;
pub(crate) use check::*;
pub(crate) use download::*;
pub(crate) use edit::*;
pub(crate) use zellij::*;

use std::path::{Path, PathBuf};

pub(crate) const ZELLIJ_ALIAS_BEGIN: &str = "// zj-radar: managed plugin alias begin";
pub(crate) const ZELLIJ_ALIAS_END: &str = "// zj-radar: managed plugin alias end";

pub struct SetupOptions<'a> {
    pub targets: &'a [String],
    pub wasm: Option<&'a Path>,
    /// Fetch the wasm matching this CLI's version instead of passing `--wasm`.
    pub download: bool,
    pub uninstall: bool,
    pub dry_run: bool,
    pub yes: bool,
    pub check: bool,
    pub force: bool,
    /// Non-interactive consent to inject the rail into the target layout.
    pub inject: bool,
    /// Layout name (`<layout dir>/<name>.kdl`) or path to inject into — see
    /// `detect::resolve_layout_path_from`.
    /// `None` means `default`.
    pub layout: Option<&'a str>,
}

/// Where the wasm artifact comes from. A total type so "both --wasm and
/// --download" is a refusal at one place, not a runtime check inside the
/// orchestrator.
pub(crate) enum WasmSource {
    None,
    Path(PathBuf),
    Download,
}

pub(crate) fn wasm_source(wasm: Option<&Path>, download: bool) -> Result<WasmSource, String> {
    match (wasm, download) {
        (Some(_), true) => Err("pass either --wasm <path> or --download, not both".to_string()),
        (Some(p), false) => Ok(WasmSource::Path(p.to_path_buf())),
        (None, true) => Ok(WasmSource::Download),
        (None, false) => Ok(WasmSource::None),
    }
}

pub(crate) struct ZellijSetupOpts<'a> {
    wasm_source: WasmSource,
    force: bool,
    inject: bool,
    layout: Option<&'a str>,
    dry_run: bool,
    yes: bool,
    is_tty: bool,
}

/// The single operation a `setup` invocation performs. Resolving this once makes
/// the precedence (check > uninstall > install) explicit instead of
/// implicit in the order of `if` blocks.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Check,
    Uninstall,
    Install,
}

/// Clap already hard-errors on `--check --uninstall`, so the
/// check-beats-uninstall rung is defensive, not a CLI surface.
pub(crate) fn mode_from_flags(check: bool, uninstall: bool) -> Mode {
    if check {
        Mode::Check
    } else if uninstall {
        Mode::Uninstall
    } else {
        Mode::Install
    }
}

pub(crate) fn which(bin: &str) -> bool {
    std::env::var_os("PATH").map(|paths| std::env::split_paths(&paths).any(|p| p.join(bin).is_file())).unwrap_or(false)
}

/// Agents that older releases wired; naming one gets the removal hint.
const REMOVED_AGENT_TARGETS: [&str; 5] = ["claude", "codex", "opencode", "pi", "omp"];

/// Why `setup <target>` refuses: `zellij` is the only target left.
fn unsupported_target_message(target: &str) -> String {
    let mut msg = format!(
        "setup does not support '{target}': zj-radar no longer wires agents \
         (agents that emit OSC 7501 need no setup)."
    );
    if REMOVED_AGENT_TARGETS.contains(&target) {
        msg.push_str(&format!(
            " Remove its old adapter by hand: docs/install.md, \"Upgrading from a release \
             with agent adapters\". A CLI from a release with adapters also removes it \
             (`zj-radar setup {target} --uninstall`)."
        ));
    }
    msg
}

/// Entry point for `zj-radar setup`. The only target is `zellij`, which is
/// also what no target means.
pub fn run(options: SetupOptions<'_>) {
    let mut unsupported = false;
    for target in options.targets.iter().filter(|t| t.as_str() != "zellij") {
        crate::exit::fail_report("zj-radar", unsupported_target_message(target));
        unsupported = true;
    }
    if unsupported {
        return;
    }

    let mode = mode_from_flags(options.check, options.uninstall);

    if mode == Mode::Check {
        // The doctor inspects the zellij install; checking needs no wasm source.
        if check_zellij(options.layout) {
            // The items above are the diagnostic; this sets the exit code so
            // `zj-radar setup --check && zj-radar run` can gate on the doctor.
            crate::exit::fail_report("zj-radar", "check found missing items (listed above)");
        }
        return;
    }

    let uninstall = mode == Mode::Uninstall;
    let wasm_source = if uninstall {
        WasmSource::None
    } else {
        match wasm_source(options.wasm, options.download) {
            Ok(s) => s,
            Err(e) => {
                crate::exit::fail_report("zellij", format!("refused — {e}"));
                return;
            }
        }
    };
    // Tty-ness resolved ONCE at the invocation boundary (the `inject_mode`
    // pattern); the consent steps take it as a parameter rather than probing
    // stdin again mid-chain.
    let is_tty = {
        use std::io::IsTerminal;
        std::io::stdin().is_terminal()
    };
    setup_zellij(
        uninstall,
        ZellijSetupOpts {
            wasm_source,
            force: options.force,
            inject: options.inject,
            layout: options.layout,
            dry_run: options.dry_run,
            yes: options.yes,
            is_tty,
        },
    );
}

/// The shared preamble for every `setup_*` step: turn an editor's
/// `Result<Outcome, String>` into an `Option<Outcome>`, reporting a refusal as the
/// standard `{label}: refused — {e}` line and yielding `None` so the caller bails.
/// Centralizes the one diagnostic every orchestrator would otherwise print by hand.
pub(crate) fn edit_or_report(label: &str, edit: Result<Outcome, String>) -> Option<Outcome> {
    match edit {
        Ok(outcome) => Some(outcome),
        Err(e) => {
            crate::exit::fail_report(label, format!("refused — {e}"));
            None
        }
    }
}

/// Ask for consent. `yes` (`-y`) grants without asking; tty-ness arrives as a
/// parameter, resolved ONCE at the invocation boundary (the `inject_mode`
/// pattern) — probing `stdin().is_terminal()` in here made the answer
/// untestable and let one chain probe twice. No tty = no one to answer: take
/// the safe "no" instead of blocking on a read that never returns (the same
/// non-tty rule `inject_mode` and `run`'s foreign-session consent apply), and
/// say how to consent non-interactively.
pub(crate) fn confirm(prompt: &str, yes: bool, is_tty: bool) -> bool {
    use std::io::Write;
    if yes {
        return true;
    }
    if !is_tty {
        println!("{prompt} — skipped (no tty — re-run with -y)");
        return false;
    }
    print!("{prompt} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut line);
    consented(&line)
}

/// The parse half of [`confirm`]: `y`/`yes` (case folded) consent; anything
/// else — including the empty string an EOF'd read leaves — declines. Split
/// out so answered-y, answered-n, and EOF-decline stay unit-tested (the tty
/// gate above keeps the real stdin out of reach in tests).
fn consented(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// [`confirm_and_write`]'s consent inputs: `-y` and the boundary tty probe.
#[derive(Clone, Copy)]
pub(crate) struct Consent {
    pub yes: bool,
    pub is_tty: bool,
}

/// The shared "commit an edit" tail for every `setup_*` step: prompt (unless
/// `--yes`), run any `pre_write` side effects (e.g. copying the wasm), then write
/// `new` to `path` atomically — emitting the standard `skipped`/`failed`
/// diagnostics under `label`. Returns whether the file was written, so the caller
/// can print its success epilogue. Callers keep `--dry-run` handling and prompt
/// wording, which differ per target. A `pre_write` error is reported as
/// `{label}: {e}`, so its message should read as a sentence without the prefix.
pub(crate) fn confirm_and_write(
    label: &str,
    path: &Path,
    new: &str,
    consent: Consent,
    prompt: &str,
    pre_write: impl FnOnce() -> Result<(), String>,
) -> bool {
    let Consent { yes, is_tty } = consent;
    if !confirm(prompt, yes, is_tty) {
        println!("{label}: skipped (declined)");
        return false;
    }
    if let Err(e) = pre_write() {
        crate::exit::fail_report(label, e);
        return false;
    }
    if let Err(e) = backup_then_write(path, new) {
        crate::exit::fail_report(label, format!("write failed — {e}"));
        return false;
    }
    true
}

/// Suffix of the pre-write backup `setup` leaves beside every file it edits.
/// The success epilogues interpolate it (via [`path_with_suffix`]) so the
/// advertised restore point can never drift from the file
/// [`backup_then_write`] actually creates.
pub(crate) const BACKUP_SUFFIX: &str = ".zj-radar.bak";

/// Back up the existing file, then write atomically (temp file + rename via the
/// shared `fsutil::atomic_write`). The `.bak` is specific to `setup` editing the
/// user's own files; `run` writes its owned dir without one. A failed backup
/// aborts the write: the success epilogues advertise the `.bak` as the restore
/// point, so the user's file must never be replaced without it existing.
pub(crate) fn backup_then_write(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    if path.exists() {
        std::fs::copy(path, path_with_suffix(path, BACKUP_SUFFIX)).map_err(|e| {
            std::io::Error::new(e.kind(), format!("backup copy failed ({e}); {} left untouched", path.display()))
        })?;
    }
    crate::fsutil::atomic_write(path, contents.as_bytes())
}

pub(crate) fn path_with_suffix(path: &std::path::Path, suffix: &str) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| format!("{}{}", name.to_string_lossy(), suffix))
        .unwrap_or_else(|| format!("config{suffix}"));
    path.with_file_name(file_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wasm_source_rejects_both_path_and_download() {
        let p = std::path::Path::new("/x.wasm");
        assert!(matches!(wasm_source(Some(p), false), Ok(WasmSource::Path(_))));
        assert!(matches!(wasm_source(None, true), Ok(WasmSource::Download)));
        assert!(matches!(wasm_source(None, false), Ok(WasmSource::None)));
        assert!(wasm_source(Some(p), true).is_err(), "both --wasm and --download must refuse");
    }

    #[test]
    fn mode_precedence_check_beats_uninstall() {
        assert!(matches!(mode_from_flags(true, true), Mode::Check));
        assert!(matches!(mode_from_flags(false, true), Mode::Uninstall));
        assert!(matches!(mode_from_flags(false, false), Mode::Install));
    }

    #[test]
    fn unsupported_target_hints_removal_only_for_the_old_agents() {
        for agent in REMOVED_AGENT_TARGETS {
            let msg = unsupported_target_message(agent);
            assert!(msg.contains("no longer wires agents"), "{msg}");
            assert!(msg.contains("by hand: docs/install.md"), "{msg}");
            assert!(msg.contains(&format!("zj-radar setup {agent} --uninstall")), "{msg}");
        }
        let msg = unsupported_target_message("gemini");
        assert!(msg.contains("no longer wires agents") && !msg.contains("--uninstall"), "{msg}");
    }

    #[test]
    fn consented_accepts_only_y_or_yes() {
        // Answered y (any case, either spelling) → consent.
        assert!(consented("y\n"));
        assert!(consented("Y\n"));
        assert!(consented("yes\n"));
        assert!(consented("  YES  \n"));
        // Answered n (or anything else) → decline.
        assert!(!consented("n\n"));
        assert!(!consented("no\n"));
        assert!(!consented("yep\n"));
        // EOF leaves the buffer empty — the default must be the safe "no".
        assert!(!consented(""));
    }

    #[test]
    fn confirm_yes_flag_consents_without_reading() {
        // `-y` wins before the tty gate: consent even with no tty to answer on.
        assert!(confirm("Write?", true, false));
        assert!(confirm("Write?", true, true));
        // No `-y`, no tty: the safe "no" (never blocks on a read).
        assert!(!confirm("Write?", false, false));
    }

    #[test]
    fn backup_then_write_aborts_when_backup_cannot_be_written() {
        // The success epilogues advertise the .bak as the restore point, so a
        // failed backup must abort the write, not overwrite-and-lie. Force the
        // copy to fail by occupying the .bak path with a directory.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.kdl");
        std::fs::write(&target, "original").unwrap();
        std::fs::create_dir(path_with_suffix(&target, BACKUP_SUFFIX)).unwrap();

        let err = backup_then_write(&target, "replacement").unwrap_err();
        assert!(err.to_string().contains("backup copy failed"), "err: {err}");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "original",
            "target must be untouched when the backup fails"
        );
    }
}
