//! Defensive parsing of Claude Code transcript JSONL lines into detection signals.
//!
//! Every artifact is untrusted input (NFR6): unparsable or unexpected lines yield
//! `None` and are skipped — never a panic. Only the high-signal line types are
//! decoded; everything else (attachment, mode, file-history-snapshot, …) is noise.

use std::time::Duration;

use moonlight_domain::phase::Phase;
use moonlight_domain::session::SessionStatus;
use serde::Deserialize;

/// The subset of transcript fields we read. All optional so an unexpected shape
/// still deserializes (and is then ignored by [`parse_line`]).
#[derive(Debug, Deserialize)]
struct RawLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "aiTitle")]
    ai_title: Option<String>,
    #[serde(rename = "customTitle")]
    custom_title: Option<String>,
    #[serde(rename = "permissionMode")]
    permission_mode: Option<String>,
    subtype: Option<String>,
    #[serde(rename = "stopReason")]
    stop_reason: Option<String>,
    #[serde(rename = "preventedContinuation")]
    prevented_continuation: Option<bool>,
    #[serde(rename = "hookErrors")]
    hook_errors: Option<Vec<serde_json::Value>>,
    cwd: Option<String>,
    /// The assistant API message (role + content blocks); we read `ExitPlanMode`
    /// tool calls out of it to surface plans.
    message: Option<serde_json::Value>,
}

/// The conversational state implied by the most recent meaningful transcript line.
/// Status is *not* taken directly from a single line (a transcript is almost always
/// full of `user`/`assistant` lines) — it is derived from the last turn plus how
/// recently the file was written (see [`status_for`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Turn {
    /// The operator submitted a prompt — the agent is (about to be) working.
    User,
    /// The model produced output — possibly mid-tool-loop, possibly the final reply.
    Assistant,
    /// A turn ended cleanly (stop-hook summary).
    StopClean,
    /// A turn ended with a hook error / prevented continuation.
    StopErrored,
}

/// One decoded signal from a transcript line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    /// CC's auto-generated `ai-title` (refreshes as the conversation moves).
    Title(String),
    /// The operator's explicit `/rename` name (`custom-title`) — outranks
    /// [`Signal::Title`]: once set, later auto-titles no longer override it.
    CustomTitle(String),
    Phase(Phase),
    Workspace(String),
    Turn(Turn),
    /// A plan proposed via `ExitPlanMode` (the plan markdown).
    Plan(String),
    /// The assistant's end-of-turn prose (final text blocks) — the review summary.
    Summary(String),
}

/// Parse one JSONL line into the signals it carries (a single line may yield
/// several, e.g. a `user` line conveys both a turn and a `cwd`). Returns an empty
/// vec for unparsable or uninteresting lines — never panics on bad input.
pub fn parse_line(line: &str) -> Vec<Signal> {
    let Ok(raw) = serde_json::from_str::<RawLine>(line) else {
        return Vec::new();
    };
    let mut signals = Vec::new();

    if let Some(cwd) = raw.cwd.as_deref().filter(|c| !c.is_empty()) {
        signals.push(Signal::Workspace(cwd.to_string()));
    }
    match raw.kind.as_deref() {
        Some("ai-title") => {
            if let Some(title) = raw.ai_title.filter(|t| !t.is_empty()) {
                signals.push(Signal::Title(title));
            }
        }
        // The operator's `/rename` in CC. Persisted in the transcript, so a rename
        // made in CC's own UI reaches the cockpit too.
        Some("custom-title") => {
            if let Some(title) = raw.custom_title.filter(|t| !t.is_empty()) {
                signals.push(Signal::CustomTitle(title));
            }
        }
        Some("permission-mode") => {
            if let Some(mode) = raw.permission_mode.as_deref() {
                signals.push(Signal::Phase(phase_from_mode(mode)));
            }
        }
        Some("user") => signals.push(Signal::Turn(Turn::User)),
        Some("assistant") => {
            signals.push(Signal::Turn(Turn::Assistant));
            if let Some(plan) = raw.message.as_ref().and_then(extract_exit_plan) {
                signals.push(Signal::Plan(plan));
            } else if let Some(text) = raw.message.as_ref().and_then(extract_assistant_text) {
                // Only non-plan prose becomes a review summary (a plan turn is the
                // plan, not a "what this covers" summary).
                signals.push(Signal::Summary(text));
            }
        }
        Some("system") if raw.subtype.as_deref() == Some("stop_hook_summary") => {
            let turn = if stop_errored(&raw) {
                Turn::StopErrored
            } else {
                Turn::StopClean
            };
            signals.push(Signal::Turn(turn));
        }
        _ => {}
    }
    signals
}

