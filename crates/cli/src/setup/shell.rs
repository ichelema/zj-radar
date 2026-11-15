//! `zj-radar setup shell [bash|zsh]` — opt-in: a marked block in the shell's
//! rc file that writes the standard OSC 133 prompt marks.
//!
//! Zellij learns a shell command's exit code only from those marks: without
//! them it sees the foreground process end, so a failed command reads Done
//! (or nothing, when short). Zellij already parses the marks; the block only
//! makes bash and zsh write them: `133;C;cmdline=…` before a command runs,
//! `133;D;<exit>` then `133;A` at the next prompt. Nushell writes them by
//! default. The plugin needs no code for this.
//!
//! Never part of the bare `setup`: it edits the user's own rc file. The block
//! sits between [`SHELL_BLOCK_BEGIN`] and [`SHELL_BLOCK_END`]; install
//! rewrites a stale block in place, uninstall removes it and leaves the rest
//! of the file byte-identical. An rc file that already writes OSC 133 marks
//! outside the block is left alone.

use super::*;

/// First line of the managed rc block — the install/uninstall marker.
pub(crate) const SHELL_BLOCK_BEGIN: &str = "# >>> zj-radar shell marks >>>";
/// Last line of the managed rc block.
pub(crate) const SHELL_BLOCK_END: &str = "# <<< zj-radar shell marks <<<";

/// The shells `setup shell` supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shell {
    Bash,
    Zsh,
}

impl Shell {
    const ALL: [Shell; 2] = [Shell::Bash, Shell::Zsh];

    fn name(self) -> &'static str {
        match self {
            Shell::Bash => "bash",
            Shell::Zsh => "zsh",
        }
    }

    fn from_name(name: &str) -> Option<Shell> {
        Shell::ALL.into_iter().find(|s| s.name() == name)
    }

    /// The hook (the block between the markers and the header comment).
    fn hook(self) -> &'static str {
        match self {
            Shell::Bash => BASH_HOOK,
            Shell::Zsh => ZSH_HOOK,
        }
    }
}

// bash has no preexec: `PS0` (bash ≥ 4.4, checked in the block) is expanded
// right before a command line runs, and not for an empty line. The arithmetic
// assignment in it (`${v:0:$((v=1,0))}`, which expands to nothing) runs in the
// shell itself and marks "a command ran", so an empty line writes no `D`. The
// command line comes from `history 1` in a command substitution: a line kept
// out of history (`HISTCONTROL=ignorespace`) names the previous one. `%q`
// quoting is what Zellij decodes, and it keeps a BEL or ESC in the line from
// ending the sequence. The precmd function goes FIRST in PROMPT_COMMAND and
// returns the status it found, so the hooks after it still see `$?` (not
// `PIPESTATUS`). Re-sourcing the rc file is safe: an rc that assigns
// PROMPT_COMMAND drops our call, so each part is added back when it is
// missing, never twice. The call hides "command not found" in a child bash
// that inherits an exported PROMPT_COMMAND without the function.
const BASH_HOOK: &str = r#"if [ -n "${ZELLIJ_PANE_ID:-}" ] &&
  ((BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4))); then
  : "${__zj_radar_ran:=0}"
  __zj_radar_cmdline() {
    local line
    line=$(HISTTIMEFORMAT= builtin history 1)
    [[ $line =~ ^[[:space:]]*[0-9]+\*?[[:space:]]+(.*)$ ]] && line=${BASH_REMATCH[1]}
    printf '%q' "$line"
  }
  __zj_radar_precmd() {
    local s=$?
    [ "$__zj_radar_ran" = 1 ] && printf '\e]133;D;%s\a' "$s"
    __zj_radar_ran=0
    printf '\e]133;A\a'
    return "$s"
  }
  [[ ${PS0:-} == *__zj_radar_cmdline* ]] ||
    PS0="${PS0:-}"'${__zj_radar_ran:0:$((__zj_radar_ran=1,0))}\e]133;C;cmdline=$(__zj_radar_cmdline)\a'
  [[ ${PROMPT_COMMAND:-} == *__zj_radar_precmd* ]] ||
    PROMPT_COMMAND="__zj_radar_precmd 2>/dev/null${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
fi"#;

