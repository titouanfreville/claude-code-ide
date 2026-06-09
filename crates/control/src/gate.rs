//! The per-session gate read-model and the tool-gating decision.
//!
//! `GateState` is plain domain data the app folds from the `EventBus` (the bus→gate
//! fold lives in the composition root so this crate stays domain-pure). `decide`
//! is the pure policy: classify the tool, then ask the [`PolicyDecisionPoint`].

use moonlight_domain::ids::SessionId;
use moonlight_domain::phase::Phase;
use moonlight_domain::ports::{PermissionRequest, PolicyDecisionPoint};
use moonlight_domain::trust::{TrustTier, WriteScope};
use serde_json::Value;

use crate::classify::classify;
use crate::ipc::HookResponse;

/// What the control server knows about a session for gating purposes. Mirrors the
/// operator-visible state, kept in sync from engine events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateState {
    pub phase: Phase,
    pub trust: TrustTier,
    /// The operator paused this session (safety halt) — deny everything.
    pub paused: bool,
    /// The operator opted this session into governance. Unadopted sessions are
    /// observe-only and never denied (day-one safety; design §10b).
    pub adopted: bool,
}

impl Default for GateState {
    /// Default-deny posture for a freshly observed session: Plan phase, untrusted,
    /// and **not** adopted (so it is observe-only until the operator opts in).
    fn default() -> Self {
        Self {
            phase: Phase::Plan,
            trust: TrustTier::Observed,
            paused: false,
            adopted: false,
        }
    }
}

/// The Claude Code tool the agent calls to leave plan mode. Its `PreToolUse` is the
/// plan-validation gate: holding it pauses the session on its proposed plan.
pub const EXIT_PLAN_MODE: &str = "ExitPlanMode";

/// What kind of operator approval a held action needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoldKind {
    /// The agent called `ExitPlanMode`: validate its proposed plan. `plan` is the
    /// markdown the agent submitted (pulled from the tool input), if present.
    Plan { plan: Option<String> },
    /// A danger-zone / prompt-required action: `reason` describes what wants doing.
    Danger { reason: String },
}

/// The gate's verdict for a tool call: act immediately, or hold for the operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDecision {
    Allow,
    Deny {
        reason: String,
    },
    /// Pause the session and wait for an operator decision (the keystone).
    Hold(HoldKind),
}

/// Classify a tool call for an adopted session into an immediate verdict or a hold.
///
/// Unadopted (or unknown) sessions always `Allow` — MoonlightCode observes but does
/// not interfere until the operator adopts the session. For adopted sessions:
/// `ExitPlanMode` always holds (plan validation); otherwise the PDP decides, and a
/// `Prompt` outcome becomes a danger-zone hold. Any PDP error fails open (`Allow`).
/// `vouched_safe` marks a tool the operator listed in `safe_tools` (the per-cwd
/// config the server resolves) — classified `Safe` instead of via [`classify`], so
/// a vouched read-only tool survives a frozen phase. The paused gate still wins.
#[allow(clippy::too_many_arguments)]
pub fn evaluate(
    gate: Option<&GateState>,
    session: &SessionId,
    tool_name: &str,
    tool_input: &Value,
    write_scope: Option<WriteScope>,
    vouched_safe: bool,
    pdp: &dyn PolicyDecisionPoint,
) -> GateDecision {
    let Some(gate) = gate.filter(|g| g.adopted) else {
        return GateDecision::Allow; // observe-only
    };
    if gate.paused {
        return GateDecision::Deny {
            reason: "MoonlightCode: session is paused".to_string(),
        };
    }

    // Plan-validation gate: hold the agent on its proposed plan regardless of the
    // PDP (leaving plan mode is itself the decision the operator must make).
    if tool_name == EXIT_PLAN_MODE {
        return GateDecision::Hold(HoldKind::Plan {
            plan: extract_plan(tool_input),
        });
    }

    let danger = if vouched_safe {
        moonlight_domain::trust::DangerClass::Safe
    } else {
        classify(tool_name, tool_input)
    };
    let req = PermissionRequest {
        session: session.clone(),
        phase: gate.phase,
        trust_tier: gate.trust,
        verb: None,
        danger,
        write_scope,
        description: format!("tool `{tool_name}`"),
    };

    use moonlight_domain::trust::PermissionOutcome::{Allow, Deny, Prompt};
    match pdp.decide(&req) {
        Ok(Allow) => GateDecision::Allow,
        Ok(Deny { reason }) => GateDecision::Deny {
            reason: format!("MoonlightCode: {reason}"),
        },
        Ok(Prompt { reason }) => GateDecision::Hold(HoldKind::Danger { reason }),
        Err(_) => GateDecision::Allow, // fail-open
    }
}

