//! The [`VerbExecutor`] behind the MCP **`request_phase`** verb — the bridge that lets
//! a CC session *ask* to move its workflow phase (Discovery → Plan → Auto → Test →
//! Review → Commit), the gate the PDP enforces.
//!
//! Composition: wraps the rest of the executor stack (run verbs, `run_with_coverage`)
//! and handles only `RequestPhase` itself, delegating everything else through. Policy
//! and approval live a layer up in `ActorService` + the PDP: by the time `execute`
//! runs the verb here, the PDP returned `Prompt` and the operator **approved** it in
//! the cockpit (the control-plane verb never reaches `Allow` on its own — see
//! [`McpVerb::is_phase_control`](moonlight_domain::trust::McpVerb::is_phase_control)).
//! So this is pure execution: translate the approved request into the operator-grade
//! engine [`Command`] (`SetPhase` for a named phase, `AdvancePhase` for "next") and
//! hand it to the engine loop on the shared command channel.
//!
//! **Containment:** the only effect is a phase transition the operator already
//! approved; an unparseable target is refused with the valid set (never a silent
//! no-op), and a session is never moved without going through the same channel the
//! operator's own phase controls use.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc::UnboundedSender;

use moonlight_domain::errors::ControlError;
use moonlight_domain::ids::SessionId;
use moonlight_domain::phase::Phase;
use moonlight_domain::ports::mcp::{SessionPolicyView, VerbExecutor};
use moonlight_domain::session::AttentionKind;
use moonlight_domain::trust::McpVerb;
use moonlight_engine::Command;

/// Cap on the self-reported reason echoed back / audited.
const REASON_CLIP: usize = 120;

/// Executes the **engine-command verbs** — `request_phase` (move the workflow gate) and
/// `report_blocked` (raise the session's stuck signal) — by emitting the matching engine
/// [`Command`] on the shared operator command channel; delegates every other verb to
/// `inner`. Both verbs reach the engine the same way the operator's own cockpit controls
/// do, so there is one path for state changes.
pub struct PhaseVerbExecutor {
    /// The engine command channel (the same one the operator's cockpit controls use).
    commands: UnboundedSender<Command>,
    /// Read side of the same policy view the PDP gates against — the authoritative
    /// per-session phase. Used to ground every verb response in the *current* phase so
    /// a session can't drift out of sync with the operator (who may have moved the
    /// phase without the agent asking).
    policy: Arc<dyn SessionPolicyView>,
    inner: Arc<dyn VerbExecutor>,
}

impl PhaseVerbExecutor {
    pub fn new(
        commands: UnboundedSender<Command>,
        policy: Arc<dyn SessionPolicyView>,
        inner: Arc<dyn VerbExecutor>,
    ) -> Self {
        Self {
            commands,
            policy,
            inner,
        }
    }

    /// The session's authoritative current phase per the policy view (what the PDP
    /// gates on), or `None` if the session isn't tracked yet.
    fn current_phase(&self, session: &SessionId) -> Option<Phase> {
        self.policy.snapshot(session).map(|s| s.phase)
    }

    /// A one-line phase footer appended to verb responses so every tool result
    /// re-grounds the agent in the *actual* current phase + its write policy — the fix
    /// for "the agent thinks it's in one phase while really in another". Empty when the
    /// session isn't tracked (nothing authoritative to report).
    fn phase_footer(&self, session: &SessionId) -> String {
        match self.current_phase(session) {
            Some(phase) => {
                let edits = if phase.allows_writes() {
                    "allowed"
                } else {
                    "denied"
                };
                format!(
                    "\n\n— MoonlightCode: you are in the **{}** phase ({}); project-file edits are {} here.",
                    phase.label(),
                    phase.mode_label(),
                    edits
                )
            }
            None => String::new(),
        }
    }

    /// Append the current-phase footer to a successful verb response.
    fn with_footer(&self, session: &SessionId, msg: String) -> String {
        format!("{msg}{}", self.phase_footer(session))
    }

