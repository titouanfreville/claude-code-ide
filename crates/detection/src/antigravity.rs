//! Detection over Antigravity (`agy`) transcripts.
//!
//! AGY writes one conversation per directory under
//! `~/.gemini/antigravity-cli/brain/<conversationId>/.system_generated/logs/transcript.jsonl`,
//! with records like `{"type":"USER_INPUT"|"PLANNER_RESPONSE"|"RUN_COMMAND"|…,"source":…}`
//! — a different layout and vocabulary than Claude's `~/.claude/projects/**/*.jsonl`.
//!
//! This mirrors [`crate::JsonlDetectionSource`]'s tail/offset/status/stall machinery
//! (reusing [`crate::file_age`] / [`crate::read_tail`] and the shared [`Turn`] →
//! [`status_for`] derivation), but is **lean by design**: it emits `Discovered`,
//! `StatusChanged`, and the `Incomplete` stall alert. Title/phase/plan/summary aren't
//! cleanly present in AGY's transcript, so they're left to a later pass (managed AGY
//! sessions already carry their title/phase/root on the persisted record). All input is
//! untrusted: an unparsable line yields no turn and is skipped — never a panic.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use moonlight_domain::errors::ControlError;
use moonlight_domain::ids::SessionId;
use moonlight_domain::ports::detection::{DetectionEvent, DetectionSource};
use moonlight_domain::session::{AttentionKind, SessionStatus};

use crate::jsonl::{status_for, Turn};
use crate::{file_age, read_tail};

const DEFAULT_LIVENESS_WINDOW: Duration = Duration::from_secs(20);
const DEFAULT_STALL_WINDOW: Duration = Duration::from_secs(180);
/// See [`crate::DEFAULT_DONE_WINDOW`] — a finished turn quiet this long cools to `Idle`.
const DEFAULT_DONE_WINDOW: Duration = Duration::from_secs(120);

/// Per-conversation tail state (a lean cousin of the Claude `FileState`).
#[derive(Default)]
struct AgyState {
    offset: u64,
    discovered: bool,
    titled: bool,
    status: Option<SessionStatus>,
    last_turn: Option<Turn>,
    missed: u8,
    alerted: bool,
}

/// Longest title we derive from a first request — a tile label, not the whole prompt.
const MAX_TITLE: usize = 60;

/// Derive a session title from an AGY `USER_INPUT` record's first request. AGY has no
/// `ai-title` in the transcript (and no stored title column), so the operator's first
/// prompt — the text inside `<USER_REQUEST>…</USER_REQUEST>` — is the best available
/// label. Returns `None` for non-`USER_INPUT` lines or an empty request.
pub fn title_for_agy_line(line: &str) -> Option<String> {
    let raw: serde_json::Value = serde_json::from_str(line).ok()?;
    if raw.get("type").and_then(|v| v.as_str()) != Some("USER_INPUT") {
        return None;
    }
    let content = raw.get("content").and_then(|v| v.as_str())?;
    // Pull the text between the request tags; fall back to the raw content if unwrapped.
    let request = match (
        content.find("<USER_REQUEST>"),
        content.find("</USER_REQUEST>"),
    ) {
        (Some(s), Some(e)) if e > s => &content[s + "<USER_REQUEST>".len()..e],
        _ => content,
    };
    let title = request.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        return None;
    }
    if title.chars().count() > MAX_TITLE {
        let mut t: String = title.chars().take(MAX_TITLE).collect();
        t.push('…');
        Some(t)
    } else {
        Some(title)
    }
}

/// Map an AGY transcript record to a conversational [`Turn`], or `None` for noise.
///
/// - `USER_INPUT` → the operator/tool handed the agent a turn → it is (about to be)
///   working ([`Turn::User`]).
/// - Conversation bookkeeping / surfaced errors → `None` (keep the prior turn; an
///   `ERROR_MESSAGE` is often a recoverable tool error, not the session erroring out).
/// - Everything else (model output / tool action: `PLANNER_RESPONSE`, `RUN_COMMAND`,
///   `VIEW_FILE`, …) → the agent acted ([`Turn::Assistant`]).
pub fn turn_for_agy_line(line: &str) -> Option<Turn> {
    let raw: serde_json::Value = serde_json::from_str(line).ok()?;
    match raw.get("type").and_then(|v| v.as_str()).unwrap_or("") {
        "USER_INPUT" => Some(Turn::User),
        "" | "CONVERSATION_HISTORY" | "CHECKPOINT" | "SYSTEM_MESSAGE" | "ERROR_MESSAGE" => None,
        _ => Some(Turn::Assistant),
    }
}

/// Tails AGY conversation transcripts and emits [`DetectionEvent`]s.
pub struct AntigravityDetectionSource {
    /// The `brain/` directory holding one subdir per conversation.
    root: PathBuf,
    active_window: Duration,
    liveness_window: Duration,
    stall_window: Duration,
    done_window: Duration,
    state: Mutex<HashMap<String, AgyState>>,
}

