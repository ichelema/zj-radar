//! Integration tests for `zj-radar setup` — the zellij install/uninstall/doctor
//! flows, the refusal of the agent targets older releases wired, and the
//! `setup shell` rc block (with real bash and zsh when installed).

use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;

// ── agent targets are gone: only `zellij` is a setup target ─────────────────

#[test]
fn setup_refuses_agent_targets_and_hints_removal_only_for_the_old_ones() {
    let home = TempDir::new().unwrap();
    let config_dir = TempDir::new().unwrap();
    let setup = |target: &str| {
        let out = Command::cargo_bin("zj-radar")
            .unwrap()
            .args(["setup", target, "--yes"])
            .env("HOME", home.path())
            .env("ZELLIJ_CONFIG_DIR", config_dir.path())
            .output()
            .unwrap();
        assert!(!out.status.success(), "`setup {target}` must fail");
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    for agent in ["claude", "codex", "opencode", "pi", "omp"] {
        let stderr = setup(agent);
        assert!(stderr.contains("no longer wires agents"), "{stderr}");
        assert!(stderr.contains(&format!("zj-radar setup {agent} --uninstall")), "{stderr}");
    }
    let stderr = setup("gemini");
    assert!(stderr.contains("no longer wires agents") && !stderr.contains("--uninstall"), "{stderr}");
    assert_eq!(fs::read_dir(config_dir.path()).unwrap().count(), 0, "a refused target writes nothing");
}

// ── `--wasm` and `--download` are mutually exclusive ─────────────────────────
// The guard must short-circuit before any download or config write.

#[test]
fn setup_zellij_refuses_wasm_and_download_together() {
    let config_dir = TempDir::new().unwrap();
    let config_path = config_dir.path().join("config.kdl");

    let assert = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--wasm", "/tmp/x.wasm", "--download", "--yes"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(stderr.contains("not both"), "expected a mutual-exclusion refusal; got stderr: {stderr:?}");

    assert!(!config_path.exists(), "the conflict guard must not write config.kdl");
}

// ── Test: an interrupted download never leaves a partial wasm behind ─────────
// curl/wget write the destination incrementally, so a killed transfer used to
// leave a partial file that the exists()/up-to-date gates then treated as a
// valid wasm forever after (and Zellij would load it with permissions). The
// download must stage to a `.part` sibling and clean it up on failure.

#[test]
fn interrupted_download_leaves_no_partial_wasm() {
    let fakebin = TempDir::new().unwrap();
    let config_dir = TempDir::new().unwrap();
    let tmp = TempDir::new().unwrap();

    // A fake `curl` that mimics an interrupted transfer: writes partial bytes
    // to the `-o` target, then fails.
    let curl = fakebin.path().join("curl");
    fs::write(
        &curl,
        "#!/bin/sh\nprev=\"\"\nfor a in \"$@\"; do\n  [ \"$prev\" = \"-o\" ] && printf 'PARTIAL' > \"$a\"\n  prev=\"$a\"\ndone\nexit 1\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&curl, fs::Permissions::from_mode(0o755)).unwrap();
    }

    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--download", "--yes"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .env("PATH", fakebin.path()) // only the fake curl is resolvable
        .env("TMPDIR", tmp.path()) // std::env::temp_dir() lands here
        .assert()
        .failure();

    // Downloads stage in a per-user `zj-radar-<user>/` subdir of the temp root;
    // sweep the whole tree so a leftover wasm/.part/.sha256 anywhere is caught.
    let mut leftovers = Vec::new();
    sweep_files(tmp.path(), &mut leftovers);
    assert!(
        leftovers.is_empty(),
        "a failed download must leave neither the wasm nor a .part behind, found {leftovers:?}"
    );
}

/// Collect every file path under `dir`, recursively. Shared by the download
/// tests that assert a failed/rejected fetch stages nothing durable.
fn sweep_files(dir: &std::path::Path, hits: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            sweep_files(&path, hits);
        } else {
            hits.push(path.display().to_string());
        }
    }
}

// A downloaded wasm whose bytes do not match the release's published `.sha256`
// must be refused, not installed — Zellij loads the wasm with permissions, so a
// payload that fails its digest is the security boundary of the whole download
// path. The generalization to `download_verified_asset` shipped without a test
// for this branch; this pins it.

#[test]
fn checksum_mismatch_refuses_to_install_the_wasm() {
    let fakebin = TempDir::new().unwrap();
    let config_dir = TempDir::new().unwrap();
    let tmp = TempDir::new().unwrap();

    // A fake `curl` that serves BOTH fetches this flow makes: real-looking wasm
    // bytes for the `.wasm` URL, and a digest that cannot match those bytes
    // (all zeros) for the `.sha256` sidecar. Distinguished by the URL suffix.
    let curl = fakebin.path().join("curl");
    fs::write(
        &curl,
        "#!/bin/sh\nurl=\"\"; out=\"\"; prev=\"\"\n\
         for a in \"$@\"; do\n\
         \x20 case \"$a\" in *github.com*) url=\"$a\";; esac\n\
         \x20 [ \"$prev\" = \"-o\" ] && out=\"$a\"\n\
         \x20 prev=\"$a\"\n\
         done\n\
         case \"$url\" in\n\
         \x20 *.sha256) printf '%s' 0000000000000000000000000000000000000000000000000000000000000000 > \"$out\";;\n\
         \x20 *) printf 'not-the-real-wasm' > \"$out\";;\n\
         esac\n\
         exit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&curl, fs::Permissions::from_mode(0o755)).unwrap();
    }

    // Prepend the fake curl but keep the inherited PATH, so `sha256sum`/`shasum`
    // still resolve — without a hash tool the code degrades to a TLS-only
    // warning and the mismatch would never be caught (a different path).
    let path = format!("{}:{}", fakebin.path().display(), std::env::var("PATH").unwrap_or_default());

    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--download", "--yes"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .env("PATH", path)
        .env("TMPDIR", tmp.path())
        .assert()
        .failure();

    // Nothing installed at the destination, and no staged bytes left behind.
    assert!(
        !config_dir.path().join("plugins").join("zj_radar.wasm").exists(),
        "a checksum mismatch must not leave an installed wasm"
    );
    // Keep only download artifacts: `zellij --version` (real zellij is on PATH
    // here) writes its own log under TMPDIR, which is not our concern.
    let mut swept = Vec::new();
    sweep_files(tmp.path(), &mut swept);
    let staged: Vec<_> = swept
        .into_iter()
        .filter(|p| p.contains("zj_radar") || p.ends_with(".part") || p.ends_with(".sha256"))
        .collect();
    assert!(staged.is_empty(), "a rejected download must leave no wasm/.part/.sha256 behind, found {staged:?}");
}

// ── Test: setup/check operate on the layout Zellij actually loads ────────────
// The layout name resolves --layout → config's `default_layout` → "default".
// Before this, both hardcoded default.kdl: a `default_layout "main"` user got
// the rail injected into a file Zellij never reads, and --check contradicted a
// successful `--inject --layout my` install.

#[test]
fn check_inspects_the_configs_default_layout_and_honors_layout_flag() {
    let config_dir = TempDir::new().unwrap();
    fs::write(config_dir.path().join("config.kdl"), "default_layout \"main\"\n").unwrap();
    let layouts = config_dir.path().join("layouts");
    fs::create_dir_all(&layouts).unwrap();
    // main.kdl HAS the rail; other.kdl does not.
    fs::write(layouts.join("main.kdl"), "layout {\n    pane\n    // zj-radar:wrap begin\n}\n").unwrap();
    fs::write(layouts.join("other.kdl"), "layout {\n    pane\n}\n").unwrap();

    let check = |extra: &[&str]| {
        let mut args = vec!["setup", "zellij", "--check"];
        args.extend_from_slice(extra);
        // No `.success()`: the doctor exits non-zero when anything is Missing,
        // and this fixture has no wasm — the layout item is the subject here.
        let output = Command::cargo_bin("zj-radar")
            .unwrap()
            .args(&args)
            .env("ZELLIJ_CONFIG_DIR", config_dir.path())
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    };

    // No --layout: the doctor must inspect main.kdl (the default_layout), which
    // has the rail — not default.kdl (absent, would report missing).
    let out = check(&[]);
    assert!(
        out.contains("ok layout"),
        "check must inspect the config's default_layout (main.kdl, has rail); got:\n{out}"
    );

    // --layout other: the doctor must inspect other.kdl, which lacks the rail.
    let out = check(&["--layout", "other"]);
    assert!(!out.contains("ok layout"), "check --layout other must inspect other.kdl (no rail); got:\n{out}");
}

// ── Test: the doctor is scriptable ───────────────────────────────────────────
// Missing items set the exit code (`setup --check && zj-radar run` can gate).

#[test]
fn check_exit_code_gates_on_missing_items() {
    let home = TempDir::new().unwrap();
    let config_dir = TempDir::new().unwrap(); // empty: the zellij half is all Missing
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "--check"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("ZELLIJ_CONFIG_FILE")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("zellij:"), "bare --check must report the zellij half; got:\n{stdout}");
    assert!(!stdout.contains("producer"), "no producer item any more; got:\n{stdout}");
    assert!(!output.status.success(), "missing items must exit non-zero so scripts can gate on the doctor");
}

