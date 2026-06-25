//! Detection adapter: implements [`moonlight_domain::ports::DetectionSource`] by
//! tailing Claude Code `~/.claude/projects/**/*.jsonl` transcripts. All Claude
//! Code artifacts are treated as untrusted input (NFR6) — see [`jsonl`].
//!
//! v1 is JSONL-only (the reconciling source of truth per Spike 0). The low-latency
//! hooks layer (`PreToolUse`/`Stop`/`Notification`) and a fusing layer land next;
//! the `ControlPort`/`DetectionSource` split keeps that change isolated.

mod antigravity;
mod jsonl;

pub use antigravity::{turn_for_agy_line, AntigravityDetectionSource};
pub use jsonl::{parse_line, phase_from_mode, status_for, Signal, Turn};

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use moonlight_domain::errors::ControlError;
use moonlight_domain::ids::SessionId;
use moonlight_domain::phase::Phase;
use moonlight_domain::ports::detection::{DetectionEvent, DetectionSource};
use moonlight_domain::session::{AttentionKind, SessionStatus};

/// Per-session tail state, so each poll emits only what changed.
#[derive(Default)]
struct FileState {
    /// Byte offset already consumed from the transcript.
    offset: u64,
    discovered: bool,
    title: Option<String>,
    /// Whether `title` came from the operator's `/rename` (`custom-title`) — once
    /// set, CC's rolling auto `ai-title` no longer overrides it.
    custom_titled: bool,
    phase: Option<Phase>,
    status: Option<SessionStatus>,
    path: Option<String>,
    /// The most recent conversational turn observed (drives live status).
    last_turn: Option<Turn>,
    /// The most recently proposed plan (dedup, so we emit it once).
    plan: Option<String>,
    /// The most recent assistant summary text (dedup, so we re-emit only on change).
    summary: Option<String>,
    /// Consecutive polls this session's transcript was absent from the active
    /// window — `Ended` only fires after several, to avoid flapping on a transient
    /// stat failure or a one-poll miss.
    missed: u8,
    /// Whether an `Incomplete` stall alert is currently raised for this session, so
    /// the alert is emitted once on stall and once on recovery (not every poll).
    alerted: bool,
    /// Whether this session has ever produced a clean stop (`stop_hook_summary`).
    /// **Self-calibrating:** only once we've seen a clean stop do we know a `Stop` hook
    /// is active for it — and only then can a quiet `Assistant` turn (no following clean
    /// stop) be trusted to mean "interrupted mid-generation" rather than the normal
    /// end-of-turn of a session that simply has no Stop hook. No clean stop seen ⇒ we
    /// fall back to the conservative `User`-turn-only stall (zero false positives).
    seen_clean_stop: bool,
}

/// Default working-vs-waiting threshold: a transcript written within this window is
/// treated as actively working. Quieter than this and status is derived from the
/// last turn. Coarse by nature — the hooks layer makes status precise later.
const DEFAULT_LIVENESS_WINDOW: Duration = Duration::from_secs(20);

/// Default **stall** threshold: a session handed a turn (the operator's prompt or a
/// tool result — a `User` turn) that then stays silent this long has stopped without
/// finishing — the agent died/hung after being handed the turn (a healthy one keeps
/// writing, so its age stays under the liveness window). Far longer than
/// [`DEFAULT_LIVENESS_WINDOW`] so a normal slow first-token / brief think never trips
/// it. Raises an `Incomplete` ⚠ overlay; cleared when the session writes again.
const DEFAULT_STALL_WINDOW: Duration = Duration::from_secs(180);

/// Tails Claude Code transcripts and emits [`DetectionEvent`]s. A session whose
/// transcript was modified within `active_window` is part of the live fleet;
/// sessions that go stale are `Ended`.
pub struct JsonlDetectionSource {
    root: PathBuf,
    active_window: Duration,
    liveness_window: Duration,
    stall_window: Duration,
    state: Mutex<HashMap<String, FileState>>,
}

impl JsonlDetectionSource {
    pub fn new(root: PathBuf, active_window: Duration) -> Self {
        Self {
            root,
            active_window,
            liveness_window: DEFAULT_LIVENESS_WINDOW,
            stall_window: DEFAULT_STALL_WINDOW,
            state: Mutex::new(HashMap::new()),
        }
    }

