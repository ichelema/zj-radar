use super::*;

use crate::setup::detect::{has_unmanaged_radar_alias, is_unmanaged_radar_alias_line, strip_managed_zellij_alias};

#[derive(Debug)]
pub enum Outcome {
    Changed(String),
    Unchanged,
    Conflict,
}

fn zellij_alias_lines(location: &str, indent: &str) -> Vec<String> {
    let escaped = kdl_string(location);
    vec![
        format!("{indent}{ZELLIJ_ALIAS_BEGIN}"),
        format!("{indent}radar location=\"{escaped}\" {{"),
        format!("{indent}    naming \"managed\""),
        format!("{indent}}}"),
        format!("{indent}{ZELLIJ_ALIAS_END}"),
    ]
}

/// The alias to splice back: every managed block setup stripped, verbatim with
/// only its `radar` location rewritten and moved to `indent`, so a re-run keeps
/// the user's options and comments. The default block when none holds a
/// `radar` node.
fn managed_alias_lines(managed: &[Vec<String>], location: &str, indent: &str) -> Result<Vec<String>, String> {
    let mut kept = Vec::new();
    for block in managed {
        if let Some(block) = with_location(block, location)? {
            kept.extend(reindent(&block, indent));
        }
    }
    Ok(if kept.is_empty() { zellij_alias_lines(location, indent) } else { kept })
}

/// `block` (starting with its BEGIN marker) moved to `indent`: the marker's
/// indentation is replaced on every non-blank line that starts with it.
fn reindent(block: &[String], indent: &str) -> Vec<String> {
    let base = block.first().map_or("", |l| &l[..l.len() - l.trim_start().len()]);
    block
        .iter()
        .map(|l| match l.strip_prefix(base) {
            Some(rest) if !l.trim().is_empty() => format!("{indent}{rest}"),
            _ => l.clone(),
        })
        .collect()
}

/// `block` with the `location` of its first `radar` node set to `location`
/// (added after `radar` when the node has none). `Ok(None)` without a `radar`
/// node. Fails closed on a `location` that isn't a one-line `location="…"`
/// (a raw string, a type annotation): adding a second one would leave Zellij
/// loading the old path, and falling back to the defaults would drop options.
fn with_location(block: &[String], location: &str) -> Result<Option<Vec<String>>, String> {
    const KEY: &str = "location=\"";
    let Some(at) = block.iter().position(|l| is_unmanaged_radar_alias_line(l)) else {
        return Ok(None);
    };
    let line = &block[at];
    let unreadable = || {
        format!(
            "refusing to write config.kdl: setup can't rewrite the managed radar alias's location in `{}` — write it as location=\"…\" on that line and re-run",
            line.trim()
        )
    };
    let value = format!("{KEY}{}\"", kdl_string(location));
    let head = match line.find(KEY) {
        Some(start) => {
            let rest = start + KEY.len();
            let mut chars = line[rest..].char_indices();
            let mut end = None;
            while let Some((i, c)) = chars.next() {
                match c {
                    '"' => {
                        end = Some(rest + i + 1);
                        break;
                    }
                    '\\' => {
                        chars.next();
                    }
                    _ => {}
                }
            }
            let end = end.ok_or_else(unreadable)?;
            format!("{}{value}{}", &line[..start], &line[end..])
        }
        None if line.contains("location=") => return Err(unreadable()),
        None => {
            let name_end = line.find("radar").ok_or_else(unreadable)? + "radar".len();
            format!("{} {value}{}", &line[..name_end], &line[name_end..])
        }
    };
    let mut block = block.to_vec();
    block[at] = head;
    Ok(Some(block))
}

/// Escape `s` for the inside of a KDL double-quoted string. Shared by the
/// managed alias block and `run`'s owned-config template, so a wasm path with
/// a `"` or `\\` can't break out of either `location="…"`.
pub(crate) fn kdl_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

pub(crate) fn split_lines(existing: &str) -> Vec<String> {
    existing.lines().map(ToString::to_string).collect()
}

