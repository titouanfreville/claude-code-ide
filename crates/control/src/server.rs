//! The control IPC server + client over a Unix domain socket.
//!
//! Claude Code runs `moonlight hook …` (the client), which forwards one hook
//! request and reads one verdict. The server lives in the running app, looks up
//! the session's [`GateState`] in a shared read-model, and asks the PDP. Every
//! failure path fails *open* (allow) so a running Claude Code is never blocked by
//! MoonlightCode being absent, slow, or wrong.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use moonlight_domain::changes::{Baseline, BaselineGap, ChangeTool, FileTouch};
use moonlight_domain::ids::{SessionId, Timestamp};
use moonlight_domain::ports::{PolicyDecisionPoint, SessionChangeStore};
use moonlight_domain::trust::DangerClass;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use crate::config::AiWorkspaceResolver;
use crate::gate::{evaluate, GateDecision, GateState, HoldKind};
use crate::ipc::{HookRequest, HookResponse};
use crate::paths::{absolutize, classify_write_scope, write_target, AiWorkspace};
use crate::pending::{ApprovalNotifier, Decision, PendingApprovals};
use crate::probe::{NoProbe, SharedProbe};
use crate::shell_scan::Fingerprint;

/// The tool whose writes have to be inferred rather than read off the call.
const SHELL_TOOL: &str = "Bash";

/// The shared per-session gate read-model. The app keeps this in sync by folding
/// engine events; the server only ever reads it.
pub type GateView = Arc<RwLock<HashMap<SessionId, GateState>>>;

/// Operator "always allow" decisions made at runtime: tool-name patterns (exact, or a
/// trailing-`*` server glob like `mcp__phoenix__*`) vouched as read-only for the rest of
/// the process. Shared between the control server (which reads it to vouch a held tool)
/// and the cockpit command router (which appends to it when the operator clicks "always
/// allow"). Gives an "always" decision **immediate** effect — the persisted
/// `~/.moonlight/config.json` entry only takes effect on the next app start.
pub type RuntimeSafeTools = Arc<RwLock<std::collections::HashSet<String>>>;

/// A type alias for resolving an external conversation ID back to the launch UUID.
pub type IdResolver = Arc<dyn Fn(&str) -> Option<SessionId> + Send + Sync>;

/// How long the server holds a connection waiting for an operator decision before
/// denying. Must stay under Claude Code's per-hook timeout (≈60s) so the *server*
/// resolves the hold (a clean deny) rather than the client giving up and failing
/// open. On timeout the action is denied with a re-runnable reason.
pub const DEFAULT_HOLD: Duration = Duration::from_secs(45);

