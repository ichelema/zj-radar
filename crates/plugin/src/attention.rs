//! Zellij's native per-pane attention (Zellij ≥ 0.46: the Program Status
//! Protocol, OSC 7501, plus shell-command tracking), mirrored as plain data so
//! the policy stays host-testable — `zellij-tile` is wasm-only. The wasm glue
//! (`lib.rs`) copies `PaneInfo::attention` / `program_status` into these types;
//! [`NativePane::observation`] turns them into the observation the rail rolls
//! up.

use crate::kind::Kind;
use crate::observation::{ObservationOrigin, TrackedObservation};
use crate::sessions::{SessionSummary, SummaryPane, SummaryTab, MAX_LABEL_CHARS, MAX_PANES_PER_TAB, MAX_TABS};
use crate::status::Status;
use crate::task::{BgTask, BgTasks, TaskState};

/// Which kind of signal decided the pane's attention (`AttentionSource`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Source {
    /// An OSC 7501 record (`ProgramStatus`).
    Program,
    /// A shell command, command pane, or foreground process.
    Command,
    /// Bell, notification, idle rule, watch, unseen output — or none.
    #[default]
    Other,
}

/// `PaneInfo::attention`, reduced to what the rail shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PaneAttention {
    pub status: Status,
    pub source: Source,
    /// Zellij's display text: already carries the kind label and progress
    /// (`needs approval: …`, `working 40%: …`).
    pub text: String,
    pub app: Option<String>,
    pub exit_code: Option<i32>,
    pub since_ms: u64,
    /// The program's deciding record, kept after the pane is seen: a focused
    /// pane's Done is seen at once and never shows as the status.
    pub remembered: Option<Remembered>,
}

/// `PaneAttention::remembered`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Remembered {
    pub status: Status,
    pub text: String,
    pub since_ms: u64,
}

/// A completion's ledger label: Zellij's text without the head the ledger
/// glyph already says (`done`, `failed`, `finished`, with an optional
/// ` · 3s` duration). `exited 2` stays: the glyph doesn't carry the code.
pub(crate) fn completion_label(text: &str) -> &str {
    match text.split_once(": ") {
        Some((head, rest)) if matches!(head.split(" · ").next(), Some("done" | "failed" | "finished")) => rest,
        _ => text,
    }
}

/// One `ProgramStatusRecord`: the root (`id == None`) or a child.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Record {
    pub id: Option<String>,
    pub status: Status,
    pub app: Option<String>,
    pub title: Option<String>,
    pub msg: Option<String>,
    pub updated_ms: u64,
}

impl Record {
    /// Title, else msg, else id.
    fn label(&self) -> String {
        [&self.title, &self.msg, &self.id]
            .into_iter()
            .flatten()
            .find(|s| !s.trim().is_empty())
            .cloned()
            .unwrap_or_default()
    }
}

/// Everything the manifest says about one terminal pane's attention.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct NativePane {
    pub attention: PaneAttention,
    pub records: Vec<Record>,
}

/// The agent mark for an OSC 7501 `app`. `zellij` is a nested or remote
/// Zellij forwarding its session's aggregate status.
pub(crate) fn kind_for_app(app: &str) -> Kind {
    match app {
        "claude-code" => Kind::Claude,
        "zellij" => Kind::Remote,
        other => Kind::from_source(other),
    }
}

/// Whole words that mark a shell command as a service (`npm run dev`,
/// `cargo watch`, `python -m http.server`): unending by design, so its row
/// holds the steady mark instead of spinning (`docs/activity-model.md` §4).
const SERVICE_WORDS: &[&str] = &["dev", "serve", "server", "start", "watch"];