fn join_lines(lines: &[String]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Brace {
    Open,
    Close,
}

/// The single string/escape/`//`-comment line scanner behind [`brace_delta`]
/// and [`one_line_block_spans`]: yields (byte offset, brace) for every
/// *structural* brace on `line`. Braces inside `"…"` strings (backslash-escape
/// aware) don't count, and scanning stops at a `//` line comment. KDL raw
/// strings and `/* */` block comments are not modeled — neither consumer ever
/// did, and the fail-closed KDL re-parse in [`edit_zellij`] backstops any
/// mis-scan on such input.
fn kdl_line_braces(line: &str) -> Vec<(usize, Brace)> {
    let mut braces = Vec::new();
    let mut chars = line.char_indices().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some((i, c)) = chars.next() {
        if !in_string && c == '/' && chars.peek().map(|&(_, c)| c) == Some('/') {
            break;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => braces.push((i, Brace::Open)),
            '}' => braces.push((i, Brace::Close)),
            _ => {}
        }
    }
    braces
}

pub(crate) fn brace_delta(line: &str) -> isize {
    kdl_line_braces(line).iter().map(|(_, b)| if *b == Brace::Open { 1 } else { -1 }).sum()
}

fn remove_unmanaged_radar_aliases(lines: &mut Vec<String>) {
    let mut i = 0;
    // Recomputed after every drain (indexes shift). Scoping to plugins blocks
    // is what keeps `--force` surgical: a user's `radar` node in some other
    // block (a keybind, a theme) must never be deleted here.
    let mut mask = crate::setup::detect::in_plugins_block_mask(lines);
    while i < lines.len() {
        if !(mask[i] && is_unmanaged_radar_alias_line(&lines[i])) {
            i += 1;
            continue;
        }

        let mut end = i;
        let mut depth = brace_delta(&lines[i]);
        while depth > 0 && end + 1 < lines.len() {
            end += 1;
            depth += brace_delta(&lines[end]);
        }
        lines.drain(i..=end);
        mask = crate::setup::detect::in_plugins_block_mask(lines);
    }
}

/// Is this line the opening of the KDL `plugins` block — not a sibling node
/// whose name merely starts with "plugins" (e.g. `plugins_extra {`)? The node
/// name must be followed by whitespace or the opening brace.
pub(crate) fn is_plugins_node_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    !trimmed.starts_with("//")
        && line.contains('{')
        && trimmed.strip_prefix("plugins").is_some_and(|rest| rest.is_empty() || rest.starts_with(['{', ' ', '\t']))
}

fn find_plugins_insert(lines: &[String]) -> Option<(usize, String)> {
    for (i, line) in lines.iter().enumerate() {
        if !is_plugins_node_line(line) {
            continue;
        }
        let trimmed = line.trim_start();
        let base_indent_len = line.len() - trimmed.len();
        let child_indent = format!("{}    ", &line[..base_indent_len]);
        let mut depth = brace_delta(line);
        // A self-contained one-liner (`plugins {}`) has depth 0 on its own line;
        // scanning forward from it would return some LATER block's closing
        // brace and splice the alias into the wrong scope. The caller expands
        // such lines first (`expand_one_line_plugins_block`), so skip here.
        if depth <= 0 {
            continue;
        }
        for (j, next) in lines.iter().enumerate().skip(i + 1) {
            depth += brace_delta(next);
            if depth <= 0 {
                return Some((j, child_indent));
            }
        }
    }
    None
}

/// Byte positions of a self-contained one-line block on `line`: the `{` that
/// opens depth 1 and the `}` that closes back to depth 0, string- and
/// comment-aware via [`kdl_line_braces`]. `None` when the line doesn't both
/// open and close a block, or opens another one after closing.
fn one_line_block_spans(line: &str) -> Option<(usize, usize)> {
    let (mut open, mut close) = (None, None);
    let mut depth = 0isize;
    for (i, brace) in kdl_line_braces(line) {
        match brace {
            Brace::Open => {
                depth += 1;
                if depth == 1 {
                    if close.is_some() {
                        return None; // a second block opens after the first closed
                    }
                    open.get_or_insert(i);
                }
            }
            Brace::Close => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                }
            }
        }
    }
    match (open, close, depth) {
        (Some(o), Some(c), 0) => Some((o, c)),
        _ => None,
    }
}