/// A notifier that does nothing — the default when no app is wired in (tests, the
/// degraded standalone server). Held actions then simply time out into a deny.
struct NoopNotifier;
impl ApprovalNotifier for NoopNotifier {
    fn approval_requested(
        &self,
        _session: &SessionId,
        _what: &str,
        _plan: Option<&str>,
        _mcp_tool: Option<&str>,
    ) {
    }
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
    /// Runtime "always allow" tool patterns (operator decisions), consulted in addition
    /// to the config `safe_tools`. Empty unless wired via [`ControlServer::with_runtime_safe`].
    runtime_safe: RuntimeSafeTools,
    /// Resolves an external session ID / conversation ID back to the launch UUID.
    id_resolver: Option<IdResolver>,
    /// Records which files a session writes, so the review surface can diff *this
    /// session's* changes rather than whatever is dirty in the tree. `None` (tests,
    /// the degraded standalone server) simply skips capture — the ledger is an
    /// observation, never a gate.
    changes: Option<Arc<dyn SessionChangeStore>>,
    /// Reads the workspace's VCS to find and explain writes made through the shell
    /// (see [`crate::shell_scan`]). Defaults to [`NoProbe`]: shell writes then go
    /// unattributed rather than blocking anything.
    probe: SharedProbe,
    /// Per-session workspace fingerprint taken at `PreToolUse(Bash)` and consumed at
    /// the matching `PostToolUse`. One entry per session: Claude Code runs a
    /// session's tools one at a time, so a pending fingerprint always belongs to the
    /// command now finishing.
    shell_pre: Mutex<HashMap<SessionId, Fingerprint>>,
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
            runtime_safe: RuntimeSafeTools::default(),
            id_resolver: None,
            changes: None,
            probe: Arc::new(NoProbe),
            shell_pre: Mutex::new(HashMap::new()),
        }
    }

    /// Share the runtime "always allow" set so operator decisions take effect without a
    /// restart (the cockpit appends to the same `Arc`; the server reads it per request).
    pub fn with_runtime_safe(mut self, runtime_safe: RuntimeSafeTools) -> Self {
        self.runtime_safe = runtime_safe;
        self
    }

    /// Wire the per-session file-change ledger. Without it the server gates exactly
    /// as before; it just records nothing.
    pub fn with_change_ledger(mut self, changes: Arc<dyn SessionChangeStore>) -> Self {
        self.changes = Some(changes);
        self
    }

    /// Wire the workspace probe that makes shell writes attributable. Without it
    /// only the path-declaring tools (`Edit`/`Write`/…) reach the ledger.
    pub fn with_workspace_probe(mut self, probe: SharedProbe) -> Self {
        self.probe = probe;
        self
    }

    /// Wire the external conversation ID resolver (e.g. for Antigravity).
    pub fn with_id_resolver(mut self, id_resolver: IdResolver) -> Self {
        self.id_resolver = Some(id_resolver);
        self
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
            runtime_safe: RuntimeSafeTools::default(),
            id_resolver: None,
            changes: None,
            probe: Arc::new(NoProbe),
            shell_pre: Mutex::new(HashMap::new()),
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
        // We only gate `PreToolUse`. `PostToolUse` is observe-only: it closes the
        // shell-write scan opened before the command ran. Any other hook event
        // (Stop, Notification, …) is allowed untouched, so registering us on a
        // different event can never wrongly block a tool. (Empty event = our own
        // internal/test messages.)
        if !req.event.is_empty() && req.event != "PreToolUse" {
            if req.event == "PostToolUse" {
                let session = self.resolve_session(&req);
                self.close_shell_scan(&session, &req);
            }
            return HookResponse::Allow;
        }
        let session = self.resolve_session(&req);

        // Resolve which part of the tree a file write touches (path + cwd vs the
        // cwd's effective AI-workspace allowlist) so a frozen phase freezes project
        // files only. The allowlist is the user + workspace config for this session.
        let ai = self.ai.for_cwd(&req.cwd);
        let write_scope = classify_write_scope(&req.tool_name, &req.tool_input, &req.cwd, &ai);
        // The operator's `safe_tools` config (or a runtime "always allow" decision)
        // vouches a tool as read-only — it then classifies Safe (survives frozen phases)
        // instead of the heuristic default.
        let vouched_safe = ai.is_safe_tool(&req.tool_name) || self.runtime_vouched(&req.tool_name);
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

        // Record only when the tool is actually about to run — a denied write never
        // happened, and a held one only happens if the operator lets it through.
        match decision {
            GateDecision::Allow => {
                self.record_touch(&session, &req);
                self.open_shell_scan(&session, &req);
                HookResponse::Allow
            }
            GateDecision::Deny { reason } => HookResponse::Deny { reason },
            GateDecision::Hold(kind) => {
                let response = self.hold(session.clone(), kind).await;
                if matches!(response, HookResponse::Allow) {
                    self.record_touch(&session, &req);
                    self.open_shell_scan(&session, &req);
                }
                response
            }
        }
    }

    /// The launch id behind a request's session id (Antigravity reports its own
    /// conversation id).
    fn resolve_session(&self, req: &HookRequest) -> SessionId {
        match &self.id_resolver {
            Some(resolve) => {
                resolve(&req.session_id).unwrap_or_else(|| SessionId::new(req.session_id.clone()))
            }
            None => SessionId::new(req.session_id.clone()),
        }
    }

    /// Whether this request is a shell command that could write, and so is worth
    /// fingerprinting the workspace around. Read-only commands (`ls`, `grep`,
    /// `git status`) are already classified `Safe`, and skipping them keeps two
    /// `git status` calls off the hot path of every shell invocation.
    fn is_scannable_shell(&self, req: &HookRequest) -> bool {
        self.changes.is_some()
            && req.tool_name == SHELL_TOOL
            && crate::classify(&req.tool_name, &req.tool_input) != DangerClass::Safe
    }

    /// Fingerprint the workspace before a shell command runs, so the matching
    /// `PostToolUse` can tell what it wrote.
    fn open_shell_scan(&self, session: &SessionId, req: &HookRequest) {
        if !self.is_scannable_shell(req) {
            return;
        }
        let before = Fingerprint::of(&self.probe.dirty_paths(&req.cwd));
        let mut pre = self.shell_pre.lock().unwrap_or_else(|p| p.into_inner());
        pre.insert(session.clone(), before);
    }

    /// Compare the workspace against the fingerprint taken before the command and
    /// record everything it wrote. Without a matching open scan (a read-only
    /// command, a server that started mid-command) there is nothing to close.
    fn close_shell_scan(&self, session: &SessionId, req: &HookRequest) {
        let Some(changes) = &self.changes else {
            return;
        };
        let before = {
            let mut pre = self.shell_pre.lock().unwrap_or_else(|p| p.into_inner());
            pre.remove(session)
        };
        let Some(before) = before else {
            return;
        };
        let after = Fingerprint::of(&self.probe.dirty_paths(&req.cwd));
        let at = now();
        for path in after.written_since(&before) {
            // `HEAD` is the only "before" available for a file nobody read: exact
            // when it was committed-clean, and flagged as HEAD-derived either way.
            // A file this session already touched keeps its earlier, exact baseline
            // — the store is first-touch-wins.
            let baseline = match self.probe.head_blob(&req.cwd, &path) {
                Some(content) => Baseline::FromHead(content),
                None => Baseline::Created,
            };
            let touch = FileTouch {
                session_id: session.clone(),
                path,
                at,
                tool: ChangeTool::Shell,
                baseline,
            };
            if let Err(error) = changes.record_touch(&touch) {
                tracing::warn!(session = %session, error = %error, "recording a shell write failed");
            }
        }
    }

    /// Note that `session` is about to write a file, capturing the file's current
    /// content as the baseline the first time this session touches it.
    ///
    /// This runs while Claude Code blocks on our verdict, which is exactly what makes
    /// it correct: the write hasn't landed, so what's on disk *is* the pre-image.
    /// Every failure is swallowed — capture is an observation and must never turn
    /// into a gate.
    fn record_touch(&self, session: &SessionId, req: &HookRequest) {
        let Some(changes) = &self.changes else {
            return;
        };
        let Some((path, tool)) = write_target(&req.tool_name, &req.tool_input) else {
            return;
        };
        let path = absolutize(path, &req.cwd);
        let touch = FileTouch {
            session_id: session.clone(),
            baseline: read_baseline(&path),
            path,
            at: now(),
            tool,
        };
        if let Err(error) = changes.record_touch(&touch) {
            tracing::warn!(session = %session, error = %error, "recording a file touch failed");
        }
    }

    /// Whether `tool_name` matches a runtime "always allow" pattern the operator set
    /// this session (exact or trailing-`*` server glob).
    fn runtime_vouched(&self, tool_name: &str) -> bool {
        let set = self.runtime_safe.read().unwrap_or_else(|p| p.into_inner());
        set.iter()
            .any(|p| crate::paths::safe_tool_matches(p, tool_name))
    }

    /// Pause the session on a held action: surface it to the operator, then await
    /// their decision. The hook stays blocked (CC waits) until this returns, so the
    /// session does not act until the operator approves.
    async fn hold(&self, session: SessionId, kind: HoldKind) -> HookResponse {
        let (what, plan, mcp_tool) = match &kind {
            HoldKind::Plan { plan } => ("approve plan", plan.as_deref(), None),
            HoldKind::Danger { reason } => (reason.as_str(), None, None),
            HoldKind::McpAuthorize { reason, tool_name } => {
                (reason.as_str(), None, Some(tool_name.as_str()))
            }
        };
        self.notifier
            .approval_requested(&session, what, plan, mcp_tool);

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
/// Capture cap for a baseline snapshot. Matches the code editor's own file cap:
/// past this the diff is unreadable anyway, and the ledger shouldn't grow without
/// bound on generated files.
const MAX_BASELINE_BYTES: u64 = 2 * 1024 * 1024;

/// How much of a file to sniff for NUL before deciding it isn't text.
const BINARY_SNIFF_BYTES: usize = 8192;

/// The pre-image of `path` for the change ledger, read while the writing tool is
/// still blocked on our verdict. A missing file means the agent is creating it.
fn read_baseline(path: &str) -> Baseline {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        // No file (or no access to stat it): treat as a creation. The review pane
        // then shows the whole file as added, which is what happened.
        Err(_) => return Baseline::Created,
    };
    if !meta.is_file() {
        return Baseline::Unavailable {
            reason: BaselineGap::Unreadable,
        };
    }
    if meta.len() > MAX_BASELINE_BYTES {
        return Baseline::Unavailable {
            reason: BaselineGap::TooLarge,
        };
    }
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => {
            return Baseline::Unavailable {
                reason: BaselineGap::Unreadable,
            }
        }
    };
    if bytes.iter().take(BINARY_SNIFF_BYTES).any(|b| *b == 0) {
        return Baseline::Unavailable {
            reason: BaselineGap::Binary,
        };
    }
    match String::from_utf8(bytes) {
        Ok(text) => Baseline::Content(text),
        Err(_) => Baseline::Unavailable {
            reason: BaselineGap::Binary,
        },
    }
}