    /// The set of phase tokens the agent may name (for refusal messages).
    fn valid_targets() -> String {
        let labels: Vec<&str> = Phase::ALL.iter().map(|p| p.label()).collect();
        format!("{}, or `next`", labels.join(", "))
    }

    /// Translate an approved `request_phase` payload into an engine command and send it.
    /// Empty / `next` → advance one step; a phase token → jump to it; anything else is
    /// refused with the valid set.
    fn request_phase(&self, session: &SessionId, payload: &str) -> Result<String, ControlError> {
        let wanted = payload.trim();
        let current = self.current_phase(session);
        // Reaching this verb means the operator already approved (control-plane verbs
        // never `Allow` on their own), so the change is applied — report the *resulting*
        // phase, computed deterministically, rather than echoing back the request.
        let (command, resulting) = if wanted.is_empty() || wanted.eq_ignore_ascii_case("next") {
            (
                Command::AdvancePhase {
                    session: session.clone(),
                },
                current.map(Phase::next),
            )
        } else if let Some(phase) = Phase::from_token(wanted) {
            (
                Command::SetPhase {
                    session: session.clone(),
                    phase,
                },
                Some(phase),
            )
        } else {
            return Err(ControlError::Unsupported(format!(
                "unknown phase `{wanted}` — valid targets: {}",
                Self::valid_targets()
            )));
        };
        self.commands
            .send(command)
            .map_err(|e| ControlError::Transport(format!("engine command channel closed: {e}")))?;
        // Spell out from→to so the agent's mental model snaps to the new phase. (Footer
        // is intentionally *not* appended here: the policy view lags the just-sent
        // command by a bus hop, so it would still read the old phase and contradict
        // this message — subsequent verbs carry the footer once the fold catches up.)
        Ok(match (current, resulting) {
            (Some(from), Some(to)) if from == to => {
                format!("Already in the {} phase — no change.", to.label())
            }
            (Some(from), Some(to)) => format!(
                "Phase change approved: {} → {} (now active). Project-file edits are {} in {}.",
                from.label(),
                to.label(),
                if to.allows_writes() { "allowed" } else { "denied" },
                to.label(),
            ),
            (None, Some(to)) => format!("Phase change approved: now in the {} phase.", to.label()),
            _ => "Advancing to the next workflow phase.".to_string(),
        })
    }

    /// Raise the session's `Stuck` attention signal (the agent self-reporting it is
    /// blocked). `payload` is the optional reason — echoed back + audited (the alert
    /// itself carries no free text). Always succeeds (a cry for help is never gated).
    fn report_blocked(&self, session: &SessionId, payload: &str) -> Result<String, ControlError> {
        self.commands
            .send(Command::FlagSession {
                session: session.clone(),
                alert: Some(AttentionKind::Stuck),
            })
            .map_err(|e| ControlError::Transport(format!("engine command channel closed: {e}")))?;
        let reason = payload.trim();
        Ok(if reason.is_empty() {
            "flagged as blocked — the operator has been signalled".to_string()
        } else {
            format!(
                "flagged as blocked — the operator has been signalled: {}",
                clip(reason, REASON_CLIP)
            )
        })
    }

    /// Report the session's current phase and exactly what it permits — the agent's
    /// read-only orientation verb (no side effect, no approval). The fix for phase
    /// *mismatch*: a session can discover precisely where it is, what it may do, and
    /// where `next` leads *before* deciding whether to request a change, instead of
    /// guessing and asking for a phase it is already in.
    fn phase_status(&self, session: &SessionId) -> Result<String, ControlError> {
        let yn = |b: bool| if b { "allowed" } else { "denied" };
        Ok(match self.current_phase(session) {
            Some(phase) => format!(
                "You are in the {} phase ({}). Project-file edits: {}. AI-workspace notes (.ai/): {}. \
                 `next` would advance to {}. To move, call request_phase with one of: {} \
                 (every change is operator-approved).",
                phase.label(),
                phase.mode_label(),
                yn(phase.allows_writes()),
                yn(phase.allows_ai_workspace_writes()),
                phase.next().label(),
                Self::valid_targets(),
            ),
            None => format!(
                "This session's phase is not tracked yet. The workflow phases are: {}.",
                Self::valid_targets()
            ),
        })
    }
}

