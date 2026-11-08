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
    strip_managed_zellij_alias(&mut lines);

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
            let alias = zellij_alias_lines(location, &indent);
            lines.splice(idx..idx, alias);
        } else {
            if !lines.is_empty() && !lines.last().is_some_and(|line| line.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push("plugins {".to_string());
            lines.extend(zellij_alias_lines(location, "    "));
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
