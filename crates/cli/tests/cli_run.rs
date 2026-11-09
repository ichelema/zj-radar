//! CI-safe end-to-end runs of `zj-radar run` under an isolated HOME/XDG, so
//! they never touch the developer's real config. The `--print-cmd` smoke test
//! never execs Zellij; the others put a fake `zellij` alone on PATH, so the
//! host's real Zellij is unreachable. The precise attach-vs-create shape is
//! covered by `plan_run` unit tests.
use assert_cmd::Command;
use tempfile::tempdir;

#[test]
fn run_print_cmd_emits_session_scoped_zellij_invocation() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let mut cmd = Command::cargo_bin("zj-radar").unwrap();
    cmd.env("HOME", home.path())
        .env("XDG_DATA_HOME", data.path()) // dirs::data_dir() on Linux
        .arg("run")
        .arg("proj")
        .arg("--print-cmd");
    let out = cmd.assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    // Robust across attach vs create branches (depends on host sessions): both
    // print a `zellij` command referencing the resolved session name.
    assert!(stdout.contains("zellij "), "stdout:\n{stdout}");
    assert!(stdout.contains("proj"), "stdout:\n{stdout}");
}

/// Where `run` pre-seeds the grant under the sandbox HOME (Linux, macOS).
#[cfg(unix)]
const PERMISSIONS_KDL: [&str; 2] =
    [".cache/zellij/permissions.kdl", "Library/Caches/org.Zellij-Contributors.Zellij/permissions.kdl"];

/// `zj-radar` with HOME, cache and data dir under `home`, and a fake `zellij`
/// whose `list-sessions` prints `sessions` and whose launch exits 0.
#[cfg(unix)]
fn sandboxed(home: &std::path::Path, sessions: &str) -> Command {
    use std::os::unix::fs::PermissionsExt;
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let zellij = bin.join("zellij");
    std::fs::write(&zellij, format!("#!/bin/sh\n[ \"$1\" = list-sessions ] && echo '{sessions}'\nexit 0\n")).unwrap();
    std::fs::set_permissions(&zellij, std::fs::Permissions::from_mode(0o755)).unwrap();
    // A stand-in wasm: without an embedded one, `run` would download first.
    let wasm = home.join("stub.wasm");
    std::fs::write(&wasm, b"\0asm").unwrap();
    let mut cmd = Command::cargo_bin("zj-radar").unwrap();
    cmd.env("HOME", home)
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("ZJ_RADAR_DATA_DIR", home.join("data"))
        .env("ZJ_RADAR_WASM", &wasm)
        .env("PATH", &bin)
        .env_remove("ZELLIJ")
        .env_remove("ZELLIJ_CONFIG_FILE");
    cmd
}

/// ICH-436: a run refused at the foreign-session consent must not have
/// pre-seeded the grant. The fake `zellij` lists a session named like the
/// target, which `run` never stamped, so the non-tty path refuses.
#[cfg(unix)]
#[test]
fn run_refused_for_a_foreign_session_leaves_permissions_untouched() {
    let home = tempdir().unwrap();
    let out = sandboxed(home.path(), "proj").args(["run", "proj"]).assert().failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("refusing to attach"), "stderr:\n{stderr}");
    assert!(!stderr.contains("pre-authoriz"), "refused run touched the grant; stderr:\n{stderr}");
    for perms in PERMISSIONS_KDL {
        assert!(!home.path().join(perms).exists(), "refused run wrote {perms}");
    }
}

/// The counterpart: a run that launches still pre-seeds, and the grant is read
/// back after it, so the first-run hint (an ungranted wasm) stays silent.
#[cfg(unix)]
#[test]
fn run_that_launches_pre_seeds_the_grant() {
    let home = tempdir().unwrap();
    let out = sandboxed(home.path(), "").args(["run", "proj"]).assert().success();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("pre-authorized"), "stderr:\n{stderr}");
    assert!(!stderr.contains("First run"), "hint ignores the fresh grant; stderr:\n{stderr}");
    assert!(PERMISSIONS_KDL.iter().any(|p| home.path().join(p).exists()), "no permissions.kdl written");
}