/// Map a Claude Code permission mode → workflow phase. Coarse by nature: only
/// plan-vs-not is observable from the transcript, so every non-plan CC mode
/// (`auto`, `acceptEdits`, `bypassPermissions`, `dontAsk`, `default`) maps to
/// `AutoImplement` (the closest non-gated phase). The richer Test/Review/Commit
/// phases aren't expressible as a CC permission mode, so they never come from here.
pub fn phase_from_mode(mode: &str) -> Phase {
    match mode {
        "plan" => Phase::Plan,
        // "auto" | "acceptEdits" | "bypassPermissions" | "dontAsk" | "default"
        _ => Phase::AutoImplement,
    }
}

/// Derive the live status from the last observed turn and how long ago the
/// transcript was last written (`age`). This is what makes status *refresh*:
/// it is recomputed every poll against the file's current mtime.
///
/// - A recently-written transcript ⇒ **Running** (actively working).
/// - Quiet after the operator's prompt ⇒ **Running** (the model is thinking).
/// - Quiet after the model replied / a clean stop ⇒ **WaitingInput** (your turn).
/// - A hook-errored stop ⇒ **Errored**.
/// - No turn observed yet ⇒ **Idle**.
pub fn status_for(
    last_turn: Option<Turn>,
    age: Duration,
    liveness_window: Duration,
) -> SessionStatus {
    if matches!(last_turn, Some(Turn::StopErrored)) {
        return SessionStatus::Errored;
    }
    if age < liveness_window {
        return SessionStatus::Running;
    }
    match last_turn {
        Some(Turn::User) => SessionStatus::Running,
        Some(Turn::Assistant) | Some(Turn::StopClean) => SessionStatus::WaitingInput,
        Some(Turn::StopErrored) => SessionStatus::Errored,
        None => SessionStatus::Idle,
    }
}

/// Pull the plan markdown out of an assistant message that contains an
/// `ExitPlanMode` tool call. Returns `None` if there is no such call.
fn extract_exit_plan(message: &serde_json::Value) -> Option<String> {
    let content = message.get("content")?.as_array()?;
    for block in content {
        let is_exit_plan = block.get("type").and_then(|v| v.as_str()) == Some("tool_use")
            && block.get("name").and_then(|v| v.as_str()) == Some("ExitPlanMode");
        if is_exit_plan {
            let plan = block
                .get("input")
                .and_then(|i| i.get("plan"))
                .and_then(|p| p.as_str())
                .unwrap_or("");
            if !plan.is_empty() {
                return Some(plan.to_string());
            }
        }
    }
    None
}

/// Concatenate the `text` blocks of an assistant message (its visible prose),
/// trimmed. Returns `None` when the message is tool-only / has no text — those
/// turns carry no summary. Capped so a runaway message can't bloat an event.
fn extract_assistant_text(message: &serde_json::Value) -> Option<String> {
    /// Cap the captured summary so an unusually long final message stays a summary.
    const MAX_SUMMARY: usize = 2000;
    let content = message.get("content")?.as_array()?;
    let mut parts = Vec::new();
    for block in content {
        if block.get("type").and_then(|v| v.as_str()) == Some("text") {
            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                let text = text.trim();
                if !text.is_empty() {
                    parts.push(text.to_string());
                }
            }
        }
    }
    if parts.is_empty() {
        return None;
    }
    let mut summary = parts.join("\n\n");
    if summary.len() > MAX_SUMMARY {
        // Truncate on a char boundary so the lossy cap never splits a UTF-8 char.
        let mut end = MAX_SUMMARY;
        while !summary.is_char_boundary(end) {
            end -= 1;
        }
        summary.truncate(end);
        summary.push('…');
    }
    Some(summary)
}

