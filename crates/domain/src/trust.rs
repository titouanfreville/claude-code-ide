//! Trust tiers, MCP actor verbs, danger classification, and permission outcomes.

use serde::{Deserialize, Serialize};

/// Graduated trust governing which actor verbs a session may invoke autonomously.
/// Default-deny: higher tiers permit more without prompting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TrustTier {
    /// Nothing autonomous — every action prompts.
    Observed,
    /// Read-only verbs autonomous (run tests, query DB read, HTTP GET).
    ReadOnly,
    /// Read + low-risk writes autonomous; risky actions still prompt.
    Standard,
    /// Broad autonomy; only danger-zone actions prompt.
    Trusted,
}

/// The actor verbs the embedded MCP server exposes (MVP set; `StartDebug` is Phase 2).
/// The `Run*` family drives the IDE's **Run console** (the JetBrains-style Run tool
/// window): start/stop the shared run, read its captured logs/status, list the
/// project's detected run targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum McpVerb {
    RunWithCoverage,
    QueryDb,
    HttpRequest,
    OpenReview,
    StartDebug,
    RunStart,
    RunStop,
    RunStatus,
    RunLogs,
    RunListTargets,
    /// **Control-plane:** request a workflow-[`phase`](crate::phase) change (advance to
    /// the next phase, or jump to a named one). Unlike every other verb it does not act
    /// on the project tree or a runner — it asks the engine to move the gate the PDP
    /// itself enforces. The PDP therefore treats it specially (always routes to operator
    /// approval, bypassing the project-write freeze so a session can escape a frozen
    /// phase) — see [`McpVerb::is_phase_control`] and the PDP's control-plane branch.
    RequestPhase,
    /// **Self-report:** the agent flags that it is blocked / stuck and needs the
    /// operator (raising the session's `Stuck` attention signal in the cockpit). A
    /// pure status signal with no project side effect — treated as a read so it passes
    /// every phase gate and runs autonomously at any tier (a stuck agent must always be
    /// able to call for help, in any phase).
    ReportBlocked,
    /// **Read-only orientation:** report the session's *current* workflow phase and what
    /// it allows (project writes? AI-workspace writes? which CC mode), plus what `next`
    /// would advance to. A pure read with no side effect — passes every phase gate and
    /// runs at any tier — so a session can always discover where it is before deciding
    /// whether to [`RequestPhase`](McpVerb::RequestPhase). The fix for phase *mismatch*:
    /// the agent never has to guess its phase.
    PhaseStatus,
}

impl McpVerb {
    /// Minimum trust tier at which this verb may run without an explicit prompt.
    pub fn min_autonomous_tier(self) -> TrustTier {
        match self {
            McpVerb::RunWithCoverage | McpVerb::HttpRequest => TrustTier::ReadOnly,
            McpVerb::QueryDb => TrustTier::ReadOnly,
            // Reading the Run console (status/logs/targets) is as safe as any read.
            McpVerb::RunStatus | McpVerb::RunLogs | McpVerb::RunListTargets => TrustTier::ReadOnly,
            McpVerb::OpenReview => TrustTier::Standard,
            // Launching/killing the project's run has side effects — Standard, like
            // other low-risk writes.
            McpVerb::RunStart | McpVerb::RunStop => TrustTier::Standard,
            McpVerb::StartDebug => TrustTier::Trusted,
            // Moving the workflow gate is the highest-authority verb; in practice the
            // PDP's control-plane branch always prompts the operator first, so this
            // tier floor is a belt-and-braces default rather than an autonomy grant.
            McpVerb::RequestPhase => TrustTier::Trusted,
            // A cry for help must never be gated — any session, any tier, can self-report.
            McpVerb::ReportBlocked => TrustTier::Observed,
            // Orientation must never be gated — any session, any tier, can ask where it is.
            McpVerb::PhaseStatus => TrustTier::Observed,
        }
    }

    /// Whether this verb is a **control-plane** action — it changes MoonlightCode's own
    /// workflow state (the phase the PDP gates on) rather than acting on the project or
    /// a runner. Control-plane verbs are not file writes, so the frozen-phase project
    /// freeze must not deny them (a session has to be able to *ask* to leave Discovery /
    /// Plan / Commit); the PDP routes them straight to operator approval instead.
    pub fn is_phase_control(self) -> bool {
        matches!(self, McpVerb::RequestPhase)
    }

    /// Whether the verb only **reads** IDE/project state (no side effects). Read
    /// verbs pass the frozen-phase project gate; everything else counts as a write.
    pub fn is_read_only(self) -> bool {
        matches!(
            self,
            McpVerb::RunWithCoverage
                | McpVerb::QueryDb
                | McpVerb::HttpRequest
                | McpVerb::RunStatus
                | McpVerb::RunLogs
                | McpVerb::RunListTargets
                // Not literally a read, but a no-project-side-effect status signal that
                // must pass the frozen-phase gate (a stuck agent reports in any phase).
                | McpVerb::ReportBlocked
                // A pure read of the session's own phase — passes the frozen-phase gate.
                | McpVerb::PhaseStatus
        )
    }
}

/// How dangerous an action is. `DangerZone` always requires explicit human
/// approval regardless of trust tier (non-overridable).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DangerClass {
    Safe,
    Risky,
    DangerZone,
}

/// Which part of the working tree a write touches. Frozen phases (Discovery, Plan,
/// Commit) freeze **project** state — source, configs, committed deliverables — so a
/// session can't change the product while looking around or planning. They still let
/// the agent write its own **AI-workspace** scratch (plan docs, BMad entries,
/// handoffs), because that never alters project state. The Commit gate is the one
/// exception: it freezes *everything* (see [`crate::phase::Phase::allows_ai_workspace_writes`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WriteScope {
    /// AI-owned scratch/planning area (e.g. `.ai/`, `.bmad-output/`). Writable in
    /// every phase except the final Commit gate.
    AiWorkspace,
    /// Project-essential file (source, config, committed docs). Writable only in the
    /// write phases (Auto/Test/Review).
    Project,
}

/// The verdict of the Policy Decision Point for a single requested action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionOutcome {
    /// Allowed to proceed autonomously.
    Allow,
    /// Requires explicit operator approval before proceeding.
    Prompt { reason: String },
    /// Denied outright; `reason` is injected back to the session as feedback.
    Deny { reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_phase_is_control_plane_and_not_a_read() {
        // Control-plane: the PDP must route it to approval, not the write freeze.
        assert!(McpVerb::RequestPhase.is_phase_control());
        // It mutates workflow state, so it is not a read verb...
        assert!(!McpVerb::RequestPhase.is_read_only());
        // ...and no other verb claims to be phase-control.
        for v in [
            McpVerb::RunWithCoverage,
            McpVerb::QueryDb,
            McpVerb::HttpRequest,
            McpVerb::OpenReview,
            McpVerb::StartDebug,
            McpVerb::RunStart,
            McpVerb::RunStop,
            McpVerb::RunStatus,
            McpVerb::RunLogs,
            McpVerb::RunListTargets,
        ] {
            assert!(!v.is_phase_control(), "{v:?}");
        }
    }
}