    /// Override the working-vs-waiting liveness threshold (mainly for tests).
    pub fn with_liveness_window(mut self, window: Duration) -> Self {
        self.liveness_window = window;
        self
    }

    /// Override the stall threshold (a handed-a-turn session silent this long is
    /// flagged `Incomplete`). Mainly for tests.
    pub fn with_stall_window(mut self, window: Duration) -> Self {
        self.stall_window = window;
        self
    }

    /// Transcript files under `root` modified within the active window.
    fn recent_transcripts(&self) -> Vec<PathBuf> {
        let now = SystemTime::now();
        let mut out = Vec::new();
        let Ok(projects) = std::fs::read_dir(&self.root) else {
            return out;
        };
        for project in projects.flatten() {
            let Ok(sessions) = std::fs::read_dir(project.path()) else {
                continue;
            };
            for session in sessions.flatten() {
                let path = session.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let recent = session
                    .metadata()
                    .and_then(|m| m.modified())
                    .map(|modified| {
                        now.duration_since(modified)
                            .map(|age| age <= self.active_window)
                            .unwrap_or(true)
                    })
                    .unwrap_or(false);
                if recent {
                    out.push(path);
                }
            }
        }
        out
    }
}

impl Default for JsonlDetectionSource {
    /// `~/.claude/projects`, 6-hour active window.
    fn default() -> Self {
        let home = std::env::var("HOME").unwrap_or_default();
        let root = Path::new(&home).join(".claude").join("projects");
        Self::new(root, Duration::from_secs(6 * 3600))
    }
}

/// How long ago `path` was last modified (its append recency). Defaults to zero
/// (treated as just-written) if the time can't be read.
pub(crate) fn file_age(path: &Path) -> Duration {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|mtime| SystemTime::now().duration_since(mtime).ok())
        .unwrap_or(Duration::ZERO)
}

/// Read `path` from `offset` to EOF as raw bytes; returns `(bytes, rotated)`. A
/// file shorter than `offset` was truncated/rotated → re-read from the start.
/// Returns raw bytes (not `read_to_string`) so a tail caught mid-multibyte-char
/// doesn't error — the caller decodes only the complete (newline-terminated) part.
pub(crate) fn read_tail(path: &Path, offset: u64) -> std::io::Result<(Vec<u8>, bool)> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let (start, rotated) = if len < offset {
        (0, true)
    } else {
        (offset, false)
    };
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok((bytes, rotated))
}

#[async_trait]
impl DetectionSource for JsonlDetectionSource {
    async fn poll(&self) -> Result<Vec<DetectionEvent>, ControlError> {
        let files = self.recent_transcripts();
        let seen: HashSet<String> = files
            .iter()
            .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(str::to_string))
            .collect();

