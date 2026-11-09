//! CI-safe end-to-end: `run --print-cmd` materializes the owned dir and emits a
//! Zellij invocation without execing Zellij. Uses an isolated HOME/XDG so it
//! never touches the developer's real config. The precise attach-vs-create shape
//! is covered by `plan_run` unit tests; this smoke test only asserts the binary
//! runs end to end and prints a session-scoped `zellij` command.
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

/// ICH-436: a run refused at the foreign-session consent must not have
/// pre-seeded the grant. A fake `zellij` lists a session named like the
/// target, which `run` never stamped, so the non-tty path refuses.
#[cfg(unix)]
#[test]
fn run_refused_for_a_foreign_session_leaves_permissions_untouched() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempdir().unwrap();
    let bin = tempdir().unwrap();
    let zellij = bin.path().join("zellij");
    std::fs::write(&zellij, "#!/bin/sh\necho proj\n").unwrap();
    std::fs::set_permissions(&zellij, std::fs::Permissions::from_mode(0o755)).unwrap();
    // A stand-in wasm: without an embedded one, `run` would download first.
    let wasm = home.path().join("stub.wasm");
    std::fs::write(&wasm, b"\0asm").unwrap();
    let path = format!("{}:{}", bin.path().display(), std::env::var("PATH").unwrap_or_default());

    let out = Command::cargo_bin("zj-radar")
        .unwrap()
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join(".cache"))
        .env("ZJ_RADAR_DATA_DIR", home.path().join("data"))
        .env("ZJ_RADAR_WASM", &wasm)
        .env("PATH", path)
        .env_remove("ZELLIJ")
        .env_remove("ZELLIJ_CONFIG_FILE")
        .args(["run", "proj"])
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("refusing to attach"), "stderr:\n{stderr}");
    assert!(!stderr.contains("pre-authoriz"), "refused run touched the grant; stderr:\n{stderr}");
    for perms in [".cache/zellij/permissions.kdl", "Library/Caches/org.Zellij-Contributors.Zellij/permissions.kdl"] {
        assert!(!home.path().join(perms).exists(), "refused run wrote {perms}");
    }
}