// zsh's preexec gets the line as typed (`$1`) and runs only for a non-empty
// line; `(qqqq)` quotes it as `$'…'`, which Zellij decodes. zsh restores `$?`
// for every precmd hook, so the order among them does not matter, and
// `add-zsh-hook` adds a hook once, so re-sourcing the rc file is safe.
const ZSH_HOOK: &str = r#"if [[ -n "${ZELLIJ_PANE_ID:-}" ]]; then
  : ${__zj_radar_ran=}
  __zj_radar_preexec() {
    __zj_radar_ran=1
    printf '\e]133;C;cmdline=%s\a' "${(qqqq)1}"
  }
  __zj_radar_precmd() {
    local s=$?
    [[ -n $__zj_radar_ran ]] && printf '\e]133;D;%s\a' "$s"
    __zj_radar_ran=
    printf '\e]133;A\a'
    return $s
  }
  autoload -Uz add-zsh-hook
  add-zsh-hook preexec __zj_radar_preexec
  add-zsh-hook precmd __zj_radar_precmd
fi"#;

/// The full managed block for `shell`, markers included, newline-terminated.
fn shell_block(shell: Shell) -> String {
    format!(
        "{SHELL_BLOCK_BEGIN}\n# Managed by `zj-radar setup shell`: OSC 133 prompt marks, so Zellij sees each\n\
         # command's exit code. Remove with `zj-radar setup shell {} --uninstall`.\n{}\n{SHELL_BLOCK_END}\n",
        shell.name(),
        shell.hook()
    )
}

/// The rc file the block goes in: `~/.bashrc` or `${ZDOTDIR:-~}/.zshrc`.
/// Pure (env passed in) so the precedence is unit-tested.
fn rc_path_from(shell: Shell, home: Option<&str>, zdotdir: Option<&str>) -> Option<PathBuf> {
    let home = home.filter(|h| !h.is_empty()).map(PathBuf::from);
    match shell {
        Shell::Bash => home.map(|h| h.join(".bashrc")),
        Shell::Zsh => zdotdir.filter(|d| !d.is_empty()).map(PathBuf::from).or(home).map(|d| d.join(".zshrc")),
    }
}

fn rc_path(shell: Shell) -> Option<PathBuf> {
    let var = |k: &str| std::env::var(k).ok();
    rc_path_from(shell, var("HOME").as_deref(), var("ZDOTDIR").as_deref())
}

/// Byte range of our block in `text`, from its begin line to past its end
/// line. A begin marker without its end marker is refused: we can't tell
/// where our block stops.
fn block_span(text: &str) -> Result<Option<(usize, usize)>, String> {
    let mut offset = 0;
    let mut begin = None;
    for line in text.split_inclusive('\n') {
        let end = offset + line.len();
        match (begin, line.trim_end()) {
            (None, SHELL_BLOCK_BEGIN) => begin = Some(offset),
            (Some(b), SHELL_BLOCK_END) => return Ok(Some((b, end))),
            _ => {}
        }
        offset = end;
    }
    match begin {
        Some(_) => Err(format!("found `{SHELL_BLOCK_BEGIN}` without `{SHELL_BLOCK_END}`; fix it by hand")),
        None => Ok(None),
    }
}

/// Pure editor: `existing` with our block installed (replacing a stale one
/// in place) or removed. `Unchanged` when there is nothing to do.
///
/// Install appends one `\n` and then the block: after a final newline that
/// `\n` is a blank separator line, without one it ends the user's last line.
/// Uninstall drops that `\n` again when it is a blank line or the block ends
/// the file, so remove-after-add gives back the original bytes and never
/// joins two user lines.
fn edit_rc(existing: &str, block: &str, install: bool) -> Result<Outcome, String> {
    let new = match (block_span(existing)?, install) {
        (None, false) => return Ok(Outcome::Unchanged),
        (None, true) if existing.is_empty() => block.to_string(),
        (None, true) => format!("{existing}\n{block}"),
        (Some((b, e)), true) => format!("{}{block}{}", &existing[..b], &existing[e..]),
        (Some((b, e)), false) => {
            let (mut before, after) = (&existing[..b], &existing[e..]);
            if before == "\n" || before.ends_with("\n\n") || (after.is_empty() && before.ends_with('\n')) {
                before = &before[..before.len() - 1];
            }
            format!("{before}{after}")
        }
    };
    if new == existing {
        Ok(Outcome::Unchanged)
    } else {
        Ok(Outcome::Changed(new))
    }
}

