//! Backend-agnostic launch-command construction.
//!
//! Every IDE-managed session is brought up by typing a CLI command into an embedded
//! terminal. That command was hardcoded as `claude …` at several sites; this module
//! centralizes it behind [`AgentBackend`] so a second backend (Antigravity / `agy`)
//! can slot in without touching every launch arm. [`ClaudeCodeBackend`] reproduces the
//! original strings **byte-for-byte** (the call sites still wrap the result in
//! [`crate::obs::with_statusline`] exactly as before), so adopting it is behavior-
//! preserving. [`AntigravityBackend`] encodes the mapped differences (no `--session-id`,
//! no `--permission-mode`/`--mcp-config` launch flags — see [`AgentKind`]).

use moonlight_domain::agent::AgentKind;
use moonlight_domain::ids::SessionId;

/// Which conversation/session a launch attaches to.
pub enum SessionSelector<'a> {
    /// Start a fresh session. Claude pins the chosen id (`--session-id <id>`);
    /// Antigravity cannot pin an id (the id is discovered after launch).
    Fresh(&'a SessionId),
    /// Resume an existing conversation (`claude --resume <id>` / `agy --conversation <id>`).
    Resume(&'a SessionId),
}

/// Everything a backend needs to assemble a launch command. The phase→mode string is
/// resolved by the caller (e.g. [`moonlight_domain::Phase::cc_permission_mode`]) and
/// passed as `permission_mode` so the relaunch path can also supply an operator pick;
/// backends that have no permission-mode flag ignore it.
pub struct LaunchSpec<'a> {
    pub selector: SessionSelector<'a>,
    /// The CLI permission mode to request at launch, if the backend supports the flag.
    /// `None` for a plain resume (Claude's `--resume` carries no mode).
    pub permission_mode: Option<&'a str>,
    /// The session's embedded `moonlight` MCP endpoint, when the host is up.
    pub mcp_url: Option<&'a str>,
}

/// Turns a [`LaunchSpec`] into the concrete shell command for one agent CLI.
pub trait AgentBackend {
    /// The launch command (no statusline wrap — the caller applies that, and only
    /// Claude supports it; see [`AgentKind::supports_statusline_injection`]).
    fn launch_command(&self, spec: &LaunchSpec) -> String;

    /// Side effects to run **before** spawning the CLI. Claude injects MCP via the
    /// `--mcp-config` launch flag, so this is a no-op there; AGY has no such flag, so
    /// it writes the embedded host into its `mcp_config.json`. Best-effort — a failure
    /// is logged and the session still launches (without the `moonlight` verbs).
    fn prepare_launch(&self, _mcp_url: Option<&str>) {}
}

/// Anthropic Claude Code (`claude`). Reproduces the original launch strings exactly.
pub struct ClaudeCodeBackend;

impl AgentBackend for ClaudeCodeBackend {
    fn launch_command(&self, spec: &LaunchSpec) -> String {
        let mut command = match spec.selector {
            SessionSelector::Fresh(id) => format!("claude --session-id {}", id.as_str()),
            SessionSelector::Resume(id) => format!("claude --resume {}", id.as_str()),
        };
        if let Some(mode) = spec.permission_mode {
            command.push_str(&format!(" --permission-mode {mode}"));
        }
        if let Some(url) = spec.mcp_url {
            command.push_str(&crate::views::mcp_host::session_launch_flags(url));
        }
        command
    }
}

/// Google Antigravity CLI (`agy`). No `--session-id` (cannot pin a chosen id), no
/// `--permission-mode` flag (phase is enforced by the PDP `PreToolUse` hook), and no
/// `--mcp-config`/`--append-system-prompt` flags (MCP rides a generated `mcp_config.json`
/// and context rides `GEMINI.md` — wired out-of-band, not on the command line).
pub struct AntigravityBackend;

impl AgentBackend for AntigravityBackend {
    fn launch_command(&self, spec: &LaunchSpec) -> String {
        // `permission_mode` and `mcp_url` are intentionally not on the command line for
        // AGY — see the type doc. A fresh launch is bare `agy`; resume targets the
        // AGY-side conversation id.
        match spec.selector {
            SessionSelector::Fresh(_) => "agy".to_string(),
            SessionSelector::Resume(id) => format!("agy --conversation {}", id.as_str()),
        }
    }

