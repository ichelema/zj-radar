// E2E tests: drive a real Zellij in a PTY, load the zj-radar plugin, and
// assert that the agent status a pane reports (OSC 7501, typed into the pane)
// renders in the sidebar.
//
// These are gated behind `--features e2e` and marked `#[ignore]` so that
// `cargo test` stays fast and hermetic. Run with:
//
//   cargo test --features e2e --test e2e -- --include-ignored
//
// or via:
//
//   just test-e2e
//
// Prerequisites:
//   - `zellij` 0.46.x on PATH
//   - The plugin wasm already built:
//     `cargo build --release --target wasm32-wasip1`

mod harness;

use harness::*;
use std::time::Duration;

/// Standard base64 (OSC 7501 wants `msg`/`title` values encoded).
fn b64(text: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in text.as_bytes().chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The OSC 7501 body of a `claude-code` root record: `state` is `working`,
/// `blocked`, `done`, `error` or `idle`; `msg` is the display text.
fn osc(state: &str, msg: &str) -> String {
    format!("state={state}:app=claude-code:msg={}", b64(msg))
}

/// The harness itself is an E2E dependency: every test must run on a private
/// Zellij server, even when the caller already has a live server and exports
/// XDG cache/config overrides. Prove the session exists through the harness's
/// socket while remaining invisible through an ordinary ambient CLI.
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn harness_runs_on_a_private_zellij_server() {
    let wasm = plugin_wasm_path();
    assert!(wasm.exists(), "Plugin wasm not found at {:?}", wasm);
    let isolated = pre_grant_permissions(&wasm);
    let session_name = format!("zjr_isolation_{}", std::process::id());
    let session = ZellijSession::start(&session_name, &sidebar_layout(&wasm), &wasm, isolated);

    let private = session.isolated_session_names();
    assert!(
        private.iter().any(|name| name == &session_name),
        "private socket must list its own session; got {private:?}"
    );

    let ambient = ambient_session_names();
    assert!(
        ambient.iter().all(|name| name != &session_name),
        "E2E session leaked onto the ambient Zellij server; ambient={ambient:?}"
    );
}

/// Smoke test: start Zellij with the plugin sidebar, report a working agent
/// over OSC 7501, and assert that its message appears in the rendered screen.
///
/// # How it works
/// 1. `pre_grant_permissions` seeds Zellij's `permissions.kdl` in a complete
///    isolated environment (never touches the real user cache).
/// 2. `ZellijSession::start` spawns Zellij with `--new-session-with-layout`
///    (works from inside an outer session) and a layout using `/tmp` as CWD
///    (avoids slow direnv/devenv init). It polls the PTY buffer until the
///    plugin's " RADAR" header appears. All Zellij subprocesses use the same
///    private HOME, XDG roots, and socket dir.
/// 3. `emit_program_status` types an OSC 7501 `printf` into the focused
///    terminal pane, so Zellij's own attention tracking sees the status and
///    hands it to the plugin in the pane manifest.
/// 4. We assert that "building" appears in the sidebar region, not the PTY
///    buffer (which also holds the terminal's own echo).
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn plugin_loads_and_renders_status() {
    let wasm = plugin_wasm_path();
    assert!(
        wasm.exists(),
        "Plugin wasm not found at {:?}. Build it first:\n  cargo build --release --target wasm32-wasip1",
        wasm
    );

    // Pre-grant plugin permissions so the modal prompt never fires.
    // Returns an isolated process environment — never touches the user's cache.
    let temp_home = pre_grant_permissions(&wasm);

    // Use a process-unique session name to avoid collisions on concurrent runs.
    let session_name = format!("zjr_smoke_{}", std::process::id());
    let layout = sidebar_layout(&wasm);

    eprintln!("[e2e] starting session '{}' with layout:\n{}", session_name, layout);
    let session = ZellijSession::start(&session_name, &layout, &wasm, temp_home);
    eprintln!("[e2e] plugin loaded (saw ' RADAR' in PTY)");

    session.emit_program_status(&osc("working", "building"));

    // Poll the vt100-parsed sidebar region (left 32 cols) for the agent's
    // activity, up to 5s. Asserting on the sidebar region — not pty_text() —
    // excludes the terminal scrollback, so a match is a real render, not a
    // false positive; and polling returns the instant the frame is ready
    // rather than fixed-sleeping.
    let appeared = session.wait_for_sidebar(32, "building", std::time::Duration::from_secs(5));
    let sidebar = sidebar_region(&session.screen(), 32);
    eprintln!("[e2e] sidebar region (32 cols):\n{}", sidebar);
    assert!(appeared, "sidebar should show the reported activity 'building';\nsidebar:\n{sidebar}");

    eprintln!("[e2e] PASS: found the reported status in the rendered sidebar");
}

/// `spinner_fps` (default 4) on the real frame: a running agent's spinner
/// must step several times a second, not once. Samples the rendered spinner
/// glyph every ~100ms for 2s and counts the changes: the 1 Hz spinner makes
/// ~2, the 4 Hz one ~8.
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn spinner_steps_several_times_a_second_by_default() {
    let wasm = plugin_wasm_path();
    assert!(wasm.exists(), "Plugin wasm not found at {:?}", wasm);
    let temp_home = pre_grant_permissions(&wasm);
    let session_name = format!("zjr_fps_{}", std::process::id());
    let session = ZellijSession::start(&session_name, &sidebar_layout(&wasm), &wasm, temp_home);
    session.emit_program_status(&osc("working", "spinning"));
    assert!(session.wait_for_sidebar(32, "spinning", Duration::from_secs(5)), "the agent row must render");

    const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let glyph = |s: &ZellijSession| {
        let screen = s.screen();
        let row = sidebar_row_index(&screen, 32, "spinning")?;
        sidebar_region(&screen, 32).lines().nth(row)?.chars().find(|c| SPINNER.contains(c))
    };
    let mut seen = Vec::new();
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        if let Some(g) = glyph(&session) {
            if seen.last() != Some(&g) {
                seen.push(g);
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let changes = seen.len().saturating_sub(1);
    eprintln!("[e2e] spinner sequence over 2s: {seen:?} ({changes} changes)");
    assert!(changes >= 5, "a 4 Hz spinner changes ~8 times in 2s (1 Hz: ~2); saw {changes}: {seen:?}");
}

/// Background tasks end to end through the real wasm plugin: an OSC 7501 child
/// record (`id=`) under a working agent must draw the `┊` task line, with the
/// record's `title` as its label — Zellij → manifest → merge → render on a
/// live rail, not the host-side renderer the insta snapshots pin.
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn background_task_lines_render_on_the_live_rail() {
    let wasm = plugin_wasm_path();
    assert!(wasm.exists(), "Plugin wasm not found at {:?}", wasm);
    let isolated = pre_grant_permissions(&wasm);
    let session_name = format!("zjr_tasks_{}", std::process::id());
    let session = ZellijSession::start(&session_name, &sidebar_layout(&wasm), &wasm, isolated);
    let secs = std::time::Duration::from_secs(5);

    // Child first: Zellij sends PaneUpdate (the only carrier of the records)
    // only when the pane's resolved attention changes, and a child under a
    // root that is already working changes nothing. The root's Idle → Working
    // step then delivers both records in one PaneUpdate.
    session.emit_program_status(&format!("state=working:app=claude-code:id=b1:title={}", b64("Run tests")));
    session.emit_program_status(&osc("working", "fixing retries"));
    assert!(
        session.wait_for_sidebar(32, "┊", secs) && session.wait_for_sidebar(32, "Run tests", secs),
        "a child record should draw a task line;\nsidebar:\n{}\nterminal pane:\n{}",
        sidebar_region(&session.screen(), 32),
        session.dump_screen()
    );
}

/// Issue #46: a rail whose tab holds no terminal pane must not take the
/// session down. Zellij closes any tab with no selectable tiled pane
/// (`Screen::render` → `tabs_to_close`), and a `children` nested inside the
/// rail's split spawns no terminal when the tab body is empty (zellij#5618) —
/// so the pre-fix rail's load-time `set_selectable(false)` closed the only
/// tab, and the client printed "Bye from Zellij!" within ~50ms of the plugin
/// loading. The rail now stays selectable until a terminal shares its tab;
/// this drives the exact reporter layout, proves the session survives, then
/// opens a pane and proves the rail hands focus to it (goes passive).
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn rail_only_tab_keeps_the_session_alive() {
    let wasm = plugin_wasm_path();
    assert!(wasm.exists(), "Plugin wasm not found at {:?}", wasm);
    let temp_home = pre_grant_permissions(&wasm);
    let session_name = format!("zjr_railonly_{}", std::process::id());
    let layout = rail_only_tab_layout(&wasm);
    eprintln!("[e2e] starting rail-only session '{session_name}' with layout:\n{layout}");
    // `start` already waits for the rail's header, so the plugin has loaded
    // and rendered by the time this returns — the pre-fix crash landed within
    // ~50ms of that, so the steady-state check below is the assertion that
    // matters, not the start itself.
    let mut session = ZellijSession::start(&session_name, &layout, &wasm, temp_home);

    // A non-event ("the session stays up") can only be asserted by waiting it
    // out; 3s dwarfs the ~50-400ms the reporter and the local repro measured.
    std::thread::sleep(std::time::Duration::from_secs(3));
    let text = session.pty_text();
    assert!(
        session.is_alive() && !text.contains("Bye from Zellij"),
        "session died with only the rail in its tab; PTY tail:\n{}",
        text.chars().rev().take(400).collect::<String>().chars().rev().collect::<String>()
    );

    // Recovery path: opening a pane splits the (still selectable, focused)
    // rail. `new-pane` focuses the new pane itself, so this first probe only
    // proves a shell exists and the session still works. `$((1+1))` makes the
    // echoed OUTPUT distinguishable from the echoed keystrokes.
    session.run_action_checked(&["new-pane"]);
    let reached_shell = session.wait_until(std::time::Duration::from_secs(10), |s| {
        s.run_action(&["write-chars", "echo RAILONLY_$((1+1))"]);
        s.run_action(&["write", "13"]);
        std::thread::sleep(std::time::Duration::from_millis(500));
        s.pty_text().contains("RAILONLY_2")
    });
    assert!(
        reached_shell,
        "keystrokes never reached a shell after `new-pane`\nscreen:\n{}",
        screen_text(&session.screen())
    );

    // Passivity: the manifest has now shown the rail a terminal neighbor, so
    // it must have gone non-selectable. The rail is the LEFT pane of the
    // split; `move-focus left` would land on it if it were still selectable
    // and swallow the next probe. A passive pane refuses focus, so the shell
    // keeps it and echoes.
    session.run_action_checked(&["move-focus", "left"]);
    let rail_refused_focus = session.wait_until(std::time::Duration::from_secs(10), |s| {
        s.run_action(&["write-chars", "echo RAILONLY_$((2+2))"]);
        s.run_action(&["write", "13"]);
        std::thread::sleep(std::time::Duration::from_millis(500));
        s.pty_text().contains("RAILONLY_4")
    });
    assert!(
        rail_refused_focus,
        "after `move-focus left` keystrokes stopped reaching the shell — the rail is still selectable\nscreen:\n{}",
        screen_text(&session.screen())
    );

    // And the pre-fix contract holds: `exit` in the tab's LAST shell closes
    // the tab, and with it the only tab — the session ends. This is the arm a
    // level rule broke (rail flipped back to selectable on the emptied
    // manifest and out-raced Zellij's deferred tab-closing render); the latch
    // keeps the rail passive so Zellij's close wins.
    session.run_action(&["write-chars", "exit"]);
    session.run_action(&["write", "13"]);
    let ended = session.wait_until(std::time::Duration::from_secs(10), |s| s.pty_text().contains("Bye from Zellij"));
    assert!(
        ended && !session.is_alive(),
        "`exit` in the tab's last shell must end the single-tab session; it is still alive\nscreen:\n{}",
        screen_text(&session.screen())
    );
    eprintln!("[e2e] PASS: rail-only tab survived, went passive once a shell joined, and `exit` still closed it");
}

/// A real left click on the exact, one-based screen coordinate of the pending
/// glyph acknowledges through Zellij's mouse routing: the plugin asks Zellij to
/// mark the pane seen and the next manifest drops the hotspot. This is the
/// layer that the unreachable right-click implementation could never exercise
/// (zellij-org/zellij#5350).
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn clicking_pending_glyph_acknowledges_the_pane() {
    let wasm = plugin_wasm_path();
    let temp_home = pre_grant_permissions(&wasm);
    let session_name = format!("zjr_ack_glyph_{}", std::process::id());
    let session = ZellijSession::start(&session_name, &sidebar_layout(&wasm), &wasm, temp_home);
    session.emit_program_status(&osc("blocked", "approve glyph"));

    let ready =
        session.wait_until(std::time::Duration::from_secs(5), |s| sidebar_region(&s.screen(), 32).contains('✓'));
    let screen = session.screen();
    let sidebar = sidebar_region(&screen, 32);
    assert!(ready, "pending glyph must be visible before clicking; rail:\n{sidebar}");
    // The acknowledge hotspot sits on the first line that carries a `✓` (the
    // tab line; a tab hotspot acknowledges every pending pane in the tab).
    let row = sidebar.lines().position(|line| line.contains('✓')).expect("a line with the pending glyph");
    let glyph_col = sidebar
        .lines()
        .nth(row)
        .and_then(|line| {
            let prefix = line.split_once('✓')?.0;
            Some(unicode_width::UnicodeWidthStr::width(prefix))
        })
        .expect("the pane line's exact checkmark column") as u16
        + 1;

    // `click_at` is one-based screen space, whereas the runtime receives the
    // zero-based display-cell offset. The +1 is explicit at this boundary.
    let acknowledged = session.wait_until(std::time::Duration::from_secs(8), |s| {
        s.click_at(glyph_col, row as u16 + 1);
        !sidebar_region(&s.screen(), 32).contains('✓')
    });
    let after = sidebar_region(&session.screen(), 32);
    eprintln!("[e2e] pending glyph click col={}, row={} rail after:\n{}", glyph_col, row + 1, after);
    assert!(acknowledged, "exact glyph click must mark the pane seen and remove its hotspot; rail:\n{after}");
}

/// Multi-agent scenario: a working agent and a blocked ("needs-you") agent in
/// two different panes. Assert that the needs-you content ("approve") appears
/// in the rendered sidebar.
///
/// Uses a unique session name to avoid collisions with the smoke test when
/// both run in the same `cargo test` invocation.
///
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn multi_agent_needs_you_is_visible() {
    let wasm = plugin_wasm_path();
    assert!(
        wasm.exists(),
        "Plugin wasm not found at {:?}. Build it first:\n  cargo build --release --target wasm32-wasip1",
        wasm
    );

    let temp_home = pre_grant_permissions(&wasm);
    let session_name = format!("zjr_multi_{}", std::process::id());
    // Use a two-terminal layout so the plugin's tab_panes has TWO entries for
    // the tab, allowing both agents' states to aggregate and render.
    let layout = sidebar_layout_two_terminal(&wasm);

    eprintln!("[e2e] starting multi-agent session '{}' with two-terminal layout", session_name);
    let session = ZellijSession::start(&session_name, &layout, &wasm, temp_home);
    eprintln!("[e2e] plugin loaded");

    // A working agent in the focused pane, a blocked one in its sibling.
    session.emit_program_status(&osc("working", "building"));
    session.emit_program_status_in_next_pane(&osc("blocked", "approve?"));

    // Poll the vt100 sidebar region (not pty_text) for the needs-you agent's
    // activity — the pending pane is highest-severity, so its msg drives the
    // tab's detail line. Returns as soon as the frame is ready.
    let appeared = session.wait_until(std::time::Duration::from_secs(5), |s| {
        let sb = sidebar_region(&s.screen(), 32);
        sb.contains("approve")
    });
    let sidebar = sidebar_region(&session.screen(), 32);
    eprintln!("[e2e] sidebar region (32 cols):\n{}", sidebar);
    assert!(appeared, "pending (needs-you) agent must surface in the sidebar;\nsidebar:\n{sidebar}");

    eprintln!("[e2e] PASS: needs-you agent appeared in the rendered sidebar");
}

/// Line-per-pane structure test: two TRACKED panes in one tab must each render
/// as a distinct line in the sidebar (the redesign's doc scenario H).
///
/// Reports distinct statuses from both terminal panes and asserts — using
/// only the SIDEBAR REGION (left 32 columns of the vt100-parsed screen) — that
/// each pane's message appears on its own line. This avoids the false-positive
/// trap of checking `pty_text()`, which also contains the terminal panes'
/// scrollback.
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn multi_pane_renders_one_line_per_pane() {
    let wasm = plugin_wasm_path();
    assert!(
        wasm.exists(),
        "Plugin wasm not found at {:?}. Build it first:\n  cargo build --release --target wasm32-wasip1",
        wasm
    );

    let temp_home = pre_grant_permissions(&wasm);
    let session_name = format!("zjr_lineper_{}", std::process::id());
    let layout = sidebar_layout_two_terminal(&wasm);

    eprintln!("[e2e] starting line-per-pane session '{}' with two-terminal layout", session_name);
    let session = ZellijSession::start(&session_name, &layout, &wasm, temp_home);
    eprintln!("[e2e] plugin loaded");

    // Pane A (focused) and pane B (its sibling) each report a distinct message.
    session.emit_program_status(&osc("working", "taska"));
    session.emit_program_status_in_next_pane(&osc("working", "taskb"));

    // Extra settle time so both renders complete before we snapshot.
    std::thread::sleep(std::time::Duration::from_millis(600));

    // Parse the final PTY frame through vt100 and extract the left 32 columns.
    let screen = session.screen();
    let sidebar = sidebar_region(&screen, 32);
    eprintln!("[e2e] sidebar region (32 cols):\n{}", sidebar);

    // Each row of the sidebar is a separate line in `sidebar`.
    // We look for: two different lines — one containing "taska", another
    // containing "taskb" — proving the line-per-pane layout.
    let lines: Vec<&str> = sidebar.lines().collect();

    let line_a = lines.iter().position(|l| l.contains("taska"));
    let line_b = lines.iter().position(|l| l.contains("taskb"));

    eprintln!("[e2e] pane-A message on sidebar row: {:?}", line_a);
    eprintln!("[e2e] pane-B message on sidebar row: {:?}", line_b);

    assert!(line_a.is_some(), "pane A (msg='taska') must appear in the sidebar region;\nsidebar:\n{}", sidebar);
    assert!(line_b.is_some(), "pane B (msg='taskb') must appear in the sidebar region;\nsidebar:\n{}", sidebar);
    assert_ne!(
        line_a, line_b,
        "pane A and pane B must appear on DIFFERENT sidebar lines (line-per-pane layout);\n\
         line_a={:?}, line_b={:?}\nsidebar:\n{}",
        line_a, line_b, sidebar
    );

    eprintln!("[e2e] PASS: two panes rendered on distinct sidebar lines (row {:?} vs row {:?})", line_a, line_b);
}

/// Focus cue, on the real frame: in the active multi-pane tab the focused
/// pane's line is painted on the header's bright band and its sibling on the
/// dimmer child band — and the cue follows `zellij action focus-next-pane`.
/// Also probes what focusing the rail itself does to the cue.
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn focused_pane_line_is_highlighted_and_follows_focus() {
    let wasm = plugin_wasm_path();
    assert!(wasm.exists(), "Plugin wasm not found at {:?}", wasm);

    let temp_home = pre_grant_permissions(&wasm);
    let session_name = format!("zjr_focus_{}", std::process::id());
    let layout = sidebar_layout_two_terminal(&wasm);
    let session = ZellijSession::start(&session_name, &layout, &wasm, temp_home);

    session.emit_program_status(&osc("working", "taska"));
    session.emit_program_status_in_next_pane(&osc("working", "taskb"));
    assert!(session.wait_for_sidebar(32, "taskb", Duration::from_secs(5)), "both panes must render");

    // (row bg of pane A's line, row bg of pane B's line, row bg of the tab header)
    let bands = |s: &ZellijSession| {
        let screen = s.screen();
        let a = sidebar_row_index(&screen, 32, "taska").expect("taska row");
        let b = sidebar_row_index(&screen, 32, "taskb").expect("taskb row");
        let header = a.min(b) - 1;
        let bg = |row: usize| sidebar_row_bg_rgb(&screen, row as u16, 32);
        (bg(a), bg(b), bg(header), sidebar_region(&screen, 32))
    };

    // Pane A focused: A takes the header's band, B the dimmer child band.
    let lit_a = |s: &ZellijSession| {
        let (a, b, h, _) = bands(s);
        a == h && b != h
    };
    let ok = session.wait_until(Duration::from_secs(3), lit_a);
    let (a, b, h, sidebar) = bands(&session);
    eprintln!("[e2e] focus A: a={a:?} b={b:?} header={h:?}\n{sidebar}");
    assert!(ok, "focused pane A must share the header band, B must not: a={a:?} b={b:?} header={h:?}\n{sidebar}");

    // Move focus to B: the highlight must follow.
    session.run_action_checked(&["focus-next-pane"]);
    let lit_b = |s: &ZellijSession| {
        let (a, b, h, _) = bands(s);
        b == h && a != h
    };
    let ok = session.wait_until(Duration::from_secs(3), lit_b);
    let (a, b, h, sidebar) = bands(&session);
    eprintln!("[e2e] focus B: a={a:?} b={b:?} header={h:?}\n{sidebar}");
    assert!(ok, "after focus-next-pane, B must take the header band: a={a:?} b={b:?} header={h:?}\n{sidebar}");

    // Clicking an empty rail row must not blank the cue: the rail is
    // unselectable once a terminal shares its tab (`desired_selectable`), so
    // the click never moves focus off pane B.
    session.click_at(10, 22);
    std::thread::sleep(Duration::from_millis(800));
    let (a, b, h, sidebar) = bands(&session);
    eprintln!("[e2e] after rail click: a={a:?} b={b:?} header={h:?}\n{sidebar}");
    assert!(b == h && a != h, "a rail click must leave B highlighted: a={a:?} b={b:?} header={h:?}\n{sidebar}");

    // A floating pane: Zellij reports focus per layer, so tiled B keeps
    // `is_focused` while the floating pane is focused too. The shown floating
    // layer holds the user's focus — the cue must move to it.
    session.run_action_checked(&["new-pane", "--floating"]);
    std::thread::sleep(Duration::from_millis(800));
    session.emit_program_status(&osc("working", "taskf"));
    let bands3 = |s: &ZellijSession| {
        let screen = s.screen();
        let row = |needle| sidebar_row_index(&screen, 32, needle);
        let (a, b, f) = (row("taska")?, row("taskb")?, row("taskf")?);
        let bg = |r: usize| sidebar_row_bg_rgb(&screen, r as u16, 32);
        Some((bg(a), bg(b), bg(f), bg(a.min(b).min(f) - 1), sidebar_region(&screen, 32)))
    };
    let lit_f = |s: &ZellijSession| bands3(s).is_some_and(|(a, b, f, h, _)| f == h && a != h && b != h);
    let ok = session.wait_until(Duration::from_secs(4), lit_f);
    let state = bands3(&session);
    eprintln!("[e2e] floating focused: {state:?}");
    assert!(ok, "a focused, shown floating pane takes the cue: {state:?}");

    // Hide the floating layer: focus is back on tiled B.
    session.run_action_checked(&["toggle-floating-panes"]);
    let lit_b_again = |s: &ZellijSession| bands3(s).is_some_and(|(a, b, f, h, _)| b == h && a != h && f != h);
    let ok = session.wait_until(Duration::from_secs(4), lit_b_again);
    let state = bands3(&session);
    eprintln!("[e2e] floating hidden: {state:?}");
    assert!(ok, "with the floating layer hidden, tiled B holds the cue again: {state:?}");
}

/// Rendered-output fidelity: assert on the ACTUAL cells Zellij painted — both
/// text and color — not just substring match in the PTY stream.
///
/// After a working agent reports from the focused pane, the parsed sidebar must show:
///   1. the " RADAR" header on row 0;
///   2. the agent card as TWO rows — a tab row carrying the focus spine `▌` and a
///      running spinner glyph (one of `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`), and the next row carrying the
///      activity (`building`) and the Claude identity mark `✳` — proving the
///      two-line agent card layout survived the full plugin → Zellij → PTY
///      round-trip;
///   3. the focused card row painted with a truecolor surface band that DIFFERS
///      from the header's rail band — proving the Cards 3-tint hierarchy renders
///      end-to-end (the unit `tint_map` oracle, but on the real frame).
///
/// Robust to the spinner tick, pane-id assignment, and timing: it pins structure
/// and the color *relationship*, not exact RGB or a byte-for-byte frame.
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn rendered_sidebar_paints_focused_card_with_text_and_tint() {
    let wasm = plugin_wasm_path();
    assert!(
        wasm.exists(),
        "Plugin wasm not found at {:?}. Build it first:\n  cargo build --release --target wasm32-wasip1",
        wasm
    );

    let temp_home = pre_grant_permissions(&wasm);
    let session_name = format!("zjr_render_{}", std::process::id());
    let layout = sidebar_layout(&wasm);
    let session = ZellijSession::start(&session_name, &layout, &wasm, temp_home);
    eprintln!("[e2e] plugin loaded");

    // Working agent on the focused pane → its tab is the active (focused) card.
    session.emit_program_status(&osc("working", "building"));
    std::thread::sleep(std::time::Duration::from_millis(800));

    let screen = session.screen();
    let sidebar = sidebar_region(&screen, 32);
    eprintln!("[e2e] sidebar region (32 cols):\n{}", sidebar);

    // (1) Header on row 0.
    let row0 = sidebar.lines().next().unwrap_or("");
    assert!(row0.contains("RADAR"), "row 0 must be the ' RADAR' header; got {row0:?}\nsidebar:\n{sidebar}");

    // (2) The agent card is two rows: a detail row carrying the activity +
    //     Claude mark, and the tab row above it carrying the focus spine + a
    //     running spinner glyph. Locate by the activity text (reliably rendered).
    let lines: Vec<&str> = sidebar.lines().collect();
    let detail_idx = sidebar_row_index(&screen, 32, "building")
        .unwrap_or_else(|| panic!("the agent activity 'building' must render in the sidebar;\nsidebar:\n{sidebar}"));
    assert!(
        lines[detail_idx].contains('✳'),
        "the Claude identity mark '✳' must render on the activity row; got {:?}\nsidebar:\n{sidebar}",
        lines[detail_idx]
    );
    let tab_idx = detail_idx.checked_sub(1).expect("the agent tab row must sit above its activity row");
    let tab_row = lines[tab_idx];
    assert!(
        tab_row.contains('▌'),
        "the focused agent's tab row must carry the focus spine '▌'; got {tab_row:?}\nsidebar:\n{sidebar}"
    );
    const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    assert!(
        tab_row.chars().any(|c| SPINNER.contains(&c)),
        "the running agent's tab row must carry a spinner glyph (one of {SPINNER:?}); got {tab_row:?}\nsidebar:\n{sidebar}"
    );

    // (3) The focused card row is painted with a truecolor surface band that
    //     differs from the header's rail band — the 3-tint hierarchy is visible
    //     in the real rendered frame.
    let card_bg = sidebar_row_bg_rgb(&screen, tab_idx as u16, 32);
    let header_bg = sidebar_row_bg_rgb(&screen, 0, 32);
    eprintln!("[e2e] focused-card bg={:?}, header bg={:?}", card_bg, header_bg);
    assert!(
        card_bg.is_some(),
        "focused card row must be painted with a truecolor surface in the real frame;\nsidebar:\n{sidebar}"
    );
    assert!(
        header_bg.is_some(),
        "header row must carry the truecolor rail band in the real frame;\nsidebar:\n{sidebar}"
    );
    assert_ne!(
        card_bg, header_bg,
        "the focused card tint must differ from the header rail band (3-tint hierarchy);\nsidebar:\n{sidebar}"
    );

    eprintln!("[e2e] PASS: focused card rendered with correct text + a distinct surface tint");
}

/// Behavior: a completion the user is WATCHING is seen at once — Zellij reads
/// the focused pane's `done` as seen, so the card does not stay lit — and the
/// rail files it in the `─ earlier` ledger from the pane's remembered record.
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn focused_done_enters_the_ledger() {
    let wasm = plugin_wasm_path();
    assert!(wasm.exists(), "Plugin wasm not found at {:?}", wasm);

    let temp_home = pre_grant_permissions(&wasm);
    let session_name = format!("zjr_recede_{}", std::process::id());
    let layout = sidebar_layout(&wasm);
    let session = ZellijSession::start(&session_name, &layout, &wasm, temp_home);

    session.emit_program_status(&osc("working", "deploying"));
    assert!(
        session.wait_for_sidebar(32, "deploying", Duration::from_secs(5)),
        "precondition: the working agent's activity must render first;\nsidebar:\n{}",
        sidebar_region(&session.screen(), 32)
    );

    // It finishes while the pane is still focused.
    session.emit_program_status(&osc("done", "deployed"));
    let filed = session.wait_until(Duration::from_secs(6), |s| {
        sidebar_region(&s.screen(), 32).split_once("─ earlier").is_some_and(|(_, after)| after.contains("deployed"))
    });
    let sidebar = sidebar_region(&session.screen(), 32);
    eprintln!("[e2e] post-done sidebar (32 cols):\n{sidebar}");
    assert!(filed, "the finished turn should resurface as an `─ earlier` ledger row;\nsidebar:\n{sidebar}");
}

/// Behavior: an error the user is watching is seen at once too — Zellij reads
/// the focused pane's `error` as seen like a `done` — and lands in the
/// `─ earlier` ledger. The sibling of `focused_done_enters_the_ledger`.
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn focused_error_enters_the_ledger() {
    let wasm = plugin_wasm_path();
    assert!(
        wasm.exists(),
        "Plugin wasm not found at {:?}. Build it first:\n  cargo build --release --target wasm32-wasip1",
        wasm
    );

    let temp_home = pre_grant_permissions(&wasm);
    let session_name = format!("zjr_errstay_{}", std::process::id());
    let layout = sidebar_layout(&wasm);
    let session = ZellijSession::start(&session_name, &layout, &wasm, temp_home);
    // An error finishes on the focused pane.
    session.emit_program_status(&osc("error", "tests failed"));
    let filed = session.wait_until(Duration::from_secs(6), |s| {
        sidebar_region(&s.screen(), 32).split_once("─ earlier").is_some_and(|(_, after)| after.contains("tests failed"))
    });
    let sidebar = sidebar_region(&session.screen(), 32);
    eprintln!("[e2e] post-error sidebar (32 cols):\n{sidebar}");
    assert!(filed, "the failed turn should resurface as an `─ earlier` ledger row;\nsidebar:\n{sidebar}");
}

/// THE headline interaction, end-to-end: a real mouse click on a tab's rail row
/// switches Zellij's active tab to that tab. Host tests pin the click→SwitchTab
/// resolution against the lockstep target map, but until now nothing exercised it
/// through an actual Zellij mouse event.
///
/// Setup: tab 1 carries the sidebar (from the layout). We open a second tab, give
/// each tab a tracked agent row (so both render a clickable rail line), then
/// return to tab 1 — the only tab showing the rail. We click tab 2's row and
/// assert Zellij switched the active tab, observed via `dump-screen` now showing
/// tab 2's terminal (its `ZPID=<pane2>` marker) — a signal *external* to the
/// rail's own rendering, so it proves the SwitchTab effect actually reached
/// Zellij rather than the rail merely repainting.
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn click_on_a_tab_row_switches_the_active_tab() {
    let wasm = plugin_wasm_path();
    assert!(
        wasm.exists(),
        "Plugin wasm not found at {:?}. Build it first:\n  cargo build --release --target wasm32-wasip1",
        wasm
    );

    let temp_home = pre_grant_permissions(&wasm);
    let session_name = format!("zjr_click_{}", std::process::id());
    let layout = sidebar_layout(&wasm);
    let session = ZellijSession::start(&session_name, &layout, &wasm, temp_home);

    // Tab 1's terminal pane (echoes ZPID=<pane1>) + a tracked agent row ("alpha").
    let pane1 = session.discover_terminal_pane_id();
    eprintln!("[e2e] tab1 pane_id={}", pane1);
    session.emit_program_status(&osc("working", "alpha"));

    // Open tab 2 (default layout → no sidebar). new-tab focuses it; discover its
    // pane id (echoes ZPID=<pane2>), then give it a tracked row ("beta"). The
    // tab-1 plugin instance sees both tabs' panes via the global pane manifest,
    // so its rail renders a row for tab 2 even though tab 2 has no sidebar.
    session.run_action(&["new-tab"]);
    std::thread::sleep(std::time::Duration::from_millis(800));
    let pane2 = session.discover_terminal_pane_id();
    eprintln!("[e2e] tab2 pane_id={}", pane2);
    assert_ne!(pane1, pane2, "tab 2 must have a distinct terminal pane id");
    session.emit_program_status(&osc("working", "beta"));

    // Return to tab 1 — the only tab showing the rail we will click. go-to-tab is
    // fire-and-forget and can be dropped under load, so poll-retry it until
    // dump-screen confirms tab 1's terminal (ZPID=pane1) is focused. This both
    // performs the switch and serves as the precondition.
    let on_tab1 = session.wait_until(std::time::Duration::from_secs(8), |s| {
        s.run_action(&["go-to-tab", "1"]);
        s.dump_screen().contains(&format!("ZPID={pane1}"))
    });
    assert!(on_tab1, "could not return to tab 1 before the click");

    // Both tab rows must render in tab 1's rail before we can click tab 2's.
    // Retain the exact screen that satisfied the poll: a subsequent timer render
    // may otherwise make the row lookup observe a different frame.
    let mut screen = session.screen();
    let ready = session.wait_until(std::time::Duration::from_secs(6), |s| {
        screen = s.screen();
        let sb = sidebar_region(&screen, 32);
        sb.contains("alpha") && sb.contains("beta")
    });
    let sidebar = sidebar_region(&screen, 32);
    eprintln!("[e2e] rail before click (32 cols):\n{}", sidebar);
    assert!(ready, "both tab rows must render in tab 1's rail before clicking;\nsidebar:\n{sidebar}");

    // Click tab 2's rail row. Every rendered line of a tab's card carries that
    // tab's SwitchTab target, so the "beta" detail row is a valid click point.
    // Re-issue the click on each poll: a single synthetic mouse event can be
    // dropped under load, and re-clicking is idempotent (clicking tab 2 once it
    // is already active is a no-op). Assert the switch via dump-screen showing
    // tab 2's terminal — external to the rail's own rendering, so it proves the
    // SwitchTab effect reached Zellij.
    let row2 = sidebar_row_index(&screen, 32, "beta").expect("tab 2 row must be locatable in the rail");
    let click_row = (row2 + 1) as u16;
    eprintln!("[e2e] clicking tab 2 row at sidebar index {} (screen row {})", row2, click_row);
    let switched = session.wait_until(std::time::Duration::from_secs(8), |s| {
        s.click_at(3, click_row);
        s.dump_screen().contains(&format!("ZPID={pane2}"))
    });
    eprintln!("[e2e] dump-screen after click:\n{}", session.dump_screen());
    assert!(
        switched,
        "clicking tab 2's rail row must switch Zellij's active tab to tab 2 \
         (dump-screen should show ZPID={pane2})"
    );

    eprintln!("[e2e] PASS: a real rail click switched the active tab end-to-end");
}

/// A manual tab name must survive the user switching to another tab — i.e. to
/// another plugin instance. Pre-fix every instance held rename authority over
/// every tab, so a background instance could land a rename computed from a
/// stale snapshot on top of the user's name. Two tabs is the smallest
/// configuration with a foreign instance; the bug itself is a timing race that
/// does not reproduce deterministically, so this is a live probe of the
/// ownership path (the owner names its tab; a foreign instance never renames
/// it), not a regression pin — `sidebar_instance_never_renames_a_foreign_tab`
/// in the runtime tests is the pin.
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn manual_tab_name_survives_switch_to_another_plugin_instance() {
    use std::time::{Duration, Instant};

    let wasm = plugin_wasm_path();
    assert!(
        wasm.exists(),
        "Plugin wasm not found at {wasm:?}. Build it:\n  cargo build --release --target wasm32-wasip1 -p zj-radar-plugin"
    );
    let isolated = pre_grant_permissions(&wasm);
    let session_name = format!("zjr_manual_name_{}", std::process::id());
    let layout = runtime_sidebar_tabs_layout(&wasm);
    let session = ZellijSession::start(&session_name, &layout, &wasm, isolated);

    // Tab 1 is active. Leave a terminal marker so the navigation back to it is
    // confirmed independently of either tab's mutable name.
    let pane1 = session.discover_terminal_pane_id();

    // Create tab 2 through `new_tab_template` and wait for its OWN instance to
    // auto-name it: a non-default name proves the new instance loaded and its
    // ownership resolved (the user renames a normal, fully-created tab).
    session.run_action(&["new-tab", "--cwd", "/home"]);
    let tab2_ready = session.wait_until(Duration::from_secs(10), |s| {
        let names = s.tab_names();
        names.len() == 2 && names.last().is_some_and(|name| !name.starts_with("Tab #"))
    });
    assert!(tab2_ready, "tab 2 never completed its initial auto-name; got {:?}", session.tab_names());

    // Rename tab 2 by its stable id, not the focused tab. `rename-tab` with no
    // id targets whatever tab the attached client has focused, and after
    // `new-tab` that focus can still be settling — the rename then lands on
    // nothing observable and the test flakes with "never landed" (issue #34).
    // Zellij tab ids are 0-based (the first tab is id 0), so the second tab is
    // id 1 — distinct from `go-to-tab`, which is 1-based by position.
    let manual_name = "keep-this-manual-name";
    session.run_action_checked(&["rename-tab", "--tab-id", "1", manual_name]);
    let (renamed, timeline) = session
        .sample_tab_names_until(Duration::from_secs(5), |names| names.get(1).is_some_and(|name| name == manual_name));
    assert!(renamed, "manual rename never landed; tab-name timeline (elapsed → names) was {timeline:?}");

    // Switch to tab 1 (a different instance) and hold on Zellij's authoritative
    // tab names for a few seconds: a rename from any instance would show here.
    let on_tab1 = session.wait_until(Duration::from_secs(8), |s| {
        s.run_action(&["go-to-tab", "1"]);
        s.dump_screen().contains(&format!("ZPID={pane1}"))
    });
    assert!(on_tab1, "could not navigate from tab 2 to tab 1");

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut observed_preserved = false;
    let mut overwritten = None;
    let mut last_names = Vec::new();
    while Instant::now() < deadline {
        last_names = session.tab_names();
        match last_names.get(1) {
            Some(name) if name == manual_name => observed_preserved = true,
            Some(_) => {
                overwritten = Some(last_names.clone());
                break;
            }
            // `query-tab-names` can briefly return an empty successful response
            // while the client switches tabs. That is no observation, not proof
            // that the host replaced the name; keep polling for a full snapshot.
            None => {}
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        overwritten.is_none(),
        "switching tabs overwrote tab 2's manual name; names={:?};\nsidebar:\n{}",
        overwritten.unwrap_or_default(),
        sidebar_region(&session.screen(), 32)
    );
    assert!(
        observed_preserved,
        "query-tab-names never returned a complete post-switch snapshot; last names={last_names:?};\nsidebar:\n{}",
        sidebar_region(&session.screen(), 32)
    );
}

/// A pane's attention reaches the plugin instance in *every* tab, not just the
/// tab whose pane reported it: a status emitted in tab one must appear in tab
/// two's rail too (Zellij hands the new manifest to the instance of the tab
/// that becomes active).
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn native_attention_reaches_background_tab_instances() {
    use std::time::Duration;
    let wasm = plugin_wasm_path();
    assert!(
        wasm.exists(),
        "Plugin wasm not found at {wasm:?}. Build it:\n  cargo build --release --target wasm32-wasip1 -p zj-radar-plugin"
    );
    let temp_home = pre_grant_permissions(&wasm);
    let session_name = format!("zjr_xclear_{}", std::process::id());
    let layout = two_sidebar_tabs_layout(&wasm);
    let session = ZellijSession::start(&session_name, &layout, &wasm, temp_home);

    // Discover each tab's terminal pane id. The rail lists ALL tab names on every
    // tab, so switches are confirmed by the per-tab `ZPID` echo (unique per pane),
    // not by rail text. go-to-tab is fire-and-forget, so retry until discovery
    // returns tab two's (distinct) pane id.
    let pane1 = session.discover_terminal_pane_id();
    let mut pane2 = pane1;
    for _ in 0..10 {
        session.run_action(&["go-to-tab", "2"]);
        std::thread::sleep(Duration::from_millis(500));
        let p = session.discover_terminal_pane_id();
        if p != pane1 && p != 0 {
            pane2 = p;
            break;
        }
    }
    eprintln!("[e2e] pane1(tab one)={pane1}  pane2(tab two)={pane2}");
    assert_ne!(pane1, pane2, "could not switch to tab two to discover its pane");

    // Back to tab one, then report a working agent from its terminal.
    let back1 = session.wait_until(Duration::from_secs(8), |s| {
        s.run_action(&["go-to-tab", "1"]);
        s.dump_screen().contains(&format!("ZPID={pane1}"))
    });
    assert!(back1, "could not return to tab one to report the status");
    session.emit_program_status(&osc("working", "elsewhere"));

    // Tab one's OWN instance must show it (baseline).
    let seen_on_1 = session.wait_for_sidebar(32, "elsewhere", Duration::from_secs(10));
    assert!(seen_on_1, "tab one's own rail should show the status;\n{}", sidebar_region(&session.screen(), 32));

    // THE QUESTION: switch to tab two and read ITS rail. Does the other
    // instance also show tab one's agent?
    let on2 = session.wait_until(Duration::from_secs(8), |s| {
        s.run_action(&["go-to-tab", "2"]);
        s.dump_screen().contains(&format!("ZPID={pane2}"))
    });
    assert!(on2, "could not switch to tab two to read its rail");
    let converged =
        session.wait_until(Duration::from_secs(6), |s| sidebar_region(&s.screen(), 32).contains("elsewhere"));
    let tab2 = sidebar_region(&session.screen(), 32);
    eprintln!("[e2e] tab-two rail (other instance):\n{tab2}");
    assert!(converged, "CONVERGENCE: tab two's instance does NOT show tab one's agent. tab-two rail:\n{tab2}");
}

/// The first-run grant path: with no cached grant, the rail itself calls
/// `request_permission` at load (no onboarding float, no deferral), so Zellij
/// shows its prompt in the rail pane. `ZellijSession::start` answers it the
/// way a user would (focus the rail, press `y`); the rail must then come up
/// past the `needs_permission` face, and the prompt must have listed every
/// requested permission, `ReadPaneContents` included.
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn ungranted_rail_is_enabled_by_its_own_prompt() {
    let wasm = plugin_wasm_path();
    assert!(
        wasm.exists(),
        "Plugin wasm not found at {wasm:?}. Build it:\n  cargo build --release --target wasm32-wasip1 -p zj-radar-plugin"
    );

    // Ungranted: an isolated HOME with nothing in permissions.kdl.
    let session_name = format!("zjr_grant_{}", std::process::id());
    let session = ZellijSession::start(&session_name, &sidebar_layout(&wasm), &wasm, isolated_temp_home());

    let pty = session.pty_text();
    assert!(pty.contains("Allow? (y/n)"), "an ungranted rail must prompt from its own pane");
    assert!(pty.contains("Read pane contents"), "the prompt must request ReadPaneContents");
    let rail = sidebar_region(&session.screen(), 32);
    eprintln!("[e2e] post-grant sidebar (32 cols):\n{rail}");
    assert!(!rail.contains("needs permission"), "the answered prompt must enable the rail;\nsidebar:\n{rail}");
}

/// A session created detached (`zellij attach --create-background`) loads its
/// rails with no client connected. On Zellij 0.45.1 the cached grant's
/// `PermissionRequestResult` was lost in that window and the rails stayed on
/// the `needs_permission` face forever. With a pre-seeded grant, once a client
/// attaches every rail must be live: both tabs' rails render the tab rows, and
/// a click on tab one's rail switches to tab two.
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn background_created_session_rails_get_their_grant() {
    let wasm = plugin_wasm_path();
    assert!(
        wasm.exists(),
        "Plugin wasm not found at {wasm:?}. Build it:\n  cargo build --release --target wasm32-wasip1 -p zj-radar-plugin"
    );
    let session_name = format!("zjr_bg_{}", std::process::id());
    let session =
        ZellijSession::start_background(&session_name, &two_sidebar_tabs_layout(&wasm), pre_grant_permissions(&wasm));

    let live = |s: &ZellijSession| {
        let rail = sidebar_region(&s.screen(), 32);
        rail.contains("one") && rail.contains("two") && !rail.contains("needs permission")
    };

    // Tab one's rail: granted, so it lists both tabs.
    let mut screen = session.screen();
    let one_live = session.wait_until(Duration::from_secs(10), |s| {
        screen = s.screen();
        live(s)
    });
    let rail = sidebar_region(&screen, 32);
    eprintln!("[e2e] tab-one rail after attach:\n{rail}");
    assert!(one_live, "tab one's rail must leave the needs-permission face after attach;\nsidebar:\n{rail}");

    // A real click on tab two's row must switch tabs: tab one's terminal
    // (ZPID=pane1) is no longer the focused pane.
    let pane1 = session.discover_terminal_pane_id();
    let row = sidebar_row_index(&session.screen(), 32, "two").expect("tab two's row must be in the rail");
    let switched = session.wait_until(Duration::from_secs(8), |s| {
        s.click_at(3, (row + 1) as u16);
        let p = s.discover_terminal_pane_id();
        p != 0 && p != pane1
    });
    assert!(switched, "clicking tab two's row must switch the active tab;\n{}", session.dump_screen());

    // Tab two's rail is a second instance with its own request.
    let two_live = session.wait_until(Duration::from_secs(10), live);
    let rail = sidebar_region(&session.screen(), 32);
    eprintln!("[e2e] tab-two rail:\n{rail}");
    assert!(two_live, "tab two's rail must leave the needs-permission face;\nsidebar:\n{rail}");
}

/// Regression pin: the rail paints every column of its pane, not just most of
/// it. Reported live (ghostty): the focused card's bright background band
/// appeared to stop 1-2 columns short of the rail pane's right edge. Reading
/// `crates/plugin/src/lib.rs::render` and `crates/plugin/src/render.rs`'s
/// `paint_card_line`/`render_header` showed every painted band is padded to
/// exactly the `width` Zellij hands the plugin — so a real shortfall could
/// only come from Zellij reporting fewer `cols` than the pane's true
/// on-screen width (an off-by-N upstream of the plugin).
///
/// This drives a REAL Zellij (`size=32 borderless=true` rail, mirroring the
/// live layout) with a focused, active card (`tab.active` selects
/// `surface_active` — the same tint implicated in the report) and reads the
/// OUTER vt100 screen — the terminal's own view of what Zellij emitted, not
/// anything internal to the plugin. It pins: painted-last-col ==
/// pane-last-col (30: under Zellij 0.46's default `pane_frame_style
/// "titles"`, a `size=32 borderless=true` pane that is not on the right edge
/// keeps column 31 for the boundary, so its content is columns `0..=30`) on
/// both the header row and the focused card's row.
///
/// A 41-outer-width sweep and live mid-session resizes were also run during
/// the investigation and found zero gap at every point; those aren't kept
/// here (too slow for CI). This test is the load-bearing pin: it proves the
/// plugin-output → outer-terminal seam is gap-free, so the live shortfall is
/// a ghostty-side presentation artifact, not a zj-radar under-paint bug.
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn rail_paints_every_column_of_its_pane() {
    let wasm = plugin_wasm_path();
    assert!(
        wasm.exists(),
        "Plugin wasm not found at {wasm:?}. Build it:\n  cargo build --release --target wasm32-wasip1 -p zj-radar-plugin"
    );

    let temp_home = pre_grant_permissions(&wasm);
    let session_name = format!("zjr_probe3_{}", std::process::id());
    // Mirrors the user's live layout shape: rail pane size=32 borderless=true
    // on the left, content pane on the right. This is exactly `sidebar_layout`.
    let layout = sidebar_layout(&wasm);
    eprintln!("[e2e] layout:\n{layout}");
    let session = ZellijSession::start(&session_name, &layout, &wasm, temp_home);
    eprintln!("[e2e] plugin loaded");

    // Active/working status on the sole tab's pane -> tab.active is true (only
    // one tab exists and it's the focused one) -> card_tint picks
    // theme.surface_active for its row (crates/plugin/src/render.rs card_tint).
    session.emit_program_status(&osc("working", "building"));
    std::thread::sleep(std::time::Duration::from_millis(800));

    let screen = session.screen();
    let (screen_rows, screen_cols) = screen.size();
    eprintln!("[e2e] outer vt100 screen size: rows={screen_rows} cols={screen_cols}");

    let sidebar = sidebar_region(&screen, 40);
    eprintln!("[e2e] left-40-col region:\n{sidebar}");

    // Locate the focused card's tab row (spine '▌' + spinner), same technique
    // as `rendered_sidebar_paints_focused_card_with_text_and_tint`.
    let detail_idx =
        sidebar_row_index(&screen, 40, "building").expect("the agent activity 'building' must render in the sidebar");
    let tab_idx = detail_idx.checked_sub(1).expect("the agent tab row must sit above its activity row") as u16;
    eprintln!("[e2e] header row=0, focused-card tab row={tab_idx}, detail row={detail_idx}");

    // Per-column background dump, columns 0..40, for both rows.
    fn dump_row_bg(screen: &vt100::Screen, row: u16, upto: u16) -> Vec<(u16, vt100::Color, String)> {
        (0..upto)
            .map(|c| {
                let cell = screen.cell(row, c);
                let bg = cell.map(|x| x.bgcolor()).unwrap_or(vt100::Color::Default);
                let ch = cell.map(|x| x.contents()).unwrap_or_default();
                (c, bg, ch)
            })
            .collect()
    }

    // End of the painted run that starts at col 0: since Zellij 0.46 the
    // sibling pane paints its own background, so "last painted col" overall
    // would land in the sibling.
    fn last_painted_col(dump: &[(u16, vt100::Color, String)]) -> Option<u16> {
        dump.iter().take_while(|(_, bg, _)| !matches!(bg, vt100::Color::Default)).last().map(|(c, _, _)| *c)
    }

    fn fmt_dump(dump: &[(u16, vt100::Color, String)]) -> String {
        dump.iter()
            .map(|(c, bg, ch)| {
                let bgs = match bg {
                    vt100::Color::Default => "default".to_string(),
                    vt100::Color::Idx(i) => format!("idx({i})"),
                    vt100::Color::Rgb(r, g, b) => format!("rgb({r},{g},{b})"),
                };
                let chs = if ch.trim().is_empty() { "' '".to_string() } else { format!("{ch:?}") };
                format!("  col={c:2} bg={bgs:<14} ch={chs}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    let header_dump = dump_row_bg(&screen, 0, 40);
    let card_dump = dump_row_bg(&screen, tab_idx, 40);

    eprintln!("[e2e] === header row (0) per-column dump ===\n{}", fmt_dump(&header_dump));
    eprintln!("[e2e] === focused-card row ({tab_idx}) per-column dump ===\n{}", fmt_dump(&card_dump));

    let header_last = last_painted_col(&header_dump);
    let card_last = last_painted_col(&card_dump);

    // The rail pane is `size=32 borderless=true`. Under the default
    // `pane_frame_style "titles"` Zellij still reserves the pane's last column
    // for the boundary to the sibling (`pane_content_offset` in
    // zellij-server's tiled_panes), so the plugin gets 31 columns: [0, 30].
    const EXPECTED_PANE_LAST_COL: u16 = 30; // 0-indexed; 31 content columns.

    eprintln!(
        "[e2e] header last painted col = {:?} (expected pane last col = {})",
        header_last, EXPECTED_PANE_LAST_COL
    );
    eprintln!(
        "[e2e] focused-card last painted col = {:?} (expected pane last col = {})",
        card_last, EXPECTED_PANE_LAST_COL
    );

    // Report what's immediately to the right of the expected boundary, so we
    // can see whether col 32+ is sibling-pane content (proving the plugin's
    // own band stopped at 31) or still rail-colored (proving the plugin
    // painted further right than expected).
    eprintln!(
        "[e2e] columns 28..40 header:  {:?}",
        header_dump[28..40].iter().map(|(c, bg, ch)| format!("{c}:{bg:?}:{ch:?}")).collect::<Vec<_>>()
    );
    eprintln!(
        "[e2e] columns 28..40 card:    {:?}",
        card_dump[28..40].iter().map(|(c, bg, ch)| format!("{c}:{bg:?}:{ch:?}")).collect::<Vec<_>>()
    );

    // Decisive assertions, written so failures self-document the exact
    // shortfall (or confirm full coverage) in the panic message.
    assert_eq!(
        header_last,
        Some(EXPECTED_PANE_LAST_COL),
        "VERDICT DATA: header row's last painted column is {header_last:?}, expected \
         {EXPECTED_PANE_LAST_COL} (pane content's last column, size=32 borderless=true). \
         If this is Some(n) with n < {EXPECTED_PANE_LAST_COL}, the plugin/harness paints \
         exactly up to col n and the remaining columns up to {EXPECTED_PANE_LAST_COL} are \
         unpainted WITHIN the pane region as this outer terminal sees it -> plugin \
         under-paints relative to the pane. If None, no truecolor bg was found at all \
         (broken probe or theme). Full dumps above."
    );
    assert_eq!(
        card_last,
        Some(EXPECTED_PANE_LAST_COL),
        "VERDICT DATA: focused-card row's last painted column is {card_last:?}, expected \
         {EXPECTED_PANE_LAST_COL} (pane region's last column). Same interpretation as the \
         header assertion. Full dumps above."
    );

    eprintln!(
        "[e2e] VERDICT: painted span covers the full advertised pane width \
         (last col {EXPECTED_PANE_LAST_COL}) for both header and focused-card rows \
         in this PTY+vt100 harness. If this test PASSES, the plugin paints every \
         column Zellij hands it and the pane region is fully covered here — any \
         live-ghostty shortfall is not reproduced at the plugin-output/outer-terminal \
         seam this harness observes, pointing at a ghostty-side (or ghostty+font) \
         rendering artifact rather than a plugin under-paint bug."
    );
}

/// Cross-session e2e: TWO independently-started, real Zellij servers — not
/// two tabs of one server (that's `native_attention_reaches_background_tab_
/// instances`) — proving the whole cross-session feature works between
/// separate processes, WITHOUT any session-manager trigger: B's agent
/// attention (OSC 7501) reaches A's plugin through Zellij's
/// `GlobalAttentionUpdate`, A renders a badge line for B, and A's
/// `session-next` (+ idle-commit ~1s later) really calls Zellij's
/// `switch_session`, physically reattaching A's PTY client onto B.
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn session_next_switches_to_the_session_with_attention() {
    use std::time::Duration;

    let wasm = plugin_wasm_path();
    assert!(
        wasm.exists(),
        "Plugin wasm not found at {wasm:?}. Build it:\n  cargo build --release --target wasm32-wasip1 -p zj-radar-plugin"
    );

    let temp_home = pre_grant_permissions(&wasm);
    let pid = std::process::id();
    let layout = sidebar_layout(&wasm);

    // `a` owns the isolated root (deletes it on drop); `b` is a sibling on the
    // SAME private server socket dir + cache root (see `start_sibling`'s doc) — the
    // two servers are otherwise completely independent processes, exactly
    // like two real terminal windows a user opened separately.
    let a = ZellijSession::start(&format!("zjr_xsess_a_{pid}"), &layout, &wasm, temp_home);
    eprintln!("[e2e] session A ({}) loaded", a.name);
    let b = a.start_sibling(&format!("zjr_xsess_b_{pid}"), &layout);
    eprintln!("[e2e] session B ({}) loaded (sibling of A, shared private server)", b.name);

    // Give B an agent needing attention, over OSC 7501: Zellij's attention
    // tracking reports it to A through `GlobalAttentionUpdate` (needs_you=1)
    // — the fact A's badge assertion below needs to have crossed sessions.
    // `msg` is base64 for "pick one".
    let pane_b = b.discover_terminal_pane_id();
    eprintln!("[e2e] B terminal pane_id={pane_b}");
    b.emit_program_status("state=blocked:app=claude-code:kind=question:msg=cGljayBvbmU=");

    // A `working` status in A's OWN terminal arms A's Fast cadence
    // (an active `Running` row animates every tick — see
    // `timer_should_continue`/`needs_fast_ticks`) without giving A any
    // attention of its own, so it doesn't interfere with the ordering
    // assertion below (B is still the only attention>0 peer).
    a.emit_program_status(&osc("working", "keep-fast"));

    // A's badge must show B's name TOGETHER WITH the attention glyph+count —
    // not just B's bare name — this needle also rules out the degenerate
    // case of a badge line with a name but zero counts.
    // `Status::Pending`'s Plain glyph (the `statuses!` table in crates/plugin/src/status.rs).
    let attention_glyph = '◆';
    let needle = format!("{} 1{attention_glyph}", b.name);
    let saw_badge = a.wait_until(Duration::from_secs(10), |s| sidebar_region(&s.screen(), 32).contains(&needle));
    eprintln!("[e2e] A's rail (32 cols):\n{}", sidebar_region(&a.screen(), 32));
    assert!(
        saw_badge,
        "A's badge never showed {needle:?}. Either Zellij never raised B's pane from the OSC 7501 \
         `blocked` record, or A's `GlobalAttentionUpdate` never reached the badge. See the \
         printed rail above.",
    );
    eprintln!("[e2e] PASS: A's badge shows B needing attention (GlobalAttentionUpdate)");

    // Cycle from A. `Sessions::ordered()` puts the current session first,
    // then attention>0 peers by name, then the rest — B is the only
    // attention>0 peer, so a single `session-next` tap selects it outright.
    // The actual switch is a LATER idle-commit (`timer`'s `Sessions::tick`),
    // firing on the very next Fast tick after the tap (~1s here, since A's
    // `running` row above keeps Fast armed) — not an immediate effect of the
    // pipe call itself.
    a.run_action(&["pipe", "--name", "zj_radar.cmd.v1", "--", "session-next"]);

    // After the idle-commit fires, `Effect::SwitchSession` calls Zellij's
    // `switch_session_with_focus`, which reattaches A's OWN PTY CLIENT onto
    // session B — session "radar-a" itself keeps running (headless), so
    // `a.dump_screen()` (which explicitly targets session A BY NAME through
    // a fresh `--session radar-a action ...` connection) would keep dumping
    // A's own terminal FOREVER and could never observe this. Only A's raw
    // PTY buffer (`pty_text()`) reflects what the physical client is
    // actually attached to now. The marker `discover_terminal_pane_id`
    // already echoed into B's terminal (`ZPID=<pane_b>`) is still on B's
    // screen (nothing since has overwritten it), so it can only show up in
    // A's PTY stream if that PTY is now rendering B's screen — the same
    // marker-based external-signal technique `click_on_a_tab_row_switches_
    // the_active_tab` uses to confirm a real `SwitchTab`, applied here to a
    // real `SwitchSession`.
    let marker = format!("ZPID={pane_b}");
    let switched = a.wait_until(Duration::from_secs(8), |s| s.pty_text().contains(&marker));
    if !switched {
        let tail: String = a.pty_text().chars().rev().take(1000).collect::<String>().chars().rev().collect();
        eprintln!("[e2e] A's PTY tail after failed switch wait:\n{tail}");
    }
    assert!(
        switched,
        "session-next's idle-commit must switch_session A's client onto B; A's PTY never \
         showed B's marker {marker:?} (session-next tapped, ~1 Fast tick should commit it)"
    );
    eprintln!("[e2e] PASS: session-next committed and A's client switched onto B");
}