/// The kind for a shell-command row, from Zellij's text (`running: npm run
/// dev`). A token with `/` or `=` is a path, URL, redirect, or assignment
/// (`2>/dev/null`, `tests/server/`, `env=dev`) and never counts. The rest
/// lose shell quoting and grouping (`'npm run dev'`, `(… dev)`) and leading
/// dashes, then split on `:`; a dotted part counts by its last segment only
/// (`http.server` matches, `webpack.dev.js` and `start.sh` do not). So
/// `start:dev` and `--watch` match while `api-server` and `restart` do not.
pub(crate) fn command_kind(text: &str) -> Kind {
    let service = text
        .split_whitespace()
        .filter(|t| !t.contains(['/', '=']))
        .flat_map(|t| t.trim_matches(['\'', '"', '(', ')', ';', '&', '|']).trim_start_matches('-').split(':'))
        .filter_map(|part| part.rsplit('.').next())
        .any(|w| SERVICE_WORDS.iter().any(|s| w.eq_ignore_ascii_case(s)));
    if service {
        Kind::Server
    } else {
        Kind::Command
    }
}

impl NativePane {
    /// The rail observation for this pane, or `None` when there is nothing to
    /// show (an idle pane no program reports on). `prev` is the pane's
    /// observation from the previous manifest: an unchanged status keeps its
    /// `last_change_tick`, so the spinner's ease and the rows memo hold.
    pub(crate) fn observation(&self, prev: Option<&TrackedObservation>, tick: u64) -> Option<TrackedObservation> {
        let a = &self.attention;
        let app = a.app.as_deref().or_else(|| self.records.iter().find_map(|r| r.app.as_deref()));
        let program_kind = app.map_or(Kind::Other, kind_for_app);
        // Zellij raises a pane from its child records only for `blocked` and
        // `error` (`deciding_record`): children that are only `working` with no
        // root record (Opencode) leave the pane Idle. Take the most severe record.
        // With a root, Zellij's status stands (a seen Done reads Idle).
        let has_root = self.records.iter().any(|r| r.id.is_none());
        let deciding = (a.source != Source::Program && !has_root)
            .then(|| self.records.iter().enumerate().max_by_key(|(_, r)| r.status))
            .flatten();
        let (status, origin, kind, msg, since_ms) = match deciding {
            Some((_, r)) => (r.status, ObservationOrigin::Program, program_kind, r.label(), r.updated_ms),
            None => match a.source {
                Source::Command => {
                    (a.status, ObservationOrigin::Command, command_kind(&a.text), a.text.clone(), a.since_ms)
                }
                Source::Program | Source::Other => {
                    let msg = if a.text.is_empty() {
                        self.records.iter().find(|r| r.id.is_none()).map(Record::label).unwrap_or_default()
                    } else {
                        a.text.clone()
                    };
                    (a.status, ObservationOrigin::Program, program_kind, msg, a.since_ms)
                }
            },
        };
        if status == Status::Idle && self.records.is_empty() {
            return None;
        }
        let since_s = since_ms / 1000;
        let skip = deciding.map(|(i, _)| i);
        let items = self
            .records
            .iter()
            .enumerate()
            .filter(|&(i, r)| r.id.is_some() && Some(i) != skip)
            .map(|(_, r)| BgTask {
                id: r.id.clone().unwrap_or_default(),
                label: r.label(),
                state: match r.status {
                    Status::Done => TaskState::Completed,
                    Status::Error => TaskState::Failed,
                    Status::Idle => TaskState::Ended,
                    Status::Running | Status::Pending => TaskState::Running,
                },
                // ponytail: OSC 7501 has no start time, `updated_ms` stands in; add one if ages mislead
                started_epoch_s: r.updated_ms / 1000,
                ended_epoch_s: r.status.is_completion().then_some(r.updated_ms / 1000),
            })
            .collect();
        Some(
            TrackedObservation {
                origin,
                status,
                msg,
                kind,
                last_change_tick: prev.filter(|p| p.status == status).map_or(tick, |p| p.last_change_tick),
                ever_active: true,
                exit_code: if origin == ObservationOrigin::Command { a.exit_code } else { None },
                completed_epoch_s: status.is_completion().then_some(since_s),
                pending_epoch_s: (status == Status::Pending).then_some(since_s),
                tasks: BgTasks { items },
            }
            .sanitized(),
        )
    }
}