/// Truncate `s` to at most `max` chars with an ellipsis (keeps the audit line bounded).
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let head: String = s.chars().take(max).collect();
        format!("{head}…")
    } else {
        s.to_string()
    }
}

#[async_trait]
impl VerbExecutor for PhaseVerbExecutor {
    async fn execute(
        &self,
        session: &SessionId,
        root: Option<&Path>,
        verb: McpVerb,
        payload: &str,
    ) -> Result<String, ControlError> {
        match verb {
            // request_phase builds its own from→to message (see the note there on why
            // the lagging footer is omitted).
            McpVerb::RequestPhase => self.request_phase(session, payload),
            // Every other verb gets the current-phase footer so each tool result
            // re-grounds the agent in the real phase + write policy.
            McpVerb::ReportBlocked => self
                .report_blocked(session, payload)
                .map(|m| self.with_footer(session, m)),
            // phase_status *is* the phase report — no footer (it would duplicate).
            McpVerb::PhaseStatus => self.phase_status(session),
            other => self
                .inner
                .execute(session, root, other, payload)
                .await
                .map(|m| self.with_footer(session, m)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tokio::sync::mpsc;

    use moonlight_domain::ports::mcp::SessionPolicySnapshot;

    /// Policy view that tracks no session — `current_phase` is always `None`, so the
    /// phase footer is empty (keeps the command-emission assertions focused).
    struct NoPolicy;
    impl SessionPolicyView for NoPolicy {
        fn snapshot(&self, _session: &SessionId) -> Option<SessionPolicySnapshot> {
            None
        }
    }

    /// Policy view pinned to a single phase for footer/`request_phase` assertions.
    struct FixedPolicy(Phase);
    impl SessionPolicyView for FixedPolicy {
        fn snapshot(&self, _session: &SessionId) -> Option<SessionPolicySnapshot> {
            Some(SessionPolicySnapshot {
                phase: self.0,
                trust_tier: moonlight_domain::trust::TrustTier::Observed,
                root: None,
            })
        }
    }

    /// Inner executor that records delegation (non-phase verbs must fall through).
    struct EchoInner;
    #[async_trait]
    impl VerbExecutor for EchoInner {
        async fn execute(
            &self,
            _session: &SessionId,
            _root: Option<&Path>,
            verb: McpVerb,
            _payload: &str,
        ) -> Result<String, ControlError> {
            Ok(format!("inner:{verb:?}"))
        }
    }

    fn run(
        ex: &PhaseVerbExecutor,
        verb: McpVerb,
        payload: &str,
    ) -> Result<String, ControlError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(ex.execute(&SessionId::new("s1"), None, verb, payload))
    }

    #[test]
    fn next_and_empty_emit_advance_phase() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let ex = PhaseVerbExecutor::new(tx, Arc::new(NoPolicy), Arc::new(EchoInner));

        let out = run(&ex, McpVerb::RequestPhase, "").unwrap();
        assert!(out.contains("next"), "{out}");
        assert!(matches!(
            rx.try_recv().unwrap(),
            Command::AdvancePhase { session } if session == SessionId::new("s1")
        ));

        run(&ex, McpVerb::RequestPhase, "NEXT").unwrap();
        assert!(matches!(rx.try_recv().unwrap(), Command::AdvancePhase { .. }));
    }

    #[test]
    fn named_phase_emits_set_phase() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let ex = PhaseVerbExecutor::new(tx, Arc::new(NoPolicy), Arc::new(EchoInner));

        let out = run(&ex, McpVerb::RequestPhase, "auto").unwrap();
        assert!(out.contains("Auto"), "{out}");
        assert!(matches!(
            rx.try_recv().unwrap(),
            Command::SetPhase { phase: Phase::AutoImplement, session } if session == SessionId::new("s1")
        ));
    }

