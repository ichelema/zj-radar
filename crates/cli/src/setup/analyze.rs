use super::*;

use crate::setup::detect::{has_unmanaged_radar_alias, is_unmanaged_radar_alias_line, strip_managed_zellij_alias};

use std::path::{Path, PathBuf};

/// Raw, already-read environment for Zellij setup. The ONLY layer that touched
/// the filesystem — `analyze_zellij` is pure over this struct.
pub(crate) struct ZellijEnv {
    pub config_text: Option<String>,
    pub layout_text: Option<String>,
    pub permissions_text: Option<String>,
    pub wasm_present: bool,
    pub config_managed: bool,
    pub wasm_path: String,
    /// `zellij --version` stdout; `None` when the binary is absent/unrunnable.
    pub zellij_version: Option<String>,
}

/// The paths [`read_zellij_env`] resolved along the way. Callers splice/copy
/// against these, so resolving them once keeps the reader and the writers
/// aimed at the same files.
pub(crate) struct ZellijPaths {
    pub config_path: PathBuf,
    pub wasm_dest: PathBuf,
    pub layout_path: PathBuf,
    /// The layout NAME `layout_path` was resolved from (`resolve_layout_name`),
    /// carried so reports name exactly the layout the read inspected.
    pub layout_name: String,
}

/// The one IO gathering point behind [`ZellijEnv`]: read every input
/// `analyze_zellij` grades, resolved against `config_dir`. Shared by the
/// installer (`setup_zellij`) and the doctor (`check_zellij`) so "what state
/// is this machine in?" cannot diverge between them.
pub(crate) fn read_zellij_env(config_dir: &Path, layout_name: Option<&str>) -> (ZellijEnv, ZellijPaths) {
    let config_path = zellij_config_path(config_dir);
    let wasm_dest = zellij_wasm_dest(config_dir);
    let config_text = std::fs::read_to_string(&config_path).ok();
    // A relative PATH given as `--layout` resolves against the current dir,
    // like Zellij's own `--layout`; relative paths from `config.kdl` resolve
    // against the config dir (`resolve_layout_path_from`).
    let cli_layout = layout_name.map(|name| {
        let relative_path = name.contains('/') && !name.starts_with('~') && !Path::new(name).is_absolute();
        match std::env::current_dir() {
            Ok(cwd) if relative_path => cwd.join(name).to_string_lossy().into_owned(),
            _ => name.to_string(),
        }
    });
    let layout_name = crate::setup::detect::resolve_layout_name(cli_layout.as_deref(), config_text.as_deref());
    let layout_path = crate::setup::detect::resolve_layout_path(config_dir, &layout_name, config_text.as_deref());
    let env = ZellijEnv {
        layout_text: std::fs::read_to_string(&layout_path).ok(),
        permissions_text: crate::run::zellij_permissions_text(),
        wasm_present: wasm_dest.is_file(),
        config_managed: path_is_managed(&config_path),
        wasm_path: wasm_dest.to_string_lossy().into_owned(),
        zellij_version: zellij_version_output(),
        config_text,
    };
    (env, ZellijPaths { config_path, wasm_dest, layout_path, layout_name })
}

/// Every derived fact about the current Zellij setup state, in one place. Both
/// `check` (renders) and `install` (gates) read these; the derivation is here so
/// "is our alias present?" has exactly one definition.
pub(crate) struct ZellijFacts {
    pub managed_alias_present: bool,
    pub unmanaged_alias_present: bool,
    pub alias_is_store_path: bool,
    pub wasm_present: bool,
    pub has_rail: Option<bool>,
    /// Body-less `tab` nodes in the inspected layout (`layout::empty_tab_bodies`),
    /// by name or ordinal. Meaningful only alongside `has_rail == Some(true)`:
    /// inside the rail's split such a tab opens with the rail alone.
    pub empty_tab_bodies: Vec<String>,
    pub granted: Option<bool>,
    pub config_managed: bool,
    pub zellij_version: Option<String>,
}

