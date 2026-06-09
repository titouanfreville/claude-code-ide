//! The control IPC server + client over a Unix domain socket.
//!
//! Claude Code runs `moonlight hook …` (the client), which forwards one hook
//! request and reads one verdict. The server lives in the running app, looks up
//! the session's [`GateState`] in a shared read-model, and asks the PDP. Every
//! failure path fails *open* (allow) so a running Claude Code is never blocked by
//! MoonlightCode being absent, slow, or wrong.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use moonlight_domain::ids::SessionId;
use moonlight_domain::ports::PolicyDecisionPoint;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use crate::config::AiWorkspaceResolver;
use crate::gate::{evaluate, GateDecision, GateState, HoldKind};
use crate::ipc::{HookRequest, HookResponse};
use crate::paths::{classify_write_scope, AiWorkspace};
use crate::pending::{ApprovalNotifier, Decision, PendingApprovals};

/// The shared per-session gate read-model. The app keeps this in sync by folding
/// engine events; the server only ever reads it.
pub type GateView = Arc<RwLock<HashMap<SessionId, GateState>>>;

/// How long the server holds a connection waiting for an operator decision before
/// denying. Must stay under Claude Code's per-hook timeout (≈60s) so the *server*
/// resolves the hold (a clean deny) rather than the client giving up and failing
/// open. On timeout the action is denied with a re-runnable reason.
pub const DEFAULT_HOLD: Duration = Duration::from_secs(45);

/// A notifier that does nothing — the default when no app is wired in (tests, the
/// degraded standalone server). Held actions then simply time out into a deny.
struct NoopNotifier;
impl ApprovalNotifier for NoopNotifier {
    fn approval_requested(&self, _session: &SessionId, _what: &str, _plan: Option<&str>) {}
}

/// Serves hook verdicts from the gate read-model + PDP, holding approval-required
/// actions until the operator decides (see [`crate::pending`]).
pub struct ControlServer {
    gates: GateView,
    pdp: Arc<dyn PolicyDecisionPoint>,
    pending: Arc<PendingApprovals>,
    notifier: Arc<dyn ApprovalNotifier>,
    /// How long to hold an approval-required action before auto-denying. `None` =
    /// **wait for the operator indefinitely** (no "elapsed" deny) — a human approval
    /// (plan review, danger gate) must not be rushed. The only ceiling is then Claude
    /// Code's own per-hook timeout (set high at install). `Some(d)` keeps a bound, used
    /// by the degraded no-approval server and tests.
    hold_timeout: Option<Duration>,
    /// Resolves the AI-workspace allowlist per session `cwd` (user + workspace
    /// `.moonlight/config.json` layered over the built-in default), used to classify a
    /// write's [`crate::paths::WriteScope`] so frozen phases freeze project files while
    /// still allowing AI-scratch writes. Defaults to the built-in allowlist for every
    /// cwd; wire config via [`ControlServer::with_ai_resolver`].
    ai: AiWorkspaceResolver,
}

impl ControlServer {
    /// Build a server with no approval routing: holds always time out into a deny.
    /// Use [`ControlServer::with_approvals`] to wire the operator into the loop.
    pub fn new(gates: GateView, pdp: Arc<dyn PolicyDecisionPoint>) -> Self {
        Self {
            gates,
            pdp,
            pending: Arc::new(PendingApprovals::new()),
            notifier: Arc::new(NoopNotifier),
            hold_timeout: Some(DEFAULT_HOLD),
            ai: AiWorkspaceResolver::default(),
        }
    }

    /// Pin a single AI-workspace allowlist for every cwd (ignores `.moonlight/config.json`).
    /// Mainly for tests / the degraded standalone server.
    pub fn with_ai_workspace(mut self, ai: AiWorkspace) -> Self {
        self.ai = AiWorkspaceResolver::fixed(ai);
        self
    }