// ── Layout injection tests ────────────────────────────────────────────────────
//
// `setup zellij --inject` (no --wasm / --download) takes the inject-only path:
// it skips the wasm/alias step and operates directly on the target layout.
//
// The fixture layout uses bare booleans (`borderless=true`, `focus=true`) exactly
// as real Zellij layouts are written, which exercises the KDL v1-fallback parser.

const FIXTURE_LAYOUT: &str = "\
layout {
    default_tab_template {
        pane size=1 borderless=true {
            plugin location=\"zellij:tab-bar\"
        }
        children
        pane size=2 borderless=true {
            plugin location=\"zellij:status-bar\"
        }
    }
    tab focus=true {
        pane
    }
}
";

/// Set up a temp config dir with a `layouts/default.kdl` fixture.
fn isolated_zellij_config(layout_text: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    let layouts_dir = dir.path().join("layouts");
    fs::create_dir_all(&layouts_dir).unwrap();
    fs::write(layouts_dir.join("default.kdl"), layout_text).unwrap();
    dir
}

// ── --inject with NO layout file creates the full layout ─────────────────────
// A stock Zellij ships no layout file at all — this is most first installs.
// The old behavior printed a fragment snippet with nothing to paste it into
// (a dead end); with --inject consent, the full known-good layout is created.

#[test]
fn setup_zellij_inject_creates_full_layout_when_none_exists() {
    let config_dir = TempDir::new().unwrap(); // no layouts/ at all
    let layout_path = config_dir.path().join("layouts").join("default.kdl");

    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--inject"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success();

    let created = fs::read_to_string(&layout_path).unwrap();
    assert!(created.contains("plugin location=\"radar\""), "created layout must carry the rail; got:\n{created}");
    assert!(created.contains("swap_tiled_layout"), "created layout must carry the swap layouts; got:\n{created}");
    assert!(
        !layout_path.with_file_name("default.kdl.zj-radar.bak").exists(),
        "a freshly created layout has no original to back up"
    );
}

#[test]
fn setup_zellij_yes_never_creates_a_layout_file() {
    // --yes takes the safe non-mutating default: snippet + guidance only.
    let config_dir = TempDir::new().unwrap();
    let layout_path = config_dir.path().join("layouts").join("default.kdl");

    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--yes"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .output()
        .unwrap();

    assert!(!layout_path.exists(), "--yes must not create files");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--inject"), "the no-layout fallback must point at the create route; got:\n{stdout}");
}

// ── --inject writes the rail into the layout and creates a .bak ───────────────

#[test]
fn setup_zellij_inject_writes_rail_and_bak() {
    let config_dir = isolated_zellij_config(FIXTURE_LAYOUT);
    let layout_path = config_dir.path().join("layouts").join("default.kdl");
    let bak_path = config_dir.path().join("layouts").join("default.kdl.zj-radar.bak");

    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--inject"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success();

    let injected = fs::read_to_string(&layout_path).unwrap();
    assert!(injected.contains("// zj-radar:wrap begin"), "--inject must add the wrap begin marker; got:\n{injected}");
    assert!(injected.contains("plugin location=\"radar\""), "--inject must add the radar plugin; got:\n{injected}");
    assert!(injected.contains("swap_tiled_layout"), "--inject must add swap layouts; got:\n{injected}");
    assert!(bak_path.exists(), "--inject must create a .zj-radar.bak backup at {}", bak_path.display());

    // The backup must be the original fixture.
    let bak = fs::read_to_string(&bak_path).unwrap();
    assert_eq!(bak, FIXTURE_LAYOUT, ".bak must contain the original layout");
}

// ── --inject with existing swaps skips swap blocks, prints advisory ────────────

#[test]
fn setup_zellij_inject_with_existing_swaps_skips_swaps_and_advises() {
    // A layout that already declares its own swap_tiled_layout: inject must
    // wrap the rail and add the `ui` template, but never append our swap
    // blocks next to the user's — and it must SAY so, or the first Alt+]
    // silently swaps the rail away.
    let layout_with_swaps = "\
layout {
    default_tab_template {
        children
    }
    swap_tiled_layout name=\"vertical\" {
        tab_template {
            pane split_direction=\"vertical\" {
                pane
                pane
            }
        }
    }
    tab focus=true {
        pane
    }
}
";
    let config_dir = isolated_zellij_config(layout_with_swaps);
    let layout_path = config_dir.path().join("layouts").join("default.kdl");

    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--inject"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success()
        .get_output()
        .clone();

    let injected = fs::read_to_string(&layout_path).unwrap();
    assert!(injected.contains("plugin location=\"radar\""), "--inject must add the radar plugin; got:\n{injected}");
    assert!(injected.contains("tab_template name=\"ui\""), "--inject must add the ui template; got:\n{injected}");
    assert_eq!(
        injected.matches("swap_tiled_layout").count(),
        1,
        "the user's lone swap block must remain the only one; got:\n{injected}"
    );

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout.contains("swap_tiled_layout blocks, which were left untouched"),
        "must print the swap advisory; stdout:\n{stdout}"
    );
}

// ── --yes without --inject → Snippet: layout unchanged, prints snippet ──────────

#[test]
fn setup_zellij_yes_without_inject_prints_snippet_and_does_not_modify() {
    let config_dir = isolated_zellij_config(FIXTURE_LAYOUT);
    let layout_path = config_dir.path().join("layouts").join("default.kdl");

    // --yes without --inject: inject_mode → Snippet (safe default, never mutate).
    // Since there is no --wasm/--download, this takes the layout_only_install path.
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--yes"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    // Layout must be unmodified.
    let after = fs::read_to_string(&layout_path).unwrap();
    assert_eq!(after, FIXTURE_LAYOUT, "--yes without --inject must not modify the layout (Snippet mode)");
    // The tailored snippet must be printed.
    assert!(
        stdout.contains("default_tab_template") || stdout.contains("Add the sidebar"),
        "must print the tailored snippet; stdout:\n{stdout}"
    );
}

// ── --uninstall reverses injection ─────────────────────────────────────────────

#[test]
fn setup_zellij_uninstall_reverses_injection() {
    // First inject the rail into the fixture layout.
    let config_dir = isolated_zellij_config(FIXTURE_LAYOUT);
    let layout_path = config_dir.path().join("layouts").join("default.kdl");

    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--inject"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success();

    let injected = fs::read_to_string(&layout_path).unwrap();
    assert!(injected.contains("// zj-radar:wrap begin"), "prerequisite: inject must have written the rail");

    // Now uninstall (layout-only, no wasm config needed).
    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--uninstall"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success();

    let after = fs::read_to_string(&layout_path).unwrap();
    assert!(
        !after.contains("// zj-radar:wrap begin") && !after.contains("// zj-radar:block begin"),
        "--uninstall must remove the begin markers"
    );
    assert!(!after.contains("plugin location=\"radar\""), "--uninstall must remove the radar plugin");
    // Must still be valid KDL with the original tab preserved.
    assert!(after.contains("default_tab_template"), "uninstall must preserve the rest of the layout");
}

// ── the mini rail: fresh inject, upgrade of a pre-mini injection, uninstall ──