fn stop_errored(raw: &RawLine) -> bool {
    raw.prevented_continuation.unwrap_or(false)
        || raw.hook_errors.as_ref().is_some_and(|e| !e.is_empty())
        || raw
            .stop_reason
            .as_deref()
            .is_some_and(|s| s.to_lowercase().contains("error"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ai_title() {
        let custom = r#"{"type":"custom-title","customTitle":"IDE Bottom Bar","sessionId":"s"}"#;
        assert_eq!(
            parse_line(custom),
            vec![Signal::CustomTitle("IDE Bottom Bar".into())]
        );

        let line = r#"{"type":"ai-title","aiTitle":"Refactor auth","sessionId":"s"}"#;
        assert_eq!(
            parse_line(line),
            vec![Signal::Title("Refactor auth".into())]
        );
    }

    #[test]
    fn maps_plan_and_default_phases() {
        let plan = r#"{"type":"permission-mode","permissionMode":"plan","sessionId":"s"}"#;
        let def = r#"{"type":"permission-mode","permissionMode":"default","sessionId":"s"}"#;
        assert_eq!(parse_line(plan), vec![Signal::Phase(Phase::Plan)]);
        assert_eq!(parse_line(def), vec![Signal::Phase(Phase::AutoImplement)]);
        // Every non-plan CC permission mode collapses to AutoImplement.
        for mode in ["auto", "acceptEdits", "bypassPermissions", "dontAsk", "default"] {
            assert_eq!(phase_from_mode(mode), Phase::AutoImplement, "mode {mode}");
        }
        assert_eq!(phase_from_mode("plan"), Phase::Plan);
    }

    #[test]
    fn activity_line_yields_turn_and_workspace() {
        // A real assistant line carries both a turn and the cwd.
        let line = r#"{"type":"assistant","timestamp":"t","cwd":"/repo/api"}"#;
        assert_eq!(
            parse_line(line),
            vec![
                Signal::Workspace("/repo/api".into()),
                Signal::Turn(Turn::Assistant)
            ]
        );
        assert_eq!(
            parse_line(r#"{"type":"user","timestamp":"t"}"#),
            vec![Signal::Turn(Turn::User)]
        );
    }

    #[test]
    fn extracts_plan_from_exit_plan_mode() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[
            {"type":"text","text":"Here is my plan"},
            {"type":"tool_use","name":"ExitPlanMode","input":{"plan":"1. do X\n2. do Y"}}
        ]}}"#;
        let signals = parse_line(line);
        assert!(signals.contains(&Signal::Turn(Turn::Assistant)));
        assert!(signals.contains(&Signal::Plan("1. do X\n2. do Y".into())));

        // A plain assistant message (no ExitPlanMode) yields no plan.
        let plain = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}"#;
        assert!(!parse_line(plain)
            .iter()
            .any(|s| matches!(s, Signal::Plan(_))));
    }

    #[test]
    fn extracts_summary_from_assistant_text() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[
            {"type":"text","text":"I added retry with backoff and a test."}
        ]}}"#;
        assert!(parse_line(line)
            .contains(&Signal::Summary("I added retry with backoff and a test.".into())));

        // A tool-only assistant turn carries no summary.
        let tool = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Edit","input":{}}]}}"#;
        assert!(!parse_line(tool)
            .iter()
            .any(|s| matches!(s, Signal::Summary(_))));

        // A plan turn yields a Plan, never a Summary.
        let planning = r#"{"type":"assistant","message":{"role":"assistant","content":[
            {"type":"text","text":"Here is my plan"},
            {"type":"tool_use","name":"ExitPlanMode","input":{"plan":"do X"}}
        ]}}"#;
        let ps = parse_line(planning);
        assert!(ps.iter().any(|s| matches!(s, Signal::Plan(_))));
        assert!(!ps.iter().any(|s| matches!(s, Signal::Summary(_))));
    }

    #[test]
    fn stop_summary_turn_reflects_errors() {
        let ok = r#"{"type":"system","subtype":"stop_hook_summary","preventedContinuation":false,"hookErrors":[]}"#;
        let prevented =
            r#"{"type":"system","subtype":"stop_hook_summary","preventedContinuation":true}"#;
        let with_errors =
            r#"{"type":"system","subtype":"stop_hook_summary","hookErrors":["boom"]}"#;
        assert_eq!(parse_line(ok), vec![Signal::Turn(Turn::StopClean)]);
        assert_eq!(parse_line(prevented), vec![Signal::Turn(Turn::StopErrored)]);
        assert_eq!(
            parse_line(with_errors),
            vec![Signal::Turn(Turn::StopErrored)]
        );
    }

    #[test]
    fn status_for_derives_live_status() {
        let fresh = Duration::from_secs(1);
        let quiet = Duration::from_secs(120);
        let window = Duration::from_secs(20);

        // Recently written ⇒ Running regardless of last turn.
        assert_eq!(
            status_for(Some(Turn::Assistant), fresh, window),
            SessionStatus::Running
        );
        // Quiet after the model replied ⇒ your turn.
        assert_eq!(
            status_for(Some(Turn::Assistant), quiet, window),
            SessionStatus::WaitingInput
        );
        // Quiet after the operator prompted ⇒ the model is thinking.
        assert_eq!(
            status_for(Some(Turn::User), quiet, window),
            SessionStatus::Running
        );
        // Errored stop is sticky even when quiet.
        assert_eq!(
            status_for(Some(Turn::StopErrored), quiet, window),
            SessionStatus::Errored
        );
        // Nothing observed yet ⇒ idle.
        assert_eq!(status_for(None, quiet, window), SessionStatus::Idle);
    }

    #[test]
    fn garbage_and_noise_are_ignored() {
        assert!(parse_line("not json at all").is_empty());
        assert!(parse_line("").is_empty());
        assert!(parse_line(r#"{"type":"attachment","foo":1}"#).is_empty());
        assert!(parse_line(r#"{"no_type":true}"#).is_empty());
        // A non-stop system line is noise.
        assert!(parse_line(r#"{"type":"system","subtype":"other"}"#).is_empty());
    }
}
