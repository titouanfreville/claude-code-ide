//! Embedded MCP server adapter: exposes actor verbs (`run_with_coverage`,
//! `query_db`, `http_request`, `open_review`; `start_debug` is Phase 2) to Claude
//! Code sessions. Slice 3.
//!
//! The heart is [`ActorService`], which enforces the **mandatory verb shape**
//! (architecture): *resolve → PDP → execute → audit → compact result*. Every verb a
//! session invokes is gated by the single [`PolicyDecisionPoint`] against the
//! session's live phase + trust tier, and every executed (or refused) verb is
//! appended to the audit log (FR33). The MCP wire transport (rmcp) and the
//! composition wiring land in the next increment; this module is the policy-correct
//! core, exercised end-to-end against the real PDP in tests.

pub mod adapters;
pub mod transport;

pub use adapters::{BusPolicyView, StoreAuditSink};
pub use transport::{serve_http, serve_stdio, McpHost, VerbToolServer};

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use moonlight_domain::audit::AuditAction;
use moonlight_domain::errors::ControlError;
use moonlight_domain::phase::Phase;
use moonlight_domain::ports::mcp::{
    ActorRequest, ActorResult, ApprovalDecision, ApprovalGate, AuditSink, McpActor,
    PermissionRequest, PolicyDecisionPoint, SessionPolicySnapshot, SessionPolicyView, VerbExecutor,
};
use moonlight_domain::trust::{DangerClass, McpVerb, PermissionOutcome};

/// The actor: turns a session's verb request into a policy-gated, audited execution.
/// Holds only ports, so it is pure orchestration — adapters decide *how* a verb runs,
/// where the policy comes from, and where the audit lands.
pub struct ActorService {
    pdp: Arc<dyn PolicyDecisionPoint>,
    policy: Arc<dyn SessionPolicyView>,
    executor: Arc<dyn VerbExecutor>,
    audit: Arc<dyn AuditSink>,
    /// Holds a `Prompt` verb until the operator decides. Use [`DenyingApprovalGate`]
    /// where no approval channel exists (preserves default-deny).
    approval: Arc<dyn ApprovalGate>,
    /// Operator's **auto-phasing** opt-in (a shared global toggle). When on, a
    /// `Prompt` on a **control-plane** verb (`request_phase`) is auto-approved instead
    /// of waiting on the cockpit gate — the operator has pre-authorized the agent to
    /// move its own phase. Off by default; never affects any other verb.
    auto_phase: Arc<std::sync::atomic::AtomicBool>,
}