/// What a version before the mini rail wrote: `layout` without any mini pane (5 lines each).
fn strip_mini(layout: &str) -> String {
    let mut lines = layout.split_inclusive('\n');
    let mut out = String::new();
    while let Some(line) = lines.next() {
        if line.trim() == "pane size=4 borderless=true {" {
            lines.by_ref().take(4).for_each(drop);
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Run `setup zellij <args>` against `config_dir`; returns stdout.
fn setup_zellij(config_dir: &TempDir, args: &[&str]) -> String {
    let out = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij"])
        .args(args)
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success()
        .get_output()
        .clone();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn setup_zellij_inject_writes_the_mini_rail_next_to_every_rail() {
    let config_dir = isolated_zellij_config(FIXTURE_LAYOUT);
    setup_zellij(&config_dir, &["--inject"]);
    let injected = fs::read_to_string(config_dir.path().join("layouts/default.kdl")).unwrap();
    // wrap + new_tab_template + ui template
    assert_eq!(injected.matches("pane size=32 borderless=true {").count(), 3, "{injected}");
    assert_eq!(injected.matches("pane size=4 borderless=true {").count(), 3, "{injected}");
    assert_eq!(injected.matches("mini true").count(), 3, "{injected}");

    let created = TempDir::new().unwrap();
    setup_zellij(&created, &["--inject"]);
    let whole = fs::read_to_string(created.path().join("layouts/default.kdl")).unwrap();
    assert_eq!(whole.matches("mini true").count(), 3, "{whole}");
}

#[test]
fn setup_zellij_inject_adds_the_mini_rail_to_a_pre_mini_injection_and_is_idempotent() {
    let config_dir = isolated_zellij_config(FIXTURE_LAYOUT);
    let layout_path = config_dir.path().join("layouts/default.kdl");
    setup_zellij(&config_dir, &["--inject"]);
    let current = fs::read_to_string(&layout_path).unwrap();
    let pre_mini = strip_mini(&current);
    assert_ne!(pre_mini, current, "fixture must actually differ");
    fs::write(&layout_path, &pre_mini).unwrap();

    // Without --inject the layout is never touched; the user is told how to upgrade.
    let stdout = setup_zellij(&config_dir, &["--yes"]);
    assert!(stdout.contains("not the mini rail") && stdout.contains("--inject"), "{stdout}");
    assert_eq!(fs::read_to_string(&layout_path).unwrap(), pre_mini);

    let stdout = setup_zellij(&config_dir, &["--inject"]);
    assert!(stdout.contains("added the mini rail"), "{stdout}");
    assert_eq!(fs::read_to_string(&layout_path).unwrap(), current, "only the mini panes are added");
    let bak = config_dir.path().join("layouts/default.kdl.zj-radar.bak");
    assert_eq!(fs::read_to_string(bak).unwrap(), pre_mini, "the backup is the pre-upgrade layout");

    let stdout = setup_zellij(&config_dir, &["--inject"]);
    assert!(stdout.contains("already has the rail"), "{stdout}");
    assert_eq!(fs::read_to_string(&layout_path).unwrap(), current, "re-inject is byte-identical");
}

/// `add_mini` only rewrites the exact pane it wrote, so a rail the user edited
/// keeps no mini. The CLI must say how many are left instead of reporting a
/// full upgrade: `sidebar-toggle` is dead in those tabs until a manual edit.
#[test]
fn setup_zellij_inject_reports_rails_left_without_a_mini() {
    let config_dir = isolated_zellij_config(FIXTURE_LAYOUT);
    let layout_path = config_dir.path().join("layouts/default.kdl");
    setup_zellij(&config_dir, &["--inject"]);
    let current = fs::read_to_string(&layout_path).unwrap();
    let resized = |layout: &str| layout.replace("pane size=32 borderless=true {", "pane size=40 borderless=true {");

    // One rail resized by hand: the other two upgrade, the edited one is reported.
    let one_edited =
        strip_mini(&current).replacen("pane size=32 borderless=true {", "pane size=40 borderless=true {", 1);
    fs::write(&layout_path, &one_edited).unwrap();
    let stdout = setup_zellij(&config_dir, &["--inject"]);
    assert!(stdout.contains("added the mini rail"), "{stdout}");
    assert!(stdout.contains("1 rail still has no mini rail"), "{stdout}");
    let upgraded = fs::read_to_string(&layout_path).unwrap();
    assert_eq!(upgraded.matches("mini true").count(), 2, "{upgraded}");
    assert!(upgraded.contains("pane size=40 borderless=true {"), "the edited rail stays as written: {upgraded}");

    // A trailing comment on one rail: the same partial upgrade, the same report.
    let commented = strip_mini(&current).replacen(
        "                plugin location=\"radar\"\n",
        "                plugin location=\"radar\" // alias\n",
        1,
    );
    fs::write(&layout_path, &commented).unwrap();
    let stdout = setup_zellij(&config_dir, &["--inject"]);
    assert!(stdout.contains("added the mini rail"), "{stdout}");
    assert!(stdout.contains("1 rail still has no mini rail"), "{stdout}");

    // Every rail resized: nothing is written, yet the report still names them.
    let all_resized = resized(&strip_mini(&current));
    fs::write(&layout_path, &all_resized).unwrap();
    let stdout = setup_zellij(&config_dir, &["--inject"]);
    assert!(stdout.contains("already has the rail"), "{stdout}");
    assert!(stdout.contains("3 rails still have no mini rail"), "{stdout}");
    assert_eq!(fs::read_to_string(&layout_path).unwrap(), all_resized, "nothing to write");
}

/// A layout `setup` wrote whole (no markers) stops matching
/// `setup_authored_layouts` the moment the user changes a line, so the old
/// message claimed a full upgrade with no mini anywhere in the file.
#[test]
fn setup_zellij_inject_reports_a_modified_whole_layout() {
    let config_dir = TempDir::new().unwrap();
    let layout_path = config_dir.path().join("layouts/default.kdl");
    setup_zellij(&config_dir, &["--inject"]);
    let whole = fs::read_to_string(&layout_path).unwrap();
    assert!(!whole.contains("zj-radar:wrap begin"), "a fresh config gets the whole layout");
    let edited = strip_mini(&whole).replacen("pane size=32 borderless=true {", "pane size=40 borderless=true {", 1);
    fs::write(&layout_path, &edited).unwrap();

    let stdout = setup_zellij(&config_dir, &["--inject"]);
    assert!(stdout.contains("already has the rail"), "{stdout}");
    assert!(stdout.contains("3 rails still have no mini rail"), "{stdout}");
    assert_eq!(fs::read_to_string(&layout_path).unwrap(), edited, "nothing to upgrade in an edited whole layout");
}

/// The count describes the file on disk: when the write fails there is nothing
/// to report on, only the error.
#[test]
fn setup_zellij_inject_reports_nothing_after_a_failed_write() {
    let config_dir = isolated_zellij_config(FIXTURE_LAYOUT);
    let layout_path = config_dir.path().join("layouts/default.kdl");
    setup_zellij(&config_dir, &["--inject"]);
    let current = fs::read_to_string(&layout_path).unwrap();
    let one_edited =
        strip_mini(&current).replacen("pane size=32 borderless=true {", "pane size=40 borderless=true {", 1);
    fs::write(&layout_path, &one_edited).unwrap();

    // A directory where the backup goes fails the write for any user, root
    // included (file permissions would not).
    let bak = config_dir.path().join("layouts/default.kdl.zj-radar.bak");
    fs::remove_file(&bak).unwrap();
    fs::create_dir(&bak).unwrap();
    let out = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--inject"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .failure()
        .get_output()
        .clone();

    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stderr.contains("write failed"), "{stderr}");
    assert!(!stdout.contains("no mini rail"), "a failed write has no layout to report on: {stdout}");
    assert_eq!(fs::read_to_string(&layout_path).unwrap(), one_edited, "the failed write leaves the file alone");
}

/// `--dry-run` reports on the layout it would write and writes nothing.
#[test]
fn setup_zellij_inject_dry_run_reports_on_the_layout_it_would_write() {
    let config_dir = isolated_zellij_config(FIXTURE_LAYOUT);
    let layout_path = config_dir.path().join("layouts/default.kdl");
    setup_zellij(&config_dir, &["--inject"]);
    let current = fs::read_to_string(&layout_path).unwrap();
    let one_edited =
        strip_mini(&current).replacen("pane size=32 borderless=true {", "pane size=40 borderless=true {", 1);
    fs::write(&layout_path, &one_edited).unwrap();

    let stdout = setup_zellij(&config_dir, &["--inject", "--dry-run"]);
    assert!(stdout.contains("would add the mini rail"), "{stdout}");
    assert!(stdout.contains("1 rail still has no mini rail"), "{stdout}");
    assert_eq!(fs::read_to_string(&layout_path).unwrap(), one_edited, "dry-run writes nothing");
}

/// A rail that names the wasm instead of the alias is recognised: no second
/// rail injected next to it, and its missing mini is reported.
#[test]
fn setup_zellij_inject_recognises_a_rail_that_names_the_wasm() {
    let layout = "layout {\n    default_tab_template {\n        pane size=32 borderless=true {\n            \
                  plugin location=\"file:~/.config/zellij/plugins/zj_radar.wasm\"\n        }\n        children\n    }\n}\n";
    let config_dir = isolated_zellij_config(layout);
    let stdout = setup_zellij(&config_dir, &["--inject"]);
    assert!(stdout.contains("already has the rail"), "{stdout}");
    assert!(stdout.contains("1 rail still has no mini rail"), "{stdout}");
    assert_eq!(fs::read_to_string(config_dir.path().join("layouts/default.kdl")).unwrap(), layout);
}

#[test]
fn setup_zellij_uninstall_removes_both_panes_and_a_pre_mini_injection() {
    for pre_mini in [false, true] {
        let config_dir = isolated_zellij_config(FIXTURE_LAYOUT);
        let layout_path = config_dir.path().join("layouts/default.kdl");
        setup_zellij(&config_dir, &["--inject"]);
        if pre_mini {
            let injected = fs::read_to_string(&layout_path).unwrap();
            fs::write(&layout_path, strip_mini(&injected)).unwrap();
        }
        setup_zellij(&config_dir, &["--uninstall"]);
        assert_eq!(fs::read_to_string(&layout_path).unwrap(), FIXTURE_LAYOUT, "pre_mini={pre_mini}");
    }
}

#[test]
fn setup_zellij_uninstall_deletes_a_pre_mini_authored_layout() {
    let config_dir = TempDir::new().unwrap();
    let layout_path = config_dir.path().join("layouts/default.kdl");
    setup_zellij(&config_dir, &["--inject"]);
    let whole = fs::read_to_string(&layout_path).unwrap();
    fs::write(&layout_path, strip_mini(&whole)).unwrap();
    setup_zellij(&config_dir, &["--uninstall"]);
    assert!(!layout_path.exists(), "the pre-mini full layout is setup-authored and must be deleted");
}

/// A layout setup created whole before the mini rail has no markers, yet it
/// is entirely ours: `--inject` upgrades it to the current one, then no-ops.
#[test]
fn setup_zellij_inject_upgrades_a_pre_mini_authored_layout() {
    let config_dir = TempDir::new().unwrap();
    let layout_path = config_dir.path().join("layouts/default.kdl");
    setup_zellij(&config_dir, &["--inject"]);
    let whole = fs::read_to_string(&layout_path).unwrap();
    fs::write(&layout_path, strip_mini(&whole)).unwrap();
    let stdout = setup_zellij(&config_dir, &["--inject"]);
    assert!(stdout.contains("added the mini rail"), "{stdout}");
    assert_eq!(fs::read_to_string(&layout_path).unwrap(), whole);
    let stdout = setup_zellij(&config_dir, &["--inject"]);
    assert!(stdout.contains("already has the rail"), "{stdout}");
}

// ── --uninstall deletes a layout that setup created whole ─────────────────────
// The no-layout `--inject` path writes `full_layout()` as a new file — no
// markers, but it references the `radar` alias. Uninstall strips the alias, so
// leaving that layout behind would strand the next plain Zellij launch on a
// dead alias. Byte-identical to the generated layout → it is entirely ours →
// delete it.

#[test]
fn setup_zellij_uninstall_deletes_layout_setup_created_whole() {
    let config_dir = TempDir::new().unwrap(); // no layouts/: --inject creates whole
    let layout_path = config_dir.path().join("layouts").join("default.kdl");

    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--inject"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success();
    assert!(layout_path.exists(), "prerequisite: --inject must have created the layout");

    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--uninstall"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success();

    assert!(
        !layout_path.exists(),
        "--uninstall must delete a layout setup created whole (it references the \
         removed `radar` alias and contains nothing of the user's)"
    );
}

// ── --uninstall never deletes an edited marker-less layout ────────────────────
// Same starting point, but the user has edited the file since: no longer
// byte-identical to `full_layout()`, so it must survive — with an advisory
// naming the dead `radar` alias reference instead of a silent strand.

#[test]
fn setup_zellij_uninstall_advises_on_edited_whole_created_layout() {
    let config_dir = TempDir::new().unwrap();
    let layout_path = config_dir.path().join("layouts").join("default.kdl");

    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--inject"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success();

    // A user edit: the file is no longer provably ours.
    let mut edited = fs::read_to_string(&layout_path).unwrap();
    edited.push_str("// my customization\n");
    fs::write(&layout_path, &edited).unwrap();

    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--uninstall"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success()
        .get_output()
        .clone();

    assert!(layout_path.exists(), "an edited layout is the user's — uninstall must never delete it");
    assert_eq!(
        fs::read_to_string(&layout_path).unwrap(),
        edited,
        "uninstall must not rewrite a marker-less layout it can't prove it authored"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("still references the removed `radar` alias"),
        "must warn about the stranded alias reference; stdout:\n{stdout}"
    );
}

// ── --dry-run prints what would change, writes nothing ─────────────────────────

#[test]
fn setup_zellij_inject_dry_run_prints_and_does_not_write() {
    let config_dir = isolated_zellij_config(FIXTURE_LAYOUT);
    let layout_path = config_dir.path().join("layouts").join("default.kdl");
    let bak_path = config_dir.path().join("layouts").join("default.kdl.zj-radar.bak");

    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--inject", "--dry-run"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    // Layout must be unchanged.
    let after = fs::read_to_string(&layout_path).unwrap();
    assert_eq!(after, FIXTURE_LAYOUT, "dry-run must not modify the layout");
    assert!(!bak_path.exists(), "dry-run must not create a .bak file");

    // stdout must show what would change.
    assert!(stdout.contains("dry-run"), "dry-run output must mention dry-run; stdout:\n{stdout}");
}

#[test]
fn setup_zellij_inject_honors_a_default_layout_given_as_a_path() {
    // Zellij takes a PATH in `default_layout`; the name rule appended a second
    // `.kdl` and offered to create a bogus `/…/mine.kdl.kdl`.
    let config_dir = TempDir::new().unwrap();
    let elsewhere = TempDir::new().unwrap();
    let mine = elsewhere.path().join("mine.kdl");
    fs::write(&mine, FIXTURE_LAYOUT).unwrap();
    fs::write(config_dir.path().join("config.kdl"), format!("default_layout \"{}\"\n", mine.display())).unwrap();
    let out = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--inject", "-y"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("mine.kdl.kdl"), "{stdout}");
    assert!(!elsewhere.path().join("mine.kdl.kdl").exists());
    assert!(fs::read_to_string(&mine).unwrap().contains("plugin location=\"radar\""), "rail injected into the path");
}

#[test]
fn setup_zellij_inject_honors_layout_dir_for_a_named_layout() {
    let config_dir = TempDir::new().unwrap();
    let layouts = TempDir::new().unwrap();
    let main = layouts.path().join("main.kdl");
    fs::write(&main, FIXTURE_LAYOUT).unwrap();
    fs::write(
        config_dir.path().join("config.kdl"),
        format!("layout_dir \"{}\"\ndefault_layout \"main\"\n", layouts.path().display()),
    )
    .unwrap();
    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--inject", "-y"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success();
    assert!(fs::read_to_string(&main).unwrap().contains("plugin location=\"radar\""));
    assert!(!config_dir.path().join("layouts").join("main.kdl").exists(), "no bogus file under the config dir");
}

/// Seed a `ZELLIJ_CONFIG_DIR` tempdir and a dummy (empty, but existing) wasm
/// file so `setup zellij --wasm <path>` passes the `src.is_file()` gate.
fn isolated_zellij_install_env() -> (TempDir, TempDir) {
    let config_dir = TempDir::new().unwrap();
    let wasm_dir = TempDir::new().unwrap();
    fs::write(wasm_dir.path().join("zj_radar.wasm"), b"").unwrap();
    (config_dir, wasm_dir)
}

// ── the grant hint is skipped when permissions.kdl already grants ─────────────
//
// The install path used to build its `ZellijFacts` with `permissions_text:
// None`, so `granted` was always None and the first-launch grant walkthrough
// printed even for a long-granted install. The probe now reads the same
// `zellij_permissions_path()` the doctor uses: the platform cache dir under
// HOME (macOS: `Library/Caches/org.Zellij-Contributors.Zellij`; elsewhere:
// `$XDG_CACHE_HOME/zellij`), keyed by the absolute wasm destination path.

#[test]
fn setup_zellij_skips_grant_hint_when_already_granted() {
    // Runs one full `setup zellij --wasm … --yes` install in an isolated HOME,
    // optionally pre-seeding a permissions.kdl granting the destination wasm.
    let install_stdout = |seed_grant: bool| -> String {
        let (config_dir, wasm_dir) = isolated_zellij_install_env();
        let wasm_path = wasm_dir.path().join("zj_radar.wasm");
        let home = TempDir::new().unwrap();
        if seed_grant {
            let wasm_dest = config_dir.path().join("plugins").join("zj_radar.wasm");
            #[cfg(target_os = "macos")]
            let perms_path = home.path().join("Library/Caches/org.Zellij-Contributors.Zellij/permissions.kdl");
            #[cfg(not(target_os = "macos"))]
            let perms_path = home.path().join(".cache/zellij/permissions.kdl");
            fs::create_dir_all(perms_path.parent().unwrap()).unwrap();
            // The FULL permission set the plugin requests — grant detection
            // deliberately rejects partial grants (a stale entry makes Zellij
            // re-prompt illegibly in the rail), so a partial seed here would
            // read as ungranted and the hint would print.
            fs::write(
                &perms_path,
                format!(
                    "\"{}\" {{\n    ReadApplicationState\n    ReadCliPipes\n    \
                     ChangeApplicationState\n    RunCommands\n    ReadPaneContents\n}}\n",
                    wasm_dest.display()
                ),
            )
            .unwrap();
        }
        let output = Command::cargo_bin("zj-radar")
            .unwrap()
            .args(["setup", "zellij", "--wasm", wasm_path.to_str().unwrap(), "--yes"])
            .env("ZELLIJ_CONFIG_DIR", config_dir.path())
            .env("HOME", home.path())
            .env("XDG_CACHE_HOME", home.path().join(".cache"))
            .assert()
            .success()
            .get_output()
            .clone();
        String::from_utf8_lossy(&output.stdout).into_owned()
    };

    let stdout = install_stdout(true);
    assert!(stdout.contains("zellij: installed"), "granted install must still install; stdout:\n{stdout}");
    assert!(
        !stdout.contains("press y to"),
        "already-granted install must not print the first-launch grant hint; stdout:\n{stdout}"
    );

    // With no prior grant, a consented install now pre-seeds the grant itself
    // (see the pre-seed section below) — the first-launch hint is superseded.
    let stdout = install_stdout(false);
    assert!(stdout.contains("pre-authorized"), "ungranted install must pre-seed the grant; stdout:\n{stdout}");
    assert!(
        !stdout.contains("press y to"),
        "a pre-seeded install needs no first-launch walkthrough; stdout:\n{stdout}"
    );
}

// ── Pre-seeded permission grant ───────────────────────────────────────────────
//
// Zellij reads permissions.kdl fresh on every plugin load, so a grant written
// at install time auto-resolves the sidebar's first-run prompt — the user
// never meets Zellij's native y/n overlay, which is illegible at rail width
// (zellij#4749) and used to present as a silently blank sidebar. The merge is
// conservative: foreign entries survive byte-for-byte, malformed files are
// refused (Zellij silently resets an unparseable file on its next write), and
// dry-run announces without writing.

/// Zellij's permissions.kdl under an isolated HOME, matching
/// `run::permissions_path_in` per OS (tests set both HOME and XDG_CACHE_HOME).
fn permissions_path(home: &std::path::Path) -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    return home.join("Library/Caches/org.Zellij-Contributors.Zellij/permissions.kdl");
    #[cfg(not(target_os = "macos"))]
    return home.join(".cache/zellij/permissions.kdl");
}

/// One consented full install (`--wasm … --yes`) against isolated config,
/// HOME, and cache dirs. Returns (config_dir, home, stdout, stderr).
fn preseed_install(seed_permissions: Option<&str>) -> (TempDir, TempDir, String, String) {
    let (config_dir, wasm_dir) = isolated_zellij_install_env();
    let wasm_path = wasm_dir.path().join("zj_radar.wasm");
    let home = TempDir::new().unwrap();
    if let Some(seed) = seed_permissions {
        let perms = permissions_path(home.path());
        fs::create_dir_all(perms.parent().unwrap()).unwrap();
        fs::write(&perms, seed).unwrap();
    }
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--wasm", wasm_path.to_str().unwrap(), "--yes"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join(".cache"))
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    (config_dir, home, stdout, stderr)
}

const FULL_PERMISSION_SET: [&str; 5] =
    ["ReadApplicationState", "ReadCliPipes", "ChangeApplicationState", "RunCommands", "ReadPaneContents"];

#[test]
fn setup_zellij_preseeds_grant_on_install() {
    let (config_dir, home, stdout, _) = preseed_install(None);
    let wasm_dest = config_dir.path().join("plugins").join("zj_radar.wasm");

    let perms =
        fs::read_to_string(permissions_path(home.path())).expect("a consented install must write permissions.kdl");
    assert!(
        perms.contains(&format!("\"{}\"", wasm_dest.display())),
        "grant must be keyed by the absolute wasm destination:\n{perms}"
    );
    for perm in FULL_PERMISSION_SET {
        assert!(perms.contains(perm), "{perm} missing from the grant:\n{perms}");
    }
    assert!(
        stdout.contains("pre-authorized"),
        "the install epilogue must say the grant was written; stdout:\n{stdout}"
    );

    // The doctor reads the same file the same way: grant must now be ok.
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--check"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join(".cache"))
        .assert()
        .get_output()
        .clone();
    let check = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(check.contains("ok grant"), "the doctor must see the pre-seeded grant; check output:\n{check}");
}

#[test]
fn setup_zellij_preseed_preserves_foreign_entries() {
    let foreign = "\"/nix/store/abc-room.wasm\" {\n    ReadApplicationState\n    ChangeApplicationState\n}\n";
    let (config_dir, home, _, _) = preseed_install(Some(foreign));
    let wasm_dest = config_dir.path().join("plugins").join("zj_radar.wasm");

    let perms = fs::read_to_string(permissions_path(home.path())).unwrap();
    assert!(perms.starts_with(foreign), "another plugin's grant must survive byte-for-byte:\n{perms}");
    assert!(perms.contains(&format!("\"{}\"", wasm_dest.display())), "our grant must be appended alongside:\n{perms}");
    // Zellij owns this file: the standard .bak restore point must exist.
    let bak = permissions_path(home.path()).with_file_name("permissions.kdl.zj-radar.bak");
    assert_eq!(
        fs::read_to_string(&bak).expect(".bak must be written before we touch the file"),
        foreign,
        ".bak must hold the pre-edit contents"
    );
}

#[test]
fn setup_zellij_preseed_refuses_malformed_permissions_kdl() {
    // An unclosed block: Zellij would treat the whole file as empty and reset
    // it on its next write — we must not touch it, and the install must still
    // succeed with the manual first-launch hint as the fallback.
    let malformed = "\"/a.wasm\" {\n    ReadApplicationState\n";
    let (_config_dir, home, stdout, stderr) = preseed_install(Some(malformed));

    assert_eq!(
        fs::read_to_string(permissions_path(home.path())).unwrap(),
        malformed,
        "a malformed permissions.kdl must be left untouched"
    );
    assert!(stderr.contains("refusing"), "the refusal must be reported; stderr:\n{stderr}");
    assert!(stdout.contains("press y to"), "with no pre-seed the first-launch hint must return; stdout:\n{stdout}");
}

#[test]
fn setup_zellij_dry_run_would_preseed_but_writes_nothing() {
    let (config_dir, wasm_dir) = isolated_zellij_install_env();
    let wasm_path = wasm_dir.path().join("zj_radar.wasm");
    let home = TempDir::new().unwrap();
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--wasm", wasm_path.to_str().unwrap(), "--dry-run"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join(".cache"))
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(stdout.contains("would pre-authorize"), "dry-run must announce the pre-seed; stdout:\n{stdout}");
    assert!(!permissions_path(home.path()).exists(), "dry-run must not create permissions.kdl");
}

#[test]
fn setup_zellij_preseed_skipped_without_tty_falls_back_to_hint() {
    // `confirm()` takes the safe "no" when stdin is not a tty (as in tests),
    // so a promptable step can no longer be consented to non-interactively.
    // Prime the config with a consented install, drop the grant, then re-run
    // WITHOUT --yes: the Unchanged arm still offers the pre-seed, the no-tty
    // confirm skips it out loud, and the first-launch hint returns.
    let (config_dir, wasm_dir) = isolated_zellij_install_env();
    let wasm_path = wasm_dir.path().join("zj_radar.wasm");
    let home = TempDir::new().unwrap();
    let run = |yes: bool| {
        let mut args = vec!["setup", "zellij", "--wasm", wasm_path.to_str().unwrap()];
        if yes {
            args.push("--yes");
        }
        Command::cargo_bin("zj-radar")
            .unwrap()
            .args(&args)
            .env("ZELLIJ_CONFIG_DIR", config_dir.path())
            .env("HOME", home.path())
            .env("XDG_CACHE_HOME", home.path().join(".cache"))
            .assert()
            .success()
            .get_output()
            .clone()
    };
    run(true); // consented install: config written, grant pre-seeded
    fs::remove_file(permissions_path(home.path())).unwrap(); // grant lost again

    let output = run(false); // no --yes, piped stdin: nothing can be consented
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(!permissions_path(home.path()).exists(), "a skipped pre-authorization must write nothing");
    assert!(
        stdout.contains("no tty — re-run with -y"),
        "the non-tty skip must say why and name the -y escape; stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("press y to"),
        "skipping the pre-seed must fall back to the first-launch hint; stdout:\n{stdout}"
    );
}

// ── Test: a symlinked layout is managed — never rewritten, never deleted ──────
// `path_is_managed` (the Nix / home-manager symlink test) used to gate only
// config.kdl; the layout was written via atomic rename, which silently replaces
// a symlink with a regular file — the next `home-manager switch` reverts it and
// the rail "mysteriously vanishes". Both inject and uninstall must refuse.

#[test]
#[cfg(unix)]
fn setup_zellij_never_writes_a_symlinked_layout() {
    let config_dir = isolated_zellij_config(FIXTURE_LAYOUT);
    let layouts = config_dir.path().join("layouts");
    // Move the real layout aside and symlink default.kdl at it (home-manager style).
    let real = config_dir.path().join("hm-source.kdl");
    fs::rename(layouts.join("default.kdl"), &real).unwrap();
    std::os::unix::fs::symlink(&real, layouts.join("default.kdl")).unwrap();

    // Inject: refusal + snippet, symlink and target intact.
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--inject"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success() // guidance, not a failure — mirrors the managed-config path
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(stderr.contains("symlink"), "must say WHY it refused (symlink / Nix); stderr:\n{stderr}");
    assert!(
        stdout.contains("Add the sidebar"),
        "must still print the tailored snippet for the Nix config; stdout:\n{stdout}"
    );
    let link = layouts.join("default.kdl");
    assert!(
        fs::symlink_metadata(&link).unwrap().file_type().is_symlink(),
        "--inject must not replace the symlink with a regular file"
    );
    assert_eq!(fs::read_to_string(&real).unwrap(), FIXTURE_LAYOUT, "the symlink target must be untouched");

    // Uninstall: same guard (no config.kdl → the layout-only uninstall path).
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--uninstall"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(stderr.contains("symlink"), "--uninstall must refuse a symlinked layout out loud; stderr:\n{stderr}");
    assert!(
        fs::symlink_metadata(&link).unwrap().file_type().is_symlink(),
        "--uninstall must leave the symlink in place"
    );
    assert_eq!(
        fs::read_to_string(&real).unwrap(),
        FIXTURE_LAYOUT,
        "--uninstall must leave the symlink target untouched"
    );
}

// ── Test: an unreadable layout is not an absent one ───────────────────────────
// A layout that exists but fails read_to_string (EACCES, non-UTF8) used to take
// the "No layout at {path} — create it?" path: consenting replaced the user's
// file under a prompt that lied. The error kinds must be discriminated —
// NotFound creates, everything else refuses and leaves the file alone.

#[test]
fn setup_zellij_refuses_an_unreadable_layout_instead_of_recreating_it() {
    let non_utf8: &[u8] = &[0xFF, 0xFE, b'l', b'a', b'y', b'o', b'u', b't'];
    let config_dir = TempDir::new().unwrap();
    let layouts = config_dir.path().join("layouts");
    fs::create_dir_all(&layouts).unwrap();
    let layout_path = layouts.join("default.kdl");
    fs::write(&layout_path, non_utf8).unwrap();

    // Inject: must NOT take the create path (which would replace the file).
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--inject"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .failure() // a real error, unlike the guidance-only symlink refusal
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("could not read layout"),
        "must report the read error, not claim the layout is absent; stderr:\n{stderr}"
    );
    assert_eq!(fs::read(&layout_path).unwrap(), non_utf8, "--inject must leave an unreadable layout byte-identical");

    // Uninstall: same discrimination (unreadable != nothing to uninstall).
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--uninstall"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(stderr.contains("could not read layout"), "--uninstall must report the read error; stderr:\n{stderr}");
    assert_eq!(fs::read(&layout_path).unwrap(), non_utf8, "--uninstall must leave an unreadable layout byte-identical");
}

// ── Test: --dry-run --download never touches the network ─────────────────────
// --dry-run is documented "Show what would change; write nothing", yet it used
// to fetch the release wasm first — and hard-fail offline. The config splice
// needs only the destination path, so the fetch must be skipped and announced.

#[test]
fn setup_zellij_dry_run_download_skips_the_fetch_and_writes_nothing() {
    let config_dir = TempDir::new().unwrap();
    let emptybin = TempDir::new().unwrap(); // no curl/wget: a fetch attempt would fail

    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--download", "--dry-run"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .env("PATH", emptybin.path()) // offline-equivalent: success proves no fetch
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(stdout.contains("would download"), "dry-run must announce the download it skipped; stdout:\n{stdout}");
    assert!(stdout.contains("dry-run"), "the config preview must still print; stdout:\n{stdout}");
    assert!(
        !config_dir.path().join("config.kdl").exists() && !config_dir.path().join("plugins").exists(),
        "--dry-run --download must write nothing"
    );
}

// ── Test: --check conflicts with --uninstall at the CLI boundary ──────────────
// `setup --check --uninstall` used to silently run the doctor and never
// uninstall — reading as "uninstalled". Clap now hard-errors.

#[test]
fn setup_check_conflicts_with_uninstall() {
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "--check", "--uninstall"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("cannot be used with"),
        "clap must reject --check --uninstall as a hard conflict; stderr:\n{stderr}"
    );
}