/// The 1-based line where `text` writes an OSC 133 mark outside our block
/// (another shell integration, or marks the user wrote by hand), if any.
/// Every way to spell the escape (`\e`, `\033`, `\x1b`) leaves `]133;`; a
/// comment line does not count.
/// ponytail: sees only this file, not the files it sources (`wezterm.sh`). Zellij 0.46
/// ignores a second `D` or `A`, but a second `C` without `cmdline=` drops the command text.
fn own_marks_line(text: &str) -> Option<usize> {
    let span = block_span(text).ok().flatten();
    let mut offset = 0;
    for (n, line) in text.split_inclusive('\n').enumerate() {
        let inside = span.is_some_and(|(b, e)| offset >= b && offset < e);
        if !inside && !line.trim_start().starts_with('#') && line.contains("]133;") {
            return Some(n + 1);
        }
        offset += line.len();
    }
    None
}

/// The shells named after `shell` on the command line, or — for a bare
/// `shell` target — the login shell from `$SHELL`.
fn shells_for(named: &[&str], login_shell: Option<&str>) -> Result<Vec<Shell>, String> {
    if !named.is_empty() {
        return named
            .iter()
            .map(|n| Shell::from_name(n).ok_or_else(|| format!("setup shell takes bash or zsh, not '{n}'")))
            .collect();
    }
    let name = login_shell.map(|s| s.rsplit('/').next().unwrap_or(s)).unwrap_or("");
    Shell::from_name(name).map(|s| vec![s]).ok_or_else(|| {
        format!(
            "can't tell which shell to mark from $SHELL ({}); name one: `zj-radar setup shell bash|zsh`",
            if name.is_empty() { "unset" } else { name }
        )
    })
}

/// Entry point for `zj-radar setup shell [bash|zsh]`. `targets` is the whole
/// target list; `shell` may stand anywhere in it, the rest name shells.
pub(crate) fn run_shell(targets: &[String], mode: Mode, dry_run: bool, consent: Consent) {
    let named: Vec<&str> = targets.iter().map(String::as_str).filter(|t| *t != "shell").collect();
    let shells = match shells_for(&named, std::env::var("SHELL").ok().as_deref()) {
        Ok(shells) => shells,
        Err(e) => {
            crate::exit::fail_report("shell", format!("refused — {e}"));
            return;
        }
    };
    if mode == Mode::Check {
        println!("shell:");
        let items: Vec<CheckItem> = shells.into_iter().map(read_check_item).collect();
        if print_check_items(&items) {
            crate::exit::fail_report("zj-radar", "check found missing items (listed above)");
        }
        return;
    }
    for shell in shells {
        setup_shell(shell, mode == Mode::Uninstall, dry_run, consent);
    }
}