    /// Wire the per-cwd allowlist resolver (user + workspace config). The composition
    /// root builds this from `~/.moonlight/config.json`; see [`AiWorkspaceResolver`].
    pub fn with_ai_resolver(mut self, resolver: AiWorkspaceResolver) -> Self {
        self.ai = resolver;
        self
    }

    /// Build a server that routes approval-required actions to the operator: it
    /// registers a pending approval in `pending`, notifies the app via `notifier`,
    /// and holds the hook until [`PendingApprovals::resolve`] fires (or, if
    /// `hold_timeout` is `Some`, the bound elapses). Pass `None` for an unbounded hold
    /// — the operator gets all the time they need to review.
    pub fn with_approvals(
        gates: GateView,
        pdp: Arc<dyn PolicyDecisionPoint>,
        pending: Arc<PendingApprovals>,
        notifier: Arc<dyn ApprovalNotifier>,
        hold_timeout: Option<Duration>,
    ) -> Self {
        Self {
            gates,
            pdp,
            pending,
            notifier,
            hold_timeout,
            ai: AiWorkspaceResolver::default(),
        }
    }

    /// Accept connections forever, one hook request per connection. Returns when
    /// the listener errors. Spawn this on a background task.
    pub async fn serve(self: Arc<Self>, listener: UnixListener) {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let server = Arc::clone(&self);
                    tokio::spawn(async move {
                        if let Err(e) = server.handle(stream).await {
                            tracing::debug!(error = %e, "hook connection error");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "control listener stopped");
                    break;
                }
            }
        }
    }

    async fn handle(&self, mut stream: UnixStream) -> std::io::Result<()> {
        // The client writes the request then half-closes, so read-to-EOF gets it.
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await?;
        let response = self.decide(&buf).await;
        let mut line = serde_json::to_vec(&response).unwrap_or_default();
        line.push(b'\n');
        stream.write_all(&line).await?;
        stream.flush().await?;
        Ok(())
    }

    /// Decide over raw request bytes, holding for the operator when the action
    /// needs approval. Non-holding verdicts return immediately; a hold notifies the
    /// app and awaits the operator (up to `hold_timeout`). Any parse error or hold
    /// timeout/cancellation denies — except an unparsable payload, which fails open.
    pub async fn decide(&self, request_bytes: &[u8]) -> HookResponse {
        let Ok(req) = serde_json::from_slice::<HookRequest>(request_bytes) else {
            return HookResponse::fail_open();
        };
        // We only gate `PreToolUse`. Any other hook event (Stop, Notification, …)
        // is observe-only → allow, so registering us on a different event can never
        // wrongly block a tool. (Empty event = our own internal/test messages.)
        if !req.event.is_empty() && req.event != "PreToolUse" {
            return HookResponse::Allow;
        }
        let session = SessionId::new(req.session_id.clone());

        // Resolve which part of the tree a file write touches (path + cwd vs the
        // cwd's effective AI-workspace allowlist) so a frozen phase freezes project
        // files only. The allowlist is the user + workspace config for this session.
        let ai = self.ai.for_cwd(&req.cwd);
        let write_scope = classify_write_scope(&req.tool_name, &req.tool_input, &req.cwd, &ai);
        // The operator's `safe_tools` config vouches a tool as read-only — it then
        // classifies Safe (survives frozen phases) instead of the heuristic default.
        let vouched_safe = ai.is_safe_tool(&req.tool_name);
        let decision = {
            let gates = self.gates.read().unwrap_or_else(|p| p.into_inner());
            evaluate(
                gates.get(&session),
                &session,
                &req.tool_name,
                &req.tool_input,
                write_scope,
                vouched_safe,
                self.pdp.as_ref(),
            )
        };

        match decision {
            GateDecision::Allow => HookResponse::Allow,
            GateDecision::Deny { reason } => HookResponse::Deny { reason },
            GateDecision::Hold(kind) => self.hold(session, kind).await,
        }
    }

    /// Pause the session on a held action: surface it to the operator, then await
    /// their decision. The hook stays blocked (CC waits) until this returns, so the
    /// session does not act until the operator approves.
    async fn hold(&self, session: SessionId, kind: HoldKind) -> HookResponse {
        let (what, plan) = match &kind {
            HoldKind::Plan { plan } => ("approve plan", plan.as_deref()),
            HoldKind::Danger { reason } => (reason.as_str(), None),
        };
        self.notifier.approval_requested(&session, what, plan);

        let rx = self.pending.register(session.clone());
        // Bounded (`Some`) → race the budget; unbounded (`None`) → await the operator
        // forever (the outer `Ok` matches the bounded `Ok(_)` arms; `Err`/elapsed is
        // then unreachable, which is the point — no "elapsed" deny for a human review).
        let outcome = match self.hold_timeout {
            Some(budget) => tokio::time::timeout(budget, rx).await,
            None => Ok(rx.await),
        };
        match outcome {
            Ok(Ok(Decision::Approve)) => HookResponse::Allow,
            Ok(Ok(Decision::Deny { reason })) => HookResponse::Deny {
                reason: format!("MoonlightCode: {reason}"),
            },
            // Sender dropped without a decision (session ended / superseded).
            Ok(Err(_)) => HookResponse::Deny {
                reason: "MoonlightCode: approval cancelled".to_string(),
            },
            // Only reachable for a bounded hold: no decision within the budget — deny
            // so CC isn't left hanging.
            Err(_) => {
                self.pending.cancel(&session);
                HookResponse::Deny {
                    reason: "MoonlightCode: approval window elapsed — re-run to review".to_string(),
                }
            }
        }
    }
}

