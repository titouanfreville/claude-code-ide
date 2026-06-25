//! The Policy Decision Point — the single authority for every gated action.
//!
//! Composes three inputs (architecture Decision E): session **phase**,
//! session **trust tier**, and the **danger class** of the action. Danger-zone
//! actions always require explicit human approval, regardless of trust tier.

use moonlight_domain::errors::TrustError;
use moonlight_domain::ports::{PermissionRequest, PolicyDecisionPoint};
use moonlight_domain::trust::{DangerClass, PermissionOutcome, WriteScope};

/// Default composition of phase + trust tier + danger class.
#[derive(Debug, Default, Clone)]
pub struct DefaultPdp;

impl PolicyDecisionPoint for DefaultPdp {
    fn decide(&self, req: &PermissionRequest) -> Result<PermissionOutcome, TrustError> {
        // 1. Danger-zone is non-overridable: always prompt for explicit approval.
        if req.danger == DangerClass::DangerZone {
            return Ok(PermissionOutcome::Prompt {
                reason: format!("danger-zone action requires approval: {}", req.description),
            });
        }

        // 1b. Control-plane verbs (a session asking to change its workflow phase) are
        //     not file writes — they move the gate this very PDP enforces — so the
        //     frozen-phase project freeze (step 2) must NOT deny them, or a session
        //     could never request its way out of Discovery / Plan / Commit. They always
        //     route to operator approval instead: the agent may *ask*, never decide.
        //     (A future per-session "auto-phasing" opt-in would relax this to Allow; the
        //     branch is the seam.)
        if req.verb.map(|v| v.is_phase_control()).unwrap_or(false) {
            return Ok(PermissionOutcome::Prompt {
                reason: format!("phase change requires operator approval: {}", req.description),
            });
        }

        // 2. Phase gate: a frozen phase freezes *project* state, but an AI-workspace
        //    write (plan docs, BMad entries, handoffs) is permitted in every phase
        //    except the fully-frozen Commit gate. The denial reason is injected back
        //    to the session as feedback.
        let is_write = matches!(req.danger, DangerClass::Risky)
            || req.verb.map(|v| !v.is_read_only()).unwrap_or(false);
        if is_write {
            // Unknown scope (a non-path write, e.g. most Bash) is treated as Project —
            // the conservative default, so an unattributable write can't slip past the
            // project freeze.
            let scope = req.write_scope.unwrap_or(WriteScope::Project);
            let permitted = match scope {
                WriteScope::AiWorkspace => req.phase.allows_ai_workspace_writes(),
                WriteScope::Project => req.phase.allows_writes(),
            };
            if !permitted {
                // An external MCP tool the freeze would block is offered to the operator
                // for ad-hoc authorization (once / always) rather than hard-denied — a
                // read/query against an external service is the kind of thing an operator
                // may well want to permit while exploring. Project file edits and Bash
                // leave the flag false and fall through to the hard deny below.
                if scope == WriteScope::Project && req.prompt_on_project_freeze {
                    return Ok(PermissionOutcome::Prompt {
                        reason: format!(
                            "phase {} freezes project state; authorize external tool? {}",
                            req.phase.label(),
                            req.description
                        ),
                    });
                }
                // Name the phase *and* the remedy: the denial is the moment the agent
                // most needs to know how to get unblocked (request a phase change), so
                // it doesn't just retry the same write.
                let (what, remedy) = match scope {
                    WriteScope::AiWorkspace => (
                        "is fully frozen (commit gate)",
                        "the commit gate freezes every write — ask the operator, or call \
                         request_phase to move off Commit",
                    ),
                    WriteScope::Project => (
                        "freezes project files",
                        "call request_phase('auto') to ask the operator for write access \
                         (or phase_status to confirm where you are)",
                    ),
                };
                return Ok(PermissionOutcome::Deny {
                    reason: format!(
                        "phase {} {}; cannot perform: {} — {}",
                        req.phase.label(),
                        what,
                        req.description,
                        remedy
                    ),
                });
            }
        }

        // 3. Trust tier: a verb may run autonomously only at/above its minimum tier.
        if let Some(verb) = req.verb {
            if req.trust_tier < verb.min_autonomous_tier() {
                return Ok(PermissionOutcome::Prompt {
                    reason: format!("trust tier too low for {:?}", verb),
                });
            }
        }

        Ok(PermissionOutcome::Allow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moonlight_domain::ids::SessionId;
    use moonlight_domain::phase::Phase;
    use moonlight_domain::trust::{McpVerb, TrustTier};

    fn req(
        phase: Phase,
        tier: TrustTier,
        verb: Option<McpVerb>,
        danger: DangerClass,
    ) -> PermissionRequest {
        req_scoped(phase, tier, verb, danger, None)
    }

    fn req_scoped(
        phase: Phase,
        tier: TrustTier,
        verb: Option<McpVerb>,
        danger: DangerClass,
        write_scope: Option<WriteScope>,
    ) -> PermissionRequest {
        PermissionRequest {
            session: SessionId::new("s1"),
            phase,
            trust_tier: tier,
            verb,
            danger,
            write_scope,
            prompt_on_project_freeze: false,
            description: "test action".into(),
        }
    }

    /// A project-scoped write request flagged to prompt-on-freeze (an external MCP tool).
    fn mcp_prompt_req(phase: Phase) -> PermissionRequest {
        PermissionRequest {
            prompt_on_project_freeze: true,
            ..req_scoped(
                phase,
                TrustTier::Trusted,
                None,
                DangerClass::Risky,
                Some(WriteScope::Project),
            )
        }
    }

    #[test]
    fn danger_zone_always_prompts_even_when_trusted() {
        let pdp = DefaultPdp;
        let out = pdp
            .decide(&req(
                Phase::AutoImplement,
                TrustTier::Trusted,
                None,
                DangerClass::DangerZone,
            ))
            .unwrap();
        assert!(matches!(out, PermissionOutcome::Prompt { .. }));
    }

    #[test]
    fn plan_phase_denies_writes() {
        let pdp = DefaultPdp;
        let out = pdp
            .decide(&req(
                Phase::Plan,
                TrustTier::Trusted,
                None,
                DangerClass::Risky,
            ))
            .unwrap();
        assert!(matches!(out, PermissionOutcome::Deny { .. }));
    }

    #[test]
    fn discovery_denies_edits_but_allows_reads_and_commands() {
        let pdp = DefaultPdp;
        // Discovery = look around freely (CC `auto`) but never touch files.
        let deny = pdp
            .decide(&req(Phase::Discovery, TrustTier::Trusted, None, DangerClass::Risky))
            .unwrap();
        assert!(matches!(deny, PermissionOutcome::Deny { .. }));
        let allow = pdp
            .decide(&req(Phase::Discovery, TrustTier::Trusted, None, DangerClass::Safe))
            .unwrap();
        assert_eq!(allow, PermissionOutcome::Allow);
    }

    #[test]
    fn review_phase_allows_writes() {
        let pdp = DefaultPdp;
        // Review now permits edits (e.g. addressing review feedback).
        let out = pdp
            .decide(&req(Phase::Review, TrustTier::Trusted, None, DangerClass::Risky))
            .unwrap();
        assert_eq!(out, PermissionOutcome::Allow);
    }

    #[test]
    fn commit_gate_is_read_only() {
        let pdp = DefaultPdp;
        let out = pdp
            .decide(&req(Phase::Commit, TrustTier::Trusted, None, DangerClass::Risky))
            .unwrap();
        assert!(matches!(out, PermissionOutcome::Deny { .. }));
    }

    #[test]
    fn frozen_phases_allow_ai_workspace_writes_but_deny_project_writes() {
        let pdp = DefaultPdp;
        // Discovery & Plan freeze project files...
        for phase in [Phase::Discovery, Phase::Plan] {
            let project = pdp
                .decide(&req_scoped(
                    phase,
                    TrustTier::Trusted,
                    None,
                    DangerClass::Risky,
                    Some(WriteScope::Project),
                ))
                .unwrap();
            assert!(matches!(project, PermissionOutcome::Deny { .. }), "{phase:?} project");
            // ...but still let the agent write its own plans / BMad entries / handoffs.
            let ai = pdp
                .decide(&req_scoped(
                    phase,
                    TrustTier::Trusted,
                    None,
                    DangerClass::Risky,
                    Some(WriteScope::AiWorkspace),
                ))
                .unwrap();
            assert_eq!(ai, PermissionOutcome::Allow, "{phase:?} ai-workspace");
        }
    }

    #[test]
    fn commit_gate_freezes_even_ai_workspace_writes() {
        let pdp = DefaultPdp;
        // The commit gate is fully frozen — AI-workspace writes are denied here too.
        let out = pdp
            .decide(&req_scoped(
                Phase::Commit,
                TrustTier::Trusted,
                None,
                DangerClass::Risky,
                Some(WriteScope::AiWorkspace),
            ))
            .unwrap();
        assert!(matches!(out, PermissionOutcome::Deny { .. }));
    }

    #[test]
    fn external_mcp_prompts_instead_of_denying_in_frozen_phases() {
        let pdp = DefaultPdp;
        // An external MCP tool the freeze would block is offered to the operator
        // (once / always) rather than hard-denied — in every frozen phase.
        for phase in [Phase::Discovery, Phase::Plan, Phase::Commit] {
            let out = pdp.decide(&mcp_prompt_req(phase)).unwrap();
            assert!(
                matches!(out, PermissionOutcome::Prompt { .. }),
                "{phase:?} should prompt for an external MCP tool, got {out:?}"
            );
        }
    }

    #[test]
    fn prompt_on_freeze_flag_does_not_relax_write_phases() {
        let pdp = DefaultPdp;
        // In a write phase the flag is moot — the write is permitted outright, no prompt.
        let out = pdp.decide(&mcp_prompt_req(Phase::AutoImplement)).unwrap();
        assert_eq!(out, PermissionOutcome::Allow);
    }

    #[test]
    fn prompt_on_freeze_flag_does_not_affect_unflagged_writes() {
        let pdp = DefaultPdp;
        // A project write WITHOUT the flag (Edit/Write/Bash) still hard-denies in a
        // frozen phase — the freeze on the product is absolute.
        let out = pdp
            .decide(&req_scoped(
                Phase::Discovery,
                TrustTier::Trusted,
                None,
                DangerClass::Risky,
                Some(WriteScope::Project),
            ))
            .unwrap();
        assert!(matches!(out, PermissionOutcome::Deny { .. }));
    }

    #[test]
    fn unknown_scope_is_treated_as_project_in_frozen_phase() {
        let pdp = DefaultPdp;
        // A write with no attributable path (e.g. Bash) must not slip past the freeze.
        let out = pdp
            .decide(&req_scoped(
                Phase::Discovery,
                TrustTier::Trusted,
                None,
                DangerClass::Risky,
                None,
            ))
            .unwrap();
        assert!(matches!(out, PermissionOutcome::Deny { .. }));
    }

    #[test]
    fn readonly_verb_allowed_at_sufficient_tier() {
        let pdp = DefaultPdp;
        let out = pdp
            .decide(&req(
                Phase::AutoImplement,
                TrustTier::ReadOnly,
                Some(McpVerb::RunWithCoverage),
                DangerClass::Safe,
            ))
            .unwrap();
        assert_eq!(out, PermissionOutcome::Allow);
    }

    #[test]
    fn request_phase_prompts_in_every_phase_even_frozen_ones() {
        let pdp = DefaultPdp;
        // The whole point: a session must be able to *ask* to leave a frozen phase, so
        // the project-write freeze must not deny the request — it prompts the operator.
        for phase in Phase::ALL {
            let out = pdp
                .decide(&req(
                    phase,
                    TrustTier::Observed,
                    Some(McpVerb::RequestPhase),
                    DangerClass::Safe,
                ))
                .unwrap();
            assert!(
                matches!(out, PermissionOutcome::Prompt { .. }),
                "{phase:?} should prompt for a phase request, got {out:?}"
            );
        }
    }

    #[test]
    fn request_phase_prompts_regardless_of_trust_tier() {
        let pdp = DefaultPdp;
        // Even a Trusted session may not change phase autonomously today — it always
        // asks (the "should not do so alone" rule). Tier never short-circuits this.
        for tier in [
            TrustTier::Observed,
            TrustTier::ReadOnly,
            TrustTier::Standard,
            TrustTier::Trusted,
        ] {
            let out = pdp
                .decide(&req(
                    Phase::AutoImplement,
                    tier,
                    Some(McpVerb::RequestPhase),
                    DangerClass::Safe,
                ))
                .unwrap();
            assert!(
                matches!(out, PermissionOutcome::Prompt { .. }),
                "{tier:?} should still prompt for a phase request, got {out:?}"
            );
        }
    }

    #[test]
    fn low_tier_prompts_for_verb() {
        let pdp = DefaultPdp;
        let out = pdp
            .decide(&req(
                Phase::AutoImplement,
                TrustTier::Observed,
                Some(McpVerb::OpenReview),
                DangerClass::Safe,
            ))
            .unwrap();
        assert!(matches!(out, PermissionOutcome::Prompt { .. }));
    }
}
