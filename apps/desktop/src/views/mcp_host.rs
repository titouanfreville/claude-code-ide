//! App-side handle for the per-session embedded MCP servers (Slice 3).
//!
//! [`McpHost`] (crates/mcp-server) binds an ephemeral loopback port per managed
//! session and serves the policy-gated actor verbs over HTTP — but `spawn_for` is
//! async and needs a tokio reactor, while the launch arms live on GPUI's executor.
//! [`McpHostHandle`] bridges that: it owns a small dedicated tokio runtime handle
//! and exposes a **sync** [`url_for`](McpHostHandle::url_for) (bind is a fast local
//! op) with a per-session URL cache, so a relaunch reuses the session's endpoint
//! instead of binding a second server.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use gpui::App;
use moonlight_domain::ids::SessionId;
use moonlight_mcp_server::McpHost;

use super::workspace::ShellDeps;

/// Sync facade over [`McpHost`] for the GPUI-side launch arms.
#[derive(Clone)]
pub struct McpHostHandle {
    host: McpHost,
    handle: tokio::runtime::Handle,
    /// One endpoint per session for the app's lifetime (relaunch reuses it).
    urls: Arc<RwLock<HashMap<SessionId, String>>>,
}

impl McpHostHandle {
    /// Stand the host up on its own single-worker tokio runtime. `None` (logged)
    /// when the runtime can't be built — sessions then launch without MCP.
    pub fn build(host: McpHost) -> Option<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("moonlight-mcp")
            .enable_all()
            .build()
            .map_err(|err| tracing::error!(error = %err, "MCP host runtime failed to build"))
            .ok()?;
        let handle = rt.handle().clone();
        // The runtime must outlive every per-session server — intentionally kept
        // for the app's lifetime (one runtime per run).
        Box::leak(Box::new(rt));
        Some(Self::with_runtime(host, handle))
    }

    /// Wrap an existing runtime handle (tests compose their own).
    pub fn with_runtime(host: McpHost, handle: tokio::runtime::Handle) -> Self {
        Self {
            host,
            handle,
            urls: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Snapshot of the live per-session endpoints, sorted by session id — the
    /// Services tool window's read of the URL cache.
    pub fn endpoints(&self) -> Vec<(SessionId, String)> {
        let mut v: Vec<_> = self
            .urls
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|(id, url)| (id.clone(), url.clone()))
            .collect();
        v.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        v
    }

    /// The MCP endpoint URL for `id`, standing the session's server up on first
    /// use. Sync (blocks on a local ephemeral-port bind — fast); `None` when the
    /// bind fails (logged; the session launches without MCP rather than not at all).
    pub fn url_for(&self, id: &SessionId) -> Option<String> {
        if let Some(hit) = self.urls.read().unwrap_or_else(|p| p.into_inner()).get(id) {
            return Some(hit.clone());
        }
        match self.handle.block_on(self.host.spawn_for(id.clone())) {
            Ok(url) => {
                self.urls
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(id.clone(), url.clone());
                Some(url)
            }
            Err(err) => {
                tracing::warn!(session = %id, error = %err,
                    "embedded MCP bind failed — launching session without MCP");
                None
            }
        }
    }
}

/// Resolve the embedded MCP endpoint for `id` from the shell global. `None` when
/// the shell isn't installed (static/test views) or the host is down — callers
/// then launch the session without MCP.
pub fn url_for_session(id: &SessionId, cx: &App) -> Option<String> {
    cx.try_global::<ShellDeps>()?.mcp_host.as_ref()?.url_for(id)
}

/// The `--mcp-config` fragment (leading space included) injecting this session's
/// embedded `moonlight` MCP server into its `claude` launch command. The JSON
/// carries no single quotes, so the single-quoted shell arg is safe.
pub fn mcp_config_flag(url: &str) -> String {
    format!(
        r#" --mcp-config '{{"mcpServers":{{"moonlight":{{"type":"http","url":"{url}"}}}}}}'"#
    )
}

/// The default context every IDE-managed session is launched with: the workflow-phase
/// gate and how to work with it (esp. `request_phase`, the agent's only way to ask for
/// a phase change), plus the `moonlight` MCP verbs it should prefer.
///
/// **Delivered via a file**, not inline: the launch command is *typed into the embedded
/// terminal's shell* as one line (`<cmd>\n`), and a ~1 KB inline `--append-system-prompt`
/// arg overflows the terminal's line-input limit — the line is truncated mid-string,
/// leaving a dangling quote so the session never launches. So we write this to a file
/// (like the statusline `--settings`) and pass `--append-system-prompt "$(cat <file>)"`,
/// keeping the typed line short. The file content may be multi-line / contain
/// apostrophes (it is not shell-parsed), but we keep it apostrophe-free for the inline
/// fallback used when the file can't be written.
const IDE_CONTEXT: &str = "You are running inside MoonlightCode, an IDE that governs this Claude Code session. Your tools are gated by a workflow phase, one of: Discovery, Plan, Auto, Test, Review, Commit. In Discovery, Plan, and Commit, edits to project files are denied; you can still read, search, run commands, and (except during Commit) write notes under .ai/. In Auto, Test, and Review, file writes are allowed. You cannot switch phase on your own. When you need a different phase (for example: done exploring and ready to plan, you need write access to implement, tests pass and you want review, or you are ready to commit), call the moonlight MCP tool request_phase with a target of discovery, plan, auto, test, review, commit, or next. Every request is approved or denied by the operator, so never assume the phase changed until it is confirmed. Prefer the moonlight MCP verbs over ad-hoc shell when they fit: run_list_targets, run_start, run_stop, run_status, and run_logs drive the shared IDE Run console, and run_with_coverage runs the tests with a compact summary. All verbs are policy-gated and audited; if one is denied, read the reason and adapt instead of retrying.";