/// Pull the proposed plan markdown out of an `ExitPlanMode` tool input
/// (`{ "plan": "…" }`). Returns `None` when absent or empty.
pub fn extract_plan(tool_input: &Value) -> Option<String> {
    let plan = tool_input.get("plan")?.as_str()?;
    (!plan.is_empty()).then(|| plan.to_string())
}

/// Decide whether a tool call may proceed, collapsing a hold to a `Deny` for
/// callers without a synchronous operator. The held-approval path uses
/// [`evaluate`] directly; this preserves the simple sync contract (and the
/// fail-open-on-error posture) for non-holding consumers and tests.
pub fn decide(
    gate: Option<&GateState>,
    session: &SessionId,
    tool_name: &str,
    tool_input: &Value,
    write_scope: Option<WriteScope>,
    pdp: &dyn PolicyDecisionPoint,
) -> HookResponse {
    match evaluate(gate, session, tool_name, tool_input, write_scope, false, pdp) {
        GateDecision::Allow => HookResponse::Allow,
        GateDecision::Deny { reason } => HookResponse::Deny { reason },
        GateDecision::Hold(HoldKind::Plan { .. }) => HookResponse::Deny {
            reason: "MoonlightCode: plan awaiting approval".to_string(),
        },
        GateDecision::Hold(HoldKind::Danger { reason }) => HookResponse::Deny {
            reason: format!("MoonlightCode: {reason} (awaiting approval)"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moonlight_domain::trust::TrustTier;
    use serde_json::json;

    /// A PDP standing in for `DefaultPdp` (kept out of this crate's deps): denies
    /// writes (`Risky`/`DangerZone`) in non-write phases, otherwise allows.
    struct TestPdp;
    impl PolicyDecisionPoint for TestPdp {
        fn decide(
            &self,
            req: &PermissionRequest,
        ) -> Result<moonlight_domain::trust::PermissionOutcome, moonlight_domain::errors::TrustError>
        {
            use moonlight_domain::trust::{DangerClass, PermissionOutcome, WriteScope};
            let is_write = !matches!(req.danger, DangerClass::Safe);
            if is_write {
                // Mirror DefaultPdp: AI-workspace writes survive a frozen phase;
                // project (or unknown-scope) writes do not.
                let permitted = match req.write_scope.unwrap_or(WriteScope::Project) {
                    WriteScope::AiWorkspace => req.phase.allows_ai_workspace_writes(),
                    WriteScope::Project => req.phase.allows_writes(),
                };
                if !permitted {
                    return Ok(PermissionOutcome::Deny {
                        reason: "frozen phase".into(),
                    });
                }
            }
            Ok(PermissionOutcome::Allow)
        }
    }

    fn edit() -> Value {
        json!({ "file_path": "/x" })
    }
    fn sid() -> SessionId {
        SessionId::new("s1")
    }

    #[test]
    fn unadopted_session_is_never_denied() {
        let gate = GateState {
            adopted: false,
            ..Default::default()
        };
        let out = decide(Some(&gate), &sid(), "Edit", &edit(), None, &TestPdp);
        assert_eq!(out, HookResponse::Allow);
    }

    #[test]
    fn unknown_session_is_allowed() {
        let out = decide(None, &sid(), "Edit", &edit(), None, &TestPdp);
        assert_eq!(out, HookResponse::Allow);
    }

    #[test]
    fn adopted_plan_session_denies_writes() {
        let gate = GateState {
            adopted: true,
            phase: Phase::Plan,
            ..Default::default()
        };
        let out = decide(Some(&gate), &sid(), "Edit", &edit(), None, &TestPdp);
        assert!(matches!(out, HookResponse::Deny { .. }));
    }

    #[test]
    fn adopted_plan_session_allows_reads() {
        let gate = GateState {
            adopted: true,
            phase: Phase::Plan,
            ..Default::default()
        };
        let out = decide(Some(&gate), &sid(), "Read", &edit(), None, &TestPdp);
        assert_eq!(out, HookResponse::Allow);
    }

    #[test]
    fn frozen_phase_threads_write_scope_to_pdp() {
        // An adopted Discovery session: a project-scoped write is denied, but an
        // AI-workspace write (the scope the server resolves from the path) is allowed
        // — proving evaluate() carries write_scope into the PDP.
        let gate = GateState {
            adopted: true,
            phase: Phase::Discovery,
            ..Default::default()
        };
        let project = decide(Some(&gate), &sid(), "Edit", &edit(), Some(WriteScope::Project), &TestPdp);
        assert!(matches!(project, HookResponse::Deny { .. }));
        let ai = decide(
            Some(&gate),
            &sid(),
            "Edit",
            &edit(),
            Some(WriteScope::AiWorkspace),
            &TestPdp,
        );
        assert_eq!(ai, HookResponse::Allow);
    }

    #[test]
    fn adopted_auto_session_allows_writes() {
        let gate = GateState {
            adopted: true,
            phase: Phase::AutoImplement,
            trust: TrustTier::Standard,
            ..Default::default()
        };
        let out = decide(Some(&gate), &sid(), "Edit", &edit(), None, &TestPdp);
        assert_eq!(out, HookResponse::Allow);
    }

    #[test]
    fn paused_session_denies_everything() {
        let gate = GateState {
            adopted: true,
            paused: true,
            phase: Phase::AutoImplement,
            ..Default::default()
        };
        let out = decide(Some(&gate), &sid(), "Read", &edit(), None, &TestPdp);
        assert!(matches!(out, HookResponse::Deny { .. }));
    }

    /// A PDP that always wants explicit approval (to exercise the danger hold).
    struct PromptPdp;
    impl PolicyDecisionPoint for PromptPdp {
        fn decide(
            &self,
            _req: &PermissionRequest,
        ) -> Result<moonlight_domain::trust::PermissionOutcome, moonlight_domain::errors::TrustError>
        {
            Ok(moonlight_domain::trust::PermissionOutcome::Prompt {
                reason: "danger zone".into(),
            })
        }
    }

    fn adopted_auto() -> GateState {
        GateState {
            adopted: true,
            phase: Phase::AutoImplement,
            trust: TrustTier::Standard,
            ..Default::default()
        }
    }

    #[test]
    fn exit_plan_mode_holds_for_plan_approval() {
        let input = json!({ "plan": "1. do X\n2. do Y" });
        let out = evaluate(
            Some(&adopted_auto()),
            &sid(),
            EXIT_PLAN_MODE,
            &input,
            None,
            false,
            &TestPdp,
        );
        assert_eq!(
            out,
            GateDecision::Hold(HoldKind::Plan {
                plan: Some("1. do X\n2. do Y".to_string())
            })
        );
    }

    #[test]
    fn exit_plan_mode_unadopted_session_just_allows() {
        let gate = GateState {
            adopted: false,
            ..Default::default()
        };
        let out = evaluate(
            Some(&gate),
            &sid(),
            EXIT_PLAN_MODE,
            &json!({}),
            None,
            false,
            &TestPdp,
        );
        assert_eq!(out, GateDecision::Allow);
    }

    #[test]
    fn prompt_outcome_becomes_a_danger_hold() {
        let out = evaluate(
            Some(&adopted_auto()),
            &sid(),
            "Bash",
            &edit(),
            None,
            false,
            &PromptPdp,
        );
        assert_eq!(
            out,
            GateDecision::Hold(HoldKind::Danger {
                reason: "danger zone".to_string()
            })
        );
    }

    #[test]
    fn plan_extraction_ignores_empty_or_missing() {
        assert_eq!(extract_plan(&json!({ "plan": "x" })), Some("x".to_string()));
        assert_eq!(extract_plan(&json!({ "plan": "" })), None);
        assert_eq!(extract_plan(&json!({})), None);
    }
}