    fn prepare_launch(&self, mcp_url: Option<&str>) {
        // No `--mcp-config` flag for AGY — write the embedded host into its config file
        // so `/mcp` exposes the `moonlight` verbs. Best-effort.
        if let Some(url) = mcp_url {
            if let Err(e) = crate::agy_setup::write_mcp_config(url) {
                tracing::warn!(error = %e,
                    "failed to write AGY mcp_config; session launches without moonlight verbs");
            }
        }
    }
}

/// The backend implementation for a given [`AgentKind`]. The launch arms call this
/// instead of hardcoding `claude`.
pub fn backend_for(kind: AgentKind) -> Box<dyn AgentBackend> {
    match kind {
        AgentKind::ClaudeCode => Box::new(ClaudeCodeBackend),
        AgentKind::Antigravity => Box::new(AntigravityBackend),
    }
}

/// Apply the statusline `--settings` wrap **only** for backends that support it (Claude).
/// AGY has no `--settings` flag (see [`AgentKind::supports_statusline_injection`]), so
/// wrapping there would inject an argument `agy` rejects — breaking the launch.
pub fn wrap_statusline(kind: AgentKind, command: String) -> String {
    if kind.supports_statusline_injection() {
        crate::obs::with_statusline(command)
    } else {
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::mcp_host::session_launch_flags;

    fn sid() -> SessionId {
        SessionId::new("abc123")
    }

    #[test]
    fn claude_fresh_matches_legacy_string() {
        // Reproduces the NewManagedSession / attach_command(fresh) arm exactly:
        // `claude --session-id <id> --permission-mode <mode>` + the shared flags.
        let id = sid();
        let url = "http://127.0.0.1:9999/mcp";
        let cmd = ClaudeCodeBackend.launch_command(&LaunchSpec {
            selector: SessionSelector::Fresh(&id),
            permission_mode: Some("plan"),
            mcp_url: Some(url),
        });
        assert_eq!(
            cmd,
            format!("claude --session-id abc123 --permission-mode plan{}", session_launch_flags(url))
        );
    }

    #[test]
    fn claude_resume_has_no_mode_when_none() {
        // attach_command(resume) carries NO --permission-mode (CC infers it on resume).
        let id = sid();
        let cmd = ClaudeCodeBackend.launch_command(&LaunchSpec {
            selector: SessionSelector::Resume(&id),
            permission_mode: None,
            mcp_url: None,
        });
        assert_eq!(cmd, "claude --resume abc123");
    }

    #[test]
    fn claude_relaunch_resume_with_mode_and_mcp() {
        // relaunch_terminal: `claude --resume <id> --permission-mode <mode>` + flags.
        let id = sid();
        let url = "http://127.0.0.1:9999/mcp";
        let cmd = ClaudeCodeBackend.launch_command(&LaunchSpec {
            selector: SessionSelector::Resume(&id),
            permission_mode: Some("auto"),
            mcp_url: Some(url),
        });
        assert_eq!(
            cmd,
            format!("claude --resume abc123 --permission-mode auto{}", session_launch_flags(url))
        );
    }

    #[test]
    fn antigravity_fresh_is_bare_binary() {
        // No id pin, no mode flag, no mcp flag — those ride out-of-band for AGY.
        let id = sid();
        let cmd = AntigravityBackend.launch_command(&LaunchSpec {
            selector: SessionSelector::Fresh(&id),
            permission_mode: Some("plan"),
            mcp_url: Some("http://127.0.0.1:9999/mcp"),
        });
        assert_eq!(cmd, "agy");
    }

    #[test]
    fn antigravity_resume_uses_conversation_flag() {
        let id = sid();
        let cmd = AntigravityBackend.launch_command(&LaunchSpec {
            selector: SessionSelector::Resume(&id),
            permission_mode: None,
            mcp_url: None,
        });
        assert_eq!(cmd, "agy --conversation abc123");
    }

    #[test]
    fn backend_for_selects_the_right_impl() {
        let id = sid();
        let spec = || LaunchSpec {
            selector: SessionSelector::Resume(&id),
            permission_mode: None,
            mcp_url: None,
        };
        // The factory dispatches to the backend whose command shape matches the kind.
        assert_eq!(backend_for(AgentKind::ClaudeCode).launch_command(&spec()), "claude --resume abc123");
        assert_eq!(backend_for(AgentKind::Antigravity).launch_command(&spec()), "agy --conversation abc123");
    }
}