// ── Test: the doctor flags a ZELLIJ_CONFIG_FILE override ─────────────────────
// Zellij's config precedence puts $ZELLIJ_CONFIG_FILE above --config-dir
// resolution; setup's resolver honors ZELLIJ_CONFIG_DIR/XDG but not this var,
// so setup can edit a config.kdl Zellij never reads while --check reports
// healthy. The doctor must name both paths when they diverge.

#[test]
fn check_warns_when_zellij_config_file_points_elsewhere() {
    let config_dir = TempDir::new().unwrap();
    let resolved = config_dir.path().join("config.kdl");

    let check = |config_file: Option<&std::path::Path>| -> String {
        let mut cmd = Command::cargo_bin("zj-radar").unwrap();
        cmd.args(["setup", "zellij", "--check"])
            .env("ZELLIJ_CONFIG_DIR", config_dir.path())
            .env_remove("ZELLIJ_CONFIG_FILE");
        if let Some(f) = config_file {
            cmd.env("ZELLIJ_CONFIG_FILE", f);
        }
        // No `.success()`: the empty fixture has Missing items by design.
        let output = cmd.output().unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    };

    // Override pointing elsewhere: warn, naming both paths.
    let elsewhere = config_dir.path().join("other.kdl");
    let out = check(Some(&elsewhere));
    assert!(
        out.contains("warn config env") && out.contains("$ZELLIJ_CONFIG_FILE"),
        "a diverging ZELLIJ_CONFIG_FILE must produce the config env warning; got:\n{out}"
    );
    assert!(
        out.contains(elsewhere.to_str().unwrap()) && out.contains(resolved.to_str().unwrap()),
        "the warning must name both the override and the resolved path; got:\n{out}"
    );

    // Unset, or pointing at the resolved path: no warning.
    let out = check(None);
    assert!(!out.contains("config env"), "no override -> no config env item; got:\n{out}");
    let out = check(Some(&resolved));
    assert!(!out.contains("config env"), "an override that matches the resolved path is consistent; got:\n{out}");
}