/// Expand a one-line `plugins { … }` node into a multi-line block so the alias
/// can be spliced in as a child: `plugins {}` becomes `plugins {` / `}`, with
/// any inline content moved to its own (still valid, `;`-separated) line and a
/// trailing comment kept on the closing line. Expands at most the FIRST such
/// node — the caller's re-scan (`find_plugins_insert`) picks the splice point.
fn expand_one_line_plugins_block(lines: &mut Vec<String>) {
    for i in 0..lines.len() {
        if !is_plugins_node_line(&lines[i]) {
            continue;
        }
        let Some((open, close)) = one_line_block_spans(&lines[i]) else {
            continue;
        };
        let line = lines[i].clone();
        let indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let inner = line[open + 1..close].trim().to_string();
        let mut repl = vec![line[..=open].to_string()];
        if !inner.is_empty() {
            repl.push(format!("{indent}    {inner}"));
        }
        repl.push(format!("{indent}{}", &line[close..]));
        lines.splice(i..=i, repl);
        return;
    }
}

/// Does any top-level `plugins` block contain a `radar` child? The post-edit
/// semantic check for an install: the alias must have landed where Zellij will
/// actually read it.
fn plugins_block_has_radar(doc: &kdl::KdlDocument) -> bool {
    doc.nodes()
        .iter()
        .filter(|n| n.name().value() == "plugins")
        .filter_map(|n| n.children())
        .any(|c| c.nodes().iter().any(|n| n.name().value() == "radar"))
}