/// Wall-clock now as epoch millis (the domain has no clock).
fn now() -> Timestamp {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Timestamp::from_millis(ms)
}

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
    use std::sync::Mutex;

    use crate::probe::WorkspaceProbe;
    use moonlight_domain::changes::{ReviewComment, TouchedPath};
    use moonlight_domain::errors::{StoreError, TrustError};
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
                    WriteScope::Ephemeral => true,
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

    /// Records what the server captured, so a test can assert on the ledger without
    /// a database.
    #[derive(Default)]
    struct FakeChanges {
        touches: Mutex<Vec<FileTouch>>,
    }

    impl SessionChangeStore for FakeChanges {
        fn record_touch(&self, touch: &FileTouch) -> Result<bool, StoreError> {
            let mut touches = self.touches.lock().unwrap_or_else(|p| p.into_inner());
            touches.push(touch.clone());
            Ok(true)
        }
        fn baseline(&self, _: &SessionId, _: &str) -> Result<Option<Baseline>, StoreError> {
            Ok(None)
        }
        fn touched_paths(&self, _: &SessionId) -> Result<Vec<TouchedPath>, StoreError> {
            Ok(Vec::new())
        }
        fn touched_counts(&self) -> Result<Vec<(SessionId, u32)>, StoreError> {
            Ok(Vec::new())
        }
        fn forget_session_changes(&self, _: &SessionId) -> Result<(), StoreError> {
            Ok(())
        }
        fn mark_reviewed(
            &self,
            _: &SessionId,
            _: &str,
            _: Option<Timestamp>,
        ) -> Result<bool, StoreError> {
            Ok(true)
        }
        fn ignored_paths(&self, _: &SessionId) -> Result<Vec<String>, StoreError> {
            Ok(Vec::new())
        }
        fn set_ignored(&self, _: &SessionId, _: &str, _: bool) -> Result<(), StoreError> {
            Ok(())
        }
        fn clear_ignored(&self, _: &SessionId) -> Result<(), StoreError> {
            Ok(())
        }
        fn add_comment(&self, _: &ReviewComment) -> Result<(), StoreError> {
            Ok(())
        }
        fn comments(&self, _: &SessionId) -> Result<Vec<ReviewComment>, StoreError> {
            Ok(Vec::new())
        }
        fn update_comment(&self, _: &ReviewComment) -> Result<bool, StoreError> {
            Ok(true)
        }
        fn delete_comment(&self, _: &str) -> Result<(), StoreError> {
            Ok(())
        }
        fn mark_comments_sent(&self, _: &[String], _: Timestamp) -> Result<(), StoreError> {
            Ok(())
        }
    }

    impl FakeChanges {
        fn recorded(&self) -> Vec<FileTouch> {
            self.touches
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone()
        }
    }

    /// A gate that lets writes through (Auto phase, adopted).
    fn writable_gate(session: &str) -> GateView {
        gates_with(
            session,
            GateState {
                adopted: true,
                phase: Phase::AutoImplement,
                ..Default::default()
            },
        )
    }

    /// Decide directly, skipping the socket — capture is what's under test here.
    async fn decide_json(server: &ControlServer, request: &HookRequest) -> HookResponse {
        server
            .decide(&serde_json::to_vec(request).expect("serialize hook request"))
            .await
    }

    #[tokio::test]
    async fn an_allowed_write_records_the_files_pre_image() {
        // A real file on disk: the point of capturing at PreToolUse is that what's
        // there right now is the "before" side of the review.
        let path = std::env::temp_dir().join(format!("ml-baseline-{}.txt", std::process::id()));
        std::fs::write(&path, "before the edit\n").unwrap();

        let changes = Arc::new(FakeChanges::default());
        let server = ControlServer::new(writable_gate("s1"), Arc::new(TestPdp))
            .with_change_ledger(changes.clone());

        let mut request = req("s1", "Edit");
        request.tool_input = json!({ "file_path": path.to_str().unwrap() });
        let resp = decide_json(&server, &request).await;

        assert!(matches!(resp, HookResponse::Allow));
        let recorded = changes.recorded();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].path, path.to_string_lossy());
        assert_eq!(recorded[0].tool, ChangeTool::Edit);
        assert_eq!(
            recorded[0].baseline,
            Baseline::Content("before the edit\n".into()),
            "the baseline is the content BEFORE the tool runs"
        );
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn a_denied_write_records_nothing() {
        let changes = Arc::new(FakeChanges::default());
        // Adopted + Plan = project writes are frozen, so this Edit is denied.
        let gates = gates_with(
            "s2",
            GateState {
                adopted: true,
                phase: Phase::Plan,
                ..Default::default()
            },
        );
        let server =
            ControlServer::new(gates, Arc::new(TestPdp)).with_change_ledger(changes.clone());

        let resp = decide_json(&server, &req("s2", "Edit")).await;

        assert!(matches!(resp, HookResponse::Deny { .. }));
        assert!(
            changes.recorded().is_empty(),
            "a write that never happened must not enter the ledger"
        );
    }

    #[tokio::test]
    async fn non_write_tools_are_not_ledger_entries() {
        let changes = Arc::new(FakeChanges::default());
        let server = ControlServer::new(writable_gate("s3"), Arc::new(TestPdp))
            .with_change_ledger(changes.clone());

        for tool in ["Read", "Bash", "Grep"] {
            let resp = decide_json(&server, &req("s3", tool)).await;
            assert!(matches!(resp, HookResponse::Allow));
        }
        assert!(
            changes.recorded().is_empty(),
            "only path-attributable writes are attributable to a file"
        );
    }

    #[tokio::test]
    async fn writing_a_new_file_records_it_as_created() {
        let changes = Arc::new(FakeChanges::default());
        let server = ControlServer::new(writable_gate("s4"), Arc::new(TestPdp))
            .with_change_ledger(changes.clone());

        let mut request = req("s4", "Write");
        let missing = std::env::temp_dir().join(format!("ml-absent-{}.txt", std::process::id()));
        std::fs::remove_file(&missing).ok();
        request.tool_input = json!({ "file_path": missing.to_str().unwrap() });

        let resp = decide_json(&server, &request).await;

        assert!(matches!(resp, HookResponse::Allow));
        let recorded = changes.recorded();
        assert_eq!(recorded[0].baseline, Baseline::Created);
        assert_eq!(recorded[0].tool, ChangeTool::Write);
    }

    #[tokio::test]
    async fn a_relative_tool_path_is_recorded_against_the_session_cwd() {
        let changes = Arc::new(FakeChanges::default());
        let server = ControlServer::new(writable_gate("s5"), Arc::new(TestPdp))
            .with_change_ledger(changes.clone());

        let mut request = req("s5", "Edit");
        request.tool_input = json!({ "file_path": "./src/main.rs" });
        request.cwd = "/repo".into();
        decide_json(&server, &request).await;

        // Absolute, so the review surface can find the file regardless of its own cwd.
        assert_eq!(changes.recorded()[0].path, "/repo/src/main.rs");
    }

    /// A probe over a fixed set of paths, standing in for a real repo.
    struct FakeProbe {
        dirty: Mutex<Vec<String>>,
        head: Option<String>,
    }

    impl WorkspaceProbe for FakeProbe {
        fn dirty_paths(&self, _cwd: &str) -> Vec<String> {
            self.dirty.lock().unwrap_or_else(|p| p.into_inner()).clone()
        }
        fn head_blob(&self, _cwd: &str, _path: &str) -> Option<String> {
            self.head.clone()
        }
    }

    /// A `Bash` request whose command mutates (so it is worth scanning around).
    fn bash(session: &str, command: &str) -> HookRequest {
        HookRequest {
            event: "PreToolUse".into(),
            session_id: session.into(),
            tool_name: "Bash".into(),
            tool_input: json!({ "command": command }),
            cwd: "/repo".into(),
        }
    }

    #[tokio::test]
    async fn a_shell_write_is_attributed_by_comparing_the_workspace() {
        // A file the command rewrites: dirty before *and* after, so only its stamp
        // gives it away — which is why the scan stats rather than diffing lists.
        let path = std::env::temp_dir().join(format!("ml-shell-{}.txt", std::process::id()));
        std::fs::write(&path, "before\n").unwrap();
        let path_str = path.to_string_lossy().into_owned();

        let changes = Arc::new(FakeChanges::default());
        let probe = Arc::new(FakeProbe {
            dirty: Mutex::new(vec![path_str.clone()]),
            head: Some("at head\n".to_string()),
        });
        let server = ControlServer::new(writable_gate("sh1"), Arc::new(TestPdp))
            .with_change_ledger(changes.clone())
            .with_workspace_probe(probe.clone());

        let mut request = bash("sh1", "sed -i s/a/b/ file.txt");
        decide_json(&server, &request).await;
        assert!(
            changes.recorded().is_empty(),
            "nothing is attributable until the command has actually run"
        );

        // The command runs and rewrites the file.
        std::thread::sleep(std::time::Duration::from_millis(5));
        std::fs::write(&path, "after the shell wrote\n").unwrap();

        request.event = "PostToolUse".into();
        let resp = decide_json(&server, &request).await;

        assert!(matches!(resp, HookResponse::Allow), "observe-only");
        let recorded = changes.recorded();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].path, path_str);
        assert_eq!(recorded[0].tool, ChangeTool::Shell);
        assert_eq!(
            recorded[0].baseline,
            Baseline::FromHead("at head\n".into()),
            "nobody read this file first, so HEAD is the only \"before\""
        );
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn a_shell_command_that_writes_nothing_records_nothing() {
        let path = std::env::temp_dir().join(format!("ml-shell-quiet-{}.txt", std::process::id()));
        std::fs::write(&path, "untouched\n").unwrap();

        let changes = Arc::new(FakeChanges::default());
        let probe = Arc::new(FakeProbe {
            dirty: Mutex::new(vec![path.to_string_lossy().into_owned()]),
            head: Some("x".into()),
        });
        let server = ControlServer::new(writable_gate("sh2"), Arc::new(TestPdp))
            .with_change_ledger(changes.clone())
            .with_workspace_probe(probe);

        let mut request = bash("sh2", "cargo build");
        decide_json(&server, &request).await;
        request.event = "PostToolUse".into();
        decide_json(&server, &request).await;

        assert!(changes.recorded().is_empty());
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn a_file_the_shell_created_has_no_head_blob() {
        let path = std::env::temp_dir().join(format!("ml-shell-new-{}.txt", std::process::id()));
        std::fs::remove_file(&path).ok();

        let changes = Arc::new(FakeChanges::default());
        let probe = Arc::new(FakeProbe {
            dirty: Mutex::new(Vec::new()),
            head: None, // untracked at HEAD
        });
        let server = ControlServer::new(writable_gate("sh3"), Arc::new(TestPdp))
            .with_change_ledger(changes.clone())
            .with_workspace_probe(probe.clone());

        let mut request = bash("sh3", "echo hi > new.txt");
        decide_json(&server, &request).await;

        // The command creates the file, so it is newly dirty.
        std::fs::write(&path, "hi\n").unwrap();
        *probe.dirty.lock().unwrap() = vec![path.to_string_lossy().into_owned()];

        request.event = "PostToolUse".into();
        decide_json(&server, &request).await;

        let recorded = changes.recorded();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].baseline, Baseline::Created);
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn read_only_shell_commands_are_never_scanned() {
        let changes = Arc::new(FakeChanges::default());
        let probe = Arc::new(FakeProbe {
            dirty: Mutex::new(vec!["/repo/a.rs".into()]),
            head: Some("x".into()),
        });
        let server = ControlServer::new(writable_gate("sh4"), Arc::new(TestPdp))
            .with_change_ledger(changes.clone())
            .with_workspace_probe(probe);

        // `ls` classifies Safe, so no fingerprint is taken…
        let mut request = bash("sh4", "ls -la");
        decide_json(&server, &request).await;
        // …and the matching PostToolUse finds no open scan to close.
        request.event = "PostToolUse".into();
        decide_json(&server, &request).await;
        assert!(changes.recorded().is_empty());
    }

    #[tokio::test]
    async fn a_post_hook_without_a_matching_pre_scan_is_harmless() {
        // The app started mid-command, so there is no fingerprint to compare against.
        let changes = Arc::new(FakeChanges::default());
        let server = ControlServer::new(writable_gate("sh5"), Arc::new(TestPdp))
            .with_change_ledger(changes.clone());

        let mut request = bash("sh5", "sed -i s/a/b/ x");
        request.event = "PostToolUse".into();
        let resp = decide_json(&server, &request).await;

        assert!(matches!(resp, HookResponse::Allow));
        assert!(changes.recorded().is_empty());
    }

    #[tokio::test]
    async fn a_denied_shell_command_opens_no_scan() {
        let changes = Arc::new(FakeChanges::default());
        let probe = Arc::new(FakeProbe {
            dirty: Mutex::new(Vec::new()),
            head: None,
        });
        // Adopted + Plan freezes project writes, so the command is denied outright.
        let gates = gates_with(
            "sh6",
            GateState {
                adopted: true,
                phase: Phase::Plan,
                ..Default::default()
            },
        );
        let server = ControlServer::new(gates, Arc::new(TestPdp))
            .with_change_ledger(changes.clone())
            .with_workspace_probe(probe);

        let mut request = bash("sh6", "sed -i s/a/b/ x");
        let resp = decide_json(&server, &request).await;
        assert!(matches!(resp, HookResponse::Deny { .. }));

        request.event = "PostToolUse".into();
        decide_json(&server, &request).await;
        assert!(
            changes.recorded().is_empty(),
            "a denied command never ran, so it wrote nothing"
        );
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
        assert_eq!(
            roundtrip(vouched_server, &vouched).await,
            HookResponse::Allow
        );

        // (4) A plan-file write under ~/.claude/plans (home-anchored AI root).
        let home = std::env::var("HOME").expect("HOME set in tests");
        let plans_server = Arc::new(ControlServer::new(
            gates_with("plan-write", plan_gate.clone()),
            Arc::new(TestPdp),
        ));
        let mut plan_write = req("plan-write", "Write");
        plan_write.tool_input = json!({ "file_path": format!("{home}/.claude/plans/p.md") });
        assert_eq!(
            roundtrip(plans_server, &plan_write).await,
            HookResponse::Allow
        );

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
    async fn runtime_always_allow_vouches_external_mcp_in_frozen_phase() {
        // The operator's "always allow" decision (runtime overlay) makes a frozen-phase
        // external MCP tool pass immediately — no restart, no config reload. A trailing
        // glob vouches the whole server.
        let runtime: RuntimeSafeTools = Default::default();
        runtime
            .write()
            .unwrap()
            .insert("mcp__phoenix__*".to_string());
        let plan_gate = GateState {
            adopted: true,
            phase: Phase::Plan,
            ..Default::default()
        };
        let server = Arc::new(
            ControlServer::new(gates_with("s1", plan_gate), Arc::new(TestPdp))
                .with_runtime_safe(runtime.clone()),
        );
        let resp = roundtrip(server, &req("s1", "mcp__phoenix__run_select_query")).await;
        assert_eq!(resp, HookResponse::Allow);
    }

    #[tokio::test]
    async fn unvouched_external_mcp_does_not_pass_via_overlay() {
        // Sanity: a server tool NOT covered by the overlay glob is not vouched (it falls
        // to the normal gate path).
        let runtime: RuntimeSafeTools = Default::default();
        runtime
            .write()
            .unwrap()
            .insert("mcp__phoenix__*".to_string());
        let plan_gate = GateState {
            adopted: true,
            phase: Phase::Plan,
            ..Default::default()
        };
        let server = Arc::new(
            ControlServer::new(gates_with("s2", plan_gate), Arc::new(TestPdp))
                .with_runtime_safe(runtime),
        );
        let resp = roundtrip(server, &req("s2", "mcp__other__run_query")).await;
        assert!(matches!(resp, HookResponse::Deny { .. }));
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
    async fn id_resolver_resolves_external_session_id() {
        let gates = gates_with(
            "internal-uuid-123",
            GateState {
                adopted: true,
                phase: Phase::Plan,
                ..Default::default()
            },
        );
        let resolver = Arc::new(|conversation_id: &str| {
            if conversation_id == "external-conversation-456" {
                Some(SessionId::new("internal-uuid-123"))
            } else {
                None
            }
        });
        let server =
            Arc::new(ControlServer::new(gates, Arc::new(TestPdp)).with_id_resolver(resolver));
        // Querying using the external conversation ID should trigger the plan-phase write protection (denied)
        let resp = roundtrip(server, &req("external-conversation-456", "Edit")).await;
        assert!(matches!(resp, HookResponse::Deny { .. }));
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
        had_mcp_tool: AtomicBool,
    }
    impl ApprovalNotifier for RecordingNotifier {
        fn approval_requested(
            &self,
            _session: &SessionId,
            _what: &str,
            plan: Option<&str>,
            mcp_tool: Option<&str>,
        ) {
            self.notified.store(true, Ordering::SeqCst);
            self.had_plan.store(plan.is_some(), Ordering::SeqCst);
            self.had_mcp_tool
                .store(mcp_tool.is_some(), Ordering::SeqCst);
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

    /// A PDP that always wants approval — to exercise the hold/notify path deterministically.
    struct PromptAllPdp;
    impl PolicyDecisionPoint for PromptAllPdp {
        fn decide(&self, _req: &PermissionRequest) -> Result<PermissionOutcome, TrustError> {
            Ok(PermissionOutcome::Prompt {
                reason: "authorize".into(),
            })
        }
    }

    #[tokio::test]
    async fn mcp_authorize_hold_forwards_the_tool_name_to_the_notifier() {
        // A held external MCP tool surfaces its name to the cockpit (so it can offer
        // per-tool / per-server "always"); a plan/danger hold carries no tool name.
        let pending = Arc::new(PendingApprovals::new());
        let notifier = Arc::new(RecordingNotifier::default());
        let server = Arc::new(ControlServer::with_approvals(
            gates_with("s1", adopted_auto()),
            Arc::new(PromptAllPdp),
            pending,
            notifier.clone(),
            Some(Duration::from_millis(20)),
        ));
        let bytes = serde_json::to_vec(&HookRequest {
            event: "PreToolUse".into(),
            session_id: "s1".into(),
            tool_name: "mcp__phoenix__run_select_query".into(),
            tool_input: json!({}),
            cwd: "/repo".into(),
        })
        .unwrap();
        // The hold times out into a deny, but the notifier was already called.
        let resp = server.decide(&bytes).await;
        assert!(matches!(resp, HookResponse::Deny { .. }));
        assert!(notifier.notified.load(Ordering::SeqCst));
        assert!(
            notifier.had_mcp_tool.load(Ordering::SeqCst),
            "external MCP tool name should be forwarded"
        );
        assert!(!notifier.had_plan.load(Ordering::SeqCst));
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
