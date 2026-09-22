//! Hook IPC wire types — the JSON exchanged between the `moonlight hook` CLI (run
//! by Claude Code) and the running app's control server over the local socket.
//!
//! Deliberately a small, stable subset of Claude Code's hook payload. All fields
//! are optional/untrusted; deserialization tolerates extra/missing keys.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A hook event forwarded from Claude Code to the control server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookRequest {
    /// The CC hook event name, e.g. `"PreToolUse"`. Claude Code sends this as
    /// `hook_event_name`; `event` is accepted as an alias for our own messages.
    #[serde(rename = "hook_event_name", alias = "event", default)]
    pub event: String,
    #[serde(rename = "session_id", default)]
    pub session_id: String,
    #[serde(rename = "tool_name", default)]
    pub tool_name: String,
    #[serde(rename = "tool_input", default)]
    pub tool_input: Value,
    #[serde(default)]
    pub cwd: String,
}

/// The control server's verdict for a hook request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "lowercase")]
pub enum HookResponse {
    /// Let the action proceed.
    Allow,
    /// Block the action; `reason` is surfaced to the session/operator.
    Deny { reason: String },
    /// Not a verdict: text to prepend to the session's turn (`UserPromptSubmit`).
    ///
    /// A new variant on a tagged enum is a one-way compatibility step — an older
    /// client decoding it fails the parse and falls back to [`HookResponse::fail_open`],
    /// so a version-skewed pair degrades to "allow, no context" rather than to an
    /// error. That is the right direction: the brief is an improvement, never a gate.
    Context { text: String },
}

impl HookResponse {
    /// The default, safe-for-CC response: allow. Used on every error/timeout so a
    /// running Claude Code is never blocked by MoonlightCode being absent or slow.
    pub fn fail_open() -> Self {
        HookResponse::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_round_trips_and_tolerates_extra_keys() {
        let raw = json!({
            "event": "PreToolUse",
            "session_id": "abc",
            "tool_name": "Edit",
            "tool_input": { "file_path": "/x" },
            "cwd": "/repo",
            "unknown_future_field": 123
        })
        .to_string();
        let req: HookRequest = serde_json::from_str(&raw).unwrap();
        assert_eq!(req.tool_name, "Edit");
        assert_eq!(req.session_id, "abc");
    }

    #[test]
    fn missing_fields_default() {
        let req: HookRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(req.tool_name, "");
        assert!(req.tool_input.is_null());
    }

    #[test]
    fn response_serializes_tagged() {
        assert_eq!(
            serde_json::to_value(HookResponse::Allow).unwrap(),
            json!({ "decision": "allow" })
        );
        assert_eq!(
            serde_json::to_value(HookResponse::Deny {
                reason: "no".into()
            })
            .unwrap(),
            json!({ "decision": "deny", "reason": "no" })
        );
    }
}