impl ActorService {
    pub fn new(
        pdp: Arc<dyn PolicyDecisionPoint>,
        policy: Arc<dyn SessionPolicyView>,
        executor: Arc<dyn VerbExecutor>,
        audit: Arc<dyn AuditSink>,
        approval: Arc<dyn ApprovalGate>,
    ) -> Self {
        Self {
            pdp,
            policy,
            executor,
            audit,
            approval,
            auto_phase: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Share the operator's auto-phasing toggle (the UI flips this `AtomicBool`); when
    /// on, control-plane `Prompt`s auto-approve. Defaults off when never set.
    pub fn with_auto_phase(mut self, flag: Arc<std::sync::atomic::AtomicBool>) -> Self {
        self.auto_phase = flag;
        self
    }
}

/// Run an approved verb and audit it. A **free function** (not a method) so the
/// deferred phase-control path can call it from a detached task without borrowing
/// `&self`. Shared by the `Allow` path, the auto-phase path, and an operator-approved
/// `Prompt`. Execution errors surface as a non-ok result, not a hard error — the agent
/// sees what failed.
async fn execute_and_audit(
    executor: &Arc<dyn VerbExecutor>,
    audit: &Arc<dyn AuditSink>,
    req: &ActorRequest,
    snapshot: &SessionPolicySnapshot,
) -> ActorResult {
    match executor
        .execute(
            &req.session,
            snapshot.root.as_deref(),
            req.verb,
            &req.payload,
        )
        .await
    {
        Ok(raw) => {
            let compact = compact_output(&raw);
            audit.record(
                &req.session,
                AuditAction::VerbExecuted {
                    verb: req.verb,
                    summary: summary_line(&compact),
                },
                true,
            );
            ActorResult {
                ok: true,
                compact_output: compact,
            }
        }
        Err(err) => {
            let message = err.to_string();
            audit.record(
                &req.session,
                AuditAction::VerbExecuted {
                    verb: req.verb,
                    summary: format!("failed: {message}"),
                },
                false,
            );
            ActorResult {
                ok: false,
                compact_output: message,
            }
        }
    }
}

#[async_trait]
impl McpActor for ActorService {
    async fn run(&self, req: &ActorRequest) -> Result<ActorResult, ControlError> {
        // 1. Resolve the session's live policy snapshot (phase + tier + repo root).
        let Some(snapshot) = self.policy.snapshot(&req.session) else {
            return Err(ControlError::SessionNotFound(req.session.clone()));
        };

        // 2. Ask the single PDP. The verb's danger class + the session's phase/tier
        //    compose the verdict; the actor never decides policy itself.
        let request = PermissionRequest {
            session: req.session.clone(),
            phase: snapshot.phase,
            trust_tier: snapshot.trust_tier,
            verb: Some(req.verb),
            danger: danger_class(req.verb),
            write_scope: None,
            // MoonlightCode's own verbs route through their own control-plane gate
            // (request_phase auto-approves; reads pass) — they don't use the external
            // MCP prompt-on-freeze path.
            prompt_on_project_freeze: false,
            description: describe(req),
        };
        let outcome = self
            .pdp
            .decide(&request)
            .map_err(|e| ControlError::Transport(e.to_string()))?;

        match outcome {
            // Allow → execute → audit → compact result (steps 3-5).
            PermissionOutcome::Allow => {
                Ok(execute_and_audit(&self.executor, &self.audit, req, &snapshot).await)
            }
            // Prompt → the operator must decide.
            PermissionOutcome::Prompt { .. } => {
                if req.verb.is_phase_control() {
                    // Auto-approve routine phase changes inline; only a move that GAINS
                    // project-write access from a frozen phase (Discovery/Plan/Commit →
                    // Auto/Test/Review) waits on the operator. The operator's global
                    // auto-phasing toggle pre-authorizes even the sensitive ones. Either
                    // way the change runs inline (fast — no hold), audited as an
                    // auto-approval.
                    let needs_operator = phase_change_needs_operator(snapshot.phase, &req.payload);
                    if !needs_operator || self.auto_phase.load(std::sync::atomic::Ordering::Relaxed)
                    {
                        self.audit.record(
                            &req.session,
                            AuditAction::Approved {
                                what: format!("auto-phase · {}", describe(req)),
                            },
                            false,
                        );
                        return Ok(
                            execute_and_audit(&self.executor, &self.audit, req, &snapshot).await,
                        );
                    }
                    // Otherwise: do NOT hold the MCP tool call open on an unbounded human
                    // decision. The approval gate waits for the operator indefinitely
                    // (`timeout: None`); a tool call held that long outlives the client's
                    // tool-call timeout and the transport drops mid-call (the response is
                    // lost — the live `request_phase` failure). Instead acknowledge now
                    // and run the operator approval + the phase change in a detached task.
                    // The agent rediscovers the new phase via `phase_status` / the next
                    // verb's phase footer — it is told never to assume it changed.
                    let approval = self.approval.clone();
                    let executor = self.executor.clone();
                    let audit = self.audit.clone();
                    let what = describe(req);
                    let req = req.clone();
                    let snapshot = snapshot.clone();
                    tokio::spawn(async move {
                        match approval.request(&req.session, &what).await {
                            ApprovalDecision::Approve => {
                                audit.record(
                                    &req.session,
                                    AuditAction::Approved { what: what.clone() },
                                    false,
                                );
                                let _ = execute_and_audit(&executor, &audit, &req, &snapshot).await;
                            }
                            ApprovalDecision::Deny { reason } => {
                                audit.record(
                                    &req.session,
                                    AuditAction::Denied { what, reason },
                                    false,
                                );
                            }
                        }
                    });
                    return Ok(ActorResult {
                        ok: true,
                        compact_output: "Phase change requested — submitted to the operator for \
                            approval. It is not applied until the operator approves, and this call \
                            does not wait for that. Call phase_status to confirm the active phase \
                            (or watch the phase footer on your next tool result)."
                            .to_string(),
                    });
                }
                // A non-phase Prompt (trust-tier / danger-zone): the agent needs the
                // verb's actual result, so block on the operator as before. Approve runs
                // it (audited Approved, then VerbExecuted); Deny refuses it.
                match self.approval.request(&req.session, &describe(req)).await {
                    ApprovalDecision::Approve => {
                        self.audit.record(
                            &req.session,
                            AuditAction::Approved {
                                what: describe(req),
                            },
                            false,
                        );
                        Ok(execute_and_audit(&self.executor, &self.audit, req, &snapshot).await)
                    }
                    ApprovalDecision::Deny { reason } => {
                        self.audit.record(
                            &req.session,
                            AuditAction::Denied {
                                what: describe(req),
                                reason: reason.clone(),
                            },
                            false,
                        );
                        Ok(ActorResult {
                            ok: false,
                            compact_output: format!("denied: {reason}"),
                        })
                    }
                }
            }
            PermissionOutcome::Deny { reason } => {
                self.audit.record(
                    &req.session,
                    AuditAction::Denied {
                        what: describe(req),
                        reason: reason.clone(),
                    },
                    false,
                );
                Ok(ActorResult {
                    ok: false,
                    compact_output: format!("denied: {reason}"),
                })
            }
        }
    }
}

/// The default-deny [`ApprovalGate`]: refuses every held verb. Used where no
/// operator approval channel is wired (so a `Prompt` verdict stays default-deny).
pub struct DenyingApprovalGate;

#[async_trait]
impl ApprovalGate for DenyingApprovalGate {
    async fn request(
        &self,
        _session: &moonlight_domain::ids::SessionId,
        _what: &str,
    ) -> ApprovalDecision {
        ApprovalDecision::Deny {
            reason: "operator approval unavailable".into(),
        }
    }
}

/// The danger class of a verb (its baseline; a per-payload escalation — e.g. an
/// `HttpRequest` to a prod host — is a later refinement). Read verbs are `Safe`;
/// side-effecting ones are `Risky` so a frozen phase (Plan/Discovery/Commit) gates
/// them like any other write.
fn danger_class(verb: McpVerb) -> DangerClass {
    match verb {
        McpVerb::RunWithCoverage | McpVerb::QueryDb | McpVerb::OpenReview => DangerClass::Safe,
        // Presenting a plan opens a review surface (no project side effect). The real
        // gating is the PreToolUse hook *hold* (operator review), so here it is `Safe` —
        // the server-side PDP must not double-prompt after the hook already held+approved.
        McpVerb::PresentPlan => DangerClass::Safe,
        // Reading the Run console (status / captured logs / detected targets) has no
        // side effects.
        McpVerb::RunStatus | McpVerb::RunLogs | McpVerb::RunListTargets => DangerClass::Safe,
        // Launching/killing the project's run is side-effecting — a frozen phase
        // (Plan/Discovery/Commit) gates it like any other write.
        McpVerb::RunStart | McpVerb::RunStop => DangerClass::Risky,
        McpVerb::HttpRequest | McpVerb::StartDebug => DangerClass::Risky,
        // Control-plane: not a file write, so `Safe` keeps it clear of the project-write
        // freeze. Its approval requirement comes from the PDP's control-plane branch
        // (always prompts), not from a danger class — see `McpVerb::is_phase_control`.
        McpVerb::RequestPhase => DangerClass::Safe,
        // A status self-report: harmless, runs autonomously in any phase.
        McpVerb::ReportBlocked => DangerClass::Safe,
        // A pure read of the session's own phase: harmless, runs in any phase.
        McpVerb::PhaseStatus => DangerClass::Safe,
    }
}

/// Whether a `request_phase` needs the operator's explicit OK, or may auto-approve.
///
/// The only sensitive move is one that **gains project-write access from a frozen
/// phase** — Discovery/Plan/Commit (no writes) → Auto/Test/Review (writes). Every other
/// transition (staying frozen, staying writable, or dropping back to a frozen phase)
/// auto-approves so the agent isn't blocked stepping through the routine workflow. The
/// target is read from the payload (empty / `next` ⇒ the next phase); an unparseable
/// target is treated as sensitive so a typo can never silently unlock writes.
fn phase_change_needs_operator(current: Phase, payload: &str) -> bool {
    let token = payload.trim();
    let target = if token.is_empty() || token.eq_ignore_ascii_case("next") {
        current.next()
    } else {
        match Phase::from_token(token) {
            Some(p) => p,
            None => return true,
        }
    };
    !current.allows_writes() && target.allows_writes()
}

/// A short audit/prompt description of a verb request (payload truncated).
fn describe(req: &ActorRequest) -> String {
    const MAX: usize = 60;
    let payload = req.payload.trim();
    let short: String = if payload.chars().count() > MAX {
        let head: String = payload.chars().take(MAX).collect();
        format!("{head}…")
    } else {
        payload.to_string()
    };
    format!("{:?} {short}", req.verb).trim_end().to_string()
}

/// RTK-style compaction: keep only the high-signal lines (errors, failures, the test
/// result line); if none match, keep the tail. Caps the line count so a verbose run
/// can't blow the agent's context.
fn compact_output(raw: &str) -> String {
    const KEEP: usize = 40;
    let signal: Vec<&str> = raw.lines().filter(|l| is_signal(l)).collect();
    let chosen: Vec<&str> = if signal.is_empty() {
        let tail: Vec<&str> = raw.lines().rev().take(KEEP).collect();
        tail.into_iter().rev().collect()
    } else {
        signal.into_iter().take(KEEP).collect()
    };
    chosen.join("\n")
}

/// Whether a line carries failure/result signal worth keeping.
fn is_signal(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    ["error", "fail", "panic", "test result", "warning"]
        .iter()
        .any(|marker| l.contains(marker))
}

/// One-line summary for the audit entry: the test-result line if present, else the
/// first non-empty line.
fn summary_line(compact: &str) -> String {
    compact
        .lines()
        .find(|l| l.to_ascii_lowercase().contains("test result"))
        .or_else(|| compact.lines().find(|l| !l.trim().is_empty()))
        .unwrap_or("(no output)")
        .to_string()
}

/// The concrete [`VerbExecutor`] for `run_with_coverage`: shells a configured test
/// command in the session's repo and returns its combined stdout+stderr (the actor
/// compacts it). Other verbs are not yet implemented (return `Unsupported`).
pub struct ShellVerbExecutor {
    /// The test command, e.g. `["cargo", "test"]` or `["npm", "test"]`.
    test_command: Vec<String>,
}

impl ShellVerbExecutor {
    pub fn new(test_command: Vec<String>) -> Self {
        Self { test_command }
    }
}

#[async_trait]
impl VerbExecutor for ShellVerbExecutor {
    async fn execute(
        &self,
        _session: &moonlight_domain::ids::SessionId,
        root: Option<&Path>,
        verb: McpVerb,
        _payload: &str,
    ) -> Result<String, ControlError> {
        if verb != McpVerb::RunWithCoverage {
            return Err(ControlError::Unsupported(format!("{verb:?}")));
        }
        let Some((program, args)) = self.test_command.split_first() else {
            return Err(ControlError::Unsupported("empty test command".into()));
        };
        let mut command = tokio::process::Command::new(program);
        command.args(args);
        if let Some(dir) = root {
            command.current_dir(dir);
        }
        let output = command
            .output()
            .await
            .map_err(|e| ControlError::Transport(format!("running {program}: {e}")))?;
        let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            combined.push('\n');
            combined.push_str(&stderr);
        }
        Ok(combined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    use moonlight_domain::ids::SessionId;
    use moonlight_domain::phase::Phase;
    use moonlight_domain::ports::mcp::SessionPolicySnapshot;
    use moonlight_domain::trust::TrustTier;
    use moonlight_trust::DefaultPdp;

    /// Hands back a fixed policy snapshot (or `None` for an unknown session).
    struct FakePolicy(Option<SessionPolicySnapshot>);
    impl SessionPolicyView for FakePolicy {
        fn snapshot(&self, _session: &SessionId) -> Option<SessionPolicySnapshot> {
            self.0.clone()
        }
    }

    /// Returns canned output (or an error) and records that it was called.
    struct FakeExecutor {
        result: Result<String, ControlError>,
        called: Mutex<bool>,
    }
    impl FakeExecutor {
        fn ok(out: &str) -> Self {
            Self {
                result: Ok(out.to_string()),
                called: Mutex::new(false),
            }
        }
        fn err(e: ControlError) -> Self {
            Self {
                result: Err(e),
                called: Mutex::new(false),
            }
        }
        fn was_called(&self) -> bool {
            *self.called.lock().unwrap()
        }
    }
    #[async_trait]
    impl VerbExecutor for FakeExecutor {
        async fn execute(
            &self,
            _session: &SessionId,
            _root: Option<&Path>,
            _verb: McpVerb,
            _payload: &str,
        ) -> Result<String, ControlError> {
            *self.called.lock().unwrap() = true;
            match &self.result {
                Ok(s) => Ok(s.clone()),
                Err(_) => Err(ControlError::Transport("boom".into())),
            }
        }
    }

    #[derive(Default)]
    struct FakeAudit(Mutex<Vec<AuditAction>>);
    impl AuditSink for FakeAudit {
        fn record(&self, _session: &SessionId, action: AuditAction, _revertible: bool) {
            self.0.lock().unwrap().push(action);
        }
    }
    impl FakeAudit {
        fn actions(&self) -> Vec<AuditAction> {
            self.0.lock().unwrap().clone()
        }
    }

    fn snapshot(phase: Phase, tier: TrustTier) -> SessionPolicySnapshot {
        SessionPolicySnapshot {
            phase,
            trust_tier: tier,
            root: None,
        }
    }

    /// A gate with a fixed verdict, for the `Prompt` paths.
    struct FakeGate(ApprovalDecision);
    #[async_trait]
    impl ApprovalGate for FakeGate {
        async fn request(&self, _session: &SessionId, _what: &str) -> ApprovalDecision {
            self.0.clone()
        }
    }

    fn service(
        snap: Option<SessionPolicySnapshot>,
        executor: Arc<FakeExecutor>,
        audit: Arc<FakeAudit>,
    ) -> ActorService {
        service_with(snap, executor, audit, Arc::new(DenyingApprovalGate))
    }

    fn service_with(
        snap: Option<SessionPolicySnapshot>,
        executor: Arc<FakeExecutor>,
        audit: Arc<FakeAudit>,
        approval: Arc<dyn ApprovalGate>,
    ) -> ActorService {
        ActorService::new(
            Arc::new(DefaultPdp),
            Arc::new(FakePolicy(snap)),
            executor,
            audit,
            approval,
        )
    }

    fn run_req() -> ActorRequest {
        ActorRequest {
            session: SessionId::new("s1"),
            verb: McpVerb::RunWithCoverage,
            payload: String::new(),
        }
    }

    fn phase_req() -> ActorRequest {
        ActorRequest {
            session: SessionId::new("s1"),
            verb: McpVerb::RequestPhase,
            payload: "auto".into(),
        }
    }

    #[tokio::test]
    async fn phase_control_prompt_acks_immediately_then_executes_on_approval() {
        // request_phase must NOT hold the MCP call open on the operator decision (that
        // unbounded hold dropped the transport mid-call). It returns an ack at once and
        // does not run synchronously; the approval + phase change happen out of band.
        // Here the gate approves, so the verb eventually runs.
        let exec = Arc::new(FakeExecutor::ok("Phase change approved: Plan -> Auto"));
        let audit = Arc::new(FakeAudit::default());
        let svc = service_with(
            Some(snapshot(Phase::Plan, TrustTier::Observed)),
            exec.clone(),
            audit.clone(),
            Arc::new(FakeGate(ApprovalDecision::Approve)),
        );
        let out = svc.run(&phase_req()).await.unwrap();
        // Immediate ack — success (the *submission* succeeded), not the executed result,
        // and the executor has not run yet.
        assert!(out.ok, "{out:?}");
        assert!(
            out.compact_output.to_lowercase().contains("operator"),
            "ack must say it is pending operator approval: {out:?}"
        );
        assert!(!exec.was_called(), "must not execute synchronously");
        // Let the detached approval task run to completion.
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        assert!(exec.was_called(), "verb runs once the operator approves");
        assert!(audit
            .actions()
            .iter()
            .any(|a| matches!(a, AuditAction::Approved { .. })));
    }

    #[tokio::test]
    async fn phase_control_prompt_denied_out_of_band_never_executes() {
        // The ack is returned regardless of the eventual decision; a background deny
        // (default-deny gate) audits a Denial and never runs the verb.
        let exec = Arc::new(FakeExecutor::ok("unused"));
        let audit = Arc::new(FakeAudit::default());
        let svc = service(
            Some(snapshot(Phase::Plan, TrustTier::Observed)),
            exec.clone(),
            audit.clone(),
        );
        let out = svc.run(&phase_req()).await.unwrap();
        assert!(out.ok, "the submission itself succeeds: {out:?}");
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        assert!(!exec.was_called(), "a denied phase change never executes");
        assert!(audit
            .actions()
            .iter()
            .any(|a| matches!(a, AuditAction::Denied { .. })));
    }

    #[tokio::test]
    async fn auto_phase_on_auto_approves_a_control_plane_prompt() {
        // With the operator's auto-phasing toggle on, the same Prompt is auto-approved
        // and the verb runs — without ever reaching the (default-deny) gate.
        let exec = Arc::new(FakeExecutor::ok("moving to the Auto phase"));
        let audit = Arc::new(FakeAudit::default());
        let svc = service(
            Some(snapshot(Phase::Plan, TrustTier::Observed)),
            exec.clone(),
            audit.clone(),
        )
        .with_auto_phase(Arc::new(std::sync::atomic::AtomicBool::new(true)));
        let out = svc.run(&phase_req()).await.unwrap();
        assert!(out.ok, "{out:?}");
        assert!(exec.was_called());
        // Audited as an approval (auto) + the execution.
        assert!(audit
            .actions()
            .iter()
            .any(|a| matches!(a, AuditAction::Approved { .. })));
    }

    #[tokio::test]
    async fn auto_phase_never_touches_a_non_control_verb() {
        // A normal verb that Prompts (low tier) is NOT auto-approved by auto-phasing —
        // it still goes to the gate (here default-deny).
        let exec = Arc::new(FakeExecutor::ok("ok"));
        let audit = Arc::new(FakeAudit::default());
        let svc = service(
            // OpenReview at Observed tier → Prompt (tier too low).
            Some(snapshot(Phase::AutoImplement, TrustTier::Observed)),
            exec.clone(),
            audit,
        )
        .with_auto_phase(Arc::new(std::sync::atomic::AtomicBool::new(true)));
        let req = ActorRequest {
            session: SessionId::new("s1"),
            verb: McpVerb::OpenReview,
            payload: String::new(),
        };
        let out = svc.run(&req).await.unwrap();
        assert!(!out.ok, "{out:?}");
        assert!(!exec.was_called());
    }

    #[test]
    fn phase_change_needs_operator_only_when_gaining_writes_from_frozen() {
        // Gaining project-write access from a frozen phase = sensitive.
        assert!(phase_change_needs_operator(Phase::Discovery, "auto"));
        assert!(phase_change_needs_operator(Phase::Plan, "auto"));
        assert!(phase_change_needs_operator(Phase::Plan, "test"));
        assert!(phase_change_needs_operator(Phase::Commit, "auto"));
        // `next` from Plan lands in AutoImplement (writable) — still sensitive.
        assert!(phase_change_needs_operator(Phase::Plan, "next"));
        // Frozen → frozen (Discovery → Plan, or `next` from Discovery) = fine.
        assert!(!phase_change_needs_operator(Phase::Discovery, "plan"));
        assert!(!phase_change_needs_operator(Phase::Discovery, "next"));
        // Writable → writable, and dropping back to a frozen phase = fine.
        assert!(!phase_change_needs_operator(Phase::AutoImplement, "test"));
        assert!(!phase_change_needs_operator(Phase::Review, "commit"));
        // An unparseable target fails safe (treated as sensitive).
        assert!(phase_change_needs_operator(Phase::Plan, "ludicrous-speed"));
    }

    #[tokio::test]
    async fn non_sensitive_phase_change_auto_approves_without_operator() {
        // Discovery → Plan stays frozen (no new write access), so it must NOT wait on
        // the operator even behind a default-deny gate: it auto-approves inline and runs.
        let exec = Arc::new(FakeExecutor::ok("now in the Plan phase"));
        let audit = Arc::new(FakeAudit::default());
        let svc = service(
            Some(snapshot(Phase::Discovery, TrustTier::Observed)),
            exec.clone(),
            audit.clone(),
        );
        let req = ActorRequest {
            session: SessionId::new("s1"),
            verb: McpVerb::RequestPhase,
            payload: "plan".into(),
        };
        let out = svc.run(&req).await.unwrap();
        assert!(out.ok, "{out:?}");
        assert!(exec.was_called(), "a non-sensitive phase change runs inline");
        assert!(audit
            .actions()
            .iter()
            .any(|a| matches!(a, AuditAction::Approved { .. })));
    }

    #[tokio::test]
    async fn allowed_verb_runs_compacts_and_audits_executed() {
        let exec = Arc::new(FakeExecutor::ok(
            "compiling...\nok\ntest result: ok. 12 passed; 0 failed\nfinished",
        ));
        let audit = Arc::new(FakeAudit::default());
        // ReadOnly tier in AutoImplement: RunWithCoverage (Safe, min ReadOnly) → Allow.
        let svc = service(
            Some(snapshot(Phase::AutoImplement, TrustTier::ReadOnly)),
            exec.clone(),
            audit.clone(),
        );

        let result = svc.run(&run_req()).await.unwrap();
        assert!(result.ok);
        assert!(exec.was_called());
        // Compacted to the signal line.
        assert!(result.compact_output.contains("test result: ok. 12 passed"));
        assert!(!result.compact_output.contains("compiling..."));
        // Audited as an executed verb with the result summary.
        assert!(matches!(
            &audit.actions()[..],
            [AuditAction::VerbExecuted { verb: McpVerb::RunWithCoverage, summary }]
                if summary.contains("test result")
        ));
    }

    #[tokio::test]
    async fn run_read_verbs_pass_the_frozen_plan_phase() {
        // Plan freezes project writes, but reading the Run console (logs/status/
        // targets) is a read verb — the agent can inspect run output while planning.
        let exec = Arc::new(FakeExecutor::ok("line-1\nline-2"));
        let audit = Arc::new(FakeAudit::default());
        let svc = service(
            Some(snapshot(Phase::Plan, TrustTier::ReadOnly)),
            exec.clone(),
            audit.clone(),
        );
        let req = ActorRequest {
            session: SessionId::new("s1"),
            verb: McpVerb::RunLogs,
            payload: "50".into(),
        };
        let result = svc.run(&req).await.unwrap();
        assert!(result.ok);
        assert!(exec.was_called());
    }

    #[tokio::test]
    async fn run_start_is_frozen_in_plan_but_runs_in_auto_at_standard() {
        let req = ActorRequest {
            session: SessionId::new("s1"),
            verb: McpVerb::RunStart,
            payload: String::new(),
        };

        // Plan: RunStart is Risky → the project freeze denies it outright — even at
        // Trusted tier (the phase gate outranks trust).
        let exec = Arc::new(FakeExecutor::ok("should not run"));
        let audit = Arc::new(FakeAudit::default());
        let svc = service(
            Some(snapshot(Phase::Plan, TrustTier::Trusted)),
            exec.clone(),
            audit.clone(),
        );
        let result = svc.run(&req).await.unwrap();
        assert!(!result.ok);
        assert!(
            !exec.was_called(),
            "frozen phase must not execute run_start"
        );
        assert!(matches!(&audit.actions()[..], [AuditAction::Denied { .. }]));

        // AutoImplement at Standard: allowed autonomously.
        let exec = Arc::new(FakeExecutor::ok("started `cargo run`"));
        let audit = Arc::new(FakeAudit::default());
        let svc = service(
            Some(snapshot(Phase::AutoImplement, TrustTier::Standard)),
            exec.clone(),
            audit.clone(),
        );
        let result = svc.run(&req).await.unwrap();
        assert!(result.ok);
        assert!(exec.was_called());
    }

    #[tokio::test]
    async fn low_tier_is_refused_not_run_and_audited_denied() {
        let exec = Arc::new(FakeExecutor::ok("should not run"));
        let audit = Arc::new(FakeAudit::default());
        // Observed tier: OpenReview needs Standard → PDP Prompt → refused autonomously.
        let svc = service(
            Some(snapshot(Phase::AutoImplement, TrustTier::Observed)),
            exec.clone(),
            audit.clone(),
        );
        let req = ActorRequest {
            session: SessionId::new("s1"),
            verb: McpVerb::OpenReview,
            payload: String::new(),
        };

        let result = svc.run(&req).await.unwrap();
        assert!(!result.ok);
        // With no approval channel (DenyingApprovalGate) a Prompt becomes a deny.
        assert!(result.compact_output.contains("approval unavailable"));
        assert!(!exec.was_called(), "a refused verb must not execute");
        assert!(matches!(&audit.actions()[..], [AuditAction::Denied { .. }]));
    }

    #[tokio::test]
    async fn prompt_verb_runs_when_the_operator_approves() {
        let exec = Arc::new(FakeExecutor::ok("test result: ok. 1 passed"));
        let audit = Arc::new(FakeAudit::default());
        // OpenReview at Observed → Prompt; an approving gate lets it through.
        let svc = service_with(
            Some(snapshot(Phase::AutoImplement, TrustTier::Observed)),
            exec.clone(),
            audit.clone(),
            Arc::new(FakeGate(ApprovalDecision::Approve)),
        );
        let req = ActorRequest {
            session: SessionId::new("s1"),
            verb: McpVerb::OpenReview,
            payload: String::new(),
        };

        let result = svc.run(&req).await.unwrap();
        assert!(result.ok);
        assert!(exec.was_called());
        // Audit trail: Approved, then VerbExecuted.
        assert!(matches!(
            &audit.actions()[..],
            [
                AuditAction::Approved { .. },
                AuditAction::VerbExecuted { .. }
            ]
        ));
    }

    #[tokio::test]
    async fn prompt_verb_is_refused_when_the_operator_denies() {
        let exec = Arc::new(FakeExecutor::ok("should not run"));
        let audit = Arc::new(FakeAudit::default());
        let svc = service_with(
            Some(snapshot(Phase::AutoImplement, TrustTier::Observed)),
            exec.clone(),
            audit.clone(),
            Arc::new(FakeGate(ApprovalDecision::Deny {
                reason: "not now".into(),
            })),
        );
        let req = ActorRequest {
            session: SessionId::new("s1"),
            verb: McpVerb::OpenReview,
            payload: String::new(),
        };

        let result = svc.run(&req).await.unwrap();
        assert!(!result.ok);
        assert!(result.compact_output.contains("not now"));
        assert!(!exec.was_called());
        assert!(matches!(&audit.actions()[..], [AuditAction::Denied { .. }]));
    }

    #[tokio::test]
    async fn frozen_phase_denies_side_effecting_verb() {
        let exec = Arc::new(FakeExecutor::ok("unused"));
        let audit = Arc::new(FakeAudit::default());
        // Plan freezes writes; HttpRequest is Risky (a write) → Deny even when Trusted.
        let svc = service(
            Some(snapshot(Phase::Plan, TrustTier::Trusted)),
            exec.clone(),
            audit.clone(),
        );
        let req = ActorRequest {
            session: SessionId::new("s1"),
            verb: McpVerb::HttpRequest,
            payload: "GET /health".into(),
        };

        let result = svc.run(&req).await.unwrap();
        assert!(!result.ok);
        assert!(result.compact_output.starts_with("denied:"));
        assert!(!exec.was_called());
        assert!(matches!(&audit.actions()[..], [AuditAction::Denied { .. }]));
    }

    #[tokio::test]
    async fn unknown_session_errs() {
        let exec = Arc::new(FakeExecutor::ok("x"));
        let audit = Arc::new(FakeAudit::default());
        let svc = service(None, exec.clone(), audit.clone());
        assert!(matches!(
            svc.run(&run_req()).await,
            Err(ControlError::SessionNotFound(_))
        ));
        assert!(!exec.was_called());
        assert!(audit.actions().is_empty());
    }

    #[tokio::test]
    async fn executor_failure_is_non_ok_and_audited() {
        let exec = Arc::new(FakeExecutor::err(ControlError::Transport("boom".into())));
        let audit = Arc::new(FakeAudit::default());
        let svc = service(
            Some(snapshot(Phase::AutoImplement, TrustTier::ReadOnly)),
            exec.clone(),
            audit.clone(),
        );
        let result = svc.run(&run_req()).await.unwrap();
        assert!(!result.ok);
        assert!(exec.was_called());
        assert!(matches!(
            &audit.actions()[..],
            [AuditAction::VerbExecuted { summary, .. }] if summary.starts_with("failed:")
        ));
    }

    #[test]
    fn compact_output_keeps_signal_lines() {
        let raw = "Compiling foo\nRunning target/debug\n   ok\nerror[E0382]: borrow\ntest result: FAILED. 1 passed; 1 failed";
        let out = compact_output(raw);
        assert!(out.contains("error[E0382]"));
        assert!(out.contains("test result: FAILED"));
        assert!(!out.contains("Compiling foo"));
    }

    #[tokio::test]
    async fn shell_executor_runs_command_in_root() {
        let dir = std::env::temp_dir();
        let exec = ShellVerbExecutor::new(vec!["echo".into(), "hello-from-actor".into()]);
        let out = exec
            .execute(
                &SessionId::new("s1"),
                Some(dir.as_path()),
                McpVerb::RunWithCoverage,
                "",
            )
            .await
            .unwrap();
        assert!(out.contains("hello-from-actor"));

        // A verb the shell executor doesn't implement is reported unsupported.
        let unsupported = exec
            .execute(&SessionId::new("s1"), None, McpVerb::QueryDb, "")
            .await;
        assert!(matches!(unsupported, Err(ControlError::Unsupported(_))));
    }
}