        let mut out = Vec::new();
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());

        for path in &files {
            let Some(sid) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            let entry = state.entry(sid.clone()).or_default();
            let prev_offset = entry.offset;

            let (bytes, rotated) = match read_tail(path, prev_offset) {
                Ok(v) => v,
                Err(_) => continue, // unreadable right now; retry next poll
            };
            if rotated {
                *entry = FileState::default();
            }
            entry.missed = 0; // seen this poll
            let start = if rotated { 0 } else { prev_offset };

            // Consume only up to the last newline; a trailing partial line (a
            // mid-write append) is left unconsumed so the complete line is parsed
            // next poll instead of being skipped. Offset stays in *file* bytes.
            let complete_len = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
            entry.offset = start + complete_len as u64;
            let text = String::from_utf8_lossy(&bytes[..complete_len]);

            let id = SessionId::new(sid.clone());
            if !entry.discovered {
                entry.discovered = true;
                out.push(DetectionEvent::Discovered {
                    session: id.clone(),
                });
            }

            for line in text.lines() {
                for signal in parse_line(line) {
                    match signal {
                        // The rolling auto ai-title — ignored once the operator has
                        // explicitly named the session (custom-title outranks it).
                        Signal::Title(title)
                            if !entry.custom_titled && entry.title.as_deref() != Some(&title) =>
                        {
                            entry.title = Some(title.clone());
                            out.push(DetectionEvent::TitleObserved {
                                session: id.clone(),
                                title,
                            });
                        }
                        Signal::CustomTitle(title) => {
                            entry.custom_titled = true;
                            if entry.title.as_deref() != Some(&title) {
                                entry.title = Some(title.clone());
                                out.push(DetectionEvent::TitleObserved {
                                    session: id.clone(),
                                    title,
                                });
                            }
                        }
                        Signal::Phase(phase) if entry.phase != Some(phase) => {
                            entry.phase = Some(phase);
                            out.push(DetectionEvent::PhaseObserved {
                                session: id.clone(),
                                phase,
                            });
                        }
                        Signal::Workspace(path) if entry.path.as_deref() != Some(&path) => {
                            entry.path = Some(path.clone());
                            out.push(DetectionEvent::WorkspaceObserved {
                                session: id.clone(),
                                path,
                            });
                        }
                        // A turn updates the conversational state; the actual status
                        // is derived below from `last_turn` + how recent the file is.
                        Signal::Turn(turn) => {
                            // A clean stop proves a Stop hook is active → we can trust a
                            // later quiet `Assistant` turn to mean "interrupted".
                            if turn == Turn::StopClean {
                                entry.seen_clean_stop = true;
                            }
                            entry.last_turn = Some(turn);
                        }
                        Signal::Plan(plan) if entry.plan.as_deref() != Some(&plan) => {
                            entry.plan = Some(plan.clone());
                            out.push(DetectionEvent::PlanProposed {
                                session: id.clone(),
                                plan,
                            });
                        }
                        Signal::Summary(summary)
                            if entry.summary.as_deref() != Some(&summary) =>
                        {
                            entry.summary = Some(summary.clone());
                            out.push(DetectionEvent::SummaryObserved {
                                session: id.clone(),
                                summary,
                            });
                        }
                        _ => {}
                    }
                }
            }

            // Recompute live status every poll (even with no new lines), so a
            // session flips working→waiting as its transcript goes quiet.
            let age = file_age(path);
            let desired = status_for(entry.last_turn, age, self.liveness_window);
            if entry.status != Some(desired) {
                entry.status = Some(desired);
                out.push(DetectionEvent::StatusChanged {
                    session: id.clone(),
                    status: desired,
                });
            }

            // Stall → `Incomplete` ⚠: a turn that stays silent far past the working
            // window stopped without finishing. Two reliable cases:
            //  - a `User` line (operator prompt / tool result) the agent never answered;
            //  - an `Assistant` turn with no following clean stop — *only* when this
            //    session has produced a clean stop before (so a `Stop` hook is active
            //    and "no clean stop" genuinely means cut off, not a hookless normal end).
            // A long *tool* runs under an `Assistant` turn but is short-lived between
            // its tool_use and tool_result writes, and a hookless session never trips the
            // `Assistant` arm — both keep false positives out. Edge-emitted (once each
            // on stall / recovery).
            let interruptible_turn = match entry.last_turn {
                Some(Turn::User) => true,
                Some(Turn::Assistant) => entry.seen_clean_stop,
                _ => false,
            };
            let stalled = interruptible_turn && age >= self.stall_window;
            if stalled && !entry.alerted {
                entry.alerted = true;
                out.push(DetectionEvent::Alert {
                    session: id.clone(),
                    alert: Some(AttentionKind::Incomplete),
                });
            } else if !stalled && entry.alerted {
                entry.alerted = false;
                out.push(DetectionEvent::Alert {
                    session: id.clone(),
                    alert: None,
                });
            }
        }

        // Sessions whose transcript dropped out of the active window: count
        // consecutive misses, and only `Ended` after several (anti-flap). The
        // supervisor decides whether to actually evict (it keeps adopted sessions).
        const STALE_MISSES: u8 = 3;
        let mut ended = Vec::new();
        for (sid, st) in state.iter_mut() {
            if st.discovered && !seen.contains(sid) {
                st.missed = st.missed.saturating_add(1);
                if st.missed >= STALE_MISSES {
                    ended.push(sid.clone());
                }
            }
        }
        for sid in ended {
            state.remove(&sid);
            out.push(DetectionEvent::Ended {
                session: SessionId::new(sid),
            });
        }

        Ok(out)
    }
}

/// Fans a poll out across several [`DetectionSource`]s and concatenates their events,
/// so the engine — which takes a single source — can observe Claude **and** Antigravity
/// sessions at once. A source that errors is skipped (its sibling still reports); the
/// composite never fails the whole poll because one backend's directory is unreadable.
pub struct CompositeDetectionSource {
    sources: Vec<Box<dyn DetectionSource>>,
}

