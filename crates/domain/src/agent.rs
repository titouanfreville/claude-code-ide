//! Which agent CLI backend powers a managed session.
//!
//! MoonlightCode was built around Claude Code (`claude`), but the launch/identity
//! seam is the same for any Claude-compatible agentic CLI. [`AgentKind`] names the
//! backend; the desktop crate's `AgentBackend` trait turns a kind into the concrete
//! launch command and capability flags. Keeping this in the domain (like [`Phase`])
//! lets a session's backend be persisted and reasoned about without the GPUI layer.
//!
//! [`Phase`]: crate::phase::Phase

use serde::{Deserialize, Serialize};

/// The agent CLI driving a managed session. `Default` is [`AgentKind::ClaudeCode`],
/// so existing records and call sites that don't yet carry a backend behave exactly
/// as before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AgentKind {
    /// Anthropic Claude Code (`claude`). The original, fully-wired backend.
    #[default]
    ClaudeCode,
    /// Google Antigravity CLI (`agy`) — the Gemini-world peer. Claude-compatible
    /// hooks (`PreToolUse`/`PostToolUse`) and MCP, but injected differently (no
    /// `--mcp-config` / `--append-system-prompt` / `--session-id` flags). See
    /// [`AgentKind::supports_forced_session_id`] and [`AgentKind::mcp_injection`].
    Antigravity,
}

impl AgentKind {
    /// The executable name typed to launch this backend.
    pub fn binary(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "claude",
            AgentKind::Antigravity => "agy",
        }
    }

    /// Human-facing label for the UI (session tile badge, picker).
    pub fn label(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "Claude Code",
            AgentKind::Antigravity => "Antigravity",
        }
    }

    /// Parse a tolerant backend token (case-insensitive, trimmed). Returns `None`
    /// for anything unrecognized so the caller can refuse with the valid set.
    pub fn from_token(token: &str) -> Option<AgentKind> {
        match token.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-code" | "claudecode" | "cc" => Some(AgentKind::ClaudeCode),
            "agy" | "antigravity" | "gemini" => Some(AgentKind::Antigravity),
            _ => None,
        }
    }

    /// Whether this CLI can be launched **pinned to a caller-chosen session id**
    /// (`claude --session-id <id>`). MoonlightCode mints a stable id and launches a
    /// fresh session into it; Antigravity has no such flag — it only resumes its own
    /// `--conversation <id>`, so a fresh AGY session's id is discovered after launch.
    pub fn supports_forced_session_id(self) -> bool {
        matches!(self, AgentKind::ClaudeCode)
    }

    /// Whether a statusline can be injected via a launch flag (`claude --settings`).
    /// Antigravity has no equivalent (`/statusline` is TUI-only), so the OMC-HUD
    /// preservation is Claude-only.
    pub fn supports_statusline_injection(self) -> bool {
        matches!(self, AgentKind::ClaudeCode)
    }

    /// How the embedded `moonlight` MCP host is wired into a launch.
    pub fn mcp_injection(self) -> McpInjection {
        match self {
            AgentKind::ClaudeCode => McpInjection::Flag,
            AgentKind::Antigravity => McpInjection::ConfigFile,
        }
    }
}

/// How a backend receives the per-session embedded MCP host endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpInjection {
    /// Inline on the launch command (`--mcp-config '{…}'`) — Claude Code.
    Flag,
    /// Written to a config file the CLI reads (`mcp_config.json`) — Antigravity.
    ConfigFile,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_claude_code() {
        assert_eq!(AgentKind::default(), AgentKind::ClaudeCode);
    }

    #[test]
    fn binaries_and_labels() {
        assert_eq!(AgentKind::ClaudeCode.binary(), "claude");
        assert_eq!(AgentKind::Antigravity.binary(), "agy");
        assert_eq!(AgentKind::ClaudeCode.label(), "Claude Code");
        assert_eq!(AgentKind::Antigravity.label(), "Antigravity");
    }

    #[test]
    fn from_token_parses_aliases() {
        assert_eq!(AgentKind::from_token("claude"), Some(AgentKind::ClaudeCode));
        assert_eq!(AgentKind::from_token("  CC "), Some(AgentKind::ClaudeCode));
        assert_eq!(AgentKind::from_token("agy"), Some(AgentKind::Antigravity));
        assert_eq!(AgentKind::from_token("Antigravity"), Some(AgentKind::Antigravity));
        assert_eq!(AgentKind::from_token("gemini"), Some(AgentKind::Antigravity));
        assert_eq!(AgentKind::from_token("nonsense"), None);
    }

    #[test]
    fn capabilities_split_claude_vs_antigravity() {
        // Claude: pin id, inject statusline, MCP via flag.
        assert!(AgentKind::ClaudeCode.supports_forced_session_id());
        assert!(AgentKind::ClaudeCode.supports_statusline_injection());
        assert_eq!(AgentKind::ClaudeCode.mcp_injection(), McpInjection::Flag);
        // Antigravity: none of those — resume-by-conversation, no statusline flag,
        // MCP via a config file.
        assert!(!AgentKind::Antigravity.supports_forced_session_id());
        assert!(!AgentKind::Antigravity.supports_statusline_injection());
        assert_eq!(AgentKind::Antigravity.mcp_injection(), McpInjection::ConfigFile);
    }
}