/// The oldest Zellij the rail works on — a floor, not a pinned minor: Zellij
/// keeps compiled plugins working on newer releases (protobuf-serialized
/// plugin API since 0.38), so later versions pass the gate. 0.46.0 is the
/// first release with the pane attention API (OSC 7501 program status,
/// `PaneAttentionChanged`, `GlobalAttentionUpdate`) the rail reads agent
/// status from. Bump alongside the `zellij-tile` pin in
/// crates/plugin/Cargo.toml and the version claims in README/docs; the plugin
/// crate's `doctor_version_gate_matches_the_zellij_tile_pin` guard test welds
/// this triple to that Cargo pin.
///
/// Zellij internals we parse, with no compatibility promise — re-verify each
/// when a new Zellij minor lands:
/// - `run.rs::wasm_is_granted` — `permissions.kdl` grant-block grammar
///   (quoted wasm path as block header, one permission per line).
/// - `run.rs::permissions_path_in` — the macOS cache sub-folder name
///   `org.Zellij-Contributors.Zellij` (Linux uses plain `zellij`).
/// - `run.rs::session_is_live` — `zellij list-sessions --short` (one plain
///   session name per line).
pub(crate) const MIN_SUPPORTED_ZELLIJ: (u32, u32, u32) = (0, 46, 0);

/// The floor as user-facing messages spell it, e.g. `0.46.0`.
pub(crate) fn min_supported_zellij_display() -> String {
    let (major, minor, patch) = MIN_SUPPORTED_ZELLIJ;
    format!("{major}.{minor}.{patch}")
}

/// The one "no zellij binary" diagnostic — shared by the doctor, the grant
/// launcher, and `run`, so the floor and the install URL can't drift between
/// them.
pub(crate) fn zellij_missing_message() -> String {
    format!(
        "zellij not found on PATH — install Zellij {}+ first \
         (https://zellij.dev/documentation/installation)",
        min_supported_zellij_display()
    )
}

/// Lenient version-floor gate: warn only on a definitely-too-old version.
/// `zellij --version` prints e.g. `zellij 0.46.0`; if that shape ever changes
/// and no version-like token is found (or its digits don't parse), err on the
/// side of "supported" — a fragile parse must not fail a working install.
pub(crate) fn zellij_version_is_supported(version_output: &str) -> bool {
    let Some(v) = version_output.split_whitespace().find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()))
    else {
        return true;
    };
    let mut nums = v.split('.').map(|part| {
        let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse::<u32>().ok()
    });
    let (Some(Some(major)), Some(Some(minor))) = (nums.next(), nums.next()) else {
        return true;
    };
    let (floor_major, floor_minor, floor_patch) = MIN_SUPPORTED_ZELLIJ;
    match (major, minor).cmp(&(floor_major, floor_minor)) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        // On the floor minor the patch decides; a missing/unparseable patch
        // is lenient, like every other parse failure here.
        std::cmp::Ordering::Equal => nums.next().flatten().is_none_or(|p| p >= floor_patch),
    }
}

/// `(managed, unmanaged)`: is our marked alias block in `config.kdl`, and is
/// a `radar` alias of the user's own there outside it?
fn alias_presence(lines: &[String]) -> (bool, bool) {
    let managed = lines.iter().any(|l| l.trim() == ZELLIJ_ALIAS_BEGIN);
    let mut lines_without_managed = lines.to_vec();
    strip_managed_zellij_alias(&mut lines_without_managed);
    (managed, has_unmanaged_radar_alias(&lines_without_managed))
}