/// One pane of another session with attention (`GlobalAttentionUpdate`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PeerItem {
    pub pane_id: u32,
    pub tab_id: usize,
    pub tab_name: String,
    pub attention: PaneAttention,
}

/// Another session on this Zellij server, as `GlobalAttention` reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PeerSession {
    pub name: String,
    pub working: usize,
    /// `needs_you + failed`: the panes waiting on the user (`Status::needs_you`).
    pub needs_you: usize,
    pub items: Vec<PeerItem>,
}

impl PeerSession {
    /// The peer as the badge's [`SessionSummary`], tabs in first-seen item order.
    /// Tab positions are ordinals, not Zellij positions: peer clicks focus a
    /// pane by id instead (`render_peer_children`).
    pub(crate) fn summary(self) -> SessionSummary {
        use crate::payload::sanitize;
        let mut tabs: Vec<(usize, SummaryTab)> = Vec::new();
        for item in self.items {
            let i = match tabs.iter().position(|(id, _)| *id == item.tab_id) {
                Some(i) => i,
                None if tabs.len() < MAX_TABS => {
                    let tab = SummaryTab {
                        position: tabs.len(),
                        name: sanitize(&item.tab_name, MAX_LABEL_CHARS),
                        panes: vec![],
                    };
                    tabs.push((item.tab_id, tab));
                    tabs.len() - 1
                }
                None => continue,
            };
            let a = item.attention;
            let kind = match a.source {
                Source::Command => Kind::Command,
                Source::Program | Source::Other => a.app.as_deref().map_or(Kind::Other, kind_for_app),
            };
            let panes = &mut tabs[i].1.panes;
            if panes.len() < MAX_PANES_PER_TAB {
                panes.push(SummaryPane {
                    pane_id: Some(item.pane_id),
                    kind: kind.as_source().into(),
                    status: a.status.as_wire().into(),
                    label: sanitize(&a.text, MAX_LABEL_CHARS),
                });
            }
        }
        SessionSummary {
            session_name: sanitize(&self.name, 128),
            running: self.working,
            attention: self.needs_you,
            attention_tab_position: None,
            tabs: tabs.into_iter().map(|(_, tab)| tab).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_items_group_by_tab_with_agent_marks() {
        let item = |pane_id, tab_id, app: &str, status| PeerItem {
            pane_id,
            tab_id,
            tab_name: format!("tab{tab_id}"),
            attention: PaneAttention {
                status,
                source: Source::Program,
                text: "needs approval: x".into(),
                app: Some(app.into()),
                ..PaneAttention::default()
            },
        };
        let p = PeerSession {
            name: "work".into(),
            working: 1,
            needs_you: 1,
            items: vec![
                item(3, 7, "claude-code", Status::Pending),
                item(4, 9, "codex", Status::Running),
                item(5, 7, "pi", Status::Done),
            ],
        }
        .summary();
        assert_eq!((p.session_name.as_str(), p.running, p.attention), ("work", 1, 1));
        let tabs: Vec<(&str, Vec<u32>)> =
            p.tabs.iter().map(|t| (t.name.as_str(), t.panes.iter().filter_map(|p| p.pane_id).collect())).collect();
        assert_eq!(tabs, vec![("tab7", vec![3, 5]), ("tab9", vec![4])]);
        let first = &p.tabs[0].panes[0];
        assert_eq!((first.kind.as_str(), first.status.as_str()), ("claude", "pending"));
    }

    fn program(status: Status, app: &str, text: &str) -> NativePane {
        NativePane {
            attention: PaneAttention {
                status,
                source: Source::Program,
                text: text.into(),
                app: Some(app.into()),
                since_ms: 5_000,
                ..PaneAttention::default()
            },
            records: vec![Record { status, app: Some(app.into()), ..Record::default() }],
        }
    }

    fn child(id: &str, status: Status, title: &str) -> Record {
        Record { id: Some(id.into()), status, title: Some(title.into()), updated_ms: 9_000, ..Record::default() }
    }

    #[test]
    fn idle_pane_without_records_is_untracked() {
        assert_eq!(NativePane::default().observation(None, 3), None);
    }

    #[test]
    fn program_status_maps_app_and_text() {
        let o = program(Status::Running, "claude-code", "working 40%: Editing main.rs").observation(None, 7).unwrap();
        assert_eq!(o.status, Status::Running);
        assert_eq!(o.kind, Kind::Claude);
        assert_eq!(o.origin, ObservationOrigin::Program);
        assert_eq!(o.msg, "working 40%: Editing main.rs");
        assert_eq!(o.last_change_tick, 7);
        assert_eq!(o.exit_code, None);
    }

    #[test]
    fn pending_stamps_the_wait_clock() {
        let o = program(Status::Pending, "codex", "needs approval: approve Bash").observation(None, 1).unwrap();
        assert_eq!(o.kind, Kind::Codex);
        assert_eq!(o.pending_epoch_s, Some(5));
        assert_eq!(o.completed_epoch_s, None);
    }

    #[test]
    fn unchanged_status_keeps_its_tick() {
        let pane = program(Status::Running, "pi", "x");
        let first = pane.observation(None, 2).unwrap();
        assert_eq!(pane.observation(Some(&first), 9).unwrap().last_change_tick, 2);
        let done = program(Status::Done, "pi", "x").observation(Some(&first), 9).unwrap();
        assert_eq!(done.last_change_tick, 9);
        assert_eq!(done.completed_epoch_s, Some(5));
    }

    #[test]
    fn idle_program_keeps_its_row_and_root_label() {
        let mut pane = program(Status::Idle, "claude-code", "");
        pane.records[0].msg = Some("ready".into());
        let o = pane.observation(None, 1).unwrap();
        assert_eq!(o.status, Status::Idle);
        assert_eq!(o.msg, "ready");
    }

    #[test]
    fn working_children_without_root_raise_the_pane() {
        // Opencode: one child record per session, no root; Zellij leaves the pane Idle.
        let pane = NativePane {
            attention: PaneAttention::default(),
            records: vec![
                Record { app: Some("opencode".into()), ..child("s1", Status::Done, "old") },
                child("s2", Status::Running, "Fix the build"),
            ],
        };
        let o = pane.observation(None, 4).unwrap();
        assert_eq!(o.status, Status::Running);
        assert_eq!(o.kind, Kind::Opencode);
        assert_eq!(o.msg, "Fix the build");
        // The deciding record is the pane line; only the other one is a sub-line.
        assert_eq!(o.tasks.items.len(), 1);
        assert_eq!(o.tasks.items[0].state, TaskState::Completed);
    }

    #[test]
    fn seen_done_with_a_root_stays_idle() {
        // A focused pane's Done is seen after ~2 s: Zellij drops the source and
        // reports Idle, while the root and child records stay.
        let mut pane = program(Status::Idle, "claude-code", "");
        pane.attention.source = Source::Other;
        pane.records[0].status = Status::Done;
        pane.records[0].msg = Some("Turn finished".into());
        pane.records.push(child("t1", Status::Done, ""));
        let o = pane.observation(None, 1).unwrap();
        assert_eq!((o.status, o.kind, o.msg.as_str()), (Status::Idle, Kind::Claude, "Turn finished"));
    }

    #[test]
    fn program_source_ignores_child_severity() {
        let mut pane = program(Status::Running, "claude-code", "turn");
        pane.records.push(child("a4cc", Status::Running, "Explore docs"));
        let o = pane.observation(None, 1).unwrap();
        assert_eq!(o.status, Status::Running);
        assert_eq!(o.msg, "turn");
        let task = &o.tasks.items[0];
        assert_eq!((task.label.as_str(), task.state), ("Explore docs", TaskState::Running));
    }

    #[test]
    fn command_source_carries_the_exit_code() {
        let pane = NativePane {
            attention: PaneAttention {
                status: Status::Error,
                source: Source::Command,
                text: "cargo test".into(),
                exit_code: Some(101),
                ..PaneAttention::default()
            },
            records: Vec::new(),
        };
        let o = pane.observation(None, 1).unwrap();
        assert_eq!((o.origin, o.kind, o.exit_code), (ObservationOrigin::Command, Kind::Command, Some(101)));
    }

    #[test]
    fn program_text_is_sanitized() {
        let o = program(Status::Running, "x", "a\x1b[31mb\nc").observation(None, 1).unwrap();
        assert!(!o.msg.contains('\x1b') && !o.msg.contains('\n'), "{:?}", o.msg);
        assert_eq!(o.kind, Kind::Other);
    }

    #[test]
    fn completion_labels_drop_the_status_head() {
        assert_eq!(completion_label("done: Refactor complete"), "Refactor complete");
        assert_eq!(completion_label("failed: exit 2"), "exit 2");
        assert_eq!(completion_label("cargo test"), "cargo test");
        // Zellij's command heads.
        assert_eq!(completion_label("done · 3s: cargo test"), "cargo test");
        assert_eq!(completion_label("finished: make"), "make");
        assert_eq!(completion_label("exited 1 · 2s: cargo test"), "exited 1 · 2s: cargo test");
        // Only the first head goes.
        assert_eq!(completion_label("done: note: see log"), "note: see log");
    }

    #[test]
    fn app_marks() {
        assert_eq!(kind_for_app("claude-code"), Kind::Claude);
        assert_eq!(kind_for_app("zellij"), Kind::Remote);
        assert_eq!(kind_for_app("omp"), Kind::Omp);
        assert_eq!(kind_for_app("vim"), Kind::Other);
    }

    #[test]
    fn service_commands_are_steady_servers() {
        for text in [
            "running: npm run dev", "running: npm start", "running: pnpm run start:dev",
            "running: python -m http.server 8000", "running: cargo watch -x test", "running: hugo server",
            "running: mkdocs serve", "running: just DEV", "running: webpack --watch", "exited 130 · 2m: npm run dev",
            // Shell quoting and grouping around the words.
            "running: sh -c 'npm run dev'", "running: bash -c \"npm start\"", "running: (cd web && npm run dev)",
            "running: npm run dev;",
        ] {
            assert_eq!(command_kind(text), Kind::Server, "{text}");
        }
        for text in [
            "running: cargo build --release",
            "running: cargo test watch_mode",
            "running: ./restart.sh",
            "running: npm run devtools",
            "running",
            "done · 3s: cargo test",
            "",
            // Paths, redirects, URLs, assignments, and dashed names are not service words.
            "running: cargo build --release 2>/dev/null",
            "running: make build > /dev/null 2>&1",
            "running: dd if=/dev/zero of=disk.img bs=1M",
            "running: ~/dev/proj/scripts/migrate.sh",
            "running: cargo build -p api-server --release",
            "running: pytest tests/server/",
            "running: terraform apply -var env=dev",
            "running: git clone https://github.com/x/dev-tools",
            "running: ./start.sh",
            // A dotted file name counts by its extension only.
            "running: tsc -p tsconfig.dev.json",
            "running: webpack --config webpack.dev.js",
            "running: docker compose -f docker-compose.dev.yml build",
            "running: terraform apply -var-file dev.tfvars",
            "running: openssl req -new -key server.key -out server.csr",
            "running: gzip -9 server.log",
            "running: start.sh",
        ] {
            assert_eq!(command_kind(text), Kind::Command, "{text}");
        }
    }

    #[test]
    fn running_service_command_does_not_animate() {
        let pane = NativePane {
            attention: PaneAttention {
                status: Status::Running,
                source: Source::Command,
                text: "running: npm run dev".into(),
                ..PaneAttention::default()
            },
            records: Vec::new(),
        };
        let o = pane.observation(None, 1).unwrap();
        assert_eq!((o.origin, o.kind), (ObservationOrigin::Command, Kind::Server));
        assert!(!o.animating());
    }
}
