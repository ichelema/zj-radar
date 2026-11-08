//! Display-text sanitizer and the length caps shared by everything that reaches
//! the rail from outside: pane titles, tab names, program messages, and the
//! pipe payloads. No zellij-tile dependency.

/// Payloads larger than this are rejected outright by the `cmd.v1`/`config.v1`
/// pipes before JSON parsing is even attempted.
pub const MAX_PAYLOAD_BYTES: usize = 65536;
/// `msg` truncates to this many chars.
pub const MAX_MSG_CHARS: usize = 60;
/// `task` truncates to this many chars.
pub const MAX_TASK_CHARS: usize = 60;
/// `repo` truncates to this many chars.
pub const MAX_REPO_CHARS: usize = 40;
/// `branch` truncates to this many chars.
pub const MAX_BRANCH_CHARS: usize = 40;
/// Intake cap for tab names and pane titles. They arrive from the Zellij host
/// (tab/pane updates) and the plugin sanitizes them to this many chars at
/// intake (the same 40 as `repo`/`branch`, the other row-column strings).
pub const MAX_TAB_NAME_CHARS: usize = 40;

/// Unicode bidi format/override characters. These can visually reorder or hide
/// surrounding text (the "Trojan Source" class) — e.g. an RLO (U+202E) in a
/// `repo`/`branch`/`msg` field could make a rail row read differently than its
/// bytes. They have no legitimate use in the short display strings we render, so
/// they are dropped. `is_control()` does NOT cover them (they are `Cf`, format
/// characters, not `Cc`). The zero-width JOINER (U+200D) is deliberately NOT here:
/// it is load-bearing for emoji sequences, and `render::truncate` already handles
/// a stranded trailing ZWJ.
fn is_bidi_control(c: char) -> bool {
    matches!(c,
        '\u{202A}'..='\u{202E}'   // LRE RLE PDF LRO RLO (embeddings + overrides)
        | '\u{2066}'..='\u{2069}' // LRI RLI FSI PDI (isolates)
        | '\u{200E}' | '\u{200F}' // LRM RLM (marks)
        | '\u{061C}'              // ALM (Arabic letter mark)
    )
}

/// Strip control/ANSI chars, fold newlines/tabs/CR to spaces, truncate to `max_chars`.
///
/// Stripped sequences:
/// - CSI (`\x1b[` … final byte in 0x40–0x7E; a C0 control or non-ASCII byte
///   ends a malformed CSI without being consumed)
/// - OSC (`\x1b]` … terminated by BEL `\x07` or ST `\x1b\`; a stray C0 control
///   ends a malformed OSC without being consumed)
/// - Any other ESC-introduced 2-byte sequence (`\x1b` + one byte)
/// - C0 control chars (0x00–0x1F) — `\n`, `\t`, `\r` become a single space; all others dropped
/// - DEL (0x7F) — dropped
/// - Unicode C1 control chars (U+0080–U+009F) — dropped
/// - Bidi format/override chars (`is_bidi_control`) — dropped (Trojan-Source-style spoofing)
pub fn sanitize(s: &str, max_chars: usize) -> String {
    let mut out = s.to_string();
    sanitize_in_place(&mut out, max_chars);
    out
}

/// [`sanitize`] for a string the caller already owns. Almost every string
/// that reaches here is already clean — a repo name, a branch, a tool label,
/// a snapshot field this very sanitizer wrote — and this runs on every field
/// of every broadcast, manifest pane, and snapshot record, under the
/// interpreter: one allocation-free scan settles the common case in place,
/// and only over-long or dirty input takes the full walk.
pub fn sanitize_in_place(s: &mut String, max_chars: usize) {
    if s.chars().all(kept) {
        if let Some((cut, _)) = s.char_indices().nth(max_chars) {
            s.truncate(cut);
        }
        return;
    }
    *s = scrub(s, max_chars);
}

/// Whether the sanitizer passes `c` through unchanged — THE predicate both
/// the fast path and [`scrub`] share, so the two cannot disagree: C0
/// controls (ESC, `\n`/`\t`/`\r` included — `scrub` folds those three to a
/// space, still a rewrite), DEL, C1, and the bidi format characters are the
/// only things `scrub` drops or rewrites. ASCII takes the cheap range test.
fn kept(c: char) -> bool {
    if c.is_ascii() {
        !c.is_ascii_control()
    } else {
        !c.is_control() && !is_bidi_control(c)
    }
}