impl AntigravityDetectionSource {
    pub fn new(root: PathBuf, active_window: Duration) -> Self {
        Self {
            root,
            active_window,
            liveness_window: DEFAULT_LIVENESS_WINDOW,
            stall_window: DEFAULT_STALL_WINDOW,
            done_window: DEFAULT_DONE_WINDOW,
            state: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_liveness_window(mut self, window: Duration) -> Self {
        self.liveness_window = window;
        self
    }

    pub fn with_done_window(mut self, window: Duration) -> Self {
        self.done_window = window;
        self
    }

    pub fn with_stall_window(mut self, window: Duration) -> Self {
        self.stall_window = window;
        self
    }

    /// The transcript path for a conversation directory.
    fn transcript_of(dir: &Path) -> PathBuf {
        dir.join(".system_generated")
            .join("logs")
            .join("transcript.jsonl")
    }

    /// `(conversationId, transcript_path)` for every conversation whose transcript was
    /// modified within the active window.
    fn recent_conversations(&self) -> Vec<(String, PathBuf)> {
        let now = SystemTime::now();
        let mut out = Vec::new();
        let Ok(convs) = std::fs::read_dir(&self.root) else {
            return out;
        };
        for conv in convs.flatten() {
            let dir = conv.path();
            if !dir.is_dir() {
                continue;
            }
            let Some(id) = dir.file_name().and_then(|s| s.to_str()).map(str::to_string) else {
                continue;
            };
            let transcript = Self::transcript_of(&dir);
            let recent = std::fs::metadata(&transcript)
                .and_then(|m| m.modified())
                .map(|modified| {
                    now.duration_since(modified)
                        .map(|age| age <= self.active_window)
                        .unwrap_or(true)
                })
                .unwrap_or(false);
            if recent {
                out.push((id, transcript));
            }
        }
        out
    }
}

impl Default for AntigravityDetectionSource {
    /// `~/.gemini/antigravity-cli/brain`, 6-hour active window.
    fn default() -> Self {
        let home = std::env::var("HOME").unwrap_or_default();
        let root = Path::new(&home)
            .join(".gemini")
            .join("antigravity-cli")
            .join("brain");
        Self::new(root, Duration::from_secs(6 * 3600))
    }
}

#[async_trait]
impl DetectionSource for AntigravityDetectionSource {
    async fn poll(&self) -> Result<Vec<DetectionEvent>, ControlError> {
        let convs = self.recent_conversations();
        let seen: HashSet<String> = convs.iter().map(|(id, _)| id.clone()).collect();

        let mut out = Vec::new();
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());

        for (sid, path) in &convs {
            let entry = state.entry(sid.clone()).or_default();
            let prev_offset = entry.offset;

            let (bytes, rotated) = match read_tail(path, prev_offset) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if rotated {
                *entry = AgyState::default();
            }
            entry.missed = 0;
            let start = if rotated { 0 } else { prev_offset };
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
                if let Some(turn) = turn_for_agy_line(line) {
                    entry.last_turn = Some(turn);
                }
                // Title from the first user request (AGY has no transcript ai-title).
                if !entry.titled {
                    if let Some(title) = title_for_agy_line(line) {
                        entry.titled = true;
                        out.push(DetectionEvent::TitleObserved {
                            session: id.clone(),
                            title,
                        });
                    }
                }
            }

            // Recompute live status every poll against the file's recency.
            let age = file_age(path);
            let desired = status_for(entry.last_turn, age, self.liveness_window, self.done_window);
            if entry.status != Some(desired) {
                entry.status = Some(desired);
                out.push(DetectionEvent::StatusChanged {
                    session: id.clone(),
                    status: desired,
                });
            }

            // Stall → `Incomplete` ⚠. AGY has no clean-stop signal in the transcript, so
            // we use the **conservative** rule: only a handed `User` turn that then goes
            // silent past the stall window counts (an `Assistant` quiet period is a
            // normal end-of-turn — flagging it would false-positive). Edge-emitted.
            let stalled = matches!(entry.last_turn, Some(Turn::User)) && age >= self.stall_window;
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

        // Sessions whose transcript dropped out of the active window: `Ended` after
        // several consecutive misses (anti-flap), same as the Claude source.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    fn write_transcript(root: &Path, conv: &str, contents: &str) -> PathBuf {
        let logs = root.join(conv).join(".system_generated").join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        let t = logs.join("transcript.jsonl");
        let mut f = File::create(&t).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        t
    }

    #[test]
    fn turn_mapping_covers_the_agy_vocabulary() {
        assert_eq!(
            turn_for_agy_line(r#"{"type":"USER_INPUT","source":"USER_EXPLICIT"}"#),
            Some(Turn::User)
        );
        assert_eq!(
            turn_for_agy_line(r#"{"type":"PLANNER_RESPONSE","source":"MODEL"}"#),
            Some(Turn::Assistant)
        );
        assert_eq!(
            turn_for_agy_line(r#"{"type":"RUN_COMMAND"}"#),
            Some(Turn::Assistant)
        );
        // Bookkeeping / errors carry no turn.
        assert_eq!(turn_for_agy_line(r#"{"type":"CHECKPOINT"}"#), None);
        assert_eq!(turn_for_agy_line(r#"{"type":"ERROR_MESSAGE"}"#), None);
        // Garbage never panics.
        assert_eq!(turn_for_agy_line("not json"), None);
    }

    #[test]
    fn title_comes_from_the_first_user_request() {
        let line = r#"{"type":"USER_INPUT","content":"<USER_REQUEST>\nRefactor the auth module\n</USER_REQUEST>\n<ADDITIONAL_METADATA>noise</ADDITIONAL_METADATA>"}"#;
        assert_eq!(
            title_for_agy_line(line).as_deref(),
            Some("Refactor the auth module")
        );
        // Non-user records carry no title.
        assert_eq!(title_for_agy_line(r#"{"type":"PLANNER_RESPONSE"}"#), None);
        // Over-long requests are truncated to a tile label.
        let long = format!(
            "{{\"type\":\"USER_INPUT\",\"content\":\"{}\"}}",
            "word ".repeat(40)
        );
        let t = title_for_agy_line(&long).unwrap();
        assert!(
            t.chars().count() <= MAX_TITLE + 1 && t.ends_with('…'),
            "{t}"
        );
    }

    #[tokio::test]
    async fn emits_title_once_from_the_first_request() {
        let root = std::env::temp_dir().join(format!("ml-agy-title-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let source = AntigravityDetectionSource::new(root.clone(), Duration::from_secs(3600))
            .with_liveness_window(Duration::ZERO);
        let id = SessionId::new("conv-t");

        let head = "{\"type\":\"USER_INPUT\",\"content\":\"<USER_REQUEST>\\nBuild the thing\\n</USER_REQUEST>\"}\n";
        write_transcript(&root, "conv-t", head);
        let events = source.poll().await.unwrap();
        assert!(
            events.contains(&DetectionEvent::TitleObserved {
                session: id.clone(),
                title: "Build the thing".into(),
            }),
            "{events:?}"
        );

        // A second user request does NOT re-title (first request is the stable label).
        write_transcript(
            &root,
            "conv-t",
            &format!("{head}{}", "{\"type\":\"USER_INPUT\",\"content\":\"<USER_REQUEST>\\nAnother\\n</USER_REQUEST>\"}\n"),
        );
        let delta = source.poll().await.unwrap();
        assert!(
            !delta
                .iter()
                .any(|e| matches!(e, DetectionEvent::TitleObserved { .. })),
            "{delta:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn discovers_and_derives_status_from_turns() {
        let root = std::env::temp_dir().join(format!("ml-agy-detect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let source = AntigravityDetectionSource::new(root.clone(), Duration::from_secs(3600))
            .with_liveness_window(Duration::ZERO);
        let id = SessionId::new("conv-1");

        // Ends on USER_INPUT → the agent is working (Running). Transcripts are
        // append-only, so each step keeps the prior bytes (offset tailing relies on it).
        let head = "{\"type\":\"USER_INPUT\",\"source\":\"USER_EXPLICIT\"}\n";
        write_transcript(&root, "conv-1", head);
        let events = source.poll().await.unwrap();
        assert!(events.contains(&DetectionEvent::Discovered {
            session: id.clone()
        }));
        assert!(
            events.contains(&DetectionEvent::StatusChanged {
                session: id.clone(),
                status: SessionStatus::Running,
            }),
            "{events:?}"
        );

        // No new bytes ⇒ nothing emitted.
        assert!(source.poll().await.unwrap().is_empty());

        // The model responds (appended) → last turn Assistant → WaitingInput.
        write_transcript(
            &root,
            "conv-1",
            &format!(
                "{head}{}",
                "{\"type\":\"PLANNER_RESPONSE\",\"source\":\"MODEL\"}\n"
            ),
        );
        let delta = source.poll().await.unwrap();
        assert_eq!(
            delta,
            vec![DetectionEvent::StatusChanged {
                session: id,
                status: SessionStatus::WaitingInput,
            }]
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_silent_handed_turn_raises_then_clears_incomplete() {
        let root = std::env::temp_dir().join(format!("ml-agy-stall-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let source = AntigravityDetectionSource::new(root.clone(), Duration::from_secs(3600))
            .with_liveness_window(Duration::ZERO)
            .with_stall_window(Duration::ZERO);
        let id = SessionId::new("conv-stall");

        write_transcript(&root, "conv-stall", "{\"type\":\"USER_INPUT\"}\n");
        let events = source.poll().await.unwrap();
        assert!(
            events.contains(&DetectionEvent::Alert {
                session: id.clone(),
                alert: Some(AttentionKind::Incomplete),
            }),
            "{events:?}"
        );

        // Edge-emitted: still stalled, no repeat.
        assert!(!source
            .poll()
            .await
            .unwrap()
            .iter()
            .any(|e| matches!(e, DetectionEvent::Alert { .. })));

        // Agent comes back (a model turn) → stall clears.
        write_transcript(
            &root,
            "conv-stall",
            "{\"type\":\"USER_INPUT\"}\n{\"type\":\"PLANNER_RESPONSE\"}\n",
        );
        let delta = source.poll().await.unwrap();
        assert!(
            delta.contains(&DetectionEvent::Alert {
                session: id,
                alert: None
            }),
            "{delta:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