/// Pure: derive every Zellij setup fact from already-read inputs. No I/O.
pub(crate) fn analyze_zellij(env: &ZellijEnv) -> ZellijFacts {
    let lines: Vec<String> = env.config_text.as_deref().map(split_lines).unwrap_or_default();
    let (managed_alias_present, unmanaged_alias_present) = alias_presence(&lines);
    // Scoped to radar alias lines inside a plugins block: other plugins
    // legitimately live in the store (e.g. `room location="file:/nix/store/…"`)
    // and must not trip the radar grant-persistence warning — nor should a
    // radar-shaped node in some non-plugins block.
    let plugins_mask = crate::setup::detect::in_plugins_block_mask(&lines);
    let alias_is_store_path = lines
        .iter()
        .zip(&plugins_mask)
        .any(|(l, in_plugins)| *in_plugins && is_unmanaged_radar_alias_line(l) && l.contains("/nix/store/"));
    let has_rail = env.layout_text.as_deref().map(|t| crate::layout::analyze(t).has_rail);
    let empty_tab_bodies = env.layout_text.as_deref().map(crate::layout::empty_tab_bodies).unwrap_or_default();
    let granted = env.permissions_text.as_deref().map(|t| crate::run::wasm_is_granted(t, &env.wasm_path));
    ZellijFacts {
        managed_alias_present,
        unmanaged_alias_present,
        alias_is_store_path,
        wasm_present: env.wasm_present,
        has_rail,
        empty_tab_bodies,
        granted,
        config_managed: env.config_managed,
        zellij_version: env.zellij_version.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyze_zellij_derives_managed_and_unmanaged_alias_separately() {
        // Managed alias block present, no unmanaged line.
        let managed =
            format!("plugins {{\n{ZELLIJ_ALIAS_BEGIN}\n    radar location=\"file:/x.wasm\"\n{ZELLIJ_ALIAS_END}\n}}\n");
        let env = ZellijEnv {
            config_text: Some(managed),
            layout_text: None,
            permissions_text: None,
            wasm_present: false,
            config_managed: false,
            wasm_path: "/x.wasm".to_string(),
            zellij_version: Some("zellij 0.46.0".to_string()),
        };
        let f = analyze_zellij(&env);
        assert!(f.managed_alias_present, "managed marker must be detected");
        assert!(!f.unmanaged_alias_present, "no unmanaged alias here");
    }

    #[test]
    fn analyze_zellij_store_path_warn_is_scoped_to_the_radar_alias() {
        // Another plugin living in the store (room, zjstatus, …) must not trip
        // the radar grant-persistence warning; only the radar alias itself
        // pointing into /nix/store/ counts.
        let other_plugin_in_store = concat!(
            "plugins {\n",
            "    room location=\"file:/nix/store/abc-room/bin/room.wasm\"\n",
            "    radar location=\"file:~/.config/zellij/plugins/zj_radar.wasm\"\n",
            "}\n",
        );
        let radar_in_store =
            concat!("plugins {\n", "    radar location=\"file:/nix/store/abc-zj-radar/bin/zj_radar.wasm\"\n", "}\n",);
        let env_for = |config: &str| ZellijEnv {
            config_text: Some(config.to_string()),
            layout_text: None,
            permissions_text: None,
            wasm_present: false,
            config_managed: false,
            wasm_path: "/x.wasm".to_string(),
            zellij_version: Some("zellij 0.46.0".to_string()),
        };
        assert!(
            !analyze_zellij(&env_for(other_plugin_in_store)).alias_is_store_path,
            "a different plugin's store path must not warn"
        );
        assert!(
            analyze_zellij(&env_for(radar_in_store)).alias_is_store_path,
            "the radar alias in the store must still warn"
        );
    }

    #[test]
    fn analyze_zellij_derives_has_rail_and_grant_from_text() {
        let wasm_path = "/home/user/.config/zellij/plugins/zj_radar.wasm";
        // The body-less `tab` rides along so the derivation of
        // `empty_tab_bodies` (the doctor's "tab bodies" input) is pinned here
        // too — `layout::empty_tab_bodies` itself is tested in `layout.rs`.
        let layout = "layout {\n    plugin location=\"radar\"\n    tab focus=true\n}\n";
        let perms = format!(
            "\"{wasm_path}\" {{\n    ReadApplicationState\n    ReadCliPipes\n    ChangeApplicationState\n    RunCommands\n    ReadPaneContents\n}}\n"
        );
        let env = ZellijEnv {
            config_text: None,
            layout_text: Some(layout.to_string()),
            permissions_text: Some(perms),
            wasm_present: true,
            config_managed: false,
            wasm_path: wasm_path.to_string(),
            zellij_version: Some("zellij 0.46.0".to_string()),
        };
        let f = analyze_zellij(&env);
        assert_eq!(f.has_rail, Some(true), "layout text with radar plugin has rail");
        assert_eq!(f.empty_tab_bodies, vec!["tab #1"], "body-less tab is named for the doctor");
        assert_eq!(f.granted, Some(true), "permissions naming the wasm path is granted");
        assert!(f.wasm_present);
    }

    #[test]
    fn analyze_zellij_absent_files_are_none_not_false() {
        let env = ZellijEnv {
            config_text: None,
            layout_text: None,
            permissions_text: None,
            wasm_present: false,
            config_managed: false,
            wasm_path: "/x.wasm".to_string(),
            zellij_version: Some("zellij 0.46.0".to_string()),
        };
        let f = analyze_zellij(&env);
        assert_eq!(f.has_rail, None, "no layout file -> None, distinct from Some(false)");
        assert_eq!(f.granted, None, "no permissions file -> None");
    }
}