// ── Review fixes: cross-version uninstall, grant recovery path, dry-run copy ──

#[test]
fn setup_zellij_uninstall_deletes_a_v013_authored_layout() {
    // v0.1.3's create_full_layout named the starter tab "shell"; 0.1.4 removed
    // the name. Uninstall must still recognize the OLD byte-shape as
    // setup-authored, or every 0.1.3-created layout is stranded on a dead
    // alias after the alias strip.
    let (config_dir, wasm_dir) = isolated_zellij_install_env();
    let wasm_path = wasm_dir.path().join("zj_radar.wasm");
    let home = TempDir::new().unwrap();
    let run = |args: &[&str]| {
        Command::cargo_bin("zj-radar")
            .unwrap()
            .args(args)
            .env("ZELLIJ_CONFIG_DIR", config_dir.path())
            .env("HOME", home.path())
            .env("XDG_CACHE_HOME", home.path().join(".cache"))
            .assert()
            .success();
    };
    run(&["setup", "zellij", "--wasm", wasm_path.to_str().unwrap(), "--inject", "--yes"]);
    let layout_path = config_dir.path().join("layouts/default.kdl");
    let current = fs::read_to_string(&layout_path).unwrap();
    let legacy = strip_mini(&current).replace("    tab focus=true {", "    tab name=\"shell\" focus=true {");
    assert_ne!(current, legacy, "fixture must actually differ (tab line drifted?)");
    fs::write(&layout_path, &legacy).unwrap();

    run(&["setup", "zellij", "--uninstall", "--yes"]);
    assert!(!layout_path.exists(), "a layout authored whole by v0.1.3 setup must still be deleted on uninstall");
}

