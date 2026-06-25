//! Normalize an Antigravity (`agy`) `PreToolUse` hook payload onto MoonlightCode's
//! backend-agnostic [`HookRequest`] (Claude Code's tool vocabulary).
//!
//! AGY's hook fires with a different shape than Claude's — `{ toolCall: { name, args },
//! conversationId, workspacePaths, … }` with tool names like `run_command` / `write_file`
//! and arg keys like `CommandLine` / `filePath`. Rather than teach the PDP a second
//! vocabulary, we translate **at the edge**: map AGY's tool name + args onto Claude's
//! (`Bash`/`Edit`/`Write`/…, `command`/`file_path`) so `crates/control`'s classify +
//! write-scope path gates both backends with zero changes.
//!
//! Tolerant by design: an unexpected payload yields a best-effort request (empty fields),
//! so the gate fails open rather than panicking — same posture as the Claude hook.

use moonlight_control::HookRequest;
use serde_json::{json, Value};

/// Parse an AGY `PreToolUse` payload and normalize it to a [`HookRequest`].
pub fn normalize(payload: &Value) -> HookRequest {
    let tool_call = payload.get("toolCall");
    let agy_name = tool_call
        .and_then(|t| t.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let args = tool_call
        .and_then(|t| t.get("args"))
        .cloned()
        .unwrap_or(Value::Null);

    let (tool_name, tool_input) = map_tool(agy_name, &args);

    let session_id = payload
        .get("conversationId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // cwd: the command's own `Cwd`, else the session's first workspace path. Used by the
    // PDP's path-scope checks to resolve relative writes against the project root.
    let cwd = args
        .get("Cwd")
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .get("workspacePaths")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(Value::as_str)
        })
        .unwrap_or_default()
        .to_string();

    HookRequest {
        event: "PreToolUse".into(),
        session_id,
        tool_name,
        tool_input,
        cwd,
    }
}

/// Map an AGY tool name + args onto a Claude-vocabulary `(tool_name, tool_input)`. This is
/// the **single** translation layer — the gate keys off Claude's names/arg-keys, so adding
/// a backend only means extending this table.
fn map_tool(agy_name: &str, args: &Value) -> (String, Value) {
    let file_path = args
        .get("filePath")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match agy_name {
        // Shell execution → Bash (`command` is what classify inspects).
        "run_command" => {
            let command = args
                .get("CommandLine")
                .and_then(Value::as_str)
                .unwrap_or_default();
            ("Bash".into(), json!({ "command": command }))
        }
        // Project-mutating tools → Claude write vocabulary, so the phase write-freeze
        // (`paths.rs::scope_of` on `file_path`) applies identically to AGY.
        "write_file" | "create_file" => ("Write".into(), json!({ "file_path": file_path })),
        "edit_file" | "replace_file_content" | "propose_code" => {
            ("Edit".into(), json!({ "file_path": file_path }))
        }
        // Read-only tools — Safe in every phase.
        "read_file" | "view_file" => ("Read".into(), json!({ "file_path": file_path })),
        "list_dir" => ("LS".into(), json!({ "path": file_path })),
        "grep_search" | "codebase_search" => {
            let pattern = args
                .get("Query")
                .or_else(|| args.get("query"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            ("Grep".into(), json!({ "pattern": pattern }))
        }
        // Unknown tool: pass the raw name + args through so classify's default governs
        // (and the danger/tier gate still applies). New AGY tools land here until mapped.
        other => (other.to_string(), args.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_command_maps_to_bash() {
        let payload = json!({
            "conversationId": "c1",
            "workspacePaths": ["/repo"],
            "toolCall": { "name": "run_command",
                          "args": { "CommandLine": "rm -rf /", "Cwd": "/repo/api" } },
        });
        let req = normalize(&payload);
        assert_eq!(req.tool_name, "Bash");
        assert_eq!(req.tool_input["command"], "rm -rf /");
        assert_eq!(req.session_id, "c1");
        assert_eq!(req.cwd, "/repo/api", "Cwd wins over workspacePaths");
        assert_eq!(req.event, "PreToolUse");
    }

    #[test]
    fn write_and_edit_tools_map_to_claude_write_vocab() {
        // These must become Write/Edit so the phase write-freeze (scope_of on file_path)
        // fires for AGY exactly as for Claude.
        for (agy, claude) in [
            ("write_file", "Write"),
            ("create_file", "Write"),
            ("edit_file", "Edit"),
            ("replace_file_content", "Edit"),
            ("propose_code", "Edit"),
        ] {
            let payload = json!({
                "conversationId": "c",
                "toolCall": { "name": agy, "args": { "filePath": "/repo/src/lib.rs" } },
            });
            let req = normalize(&payload);
            assert_eq!(req.tool_name, claude, "{agy} → {claude}");
            assert_eq!(req.tool_input["file_path"], "/repo/src/lib.rs");
        }
    }

    #[test]
    fn read_tools_are_safe_vocab() {
        let payload = json!({
            "toolCall": { "name": "read_file", "args": { "filePath": "/repo/x" } },
        });
        assert_eq!(normalize(&payload).tool_name, "Read");
    }

    #[test]
    fn cwd_falls_back_to_first_workspace_path() {
        let payload = json!({
            "workspacePaths": ["/ws/a", "/ws/b"],
            "toolCall": { "name": "read_file", "args": { "filePath": "/x" } },
        });
        assert_eq!(normalize(&payload).cwd, "/ws/a");
    }

    #[test]
    fn unknown_tool_passes_through() {
        let payload = json!({
            "toolCall": { "name": "browser_navigate", "args": { "url": "x" } },
        });
        let req = normalize(&payload);
        assert_eq!(req.tool_name, "browser_navigate");
        assert_eq!(req.tool_input["url"], "x");
    }

    #[test]
    fn empty_payload_fails_safe() {
        let req = normalize(&json!({}));
        assert_eq!(req.tool_name, "");
        assert_eq!(req.event, "PreToolUse");
    }
}