impl CompositeDetectionSource {
    pub fn new(sources: Vec<Box<dyn DetectionSource>>) -> Self {
        Self { sources }
    }
}

#[async_trait]
impl DetectionSource for CompositeDetectionSource {
    async fn poll(&self) -> Result<Vec<DetectionEvent>, ControlError> {
        let mut out = Vec::new();
        for source in &self.sources {
            if let Ok(mut events) = source.poll().await {
                out.append(&mut events);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(path: &Path, contents: &str) {
        let mut f = File::create(path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
    }

    #[tokio::test]
    async fn a_silent_handed_turn_raises_then_clears_an_incomplete_alert() {
        let dir = std::env::temp_dir().join(format!("ml-detect-stall-{}", std::process::id()));
        let project = dir.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let transcript = project.join("sess-stall.jsonl");

        // stall_window = 0 → a handed-a-turn (`user`) transcript counts as stalled at
        // once; liveness 0 keeps status deterministic.
        let source = JsonlDetectionSource::new(dir.clone(), Duration::from_secs(3600))
            .with_liveness_window(Duration::ZERO)
            .with_stall_window(Duration::ZERO);
        let id = SessionId::new("sess-stall");

        // Handed the turn (a user / tool-result line) then silent → Incomplete ⚠.
        write(&transcript, "{\"type\":\"user\",\"cwd\":\"/w\"}\n");
        let events = source.poll().await.unwrap();
        assert!(
            events.contains(&DetectionEvent::Alert {
                session: id.clone(),
                alert: Some(AttentionKind::Incomplete),
            }),
            "{events:?}"
        );

        // Idempotent: still stalled, but the alert is edge-emitted (no repeat).
        let again = source.poll().await.unwrap();
        assert!(
            !again.iter().any(|e| matches!(e, DetectionEvent::Alert { .. })),
            "{again:?}"
        );

        // The agent comes back (an assistant line) → last turn is no longer `User` →
        // the stall clears.
        write(
            &transcript,
            "{\"type\":\"user\",\"cwd\":\"/w\"}\n{\"type\":\"assistant\"}\n",
        );
        let delta = source.poll().await.unwrap();
        assert!(
            delta.contains(&DetectionEvent::Alert {
                session: id.clone(),
                alert: None,
            }),
            "{delta:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn an_assistant_stall_only_fires_after_a_clean_stop_proves_a_stop_hook() {
        let dir = std::env::temp_dir().join(format!("ml-detect-astall-{}", std::process::id()));
        let project = dir.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let transcript = project.join("sess-astall.jsonl");

        let source = JsonlDetectionSource::new(dir.clone(), Duration::from_secs(3600))
            .with_liveness_window(Duration::ZERO)
            .with_stall_window(Duration::ZERO);
        let id = SessionId::new("sess-astall");

        // A quiet `Assistant` turn with NO clean stop ever seen = a hookless session's
        // normal end → conservatively NOT flagged (no false positive).
        write(&transcript, "{\"type\":\"assistant\"}\n");
        let events = source.poll().await.unwrap();
        assert!(
            !events.iter().any(|e| matches!(
                e,
                DetectionEvent::Alert { alert: Some(_), .. }
            )),
            "{events:?}"
        );

        // A clean stop appears (stop_hook_summary) → proves a Stop hook is active.
        write(
            &transcript,
            "{\"type\":\"assistant\"}\n{\"type\":\"system\",\"subtype\":\"stop_hook_summary\"}\n",
        );
        let _ = source.poll().await.unwrap(); // last turn is now a clean stop → calm.

        // A fresh `Assistant` turn with no following clean stop, gone quiet → the
        // generation was cut off → Incomplete ⚠.
        write(
            &transcript,
            "{\"type\":\"assistant\"}\n{\"type\":\"system\",\"subtype\":\"stop_hook_summary\"}\n{\"type\":\"assistant\"}\n",
        );
        let delta = source.poll().await.unwrap();
        assert!(
            delta.contains(&DetectionEvent::Alert {
                session: id.clone(),
                alert: Some(AttentionKind::Incomplete),
            }),
            "{delta:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn discovers_tails_and_refreshes_status() {
        let dir = std::env::temp_dir().join(format!("ml-detect-{}", std::process::id()));
        let project = dir.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let transcript = project.join("sess-abc.jsonl");

        // liveness_window = 0 → status is purely turn-derived (deterministic here).
        let source = JsonlDetectionSource::new(dir.clone(), Duration::from_secs(3600))
            .with_liveness_window(Duration::ZERO);

        // Head ends on the operator's prompt → the agent is working (Running).
        let head = "{\"type\":\"ai-title\",\"aiTitle\":\"Build it\"}\n\
             {\"type\":\"permission-mode\",\"permissionMode\":\"plan\"}\n\
             {\"type\":\"user\",\"cwd\":\"/work/repo\"}\n";
        write(&transcript, head);

        let events = source.poll().await.unwrap();
        let id = SessionId::new("sess-abc");
        assert!(events.contains(&DetectionEvent::Discovered {
            session: id.clone()
        }));
        assert!(events.contains(&DetectionEvent::TitleObserved {
            session: id.clone(),
            title: "Build it".into()
        }));
        assert!(events.contains(&DetectionEvent::PhaseObserved {
            session: id.clone(),
            phase: Phase::Plan
        }));
        assert!(events.contains(&DetectionEvent::WorkspaceObserved {
            session: id.clone(),
            path: "/work/repo".into()
        }));
        assert!(events.contains(&DetectionEvent::StatusChanged {
            session: id.clone(),
            status: SessionStatus::Running
        }));

        // No new bytes ⇒ status unchanged ⇒ nothing emitted.
        assert!(source.poll().await.unwrap().is_empty());

        // The model replies → last turn is Assistant → status flips to WaitingInput.
        write(
            &transcript,
            &format!("{head}{}", "{\"type\":\"assistant\"}\n"),
        );
        let delta = source.poll().await.unwrap();
        assert_eq!(
            delta,
            vec![DetectionEvent::StatusChanged {
                session: id,
                status: SessionStatus::WaitingInput
            }]
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn partial_final_line_is_buffered_until_complete() {
        let dir = std::env::temp_dir().join(format!("ml-detect-partial-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("p")).unwrap();
        let t = dir.join("p").join("sx.jsonl");
        let source = JsonlDetectionSource::new(dir.clone(), Duration::from_secs(3600))
            .with_liveness_window(Duration::ZERO);
        let id = SessionId::new("sx");

        // A complete title line + a partial (no trailing newline) second line.
        write(
            &t,
            "{\"type\":\"ai-title\",\"aiTitle\":\"A\"}\n{\"type\":\"ai-title\",\"aiTit",
        );
        let ev = source.poll().await.unwrap();
        assert!(ev.contains(&DetectionEvent::TitleObserved {
            session: id.clone(),
            title: "A".into()
        }));
        // The partial line is NOT parsed yet.
        assert!(!ev
            .iter()
            .any(|e| matches!(e, DetectionEvent::TitleObserved { title, .. } if title == "B")));

        // Completing the line (same prefix bytes) yields the buffered title.
        write(
            &t,
            "{\"type\":\"ai-title\",\"aiTitle\":\"A\"}\n{\"type\":\"ai-title\",\"aiTitle\":\"B\"}\n",
        );
        let ev2 = source.poll().await.unwrap();
        assert!(ev2.contains(&DetectionEvent::TitleObserved {
            session: id,
            title: "B".into()
        }));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn stale_session_ends_only_after_consecutive_misses() {
        let dir = std::env::temp_dir().join(format!("ml-detect-stale-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("p")).unwrap();
        let t = dir.join("p").join("sy.jsonl");
        let source = JsonlDetectionSource::new(dir.clone(), Duration::from_secs(3600));
        let id = SessionId::new("sy");

        write(&t, "{\"type\":\"user\"}\n");
        assert!(source
            .poll()
            .await
            .unwrap()
            .contains(&DetectionEvent::Discovered {
                session: id.clone()
            }));

        std::fs::remove_file(&t).unwrap(); // session now absent from the active window
        let ended = |evs: &[DetectionEvent]| evs.iter().any(|e| matches!(e, DetectionEvent::Ended { .. }));
        assert!(!ended(&source.poll().await.unwrap()), "miss 1");
        assert!(!ended(&source.poll().await.unwrap()), "miss 2");
        assert!(
            source
                .poll()
                .await
                .unwrap()
                .contains(&DetectionEvent::Ended { session: id }),
            "miss 3 ends it"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
