//! Native CLI (`zj-radar`): the host front door for the sidebar.
//!
//! Three subcommands, one per module:
//! - `setup [zellij]` ([`setup`]) — idempotent wiring: install the wasm plugin,
//!   manage the `radar` alias in `config.kdl`, and inject the rail into a
//!   Zellij layout ([`layout`]).
//! - `run` ([`run`]) — turnkey launch of a Zellij session that owns its own
//!   config with the rail preinstalled.
//! - `update` ([`update`]) — move the CLI and the sidebar wasm to a newer
//!   release together (self-replace, then re-exec `setup zellij --download`);
//!   hands Nix/cargo-managed binaries back to their package manager.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// The wasm artifact's file name — identical at the release-asset URL, the
/// installed plugin path, and `run`'s owned config dir, so the three can't
/// drift. (`build.rs` repeats the literal: it can't import the crate it builds.)
pub(crate) const WASM_FILE_NAME: &str = "zj_radar.wasm";

mod fsutil;
pub(crate) mod layout;
mod run;
mod setup;
mod update;

/// Process-wide failure flag. The setup/run orchestrators report refusals and
/// write failures by printing a diagnostic and returning early through several
/// layers; they mark the invocation failed here instead of threading a Result
/// through every signature. [`run`] maps it to the process exit code, so
/// `zj-radar setup … && next` composes correctly in scripts and installers.
/// A user *declining* a confirmation prompt is not a failure.
pub(crate) mod exit {
    use std::sync::atomic::{AtomicBool, Ordering};

    static FAILED: AtomicBool = AtomicBool::new(false);

    /// Report a failure as `{label}: {msg}` on stderr AND mark the invocation
    /// failed. This is the only way to set the flag, so a failure diagnostic
    /// can never print without the exit code following it. Raw `eprintln!` is
    /// reserved for warnings, guidance, and continuation lines — anything that
    /// reports a *failure* goes through here.
    pub(crate) fn fail_report(label: &str, msg: impl std::fmt::Display) {
        FAILED.store(true, Ordering::Relaxed);
        eprintln!("{label}: {msg}");
    }

    pub(crate) fn failed() -> bool {
        FAILED.load(Ordering::Relaxed)
    }
}

#[derive(Parser)]
#[command(
    name = "zj-radar",
    version,
    about = "Install the zj-radar sidebar into Zellij and launch Zellij sessions with it."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Launch a turnkey Zellij session with the radar rail (owns its own config).
    Run {
        /// Session name (default: current directory's name).
        name: Option<String>,
        /// Print the zellij command instead of launching it.
        #[arg(long)]
        print_cmd: bool,
    },
    /// Idempotently set up Zellij to use the zj-radar sidebar.
    Setup {
        /// Target to set up (default and only supported: zellij).
        targets: Vec<String>,
        /// Wasm artifact to install when setting up Zellij.
        #[arg(long, value_name = "PATH")]
        wasm: Option<PathBuf>,
        /// Download the wasm matching this CLI's version instead of passing --wasm
        /// (set ZJ_RADAR_VERSION to pin a different release tag).
        #[arg(long)]
        download: bool,
        /// Remove our entries instead of adding them.
        #[arg(long)]
        uninstall: bool,
        /// Show what would change; write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation prompt.
        // The doctor's remedy hints print the short form (`setup zellij -y`),
        // so the alias must exist for them to be copy-pasteable.
        #[arg(long, short = 'y')]
        yes: bool,
        /// Check setup status without writing files.
        // Conflicts with --uninstall: silently running the doctor instead of
        // uninstalling would read as "uninstalled".
        #[arg(long, conflicts_with = "uninstall")]
        check: bool,
        /// Overwrite a conflicting `radar` plugin alias in config.kdl.
        #[arg(long)]
        force: bool,
        /// Inject the rail into the target layout without prompting (consent flag).
        #[arg(long)]
        inject: bool,
        /// Target layout name (default: the config's `default_layout`, else
        /// "default"). Looks up `<config_dir>/layouts/<name>.kdl`; honored by
        /// install, --uninstall, and --check alike.
        #[arg(long, value_name = "NAME")]
        layout: Option<String>,
    },
    /// Update the CLI and the sidebar wasm to the latest release together
    /// (set ZJ_RADAR_VERSION to pin a specific release tag).
    Update {
        /// Report whether an update is available without writing anything;
        /// exits non-zero when one is.
        #[arg(long)]
        check: bool,
    },
    /// Removed in favour of OSC 7501. Kept hidden so a hook left by an older
    /// release exits 0: Claude Code treats a non-zero hook exit as blocking
    /// (a PreToolUse hook would block every tool call).
    #[command(hide = true)]
    Notify {
        /// Whatever the old hook passes (agent name, flags, payload).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

/// Printed by the hidden `notify`: hook stderr is often discarded, but a user
/// who runs the hook by hand learns what to remove.
pub(crate) const NOTIFY_REMOVED: &str = "zj-radar: `notify` was removed — agents report through OSC 7501 \
    now. Remove the old zj-radar hook, plugin or extension of this agent (see docs/install.md, \
    \"Upgrading from a release with agent adapters\").";

/// CLI entry point (called by `src/main.rs`). Returns the process exit code:
/// failure when any orchestrator flagged a refusal/error via [`exit::fail_report`].
pub fn run() -> std::process::ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Run { name, print_cmd } => {
            run::run(run::RunOptions { name, print_cmd });
        }
        Command::Setup { targets, wasm, download, uninstall, dry_run, yes, check, force, inject, layout } => {
            setup::run(setup::SetupOptions {
                targets: &targets,
                wasm: wasm.as_deref(),
                download,
                uninstall,
                dry_run,
                yes,
                check,
                force,
                inject,
                layout: layout.as_deref(),
            });
        }
        Command::Update { check } => {
            update::run(update::UpdateOptions { check });
        }
        Command::Notify { args: _ } => eprintln!("{NOTIFY_REMOVED}"),
    }
    if exit::failed() {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}