#[test]
fn setup_zellij_yes_preseeds_when_wasm_already_installed() {
    // The doctor's grant remedy says to re-run setup: with the wasm already at
    // the stable path, `setup zellij -y` (no wasm source) must be able to
    // pre-authorize — the grant is keyed by the destination path, which
    // exists. This used to dead-end in RefuseNoWasm/LayoutOnlyInstall.
    let (config_dir, wasm_dir) = isolated_zellij_install_env();
    let wasm_path = wasm_dir.path().join("zj_radar.wasm");
    let home = TempDir::new().unwrap();
    let run = |args: &[&str]| {
        Command::cargo_bin("zj-radar")
            .unwrap()
            .args(args)
            .env("ZELLIJ_CONFIG_DIR", config_dir.path())
            .env("HOME", home.path())
            .env("XDG_CACHE_HOME", home.path().join(".cache"))
            .assert()
            .success();
    };
    run(&["setup", "zellij", "--wasm", wasm_path.to_str().unwrap(), "--inject", "--yes"]);
    fs::remove_file(permissions_path(home.path())).unwrap(); // grant lost (e.g. answered n once)

    run(&["setup", "zellij", "--yes"]); // the doctor's advertised recovery
    assert!(
        permissions_path(home.path()).exists(),
        "wasm-less re-run must restore the grant when the wasm is already installed"
    );
}

#[test]
fn setup_zellij_dry_run_never_contradicts_itself_about_the_grant() {
    // Unchanged arm + --dry-run: "would pre-authorize" and "permissions not
    // pre-authorized — the rail will look BLANK" must not both print.
    let (config_dir, wasm_dir) = isolated_zellij_install_env();
    let wasm_path = wasm_dir.path().join("zj_radar.wasm");
    let home = TempDir::new().unwrap();
    let run = |extra: &[&str]| {
        let mut args = vec!["setup", "zellij", "--wasm", wasm_path.to_str().unwrap(), "--yes"];
        args.extend_from_slice(extra);
        Command::cargo_bin("zj-radar")
            .unwrap()
            .args(&args)
            .env("ZELLIJ_CONFIG_DIR", config_dir.path())
            .env("HOME", home.path())
            .env("XDG_CACHE_HOME", home.path().join(".cache"))
            .assert()
            .success()
            .get_output()
            .clone()
    };
    run(&[]); // real install -> config now up to date
    fs::remove_file(permissions_path(home.path())).unwrap(); // grant gone again
    let output = run(&["--dry-run"]); // Unchanged arm under dry-run
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(stdout.contains("would pre-authorize"), "dry-run must announce the pre-seed; stdout:\n{stdout}");
    assert!(
        !stdout.contains("permissions not pre-authorized"),
        "the BLANK-rail warning contradicts the would-pre-authorize line; stdout:\n{stdout}"
    );
}