/// Pure editor for `~/.config/zellij/config.kdl`. It manages only the
/// marker-tagged `radar` plugin alias; layout templates remain user-owned.
pub fn edit_zellij(existing: &str, location: &str, install: bool, force: bool) -> Result<Outcome, String> {
    let mut lines = split_lines(existing);
    let managed = strip_managed_zellij_alias(&mut lines);

    if install {
        if has_unmanaged_radar_alias(&lines) {
            if !force {
                return Ok(Outcome::Conflict);
            }
            remove_unmanaged_radar_aliases(&mut lines);
        }

        // A one-line `plugins {}`/`plugins { … }` can't take a child as-is —
        // expand it to a multi-line block first so the insert scan finds it.
        if find_plugins_insert(&lines).is_none() {
            expand_one_line_plugins_block(&mut lines);
        }
        if let Some((idx, indent)) = find_plugins_insert(&lines) {
            let alias = managed_alias_lines(&managed, location, &indent)?;
            lines.splice(idx..idx, alias);
        } else {
            if !lines.is_empty() && !lines.last().is_some_and(|line| line.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push("plugins {".to_string());
            lines.extend(managed_alias_lines(&managed, location, "    ")?);
            lines.push("}".to_string());
        }
    }

    let new = join_lines(&lines);
    if new == existing {
        return Ok(Outcome::Unchanged);
    }

    // Fail-closed backstop: the edits above are line-level, so any config shape
    // they mis-model must surface as an error, never as a corrupt write. Only
    // enforced when the ORIGINAL parses — a dialect our (v1-fallback) parser
    // can't read at all is out of scope for the guard, and refusing on it would
    // regress configs we edited fine before.
    if existing.parse::<kdl::KdlDocument>().is_ok() {
        let doc: kdl::KdlDocument = new.parse().map_err(|e| {
            format!("refusing to write config.kdl: the edited result is no longer valid KDL ({e}) — please report this")
        })?;
        if install && !plugins_block_has_radar(&doc) {
            return Err(
                "refusing to write config.kdl: the radar alias did not land inside a `plugins` block — please report this"
                    .to_string(),
            );
        }
    }
    Ok(Outcome::Changed(new))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Direct coverage of the shared scanner. `brace_delta` and
    /// `one_line_block_spans` used to duplicate this state machine; both were
    /// only covered indirectly (via `edit_zellij` / the detect-mask tests), so
    /// these cases now exercise the string/escape/`//`-comment handling for
    /// both consumers at once.
    #[test]
    fn kdl_line_braces_ignores_strings_and_comments() {
        // Structural braces, with byte offsets.
        assert_eq!(kdl_line_braces("plugins {}"), vec![(8, Brace::Open), (9, Brace::Close)]);
        // Braces inside strings don't count; the string's braces are skipped
        // but the real closing brace after it is still seen.
        assert_eq!(kdl_line_braces(r#"radar name="{not a block}" {"#), vec![(27, Brace::Open)]);
        // An escaped quote does not close the string.
        assert_eq!(kdl_line_braces(r#"radar x="a\"{" {"#), vec![(15, Brace::Open)]);
        // Everything after a `//` line comment is ignored…
        assert_eq!(kdl_line_braces("plugins { // }"), vec![(8, Brace::Open)]);
        // …but a `//` inside a string is content, not a comment.
        assert_eq!(kdl_line_braces(r#"radar url="https://x" {"#), vec![(22, Brace::Open)]);
    }

    #[test]
    fn brace_delta_matches_scanner_semantics() {
        assert_eq!(brace_delta("plugins {"), 1);
        assert_eq!(brace_delta("}"), -1);
        assert_eq!(brace_delta("plugins {}"), 0);
        assert_eq!(brace_delta(r#"radar name="{" { // }"#), 1);
    }

    #[test]
    fn one_line_block_spans_finds_self_contained_blocks() {
        assert_eq!(one_line_block_spans("plugins {}"), Some((8, 9)));
        assert_eq!(one_line_block_spans("plugins { a { b } }"), Some((8, 18)));
        // Unbalanced, multi-block, or comment-hidden closers are rejected.
        assert_eq!(one_line_block_spans("plugins {"), None);
        assert_eq!(one_line_block_spans("plugins {} keybinds {}"), None);
        assert_eq!(one_line_block_spans("plugins { // }"), None);
    }

    #[test]
    fn zellij_fresh_config_adds_plugins_alias_block() {
        match edit_zellij("", "file:~/.config/zellij/plugins/zj_radar.wasm", true, false).unwrap() {
            Outcome::Changed(s) => {
                assert!(s.contains("plugins {"));
                assert!(s.contains(ZELLIJ_ALIAS_BEGIN));
                assert!(s.contains("radar location=\"file:~/.config/zellij/plugins/zj_radar.wasm\""));
                assert!(s.contains("naming \"managed\""));
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn zellij_existing_plugins_block_gets_alias_child() {
        let existing = "keybinds {}\n\nplugins {\n    tab-bar location=\"zellij:tab-bar\"\n}\n";
        match edit_zellij(existing, "file:/tmp/zj_radar.wasm", true, false).unwrap() {
            Outcome::Changed(s) => {
                assert!(s.contains("tab-bar location=\"zellij:tab-bar\""));
                assert!(s.contains("    radar location=\"file:/tmp/zj_radar.wasm\""));
                assert!(s.contains("plugins {\n    tab-bar"));
            }
            o => panic!("{o:?}"),
        }
    }

    fn assert_radar_inside_plugins(config: &str) {
        let doc: kdl::KdlDocument = config.parse().expect("edited config must stay valid KDL");
        assert!(plugins_block_has_radar(&doc), "radar alias must land inside a `plugins` block:\n{config}");
        // The alias must not have leaked into any OTHER top-level block.
        for node in doc.nodes().iter().filter(|n| n.name().value() != "plugins") {
            if let Some(children) = node.children() {
                assert!(
                    !children.nodes().iter().any(|n| n.name().value() == "radar"),
                    "radar alias leaked into `{}`:\n{config}",
                    node.name().value()
                );
            }
        }
    }

    /// Regression: a one-line `plugins {}` followed by another block used to
    /// splice the alias into the WRONG scope (the next block's interior, or top
    /// level), writing a config Zellij rejects at startup.
    #[test]
    fn zellij_one_line_plugins_block_followed_by_sibling_block() {
        let existing = "plugins {}\nkeybinds {\n    normal {\n    }\n}\n";
        match edit_zellij(existing, "file:/tmp/zj_radar.wasm", true, false).unwrap() {
            Outcome::Changed(s) => {
                assert_radar_inside_plugins(&s);
                assert!(s.contains("keybinds"), "keybinds block preserved");
            }
            other => panic!("expected Changed, got {other:?}"),
        }
    }

    #[test]
    fn zellij_one_line_plugins_block_variants_get_alias_inside_plugins() {
        // Empty, inline-content (`;`-separated), and trailing-comment
        // one-liners — each must end with the alias inside `plugins`.
        for existing in
            ["plugins {}\n", "plugins { tab-bar location=\"zellij:tab-bar\"; }\n", "plugins {} // aliases live here\n"]
        {
            match edit_zellij(existing, "file:/tmp/zj_radar.wasm", true, false).unwrap() {
                Outcome::Changed(s) => assert_radar_inside_plugins(&s),
                other => panic!("expected Changed for {existing:?}, got {other:?}"),
            }
        }
    }

    /// The one-liner expansion must not break idempotency: installing twice
    /// over an expanded block is byte-identical after the first install.
    #[test]
    fn zellij_one_line_plugins_block_install_is_idempotent() {
        let once = match edit_zellij("plugins {}\n", "file:/tmp/zj_radar.wasm", true, false).unwrap() {
            Outcome::Changed(s) => s,
            other => panic!("expected Changed, got {other:?}"),
        };
        assert!(matches!(edit_zellij(&once, "file:/tmp/zj_radar.wasm", true, false).unwrap(), Outcome::Unchanged));
    }

    #[test]
    fn zellij_managed_alias_is_idempotent() {
        let once = match edit_zellij("", "file:/tmp/zj_radar.wasm", true, false).unwrap() {
            Outcome::Changed(s) => s,
            o => panic!("{o:?}"),
        };
        assert!(matches!(edit_zellij(&once, "file:/tmp/zj_radar.wasm", true, false).unwrap(), Outcome::Unchanged));
    }

    fn changed(existing: &str, location: &str) -> String {
        match edit_zellij(existing, location, true, false).unwrap() {
            Outcome::Changed(s) => s,
            o => panic!("expected Changed, got {o:?}"),
        }
    }

    /// Regression (ICH-453): re-running setup over a managed block kept only
    /// `naming "managed"`, silently resetting every option the user wrote.
    #[test]
    fn zellij_managed_alias_keeps_user_options_and_updates_location() {
        let options = "        naming \"off\"\n        glyphs \"nerd\"\n        spinner_fps 6\n        density \"comfortable\"\n        ledger false\n        session_tree false\n";
        let existing = format!(
            "plugins {{\n    tab-bar location=\"zellij:tab-bar\"\n    {ZELLIJ_ALIAS_BEGIN}\n    radar location=\"file:/old/zj_radar.wasm\" {{\n{options}    }}\n    {ZELLIJ_ALIAS_END}\n}}\n"
        );
        let new = changed(&existing, "file:/new/zj_radar.wasm");
        assert_eq!(new, existing.replace("file:/old/", "file:/new/"), "only the location changes");
        assert!(!new.contains("naming \"managed\""), "the user's naming \"off\" is not reset");
        assert_radar_inside_plugins(&new);
        assert!(matches!(edit_zellij(&new, "file:/new/zj_radar.wasm", true, false).unwrap(), Outcome::Unchanged));
    }

    #[test]
    fn zellij_managed_alias_without_options_stays_without_options() {
        let existing = format!(
            "plugins {{\n    {ZELLIJ_ALIAS_BEGIN}\n    radar location=\"file:/old.wasm\"\n    {ZELLIJ_ALIAS_END}\n}}\n"
        );
        let new = changed(&existing, "file:/new.wasm");
        assert_eq!(new, existing.replace("/old.wasm", "/new.wasm"));
    }

    /// Whatever shape the managed block has, setup copies it verbatim and
    /// changes only the location: no option, property or comment is lost.
    #[test]
    fn zellij_managed_alias_keeps_every_block_shape_verbatim() {
        let bodies = [
            // one line, `;`-separated
            "    radar location=\"file:/old.wasm\" { naming \"off\"; ledger false; }\n",
            // an option on the closing line
            "    radar location=\"file:/old.wasm\" {\n        naming \"off\"\n        ledger false; }\n",
            // an option on the head line
            "    radar location=\"file:/old.wasm\" { naming \"off\"\n        ledger false\n    }\n",
            // a nested block opened on the head line
            "    radar location=\"file:/old.wasm\" { foo {\n            bar 1\n        }\n        naming \"off\"\n    }\n",
            // extra properties and comments on the head and closing lines
            "    radar location=\"file:/old.wasm\" _allow_exec_host_cmd=true { // my radar\n        naming \"off\"\n    } // end radar\n",
            // tab indentation
            "\tradar location=\"file:/old.wasm\" {\n\t\tnaming \"off\"\n\t}\n",
        ];
        for body in bodies {
            let existing = format!("plugins {{\n    {ZELLIJ_ALIAS_BEGIN}\n{body}    {ZELLIJ_ALIAS_END}\n}}\n");
            let new = changed(&existing, "file:/new.wasm");
            assert_eq!(new, existing.replace("/old.wasm", "/new.wasm"), "only the location changes");
            assert_radar_inside_plugins(&new);
            assert!(matches!(edit_zellij(&new, "file:/new.wasm", true, false).unwrap(), Outcome::Unchanged));
        }
    }

    /// Two managed blocks (a hand-merged config): both keep their options.
    #[test]
    fn zellij_every_managed_block_keeps_its_options() {
        let block = |opt: &str| {
            format!("    {ZELLIJ_ALIAS_BEGIN}\n    radar location=\"file:/old.wasm\" {{\n        {opt}\n    }}\n    {ZELLIJ_ALIAS_END}\n")
        };
        let existing = format!("plugins {{\n{}{}}}\n", block("naming \"off\""), block("ledger false"));
        let new = changed(&existing, "file:/new.wasm");
        assert_eq!(new, existing.replace("/old.wasm", "/new.wasm"));
    }

    #[test]
    fn with_location_rewrites_or_adds_the_location() {
        let set =
            |line: &str| with_location(&[line.to_string()], "file:/new \"x\".wasm").map(|b| b.map(|b| b[0].clone()));
        let want = r#"location="file:/new \"x\".wasm""#;
        // An escaped quote in the old value doesn't end it early.
        assert_eq!(set(r#"  radar location="file:/a\"b.wasm" {"#), Ok(Some(format!("  radar {want} {{"))));
        assert_eq!(set("  radar {"), Ok(Some(format!("  radar {want} {{"))));
        assert_eq!(set("  radar"), Ok(Some(format!("  radar {want}"))));
        // No `radar` node: nothing to rewrite.
        assert_eq!(set("  // radar location=\"x\""), Ok(None));
        // A location setup can't rewrite in place fails closed, never duplicated.
        for line in ["  radar location=\"file:/a", "  radar location=r\"file:/a\" {", "  radar location=(path)\"x\""] {
            assert!(set(line).unwrap_err().contains("refusing to write config.kdl"), "{line}");
        }
    }

    /// Regression: a raw-string location got a second `location` added in front,
    /// and Zellij (last entry wins) kept loading the old wasm.
    #[test]
    fn zellij_managed_alias_with_raw_string_location_refuses_to_write() {
        let existing = format!(
            "plugins {{\n    {ZELLIJ_ALIAS_BEGIN}\n    radar location=r\"file:/old.wasm\" {{\n        naming \"off\"\n    }}\n    {ZELLIJ_ALIAS_END}\n}}\n"
        );
        let err = edit_zellij(&existing, "file:/new.wasm", true, false).unwrap_err();
        assert!(
            err.starts_with("refusing to write config.kdl") && err.contains("location=r\"file:/old.wasm\""),
            "{err}"
        );
    }

    /// A managed block outside `plugins` moves inside it at the child indent,
    /// keeping its options.
    #[test]
    fn zellij_top_level_managed_block_moves_into_plugins_reindented() {
        let existing = format!(
            "{ZELLIJ_ALIAS_BEGIN}\nradar location=\"file:/old.wasm\" {{\n    naming \"off\"\n}}\n{ZELLIJ_ALIAS_END}\nplugins {{\n    tab-bar location=\"zellij:tab-bar\"\n}}\n"
        );
        let new = changed(&existing, "file:/new.wasm");
        let want = format!(
            "plugins {{\n    tab-bar location=\"zellij:tab-bar\"\n    {ZELLIJ_ALIAS_BEGIN}\n    radar location=\"file:/new.wasm\" {{\n        naming \"off\"\n    }}\n    {ZELLIJ_ALIAS_END}\n}}\n"
        );
        assert_eq!(new, want);
        assert!(matches!(edit_zellij(&new, "file:/new.wasm", true, false).unwrap(), Outcome::Unchanged));
    }

    /// A managed block without a `radar` node holds no alias: setup writes the
    /// default block in its place.
    #[test]
    fn zellij_managed_block_without_radar_gets_default_alias() {
        let existing = format!("plugins {{\n    {ZELLIJ_ALIAS_BEGIN}\n    /- radar location=\"file:/old.wasm\"\n    {ZELLIJ_ALIAS_END}\n}}\n");
        let new = changed(&existing, "file:/new.wasm");
        assert!(new.contains("radar location=\"file:/new.wasm\" {\n        naming \"managed\""), "{new}");
        assert!(!new.contains("/old.wasm"), "{new}");
        assert_radar_inside_plugins(&new);
    }

    #[test]
    fn zellij_unmanaged_radar_alias_conflicts_without_force() {
        let existing = "plugins {\n    radar location=\"file:/other.wasm\"\n}\n";
        assert!(matches!(edit_zellij(existing, "file:/tmp/zj_radar.wasm", true, false).unwrap(), Outcome::Conflict));
    }

    #[test]
    fn zellij_force_replaces_unmanaged_radar_alias() {
        let existing = "plugins {\n    radar location=\"file:/other.wasm\" {\n        x \"y\"\n    }\n    tab-bar location=\"zellij:tab-bar\"\n}\n";
        match edit_zellij(existing, "file:/tmp/zj_radar.wasm", true, true).unwrap() {
            Outcome::Changed(s) => {
                assert!(!s.contains("/other.wasm"));
                assert!(s.contains("tab-bar location=\"zellij:tab-bar\""));
                assert!(s.contains("radar location=\"file:/tmp/zj_radar.wasm\""));
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn zellij_uninstall_removes_only_managed_alias() {
        let installed = match edit_zellij(
            "plugins {\n    tab-bar location=\"zellij:tab-bar\"\n}\n",
            "file:/tmp/zj_radar.wasm",
            true,
            false,
        )
        .unwrap()
        {
            Outcome::Changed(s) => s,
            o => panic!("{o:?}"),
        };
        match edit_zellij(&installed, "file:/tmp/zj_radar.wasm", false, false).unwrap() {
            Outcome::Changed(s) => {
                assert!(!s.contains("zj_radar.wasm"));
                assert!(s.contains("tab-bar location=\"zellij:tab-bar\""));
            }
            o => panic!("{o:?}"),
        }
    }
}
