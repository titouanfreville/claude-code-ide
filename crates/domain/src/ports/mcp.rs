//! Embedded MCP actor port + the single Policy Decision Point.
//!
//! Mandatory verb shape (architecture): resolve → PDP → execute → audit → compact result.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::audit::AuditAction;
use crate::errors::{ControlError, TrustError};
use crate::ids::SessionId;
use crate::phase::Phase;
use crate::trust::{DangerClass, McpVerb, PermissionOutcome, TrustTier, WriteScope};

/// A request to run an actor verb on behalf of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorRequest {
    pub session: SessionId,
    pub verb: McpVerb,
    /// Verb-specific payload (e.g. the SQL, the HTTP request, the test target).
    pub payload: String,
}

/// The compact (RTK-shaped) result returned to the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorResult {
    pub ok: bool,
    pub compact_output: String,
}

/// Executes actor verbs. Every implementation MUST route through the
/// [`PolicyDecisionPoint`] and append to the audit log before returning.
#[async_trait]
pub trait McpActor: Send + Sync {
    async fn run(&self, req: &ActorRequest) -> Result<ActorResult, ControlError>;
}

/// Inputs the PDP composes to reach a verdict (phase + tier + danger class).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRequest {
    pub session: SessionId,
    pub phase: Phase,
    pub trust_tier: TrustTier,
    pub verb: Option<McpVerb>,
    pub danger: DangerClass,
    /// For a file-writing tool, which part of the tree it touches (`AiWorkspace` vs
    /// `Project`). `None` when the action isn't a path-attributable file write (reads,
    /// most Bash commands) — the PDP then treats any write as `Project` (conservative).
    pub write_scope: Option<WriteScope>,
    /// When a frozen phase would deny this as a project write, prompt the operator for
    /// approval instead of denying outright. Set for external MCP tool calls
    /// (`mcp__*`, excluding MoonlightCode's own verbs): a frozen phase should let the
    /// operator authorize an external read/query ad hoc (once / always) rather than
    /// hard-refusing it. Project file edits and Bash leave this `false` — the product
    /// freeze on them is absolute.
    pub prompt_on_project_freeze: bool,
    /// Human-readable description of the action (for prompts / audit).
    pub description: String,
}

/// The single authority for every gated/autonomous action. Pure and synchronous:
/// no scattered permission checks anywhere else in the system.
pub trait PolicyDecisionPoint: Send + Sync {
    fn decide(&self, req: &PermissionRequest) -> Result<PermissionOutcome, TrustError>;
}

/// The policy-relevant snapshot of a session, resolved by the actor before it asks
/// the PDP: the current workflow `phase`, the `trust_tier`, and the repo `root` a
/// verb runs in. Read from the engine's live read-model at call time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPolicySnapshot {
    pub phase: Phase,
    pub trust_tier: TrustTier,
    pub root: Option<PathBuf>,
}

/// Resolves a session's current [`SessionPolicySnapshot`] for the actor. `None` when
/// the session isn't tracked (the actor then refuses the verb).
pub trait SessionPolicyView: Send + Sync {
    fn snapshot(&self, session: &SessionId) -> Option<SessionPolicySnapshot>;
}

/// Runs an approved verb and returns its raw (uncompacted) output. Side-effecting
/// adapter (shells a test runner, opens a DB, fires HTTP); the actor compacts the
/// result and audits it. `root` is the session's repo working directory.
#[async_trait]
pub trait VerbExecutor: Send + Sync {
    async fn execute(
        &self,
        session: &SessionId,
        root: Option<&Path>,
        verb: McpVerb,
        payload: &str,
    ) -> Result<String, ControlError>;
}

/// Append-only audit sink for autonomous actions (FR33). Infallible from the
/// actor's view — a failed write is the adapter's problem to log, never a reason to
/// fail the verb — mirroring the supervisor's best-effort audit.
pub trait AuditSink: Send + Sync {
    fn record(&self, session: &SessionId, action: AuditAction, revertible: bool);
}

/// The operator's verdict on a held actor verb (the MCP analogue of the hook
/// keystone's decision, kept in the domain so the actor stays adapter-free).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approve,
    Deny { reason: String },
}

/// Holds a verb the PDP said needs approval (`Prompt`) until the operator decides,
/// or a timeout denies. The concrete adapter bridges to the cockpit's approval
/// registry (the same keystone the plan/danger holds use); a no-op impl that always
/// denies preserves the default-deny posture where no channel exists.
#[async_trait]
pub trait ApprovalGate: Send + Sync {
    async fn request(&self, session: &SessionId, what: &str) -> ApprovalDecision;
}
