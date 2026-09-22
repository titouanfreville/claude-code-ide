//! `moonlightd mcp` — the moonlight verbs for a session MoonlightCode did not launch.
//!
//! The IDEs hand an agent its verbs at launch: they mint a session id, bind a
//! per-session MCP endpoint, and put its URL in the `--mcp-config` flag (see
//! `clients/vscode`'s `SessionTerminals`, and `apps/desktop`'s `attach_command`). That
//! works precisely because they own the launch.
//!
//! Plenty of sessions are not launched that way — an editor's own Claude Code
//! integration, a plain terminal, an SDK harness. Those still get gated, because the
//! hooks are registered machine-wide, but they get no verbs: no `present_plan`, no
//! `request_phase`, no `phase_status`. From the operator's side that is an agent stuck
//! in Plan with no way to say so, and from the agent's side it is a phase it cannot
//! leave and no tool to ask with.
//!
//! This closes that gap. Registered once as an ordinary MCP server, Claude Code spawns
//! it per session and speaks MCP on stdin/stdout, so no URL has to be known in advance.
//! Two things make that possible:
//!
//! - **Which session.** Claude Code exports `CLAUDE_CODE_SESSION_ID` into the
//!   environment of the processes it spawns, and an stdio MCP server is one of them. So
//!   the shim reads its own session id rather than guessing from the working directory,
//!   which would be ambiguous the moment two sessions share a repo.
//! - **Which daemon.** `~/.moonlight/control.json` — the same discovery file every
//!   client reads, written by whichever host won the socket.
//!
//! It runs no policy of its own. Every verb is forwarded to `/control/verb` and decided
//! in the daemon, which is the process holding the phase state, the PDP, the audit log
//! and — the part that cannot be delegated — the pending approvals. A shim that made its
//! own decisions would be a second permission authority, which the architecture does not
//! allow and which would in any case be answering for holds it cannot see.

use std::sync::Arc;
use std::time::Duration;

use moonlight_domain::ids::SessionId;
use moonlight_domain::ports::mcp::{ActorRequest, ActorResult, McpActor};
use moonlight_domain::ControlError;
use moonlight_mcp_server::{serve_stdio, VerbScope, VerbToolServer};

/// The environment variable Claude Code exports into the processes it spawns.
const SESSION_ENV: &str = "CLAUDE_CODE_SESSION_ID";

/// How long to wait on a forwarded verb.
///
/// Long, deliberately: `present_plan` and `request_phase` block until a human decides,
/// and the daemon holds them indefinitely by design ("bounding it would deny plans for
/// being thought about" — see the daemon's approval gate). This is the same posture as
/// the `PreToolUse` hook's budget, for the same reason: the ceiling exists so a wedged
/// daemon eventually surfaces, not to hurry the operator.
const VERB_TIMEOUT: Duration = moonlight_control::HOOK_TIMEOUT;

/// Runs verbs by asking the daemon to run them, over the control API.
///
/// Holds no address. The daemon's port changes every time it restarts — and it does
/// restart, because the IDEs autostart it and a new build displaces the old one — while
/// this process lives as long as the agent's session does. An address cached at startup
/// is therefore wrong from the first restart onward, and fails as a refused connection
/// that reads like "moonlight is not running" when it is running perfectly well on
/// another port. Re-reading the discovery file per call costs one small local read
/// against a round trip that can block for minutes.
struct RemoteActor;

#[async_trait::async_trait]
impl McpActor for RemoteActor {
    async fn run(&self, req: &ActorRequest) -> Result<ActorResult, ControlError> {
        let base_url = control_base_url().ok_or_else(|| {
            ControlError::Transport(
                "no moonlight daemon is running (no readable ~/.moonlight/control.json)"
                    .to_string(),
            )
        })?;
        let url = format!("{base_url}/control/verb");
        let body = serde_json::json!({
            "session_id": req.session.as_str(),
            "verb": req.verb,
            "payload": req.payload,
        });

        // `ureq` is blocking and this is an async trait method, so the call goes to a
        // blocking thread rather than stalling the rmcp reactor — which still has to
        // answer pings and the peer's other traffic while a verb holds for an operator.
        let response = tokio::task::spawn_blocking(move || {
            ureq::AgentBuilder::new()
                .timeout_read(VERB_TIMEOUT)
                .timeout_write(VERB_TIMEOUT)
                .build()
                .post(&url)
                .set("Content-Type", "application/json")
                .send_string(&body.to_string())
                // `ureq::Error` is large enough that carrying it by value through the
                // task's `Result` is a lint in its own right; we only ever render it.
                .map_err(Box::new)
        })
        .await
        .map_err(|e| ControlError::Transport(format!("verb task failed: {e}")))?;

        let result: moonlight_mcp_server::control_api::VerbResponse = match response {
            Ok(ok) => {
                let text = ok
                    .into_string()
                    .map_err(|e| ControlError::Transport(format!("unreadable verb reply: {e}")))?;
                serde_json::from_str(&text)
                    .map_err(|e| ControlError::Transport(format!("unreadable verb reply: {e}")))?
            }
            // The daemon went away mid-session, or never had an MCP host. Say which,
            // because "moonlight is not running" and "this verb is not served here" send
            // the agent to very different next moves.
            Err(e) => {
                return Err(ControlError::Transport(format!(
                    "moonlight daemon did not answer: {e}"
                )))
            }
        };

        Ok(ActorResult {
            ok: result.ok,
            compact_output: result.output,
        })
    }
}

/// Where the daemon that owns this machine's sessions is listening.
fn control_base_url() -> Option<String> {
    let path = moonlight_core::support::moonlight_dir()?.join("control.json");
    let text = std::fs::read_to_string(path).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&text).ok()?;
    let port = parsed.get("port")?.as_u64()?;
    Some(format!("http://127.0.0.1:{port}"))
}

/// Serve the workflow verbs for this session on stdin/stdout until the peer hangs up.
///
/// Every failure path exits non-zero **without** writing to stdout: stdout is the MCP
/// channel, so a diagnostic printed there is a protocol violation that reads to Claude
/// Code as a corrupt frame rather than as the plain "this server is unavailable" it is.
pub fn run() {
    let Some(session) = std::env::var(SESSION_ENV).ok().filter(|s| !s.is_empty()) else {
        eprintln!(
            "moonlightd mcp: no {SESSION_ENV} in the environment — this is meant to be \
             spawned by Claude Code as an MCP server, not run by hand"
        );
        std::process::exit(2);
    };
    // Checked, but not captured: the address is resolved per call (see `RemoteActor`).
    // Refusing to start when no daemon is up would strand the session's verbs for the
    // rest of its life over a daemon that may be seconds from starting.
    if control_base_url().is_none() {
        eprintln!(
            "moonlightd mcp: no moonlight daemon found yet (no readable \
             ~/.moonlight/control.json) — verbs will work once one is running"
        );
    }

    let actor: Arc<dyn McpActor> = Arc::new(RemoteActor);
    // Workflow only, matching the daemon's own host: `run_*` addresses an IDE's Run
    // console, and the daemon behind us has none to drive.
    let server =
        VerbToolServer::with_scope(actor, SessionId::new(session), VerbScope::WorkflowOnly);

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("moonlightd mcp: no tokio runtime: {e}");
            std::process::exit(2);
        }
    };
    if let Err(e) = runtime.block_on(serve_stdio(server)) {
        eprintln!("moonlightd mcp: {e}");
        std::process::exit(1);
    }
}
