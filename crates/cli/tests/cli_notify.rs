//! `notify` is gone, but hooks left by older releases still exec it. It must
//! exit 0 on any input: Claude Code treats a non-zero hook exit as blocking.
use assert_cmd::Command;

#[test]
fn removed_notify_exits_zero_on_any_old_hook_invocation() {
    let invocations: [&[&str]; 4] = [
        &["notify", "claude", "--status", "running"],
        &["notify", "codex", "{\"type\":\"agent-turn-complete\"}"],
        &["notify", "generic", "--status", "done", "--msg", "x", "--task", "t", "--source", "deploy", "--dry-run"],
        &["notify"],
    ];
    for args in invocations {
        let out = Command::cargo_bin("zj-radar").unwrap().args(args).write_stdin("{}").assert().success();
        let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
        assert!(stderr.contains("`notify` was removed"), "{args:?}: {stderr}");
        assert!(out.get_output().stdout.is_empty(), "{args:?} must print nothing on stdout");
    }
}

#[test]
fn removed_notify_stays_out_of_help() {
    let out = Command::cargo_bin("zj-radar").unwrap().arg("--help").assert().success();
    let help = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(!help.contains("notify"), "hidden subcommand leaked into --help:\n{help}");
}