/// Client side (the `moonlight hook` CLI): forward `request` to the server at
/// `socket` and return its verdict. Any error or a timeout yields
/// [`HookResponse::fail_open`] — Claude Code must never be blocked by us.
pub async fn query_hook(socket: &Path, request: &HookRequest, timeout: Duration) -> HookResponse {
    match tokio::time::timeout(timeout, query_inner(socket, request)).await {
        Ok(Ok(resp)) => resp,
        _ => HookResponse::fail_open(),
    }
}

async fn query_inner(socket: &Path, request: &HookRequest) -> std::io::Result<HookResponse> {
    let mut stream = UnixStream::connect(socket).await?;
    let buf = serde_json::to_vec(request).unwrap_or_default();
    stream.write_all(&buf).await?;
    stream.shutdown().await?; // half-close so the server sees EOF
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).await?;
    Ok(serde_json::from_slice(&resp).unwrap_or_else(|_| HookResponse::fail_open()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use moonlight_domain::errors::TrustError;
    use moonlight_domain::phase::Phase;
    use moonlight_domain::ports::PermissionRequest;
    use moonlight_domain::trust::{DangerClass, PermissionOutcome, WriteScope};
    use serde_json::json;

    struct TestPdp;
    impl PolicyDecisionPoint for TestPdp {
        fn decide(&self, req: &PermissionRequest) -> Result<PermissionOutcome, TrustError> {
            let is_write = !matches!(req.danger, DangerClass::Safe);
            if is_write {
                let permitted = match req.write_scope.unwrap_or(WriteScope::Project) {
                    WriteScope::AiWorkspace => req.phase.allows_ai_workspace_writes(),
                    WriteScope::Project => req.phase.allows_writes(),
                };
                if !permitted {
                    return Ok(PermissionOutcome::Deny {
                        reason: "frozen".into(),
                    });
                }
            }
            Ok(PermissionOutcome::Allow)
        }
    }

    fn gates_with(session: &str, gate: GateState) -> GateView {
        let mut map = HashMap::new();
        map.insert(SessionId::new(session), gate);
        Arc::new(RwLock::new(map))
    }

    fn req(session: &str, tool: &str) -> HookRequest {
        HookRequest {
            event: "PreToolUse".into(),
            session_id: session.into(),
            tool_name: tool.into(),
            tool_input: json!({ "file_path": "/x" }),
            cwd: "/repo".into(),
        }
    }

    async fn roundtrip(server: Arc<ControlServer>, request: &HookRequest) -> HookResponse {
        let dir = std::env::temp_dir();
        // Unique per (pid, session, tool) so parallel tests don't collide on bind.
        // The tool tag is truncated+length-stamped: a long MCP tool name would push
        // the socket path past SUN_LEN (~104 bytes on macOS) and fail the bind.
        let tool = &request.tool_name;
        let tool_tag = format!("{}{}", &tool[..tool.len().min(8)], tool.len());
        let path = dir.join(format!(
            "ml-ctl-{}-{}-{}.sock",
            std::process::id(),
            request.session_id,
            tool_tag
        ));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let srv = Arc::clone(&server);
        let handle = tokio::spawn(async move { srv.serve(listener).await });

        let resp = query_hook(&path, request, Duration::from_secs(2)).await;

        handle.abort();
        let _ = std::fs::remove_file(&path);
        resp
    }

    #[tokio::test]
    async fn adopted_plan_session_denies_write_over_socket() {
        let gates = gates_with(
            "s1",
            GateState {
                adopted: true,
                phase: Phase::Plan,
                ..Default::default()
            },
        );
        let server = Arc::new(ControlServer::new(gates, Arc::new(TestPdp)));
        let resp = roundtrip(server, &req("s1", "Edit")).await;
        assert!(matches!(resp, HookResponse::Deny { .. }));
    }

    #[tokio::test]
    async fn frozen_phase_allows_ai_workspace_write_over_socket() {
        // End-to-end: an adopted Plan session writing to `.ai/` is allowed (the server
        // resolves AiWorkspace scope from file_path + cwd), while a project write is
        // denied — the "read-only protects project state, not notes" rule over IPC.
        // Distinct session ids so each roundtrip gets its own (pid, session, tool)
        // socket path — no collision with other tests, no same-path rebind here.
        let plan_gate = GateState {
            adopted: true,
            phase: Phase::Plan,
            ..Default::default()
        };

        let ai_server = Arc::new(ControlServer::new(
            gates_with("scope-ai", plan_gate.clone()),
            Arc::new(TestPdp),
        ));
        let mut ai_write = req("scope-ai", "Edit");
        ai_write.tool_input = json!({ "file_path": "/repo/.ai/handoffs/plan.md" });
        assert_eq!(roundtrip(ai_server, &ai_write).await, HookResponse::Allow);

        let proj_server = Arc::new(ControlServer::new(
            gates_with("scope-proj", plan_gate),
            Arc::new(TestPdp),
        ));
        let mut project_write = req("scope-proj", "Edit");
        project_write.tool_input = json!({ "file_path": "/repo/crates/core/src/lib.rs" });
        assert!(matches!(
            roundtrip(proj_server, &project_write).await,
            HookResponse::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn plan_phase_allows_exploration_and_plan_writes_over_socket() {
        // The operator rule: a frozen phase rejects only *project* writes — all
        // exploration (read-only Bash incl. quoted pipes, read-verb MCP tools) and
        // plan/spec writes (~/.claude/plans) must pass. Uses the real DefaultPdp-shaped
        // TestPdp + the real classify/scope pipeline over IPC.
        let plan_gate = GateState {
            adopted: true,
            phase: Phase::Plan,
            ..Default::default()
        };

        // (1) Read-only Bash with a quoted `|` in the pattern + a real pipe.
        let bash_server = Arc::new(ControlServer::new(
            gates_with("explore-bash", plan_gate.clone()),
            Arc::new(TestPdp),
        ));
        let mut grep = req("explore-bash", "Bash");
        grep.tool_input = json!({ "command": r#"rtk grep -n "^#\|^## " prd.md | head -60"# });
        assert_eq!(roundtrip(bash_server, &grep).await, HookResponse::Allow);

        // (2) A read-verb MCP tool.
        let mcp_server = Arc::new(ControlServer::new(
            gates_with("explore-mcp", plan_gate.clone()),
            Arc::new(TestPdp),
        ));
        let probe = req("explore-mcp", "mcp__rustrover__search_in_files_by_regex");
        assert_eq!(roundtrip(mcp_server, &probe).await, HookResponse::Allow);

        // (3) A vouched tool the heuristic can't tell (safe_tools config).
        let vouched_server = Arc::new(
            ControlServer::new(
                gates_with("explore-vouched", plan_gate.clone()),
                Arc::new(TestPdp),
            )
            .with_ai_workspace(
                AiWorkspace::default().with_safe_tools(["mcp__x__ast_grep_search".to_string()]),
            ),
        );
        let vouched = req("explore-vouched", "mcp__x__ast_grep_search");
        assert_eq!(roundtrip(vouched_server, &vouched).await, HookResponse::Allow);

        // (4) A plan-file write under ~/.claude/plans (home-anchored AI root).
        let home = std::env::var("HOME").expect("HOME set in tests");
        let plans_server = Arc::new(ControlServer::new(
            gates_with("plan-write", plan_gate.clone()),
            Arc::new(TestPdp),
        ));
        let mut plan_write = req("plan-write", "Write");
        plan_write.tool_input = json!({ "file_path": format!("{home}/.claude/plans/p.md") });
        assert_eq!(roundtrip(plans_server, &plan_write).await, HookResponse::Allow);

        // (5) Project writes stay frozen: a project Edit and a mutating Bash.
        let deny_server = Arc::new(ControlServer::new(
            gates_with("deny-edit", plan_gate.clone()),
            Arc::new(TestPdp),
        ));
        let mut project_edit = req("deny-edit", "Edit");
        project_edit.tool_input = json!({ "file_path": "/repo/src/main.rs" });
        assert!(matches!(
            roundtrip(deny_server, &project_edit).await,
            HookResponse::Deny { .. }
        ));
        let build_server = Arc::new(ControlServer::new(
            gates_with("deny-bash", plan_gate),
            Arc::new(TestPdp),
        ));
        let mut build = req("deny-bash", "Bash");
        build.tool_input = json!({ "command": "rtk cargo build" });
        assert!(matches!(
            roundtrip(build_server, &build).await,
            HookResponse::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn unknown_session_is_allowed_over_socket() {
        let gates: GateView = Arc::new(RwLock::new(HashMap::new()));
        let server = Arc::new(ControlServer::new(gates, Arc::new(TestPdp)));
        let resp = roundtrip(server, &req("ghost", "Edit")).await;
        assert_eq!(resp, HookResponse::Allow);
    }

    #[tokio::test]
    async fn non_pretooluse_event_is_allowed() {
        // An adopted plan-mode Edit would normally be denied; but a non-PreToolUse
        // event must be observe-only (never gate).
        let gates = gates_with(
            "s1",
            GateState {
                adopted: true,
                phase: Phase::Plan,
                ..Default::default()
            },
        );
        let server = Arc::new(ControlServer::new(gates, Arc::new(TestPdp)));
        let mut r = req("s1", "Edit");
        r.event = "Stop".into();
        let bytes = serde_json::to_vec(&r).unwrap();
        assert_eq!(server.decide(&bytes).await, HookResponse::Allow);
    }

    #[tokio::test]
    async fn missing_socket_fails_open() {
        let path = std::env::temp_dir().join("ml-ctl-does-not-exist.sock");
        let _ = std::fs::remove_file(&path);
        let resp = query_hook(&path, &req("s1", "Edit"), Duration::from_millis(200)).await;
        assert_eq!(resp, HookResponse::Allow);
    }

    // ---- Held-approval path ---------------------------------------------

    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Default)]
    struct RecordingNotifier {
        notified: AtomicBool,
        had_plan: AtomicBool,
    }
    impl ApprovalNotifier for RecordingNotifier {
        fn approval_requested(&self, _session: &SessionId, _what: &str, plan: Option<&str>) {
            self.notified.store(true, Ordering::SeqCst);
            self.had_plan.store(plan.is_some(), Ordering::SeqCst);
        }
    }

    fn adopted_auto() -> GateState {
        GateState {
            adopted: true,
            phase: moonlight_domain::phase::Phase::AutoImplement,
            trust: moonlight_domain::trust::TrustTier::Standard,
            ..Default::default()
        }
    }

    fn exit_plan_bytes(session: &str) -> Vec<u8> {
        serde_json::to_vec(&HookRequest {
            event: "PreToolUse".into(),
            session_id: session.into(),
            tool_name: crate::gate::EXIT_PLAN_MODE.into(),
            tool_input: json!({ "plan": "1. do X" }),
            cwd: "/repo".into(),
        })
        .unwrap()
    }

    /// Spin until an approval is registered for `session` (the hold runs on another
    /// task). Bounded so a bug fails the test instead of hanging.
    async fn wait_pending(pending: &PendingApprovals, session: &str) {
        let id = SessionId::new(session);
        for _ in 0..1000 {
            if pending.is_pending(&id) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("approval never registered for {session}");
    }

    fn held_server(
        pending: Arc<PendingApprovals>,
        notifier: Arc<RecordingNotifier>,
        hold: Duration,
    ) -> Arc<ControlServer> {
        let gates = gates_with("s1", adopted_auto());
        Arc::new(ControlServer::with_approvals(
            gates,
            Arc::new(TestPdp),
            pending,
            notifier,
            Some(hold),
        ))
    }

    #[tokio::test]
    async fn exit_plan_hold_notifies_with_plan_then_approves() {
        let pending = Arc::new(PendingApprovals::new());
        let notifier = Arc::new(RecordingNotifier::default());
        let server = held_server(pending.clone(), notifier.clone(), Duration::from_secs(5));

        let bytes = exit_plan_bytes("s1");
        let srv = server.clone();
        let task = tokio::spawn(async move { srv.decide(&bytes).await });

        wait_pending(&pending, "s1").await;
        assert!(notifier.notified.load(Ordering::SeqCst));
        assert!(
            notifier.had_plan.load(Ordering::SeqCst),
            "plan should be forwarded"
        );

        pending.resolve(&SessionId::new("s1"), Decision::Approve);
        assert_eq!(task.await.unwrap(), HookResponse::Allow);
    }

    #[tokio::test]
    async fn hold_deny_returns_reason() {
        let pending = Arc::new(PendingApprovals::new());
        let notifier = Arc::new(RecordingNotifier::default());
        let server = held_server(pending.clone(), notifier, Duration::from_secs(5));

        let bytes = exit_plan_bytes("s1");
        let srv = server.clone();
        let task = tokio::spawn(async move { srv.decide(&bytes).await });

        wait_pending(&pending, "s1").await;
        pending.resolve(
            &SessionId::new("s1"),
            Decision::Deny {
                reason: "revise the plan".into(),
            },
        );
        match task.await.unwrap() {
            HookResponse::Deny { reason } => {
                assert!(reason.contains("revise the plan"), "{reason}")
            }
            other => panic!("expected deny, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hold_times_out_into_deny() {
        let pending = Arc::new(PendingApprovals::new());
        let notifier = Arc::new(RecordingNotifier::default());
        let server = held_server(pending.clone(), notifier, Duration::from_millis(30));

        let resp = server.decide(&exit_plan_bytes("s1")).await;
        match resp {
            HookResponse::Deny { reason } => assert!(reason.contains("elapsed"), "{reason}"),
            other => panic!("expected timeout deny, got {other:?}"),
        }
        // The timed-out waiter is cleaned up.
        assert!(!pending.is_pending(&SessionId::new("s1")));
    }
}