/// The full sanitizer walk (see [`sanitize`], whose fast path skips it for
/// already-clean input). Kept separate so the property test can pin the
/// fast path to this reference on arbitrary input.
fn scrub(s: &str, max_chars: usize) -> String {
    let mut cleaned = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1b {
            // ESC — decide what kind of sequence follows
            let next = bytes.get(i + 1).copied();
            match next {
                Some(b'[') => {
                    // CSI: consume until a final byte in 0x40–0x7E (inclusive).
                    // A well-formed CSI body is parameter/intermediate bytes in
                    // 0x20–0x3F only, so a C0 control or a non-ASCII byte ends a
                    // malformed/unterminated CSI WITHOUT being consumed — the
                    // outer loop reprocesses it (the same bound the OSC arm
                    // below applies). This used to run to the next 0x40–0x7E
                    // byte regardless, so `\x1b[` followed by non-ASCII text
                    // (every UTF-8 byte is >= 0x80) or a newline swallowed text
                    // that should survive.
                    i += 2;
                    while i < bytes.len() {
                        let fb = bytes[i];
                        if !(0x20..0x80).contains(&fb) {
                            break; // stray byte ends the CSI; leave it for the outer loop
                        }
                        i += 1;
                        if (0x40..=0x7e).contains(&fb) {
                            break; // final byte consumed
                        }
                    }
                }
                Some(b']') => {
                    // OSC: consume until BEL (0x07) or ST (ESC \). A well-formed
                    // OSC string is text + terminator with no bare control bytes,
                    // so ANY other C0 control (newline, a fresh ESC, …) ends a
                    // malformed/unterminated OSC WITHOUT being consumed — the outer
                    // loop reprocesses it. This bounds an unterminated OSC: it used
                    // to run to end-of-input and swallow the whole field, blanking
                    // it. (A fully-printable OSC with no terminator anywhere still
                    // consumes to the end — matching how a real terminal waits for a
                    // terminator. The CSI arm above keeps the analogous residual
                    // swallow: a tail that is entirely parameter-shaped, 0x20–0x3F.)
                    i += 2;
                    while i < bytes.len() {
                        let b = bytes[i];
                        if b == 0x07 {
                            i += 1; // BEL terminator consumed
                            break;
                        }
                        if b == 0x1b && bytes.get(i + 1).copied() == Some(b'\\') {
                            i += 2; // ST terminator consumed
                            break;
                        }
                        if b < 0x20 {
                            break; // stray control ends the OSC; leave it for the outer loop
                        }
                        i += 1;
                    }
                }
                Some(_) => {
                    // Any other ESC + one byte: skip both
                    i += 2;
                }
                None => {
                    // Lone ESC at end of string: skip it
                    i += 1;
                }
            }
        } else if b == b'\n' || b == b'\t' || b == b'\r' {
            cleaned.push(' ');
            i += 1;
        } else if b < 0x20 || b == 0x7f {
            // Other C0 control chars and DEL: drop
            i += 1;
        } else {
            // Normal byte — reconstruct as char (handle multi-byte UTF-8).
            // Use `get` instead of direct indexing: ESC-sequence scanners advance
            // byte-by-byte and can leave `i` on a UTF-8 continuation byte (e.g.
            // "\x1b" + lead-byte of a 2-byte char consumed as "ESC + one byte").
            // If `i` is not on a char boundary we skip the stray continuation byte.
            // Also drop Unicode C1 control chars (U+0080–U+009F) whose lead byte
            // (0xC2) passes the ASCII-range check above but are still control chars,
            // and bidi format/override chars (`is_bidi_control`) that could visually
            // reorder or hide rail text (Trojan-Source-style spoofing).
            match s.get(i..) {
                Some(remaining) => match remaining.chars().next() {
                    Some(c) if kept(c) => {
                        cleaned.push(c);
                        i += c.len_utf8();
                    }
                    Some(c) => {
                        i += c.len_utf8();
                    }
                    None => {
                        i += 1;
                    }
                },
                None => {
                    i += 1;
                }
            }
        }
    }
    cleaned.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn sanitize_strips_control_newlines_ansi_and_truncates() {
        let dirty = "a\nb\t\x1b[31mc\x07";
        assert_eq!(sanitize(dirty, 10), "a b c");
        assert_eq!(sanitize("abcdef", 3), "abc");
    }

    #[test]
    fn sanitize_strips_truecolor_csi() {
        // CSI 38;2;R;G;Bm (truecolor foreground) + reset — only the text survives
        let input = "\x1b[38;2;255;0;0mred\x1b[0m";
        assert_eq!(sanitize(input, 100), "red");
    }

    #[test]
    fn sanitize_strips_osc_bel_terminated() {
        // OSC 0 ; title BEL — the sequence is removed, trailing text survives
        let input = "\x1b]0;evil title\x07ok";
        assert_eq!(sanitize(input, 100), "ok");
    }

    #[test]
    fn sanitize_strips_osc_st_terminated() {
        // The OSC's other well-formed terminator is ST (`ESC \`), the third exit
        // of the scanner alongside BEL and the stray-control bound. The doc
        // comment promises ST support, so pin it: the sequence is removed and
        // only the trailing text survives.
        assert_eq!(sanitize("\x1b]0;evil title\x1b\\ok", 100), "ok");
    }

    #[test]
    fn sanitize_converts_newline_to_space() {
        assert_eq!(sanitize("a\nb", 100), "a b");
    }

    #[test]
    fn sanitize_drops_bel_control_char() {
        // BEL (0x07) not preceded by ESC+] should be dropped entirely
        assert_eq!(sanitize("a\x07b", 100), "ab");
    }

    #[test]
    fn sanitize_length_cap_still_applies_after_hardening() {
        // A truecolor sequence followed by many chars — cap must still hold
        let input = format!("\x1b[38;2;0;128;0m{}\x1b[0m", "x".repeat(200));
        let out = sanitize(&input, 50);
        assert_eq!(out.chars().count(), 50);
        assert_eq!(out, "x".repeat(50));
    }

    #[test]
    fn sanitize_unterminated_osc_is_bounded_by_a_control() {
        // An OSC with no BEL/ST terminator used to run to end-of-input and swallow
        // the whole field. A stray control (here a newline) now ends it, so the
        // trailing text survives (the newline folds to a space).
        assert_eq!(sanitize("\x1b]0;title\nreal text", 100), " real text");
        // A fresh ESC-introduced CSI inside an unterminated OSC also ends it; the
        // CSI is then stripped and its trailing text survives.
        assert_eq!(sanitize("\x1b]0;title\x1b[0mkept", 100), "kept");
    }

    #[test]
    fn sanitize_unterminated_csi_is_bounded() {
        // A CSI whose next byte is outside the valid parameter/final ranges used
        // to be swallowed along with everything up to the next 0x40–0x7E byte.
        // Non-ASCII text (every UTF-8 byte is >= 0x80) now ends the CSI without
        // being consumed, so the text survives.
        assert_eq!(sanitize("\x1b[тест done", 100), "тест done");
        // A C0 control also ends it; the newline then folds to a space.
        assert_eq!(sanitize("\x1b[\nhello", 100), " hello");
    }

    #[test]
    fn sanitize_parameter_shaped_unterminated_csi_still_drops() {
        // The CSI analogue of the fully-printable OSC swallow below: a tail
        // that is entirely parameter-shaped (0x20–0x3F) with no final byte
        // still consumes to the end — a real terminal also keeps waiting for
        // the final byte. Pinned so the bound above isn't mistaken for a full fix.
        assert_eq!(sanitize("\x1b[38;2;255", 100), "");
    }

    #[test]
    fn sanitize_fully_printable_unterminated_osc_still_drops() {
        // A remaining swallow case: an OSC with no terminator AND no control
        // byte anywhere. A real terminal also waits indefinitely for a terminator,
        // so dropping (rather than emitting the raw OSC payload as text) is the
        // defensive choice. Pinned so the bound above isn't mistaken for a full fix.
        assert_eq!(sanitize("\x1b]0;title with no terminator", 100), "");
    }

    #[test]
    fn sanitize_strips_bidi_controls_but_keeps_zwj() {
        // Trojan-Source-style bidi overrides/marks/isolates are dropped so a
        // producer can't visually reorder a rail row (e.g. spoof "error" as "done").
        assert_eq!(sanitize("abc\u{202e}def", 100), "abcdef"); // RLO
        assert_eq!(sanitize("a\u{200f}b\u{200e}c", 100), "abc"); // RLM / LRM
        assert_eq!(sanitize("x\u{2066}y\u{2069}z", 100), "xyz"); // LRI / PDI isolates
                                                                 // The zero-width JOINER is preserved — it is load-bearing for emoji.
        let zwj = "👩\u{200d}💻";
        assert_eq!(sanitize(zwj, 100), zwj);
    }

    proptest::proptest! {
        #[test]
        fn sanitize_never_emits_control_or_overlong(input in ".{0,500}", max in 1usize..120) {
            let out = sanitize(&input, max);
            prop_assert!(out.chars().count() <= max, "len {} > max {}", out.chars().count(), max);
            for ch in out.chars() {
                prop_assert!(ch != '\x1b', "ESC leaked");
                prop_assert!(!ch.is_control(), "control char leaked: {:?}", ch);
            }
        }

        /// The clean-input fast path must be indistinguishable from the full
        /// scrub — on arbitrary input, including strings that mix clean text
        /// with the bidi/control/ESC cases that force the slow walk.
        #[test]
        fn sanitize_fast_path_matches_the_scrub(
            input in "(.|\\x1b\\[[0-9;]*m|\\u{202e}|\\x07|\\t){0,80}",
            max in 1usize..120,
        ) {
            prop_assert_eq!(sanitize(&input, max), scrub(&input, max));
            let mut in_place = input.clone();
            sanitize_in_place(&mut in_place, max);
            prop_assert_eq!(in_place, scrub(&input, max));
        }
    }
}