// ── re-running setup zellij with a new wasm refreshes the installed file ────
//
// The alias in config.kdl is already managed on every machine that ran setup
// once, so the second run lands in the "config already up to date" arm. That
// arm must still install a NEW wasm — `zj-radar update` relies on exactly this
// re-run to move the sidebar, and until it did, "re-run setup zellij
// --download" upgraded nothing.

fn setup_zellij_wasm(config_dir: &TempDir, wasm: &std::path::Path) -> assert_cmd::assert::Assert {
    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--wasm"])
        .arg(wasm)
        .arg("--yes")
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        // Consented installs pre-seed permissions.kdl: keep it out of the
        // real user cache.
        .env("HOME", config_dir.path().join("home"))
        .env("XDG_CACHE_HOME", config_dir.path().join("home/.cache"))
        .assert()
}

#[test]
fn setup_zellij_rerun_with_a_different_wasm_replaces_the_installed_one() {
    let config_dir = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let a = src.path().join("a.wasm");
    let b = src.path().join("b.wasm");
    fs::write(&a, b"\0asm-A").unwrap();
    fs::write(&b, b"\0asm-B").unwrap();
    let dest = config_dir.path().join("plugins").join("zj_radar.wasm");

    setup_zellij_wasm(&config_dir, &a).success();
    assert_eq!(fs::read(&dest).unwrap(), b"\0asm-A");

    let out = setup_zellij_wasm(&config_dir, &b).success().get_output().clone();
    assert_eq!(fs::read(&dest).unwrap(), b"\0asm-B", "second run must install the new wasm");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("wasm updated"), "should say the wasm was refreshed: {stdout}");
}

#[test]
fn setup_zellij_rerun_with_the_same_wasm_does_not_rewrite() {
    let config_dir = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let a = src.path().join("a.wasm");
    fs::write(&a, b"\0asm-A").unwrap();
    let dest = config_dir.path().join("plugins").join("zj_radar.wasm");

    setup_zellij_wasm(&config_dir, &a).success();
    let before = fs::metadata(&dest).unwrap().modified().unwrap();
    let out = setup_zellij_wasm(&config_dir, &a).success().get_output().clone();
    assert_eq!(fs::metadata(&dest).unwrap().modified().unwrap(), before, "identical bytes must not rewrite");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("wasm updated"), "{stdout}");
}

#[test]
fn setup_zellij_refreshes_the_wasm_behind_a_hand_owned_alias() {
    // #96: a hand-owned `radar` alias at the stable wasm path is a supported
    // setup (docs/install.md "Manual setup"). `update` re-runs this install,
    // which used to refuse it as a conflict and never refresh the wasm.
    let config_dir = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let b = src.path().join("b.wasm");
    fs::write(&b, b"\0asm-B").unwrap();
    let dest = config_dir.path().join("plugins").join("zj_radar.wasm");
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::write(&dest, b"\0asm-A").unwrap();
    let config = config_dir.path().join("config.kdl");
    let text = format!(
        "plugins {{\n    radar location=\"file:{}\" {{\n        naming \"managed\"\n        custom \"mine\"\n    }}\n}}\n",
        dest.display()
    );
    fs::write(&config, &text).unwrap();

    let out = setup_zellij_wasm(&config_dir, &b).success().get_output().clone();
    assert_eq!(fs::read(&dest).unwrap(), b"\0asm-B", "the wasm must be refreshed");
    assert_eq!(fs::read_to_string(&config).unwrap(), text, "the user's alias is left as it is");
    assert!(!config_dir.path().join("config.kdl.zj-radar.bak").exists(), "no config write, so no backup");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("wasm updated"), "{stdout}");
}

#[test]
fn setup_zellij_hand_owned_alias_elsewhere_still_conflicts() {
    let config_dir = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let b = src.path().join("b.wasm");
    fs::write(&b, b"\0asm-B").unwrap();
    let config = config_dir.path().join("config.kdl");
    let text = "plugins {\n    radar location=\"file:/opt/other/zj_radar.wasm\"\n}\n";
    fs::write(&config, text).unwrap();

    let out = setup_zellij_wasm(&config_dir, &b).failure().get_output().clone();
    assert_eq!(fs::read_to_string(&config).unwrap(), text);
    assert!(!config_dir.path().join("plugins").join("zj_radar.wasm").exists(), "nothing installed");
    assert!(String::from_utf8_lossy(&out.stderr).contains("unmanaged `radar` plugin alias"));
}

#[cfg(unix)]
#[test]
fn setup_zellij_rerun_never_writes_through_a_symlinked_wasm() {
    // home-manager installs the wasm as a symlink into /nix/store. Writing
    // through (or renaming over) it would either fail or replace the link with
    // a file the next `home-manager switch` reverts — same guard as config.kdl.
    let config_dir = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let a = src.path().join("a.wasm");
    let b = src.path().join("b.wasm");
    fs::write(&a, b"\0asm-A").unwrap();
    fs::write(&b, b"\0asm-B").unwrap();
    setup_zellij_wasm(&config_dir, &a).success();

    // Swap the installed file for a symlink to a "store" copy.
    let dest = config_dir.path().join("plugins").join("zj_radar.wasm");
    let store = src.path().join("store.wasm");
    fs::write(&store, b"\0asm-STORE").unwrap();
    fs::remove_file(&dest).unwrap();
    std::os::unix::fs::symlink(&store, &dest).unwrap();

    let out = setup_zellij_wasm(&config_dir, &b).success().get_output().clone();
    assert!(fs::symlink_metadata(&dest).unwrap().file_type().is_symlink(), "symlink must survive");
    assert_eq!(fs::read(&store).unwrap(), b"\0asm-STORE", "store target must be untouched");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Nix"), "should explain why the wasm was skipped: {stderr}");
}

#[cfg(unix)]
#[test]
fn setup_zellij_refreshes_the_wasm_beside_a_symlinked_config() {
    // home-manager owns config.kdl (a symlink), but the wasm at the stable
    // path is a plain file (curl-installed): the managed config must not stop
    // the wasm refresh, or `update` reads "stale" forever and promises a
    // restart that loads nothing new.
    let config_dir = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let managed = src.path().join("hm-config.kdl");
    fs::write(&managed, "// generated by home-manager\n").unwrap();
    std::os::unix::fs::symlink(&managed, config_dir.path().join("config.kdl")).unwrap();
    let dest = config_dir.path().join("plugins").join("zj_radar.wasm");
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::write(&dest, b"\0asm-OLD").unwrap();
    let b = src.path().join("b.wasm");
    fs::write(&b, b"\0asm-B").unwrap();

    let out = setup_zellij_wasm(&config_dir, &b).success().get_output().clone();
    assert_eq!(fs::read(&dest).unwrap(), b"\0asm-B", "the wasm is not the managed file");
    assert_eq!(fs::read_to_string(&managed).unwrap(), "// generated by home-manager\n", "config untouched");
    assert!(String::from_utf8_lossy(&out.stdout).contains("wasm updated"));

    // A symlinked wasm beside the symlinked config stays Nix's to move.
    let store = src.path().join("store.wasm");
    fs::write(&store, b"\0asm-STORE").unwrap();
    fs::remove_file(&dest).unwrap();
    std::os::unix::fs::symlink(&store, &dest).unwrap();
    let out = setup_zellij_wasm(&config_dir, &b).success().get_output().clone();
    assert_eq!(fs::read(&store).unwrap(), b"\0asm-STORE");
    assert!(fs::symlink_metadata(&dest).unwrap().file_type().is_symlink());
    assert!(String::from_utf8_lossy(&out.stderr).contains("wasm at"), "should name the skipped wasm");
}

#[cfg(unix)]
#[test]
fn setup_zellij_yes_preseeds_the_grant_beside_a_symlinked_config() {
    // #82: on home-manager, `--check` sends users to `setup zellij -y` for the
    // grant, but the managed-config arm used to return before the preseed.
    // permissions.kdl is Zellij's cache, not the managed file, so seed it.
    let config_dir = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let managed = src.path().join("hm-config.kdl");
    fs::write(&managed, "// generated by home-manager\n").unwrap();
    std::os::unix::fs::symlink(&managed, config_dir.path().join("config.kdl")).unwrap();
    // The home-manager snippet links the wasm to the stable path.
    let store = src.path().join("store.wasm");
    fs::write(&store, b"\0asm-STORE").unwrap();
    let dest = config_dir.path().join("plugins").join("zj_radar.wasm");
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&store, &dest).unwrap();
    let home = TempDir::new().unwrap();

    let out = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--yes"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join(".cache"))
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(String::from_utf8_lossy(&out.stderr).contains("managed by Nix / home-manager"));
    assert_eq!(fs::read_to_string(&managed).unwrap(), "// generated by home-manager\n", "config untouched");
    let perms = fs::read_to_string(permissions_path(home.path())).expect("-y must write permissions.kdl");
    assert!(perms.contains(&dest.display().to_string()), "grant keyed by the stable path:\n{perms}");
    assert!(fs::symlink_metadata(&dest).unwrap().file_type().is_symlink(), "managed wasm untouched");
}