/// `--check` for one shell, reading its rc file.
fn read_check_item(shell: Shell) -> CheckItem {
    let Some(path) = rc_path(shell) else {
        return CheckItem::missing(shell.name(), "can't resolve the rc file (set $HOME)");
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => shell_check_item(shell, &path, &text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => shell_check_item(shell, &path, ""),
        Err(e) => CheckItem::missing(shell.name(), format!("can't read {}: {e}", path.display())),
    }
}

/// `--check` for one shell over its rc file's text. Pure, so every state is
/// unit-tested.
fn shell_check_item(shell: Shell, path: &Path, text: &str) -> CheckItem {
    let (name, rc) = (shell.name(), path.display());
    let rerun = format!("run `zj-radar setup shell {name}`");
    match (edit_rc(text, &shell_block(shell), true), own_marks_line(text)) {
        (Err(e), _) => CheckItem::missing(name, format!("{rc}: {e}")),
        (Ok(Outcome::Unchanged), _) => CheckItem::ok(name, format!("OSC 133 marks block in {rc}")),
        // Only a warn: whether those marks carry the exit code (`D;<exit>`) is not checked.
        (Ok(_), Some(line)) => {
            CheckItem::warn(name, format!("{rc} writes its own OSC 133 marks (line {line}), so setup leaves it alone"))
        }
        (Ok(_), None) if text.contains(SHELL_BLOCK_BEGIN) => {
            CheckItem::missing(name, format!("the marks block in {rc} is out of date — {rerun}"))
        }
        (Ok(_), None) => CheckItem::missing(
            name,
            format!("no OSC 133 marks in {rc}: a failed command reads done or nothing — {rerun}"),
        ),
    }
}

/// Install or uninstall the block for one shell.
fn setup_shell(shell: Shell, uninstall: bool, dry_run: bool, consent: Consent) {
    let label = format!("shell ({})", shell.name());
    let Some(path) = rc_path(shell) else {
        crate::exit::fail_report(&label, "refused — set $HOME so the rc file can be resolved");
        return;
    };
    // Write through a symlinked rc file (dotfile managers) rather than
    // replacing the link with a regular file.
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    let existing = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            crate::exit::fail_report(&label, format!("refused — can't read {}: {e}", path.display()));
            return;
        }
    };
    let Some(outcome) = edit_or_report(&label, edit_rc(&existing, &shell_block(shell), !uninstall)) else {
        return;
    };
    let new = match outcome {
        Outcome::Changed(new) => new,
        Outcome::Unchanged | Outcome::Conflict => {
            if uninstall {
                println!("{label}: no marks block in {}", path.display());
            } else {
                println!("{label}: marks block already up to date ({})", path.display());
            }
            return;
        }
    };
    if !uninstall && !existing.contains(SHELL_BLOCK_BEGIN) {
        if let Some(line) = own_marks_line(&existing) {
            println!(
                "{label}: skipped — {} already writes OSC 133 marks (line {line}); nothing to add",
                path.display()
            );
            return;
        }
    }
    if dry_run {
        if uninstall {
            println!("{label}: would remove the marks block from {} (dry-run)", path.display());
        } else {
            println!("{label}: would add to {} (dry-run):\n{}", path.display(), shell_block(shell));
        }
        return;
    }
    let prompt = if uninstall {
        format!("Remove the zj-radar marks block from {}?", path.display())
    } else {
        format!("Add the zj-radar marks block to {}?", path.display())
    };
    let parent = path.parent().map(Path::to_path_buf);
    let written = confirm_and_write(&label, &path, &new, consent, &prompt, move || match parent {
        Some(dir) => std::fs::create_dir_all(&dir).map_err(|e| format!("can't create {}: {e}", dir.display())),
        None => Ok(()),
    });
    if written {
        let verb = if uninstall { "removed the marks block from" } else { "added the marks block to" };
        println!("{label}: {verb} {} — open a new shell in a Zellij pane", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changed(r: Result<Outcome, String>) -> String {
        match r {
            Ok(Outcome::Changed(s)) => s,
            other => panic!("expected a change, got {other:?}"),
        }
    }

    #[test]
    fn remove_after_add_gives_back_the_original_bytes() {
        for shell in Shell::ALL {
            let block = shell_block(shell);
            for original in ["", "\n", "a", "a\n", "a\n\n", "alias ll='ls -l'\nexport EDITOR=vim", "x\r\ny\r\n"] {
                let installed = changed(edit_rc(original, &block, true));
                assert_eq!(block_span(&installed).unwrap().map(|(_, e)| e), Some(installed.len()), "{original:?}");
                assert!(matches!(edit_rc(&installed, &block, true), Ok(Outcome::Unchanged)), "idempotent");
                let removed = edit_rc(&installed, &block, false);
                if original.is_empty() {
                    assert_eq!(changed(removed), "");
                } else {
                    assert_eq!(changed(removed), original, "{shell:?} {original:?}");
                }
                assert!(matches!(edit_rc(original, &block, false), Ok(Outcome::Unchanged)));
            }
        }
    }

    #[test]
    fn a_block_between_user_lines_comes_out_without_joining_them() {
        let block = shell_block(Shell::Zsh);
        // The user moved our block up, between two of their lines.
        assert_eq!(changed(edit_rc(&format!("a\n{block}b\n"), &block, false)), "a\nb\n");
        assert_eq!(changed(edit_rc(&format!("{block}b\n"), &block, false)), "b\n");
        assert_eq!(changed(edit_rc(&format!("a\n\n{block}b\n"), &block, false)), "a\nb\n");
    }

    #[test]
    fn a_stale_block_is_replaced_in_place_keeping_what_follows() {
        let block = shell_block(Shell::Bash);
        let stale = format!("a\n{SHELL_BLOCK_BEGIN}\nold\n{SHELL_BLOCK_END}\nb\n");
        assert_eq!(changed(edit_rc(&stale, &block, true)), format!("a\n{block}b\n"));
    }

    #[test]
    fn an_unterminated_block_is_refused() {
        let broken = format!("a\n{SHELL_BLOCK_BEGIN}\nstuff\n");
        assert!(edit_rc(&broken, &shell_block(Shell::Bash), true).is_err());
        assert!(edit_rc(&broken, &shell_block(Shell::Bash), false).is_err());
    }

    #[test]
    fn own_marks_are_found_outside_our_block_only() {
        let block = shell_block(Shell::Zsh);
        assert_eq!(own_marks_line(&block), None, "our own block's marks don't count");
        assert_eq!(own_marks_line(&format!("a\n{block}")), None);
        assert_eq!(own_marks_line("a\nprintf '\\033]133;A\\007'\n"), Some(2));
        assert_eq!(own_marks_line(&format!("{block}x\nprint -n '\\e]133;D;0\\a'\n")), Some(block.lines().count() + 2));
        assert_eq!(own_marks_line("export PORT=8133\n"), None);
        assert_eq!(own_marks_line("  # old: printf '\\e]133;A\\a'\n"), None, "a comment is not a mark");
    }

    #[test]
    fn rc_paths_honor_zdotdir() {
        let p = |s, z| rc_path_from(s, Some("/h"), z).unwrap();
        assert_eq!(p(Shell::Bash, None), PathBuf::from("/h/.bashrc"));
        assert_eq!(p(Shell::Bash, Some("/z")), PathBuf::from("/h/.bashrc"));
        assert_eq!(p(Shell::Zsh, None), PathBuf::from("/h/.zshrc"));
        assert_eq!(p(Shell::Zsh, Some("")), PathBuf::from("/h/.zshrc"));
        assert_eq!(p(Shell::Zsh, Some("/z")), PathBuf::from("/z/.zshrc"));
        assert_eq!(rc_path_from(Shell::Bash, None, None), None);
        assert_eq!(rc_path_from(Shell::Zsh, Some(""), None), None);
    }

    #[test]
    fn shells_come_from_the_names_or_the_login_shell() {
        assert_eq!(shells_for(&["zsh", "bash"], None), Ok(vec![Shell::Zsh, Shell::Bash]));
        assert_eq!(shells_for(&[], Some("/usr/bin/bash")), Ok(vec![Shell::Bash]));
        assert_eq!(shells_for(&[], Some("zsh")), Ok(vec![Shell::Zsh]));
        assert!(shells_for(&["zellij"], Some("/bin/zsh")).unwrap_err().contains("not 'zellij'"));
        assert!(shells_for(&[], Some("/usr/bin/fish")).unwrap_err().contains("(fish)"));
        assert!(shells_for(&[], None).unwrap_err().contains("(unset)"));
    }

    #[test]
    fn every_hook_writes_the_three_marks_only_inside_zellij() {
        for shell in Shell::ALL {
            let hook = shell.hook();
            for mark in ["]133;C;cmdline=", "]133;D;%s", "]133;A"] {
                assert!(hook.contains(mark), "{shell:?} lacks {mark}");
            }
            assert!(hook.starts_with("if ") && hook.contains("ZELLIJ_PANE_ID"), "{shell:?}");
        }
    }

    #[test]
    fn check_reports_each_state_of_the_rc_file() {
        let path = Path::new("/h/.zshrc");
        let block = shell_block(Shell::Zsh);
        let level = |text: &str| shell_check_item(Shell::Zsh, path, text).level;
        assert_eq!(level(&format!("a\n\n{block}")), CheckLevel::Ok);
        assert_eq!(level("printf '\\e]133;A\\a'\n"), CheckLevel::Warn);
        assert_eq!(level(""), CheckLevel::Missing);
        assert_eq!(level(&format!("{SHELL_BLOCK_BEGIN}\nold\n{SHELL_BLOCK_END}\n")), CheckLevel::Missing);
        assert_eq!(level(&format!("{SHELL_BLOCK_BEGIN}\nold\n")), CheckLevel::Missing);
        let stale = shell_check_item(Shell::Zsh, path, &format!("{SHELL_BLOCK_BEGIN}\nold\n{SHELL_BLOCK_END}\n"));
        assert!(stale.detail.contains("out of date"), "{stale:?}");
    }
}