    #[test]
    fn unknown_phase_is_refused_with_the_valid_set_and_emits_nothing() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let ex = PhaseVerbExecutor::new(tx, Arc::new(NoPolicy), Arc::new(EchoInner));

        let err = run(&ex, McpVerb::RequestPhase, "ludicrous-speed").unwrap_err();
        assert!(err.to_string().contains("unknown phase"), "{err}");
        // A refusal must not move the session.
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn report_blocked_emits_a_stuck_flag_and_echoes_the_reason() {
        use moonlight_domain::session::AttentionKind;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let ex = PhaseVerbExecutor::new(tx, Arc::new(NoPolicy), Arc::new(EchoInner));

        let out = run(&ex, McpVerb::ReportBlocked, "waiting on a missing API key").unwrap();
        assert!(out.contains("missing API key"), "{out}");
        assert!(matches!(
            rx.try_recv().unwrap(),
            Command::FlagSession { alert: Some(AttentionKind::Stuck), session } if session == SessionId::new("s1")
        ));
    }

    #[test]
    fn other_verbs_delegate_to_the_inner_executor() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let ex = PhaseVerbExecutor::new(tx, Arc::new(NoPolicy), Arc::new(EchoInner));
        let out = run(&ex, McpVerb::RunWithCoverage, "").unwrap();
        assert_eq!(out, "inner:RunWithCoverage");
    }

    #[test]
    fn delegated_verbs_carry_the_current_phase_footer() {
        let (tx, _rx) = mpsc::unbounded_channel();
        // A tracked session in a read-only phase: every verb result must re-ground the
        // agent in the real phase + write policy.
        let ex = PhaseVerbExecutor::new(
            tx,
            Arc::new(FixedPolicy(Phase::Discovery)),
            Arc::new(EchoInner),
        );
        let out = run(&ex, McpVerb::RunWithCoverage, "").unwrap();
        assert!(out.starts_with("inner:RunWithCoverage"), "{out}");
        assert!(out.contains("Discovery"), "{out}");
        assert!(out.contains("denied"), "{out}");
    }

    #[test]
    fn request_phase_reports_the_resulting_phase_from_the_current_one() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        // Currently in Plan; asking for `next` must report Plan → Auto (computed from
        // the authoritative current phase), not a generic "advancing".
        let ex =
            PhaseVerbExecutor::new(tx, Arc::new(FixedPolicy(Phase::Plan)), Arc::new(EchoInner));

        let out = run(&ex, McpVerb::RequestPhase, "next").unwrap();
        assert!(out.contains("Plan"), "{out}");
        assert!(out.contains("Auto"), "{out}");
        assert!(matches!(rx.try_recv().unwrap(), Command::AdvancePhase { .. }));

        // Asking to jump straight to the phase it's already in is reported as no-op.
        let out = run(&ex, McpVerb::RequestPhase, "plan").unwrap();
        assert!(out.contains("Already in the Plan phase"), "{out}");
    }

    #[test]
    fn phase_status_reports_the_current_phase_and_its_write_policy() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        // Discovery is read-only for project files but allows AI-workspace notes.
        let ex = PhaseVerbExecutor::new(
            tx,
            Arc::new(FixedPolicy(Phase::Discovery)),
            Arc::new(EchoInner),
        );

        let out = run(&ex, McpVerb::PhaseStatus, "").unwrap();
        assert!(out.contains("Discovery"), "{out}");
        // Project edits denied, AI-workspace notes allowed, and `next` → Plan.
        assert!(out.contains("Project-file edits: denied"), "{out}");
        assert!(out.contains("AI-workspace notes (.ai/): allowed"), "{out}");
        assert!(out.contains("Plan"), "{out}");
        // A pure read: it must not emit any engine command.
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn phase_status_handles_an_untracked_session() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let ex = PhaseVerbExecutor::new(tx, Arc::new(NoPolicy), Arc::new(EchoInner));
        let out = run(&ex, McpVerb::PhaseStatus, "").unwrap();
        assert!(out.contains("not tracked yet"), "{out}");
    }
}