/// Short inline fallback used only when the context file can't be written. Must stay a
/// single apostrophe-free line that is comfortably under the terminal's line limit.
const IDE_CONTEXT_SHORT: &str = "You are inside MoonlightCode, an IDE that gates your tools by a workflow phase (Discovery/Plan/Commit are read-only for project files; Auto/Test/Review allow writes). You cannot change phase yourself: call the moonlight MCP tool request_phase (target discovery|plan|auto|test|review|commit|next; operator-approved). Prefer the moonlight run_* verbs over ad-hoc shell.";

/// Write [`IDE_CONTEXT`] to the support dir and return its path (best-effort; `None`
/// when the support dir is unavailable). Idempotent — rewritten on each launch, like
/// the statusline settings file.
fn write_context_file() -> Option<std::path::PathBuf> {
    let dir = crate::obs::support_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("session-context.txt");
    std::fs::write(&path, IDE_CONTEXT).ok()?;
    Some(path)
}

/// The `--append-system-prompt` fragment (leading space included). Prefers the
/// **file form** `"$(cat '<path>')"` (short typed line, immune to length limits);
/// falls back to a short single-quoted inline prompt if the file can't be written.
pub fn ide_context_flag() -> String {
    match write_context_file() {
        Some(path) => format!(" --append-system-prompt \"$(cat '{}')\"", path.display()),
        None => format!(" --append-system-prompt '{IDE_CONTEXT_SHORT}'"),
    }
}

/// The launch-command fragments (leading spaces included) shared by every IDE-managed
/// `claude` session: the `--mcp-config` for this session's embedded `moonlight` server,
/// immediately followed by the `--append-system-prompt` default context. They ride
/// together because the phase/verb guidance only makes sense when the verbs are wired,
/// so callers append both wherever they would have appended the MCP flag alone.
pub fn session_launch_flags(url: &str) -> String {
    format!("{}{}", mcp_config_flag(url), ide_context_flag())
}

#[cfg(test)]
mod tests {
    use super::*;

    use async_trait::async_trait;
    use moonlight_domain::errors::ControlError;
    use moonlight_domain::ports::mcp::{ActorRequest, ActorResult, McpActor};
    use moonlight_mcp_server::BusPolicyView;

    struct StubActor;
    #[async_trait]
    impl McpActor for StubActor {
        async fn run(&self, _req: &ActorRequest) -> Result<ActorResult, ControlError> {
            Ok(ActorResult {
                ok: true,
                compact_output: "ok".into(),
            })
        }
    }

    fn handle_on(rt: &tokio::runtime::Runtime) -> McpHostHandle {
        let host = McpHost::new(Arc::new(StubActor), Arc::new(BusPolicyView::new()));
        McpHostHandle::with_runtime(host, rt.handle().clone())
    }

    #[test]
    fn url_for_binds_once_and_caches_per_session() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let host = handle_on(&rt);

        let a1 = host.url_for(&SessionId::new("s1")).expect("bind s1");
        assert!(a1.starts_with("http://127.0.0.1:") && a1.ends_with("/mcp"), "{a1}");
        // Same session → the cached endpoint, not a second bind.
        let a2 = host.url_for(&SessionId::new("s1")).expect("cached s1");
        assert_eq!(a1, a2);
        // Another session → its own endpoint (a different port).
        let b = host.url_for(&SessionId::new("s2")).expect("bind s2");
        assert_ne!(a1, b);
    }

    #[test]
    fn mcp_config_flag_shapes_the_cc_json() {
        let flag = mcp_config_flag("http://127.0.0.1:9999/mcp");
        assert_eq!(
            flag,
            r#" --mcp-config '{"mcpServers":{"moonlight":{"type":"http","url":"http://127.0.0.1:9999/mcp"}}}'"#
        );
    }

    #[test]
    fn ide_context_flag_keeps_the_typed_line_short() {
        let flag = ide_context_flag();
        // Whichever branch, the flag must be short — the long context rides in a file
        // (file form) or is the short inline fallback — so the typed launch line never
        // overflows the terminal's line-input limit (the bug this fixes).
        assert!(flag.len() < 400, "launch flag must stay short: {} chars", flag.len());
        assert!(flag.starts_with(" --append-system-prompt "), "{flag}");
        // The short inline fallback must be a safe single-quoted shell arg.
        assert!(!IDE_CONTEXT_SHORT.contains('\n'), "short context must be one line");
        assert!(!IDE_CONTEXT_SHORT.contains('\''), "short context must have no apostrophes");
        // Both forms teach the key affordance.
        assert!(IDE_CONTEXT.contains("request_phase"), "{IDE_CONTEXT}");
        assert!(IDE_CONTEXT_SHORT.contains("request_phase"), "{IDE_CONTEXT_SHORT}");
    }

    #[test]
    fn session_launch_flags_balance_their_shell_quotes() {
        // The assembled fragment is typed into a shell; unbalanced quotes were exactly
        // the launch bug. Both single and double quotes must come in pairs.
        let flags = session_launch_flags("http://127.0.0.1:9999/mcp");
        assert_eq!(flags.matches('\'').count() % 2, 0, "unbalanced single quotes: {flags}");
        assert_eq!(flags.matches('"').count() % 2, 0, "unbalanced double quotes: {flags}");
    }

    #[test]
    fn session_launch_flags_are_mcp_then_context() {
        let url = "http://127.0.0.1:9999/mcp";
        assert_eq!(
            session_launch_flags(url),
            format!("{}{}", mcp_config_flag(url), ide_context_flag())
        );
    }
}