// ── `setup shell`: OSC 133 prompt marks in the user's rc file ────────────────
// Every test runs under a temp HOME/ZDOTDIR: never the caller's rc files.

/// `zj-radar setup <args>` with HOME and ZDOTDIR pointed at `home`.
fn setup_shell(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::cargo_bin("zj-radar")
        .unwrap()
        .arg("setup")
        .args(args)
        .env("HOME", home)
        .env("ZDOTDIR", home.join("zdot"))
        .env_remove("SHELL")
        .output()
        .unwrap()
}

#[test]
fn setup_shell_twice_leaves_one_block_and_uninstall_restores_the_bytes() {
    let home = TempDir::new().unwrap();
    let bashrc = home.path().join(".bashrc");
    let zshrc = home.path().join("zdot/.zshrc");
    let original = "alias ll='ls -l'\nexport EDITOR=vim"; // no final newline
    fs::write(&bashrc, original).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&bashrc, fs::Permissions::from_mode(0o600)).unwrap();
    let mode = || fs::metadata(&bashrc).unwrap().permissions().mode() & 0o777;

    let check = |want_ok: bool| {
        let out = setup_shell(home.path(), &["shell", "bash", "zsh", "--check"]);
        assert_eq!(out.status.success(), want_ok, "{}", String::from_utf8_lossy(&out.stdout));
    };
    check(false);
    for _ in 0..2 {
        assert!(setup_shell(home.path(), &["shell", "bash", "zsh", "-y"]).status.success());
    }
    for rc in [&bashrc, &zshrc] {
        let text = fs::read_to_string(rc).unwrap();
        assert_eq!(text.matches("# >>> zj-radar shell marks >>>").count(), 1, "{rc:?}:\n{text}");
    }
    assert!(fs::read_to_string(&bashrc).unwrap().starts_with(&format!("{original}\n# >>> zj-radar")));
    assert_eq!(mode(), 0o600, "a private rc stays private");
    check(true);

    assert!(setup_shell(home.path(), &["shell", "bash", "zsh", "--uninstall", "-y"]).status.success());
    assert_eq!(fs::read(&bashrc).unwrap(), original.as_bytes(), "the rest of .bashrc is byte-identical");
    assert_eq!(mode(), 0o600);
    assert_eq!(fs::read_to_string(&zshrc).unwrap(), "", "a .zshrc setup created ends empty");
    check(false);
}

#[test]
fn setup_shell_dry_run_and_no_tty_write_nothing() {
    let home = TempDir::new().unwrap();
    let bashrc = home.path().join(".bashrc");
    fs::write(&bashrc, "x\n").unwrap();
    let out = setup_shell(home.path(), &["shell", "bash", "--dry-run"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("]133;C;cmdline="), "dry-run shows the block");
    // No -y and stdin is not a tty: the safe "no".
    let out = setup_shell(home.path(), &["shell", "bash"]);
    assert!(out.status.success() && String::from_utf8_lossy(&out.stdout).contains("skipped"));
    assert_eq!(fs::read_to_string(&bashrc).unwrap(), "x\n");
}

#[test]
fn setup_shell_leaves_an_rc_that_writes_its_own_marks_alone() {
    let home = TempDir::new().unwrap();
    let bashrc = home.path().join(".bashrc");
    let own = "PS1='\\[\\e]133;A\\a\\]$ '\n";
    fs::write(&bashrc, own).unwrap();
    let out = setup_shell(home.path(), &["shell", "bash", "-y"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("already writes OSC 133 marks (line 1)"));
    assert_eq!(fs::read_to_string(&bashrc).unwrap(), own);
    assert!(setup_shell(home.path(), &["shell", "bash", "--check"]).status.success());
}

#[test]
fn setup_shell_refuses_an_unknown_shell_and_a_bare_shell_without_shell_var() {
    let home = TempDir::new().unwrap();
    for args in [&["shell", "fish"][..], &["zellij", "shell"], &["shell"]] {
        let out = setup_shell(home.path(), args);
        assert!(!out.status.success(), "{args:?} must fail");
    }
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0, "a refusal writes nothing");
}

/// The rc file `setup shell` writes for `shell` under [`setup_shell`]'s env.
fn rc_file(home: &std::path::Path, shell: &str) -> std::path::PathBuf {
    if shell == "bash" {
        home.join(".bashrc")
    } else {
        home.join("zdot/.zshrc")
    }
}

/// `cmd` with only a temp HOME/ZDOTDIR and PATH: no rc, history file or
/// variable of the caller's reaches the shell (ICH-471).
fn isolated(cmd: &mut std::process::Command, home: &std::path::Path) {
    cmd.env_clear()
        .env("HOME", home)
        .env("ZDOTDIR", home.join("zdot"))
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("TERM", "dumb");
}

/// The whole terminal output of an interactive `shell` that reads `script`
/// from a pipe (it still runs its prompt hooks). Its rc file is `rc_before`
/// followed by the block `setup shell` adds. `None` when `shell` is not
/// installed.
fn run_marked_shell(shell: &str, inside_zellij: bool, rc_before: &str, script: &str) -> Option<String> {
    use std::io::Write as _;
    let home = TempDir::new().unwrap();
    let mut probe = std::process::Command::new(shell);
    isolated(probe.args(["-c", "exit 0"]), home.path());
    probe.output().ok()?;
    let rc = rc_file(home.path(), shell);
    fs::create_dir_all(rc.parent().unwrap()).unwrap();
    fs::write(&rc, rc_before).unwrap();
    assert!(setup_shell(home.path(), &["shell", shell, "-y"]).status.success());
    let log = home.path().join("out");
    let out = fs::File::create(&log).unwrap();
    let mut cmd = std::process::Command::new(shell);
    isolated(cmd.arg("-i"), home.path());
    cmd.stdin(std::process::Stdio::piped()).stdout(out.try_clone().unwrap()).stderr(out);
    if inside_zellij {
        cmd.env("ZELLIJ_PANE_ID", "7");
    }
    let mut child = cmd.spawn().unwrap();
    child.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
    assert!(child.wait().unwrap().success());
    Some(fs::read_to_string(&log).unwrap())
}

/// The OSC 133 marks in `out`, without the `ESC ] 133 ;` prefix and the BEL.
fn marks(out: &str) -> Vec<&str> {
    out.split("\x1b]133;").skip(1).map(|m| m.split('\x07').next().unwrap()).collect()
}

#[test]
fn bash_and_zsh_blocks_mark_each_command_with_its_exit_code() {
    // A failure, an empty line (no command, so no mark), a success, then exit.
    let script = "false\n\ntrue\nexit\n";
    // bash quotes the line with `printf %q`, zsh with `${(qqqq)…}`: Zellij decodes both.
    for (shell, quote) in [("bash", "{}"), ("zsh", "$'{}'")] {
        let Some(out) = run_marked_shell(shell, true, "", script) else {
            eprintln!("{shell} not installed — skipping");
            continue;
        };
        let marks = marks(&out);
        let c = |cmd: &str| format!("C;cmdline={}", quote.replace("{}", cmd));
        let want = ["A", &c("false"), "D;1", "A", "A", &c("true"), "D;0", "A"];
        assert!(marks.starts_with(&want), "{shell}: {marks:?}\n{out:?}");
        assert_eq!(marks.iter().filter(|m| m.starts_with('D')).count(), 2, "{shell}: {marks:?}");

        let outside = run_marked_shell(shell, false, "", script).unwrap();
        assert!(!outside.contains("]133;"), "{shell} outside Zellij writes no marks: {outside:?}");
    }
}

#[test]
fn re_sourcing_the_rc_keeps_every_mark_once() {
    // An rc that assigns PROMPT_COMMAND drops our call when sourced again.
    for (shell, rc_before, rc) in
        [("bash", "PROMPT_COMMAND='true'\n", "~/.bashrc"), ("zsh", "PS1='$ '\n", "$ZDOTDIR/.zshrc")]
    {
        let script = format!("false\nsource {rc}\nfalse\nexit 0\n");
        let Some(out) = run_marked_shell(shell, true, rc_before, &script) else {
            eprintln!("{shell} not installed — skipping");
            continue;
        };
        let marks = marks(&out);
        let ends: Vec<&str> = marks.iter().copied().filter(|m| m.starts_with('D')).collect();
        assert_eq!(ends, ["D;1", "D;0", "D;1"], "{shell}: {marks:?}");
        // One `A` per prompt (first, then after each of the three commands).
        assert_eq!(marks.iter().filter(|m| **m == "A").count(), 4, "{shell}: {marks:?}");
    }
}

#[test]
fn a_child_bash_without_the_rc_prints_no_error() {
    // A child bash inherits an exported PROMPT_COMMAND that names our function,
    // but not the function.
    let rc_before = "export PROMPT_COMMAND='true'\n";
    let Some(out) = run_marked_shell("bash", true, rc_before, "bash --norc -i\necho child\nexit\nexit\n") else {
        eprintln!("bash not installed — skipping");
        return;
    };
    assert!(out.contains("child") && !out.contains("not found"), "{out:?}");
}
