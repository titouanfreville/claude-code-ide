//! Control adapters implementing [`moonlight_domain::ports::ControlPort`].
//!
//! Hybrid posture from Spike 0: the SDK adapter (spawn/govern) is primary and the
//! hooks/JSONL adapter is the secondary observe-and-gate net. Both land in build
//! Slice 1+. The L0 [`ObserveOnlyControl`] fallback below is implemented now so the
//! engine always has a working `ControlPort`, degrading gracefully (NFR8).
//!
//! Hook-based control (the chosen "enrich, don't own" posture — see the control
//! adapter design doc) is built up here: [`classify`] maps a Claude Code tool to a
//! danger class, [`gate`] holds the per-session [`gate::GateState`] and the pure
//! gating [`gate::decide`], and [`ipc`] defines the hook wire protocol.

pub mod classify;
pub mod config;
pub mod gate;
pub mod ipc;
pub mod paths;
pub mod pending;
pub mod server;

pub use classify::classify;
pub use config::{load_config, user_config_path, AiWorkspaceResolver};
pub use gate::{decide, evaluate, GateDecision, GateState, HoldKind, EXIT_PLAN_MODE};
pub use paths::{classify_write_scope, AiWorkspace, AiWorkspaceConfig};
pub use ipc::{HookRequest, HookResponse};
pub use pending::{ApprovalNotifier, Decision, PendingApprovals};
pub use server::{query_hook, ControlServer, GateView, DEFAULT_HOLD};

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use moonlight_domain::errors::ControlError;
use moonlight_domain::ids::SessionId;
use moonlight_domain::phase::Phase;
use moonlight_domain::ports::mcp::{ApprovalDecision, ApprovalGate};
use moonlight_domain::ports::{ControlLevel, ControlPort};
use moonlight_domain::review::Feedback;
use tokio::sync::mpsc::UnboundedSender;

/// L0 fallback: observation only. Every write operation reports `Unavailable`,
/// so the product runs in observe-and-notify mode if no higher control is present.
#[derive(Debug, Default, Clone)]
pub struct ObserveOnlyControl;

#[async_trait]
impl ControlPort for ObserveOnlyControl {
    fn level(&self) -> ControlLevel {
        ControlLevel::Observe
    }

    async fn spawn(
        &self,
        _prompt: &str,
        _attached_path: Option<&str>,
        _starting_phase: Phase,
    ) -> Result<SessionId, ControlError> {
        Err(ControlError::Unavailable)
    }

    async fn inject_feedback(&self, _feedback: &Feedback) -> Result<(), ControlError> {
        Err(ControlError::Unavailable)
    }

    async fn set_phase(&self, _session: &SessionId, _phase: Phase) -> Result<(), ControlError> {
        Err(ControlError::Unavailable)
    }

    async fn pause(&self, _session: &SessionId) -> Result<(), ControlError> {
        Err(ControlError::Unavailable)
    }

    async fn resume(&self, _session: &SessionId) -> Result<(), ControlError> {
        Err(ControlError::Unavailable)
    }
}

/// L1 (Steer) control: delivers injected feedback (FR18-20) through a channel the
/// UI drains into the target session's embedded terminal — "Option C", the clean
/// home for rejection-as-feedback (delivery stays *behind the port*; the UI just
/// actuates the PTY the engine can't reach). Every other write op is still
/// `Unavailable` (Claude Code owns sessions; the PDP/hook enforce governance), so
/// this is [`ObserveOnlyControl`] plus a working `inject_feedback`.
pub struct SteerControl {
    /// Outbound feedback; the UI side owns the receiver and writes to terminals.
    feedback: UnboundedSender<Feedback>,
}

impl SteerControl {
    pub fn new(feedback: UnboundedSender<Feedback>) -> Self {
        Self { feedback }
    }
}

#[async_trait]
impl ControlPort for SteerControl {
    fn level(&self) -> ControlLevel {
        ControlLevel::Steer
    }

    async fn spawn(
        &self,
        _prompt: &str,
        _attached_path: Option<&str>,
        _starting_phase: Phase,
    ) -> Result<SessionId, ControlError> {
        Err(ControlError::Unavailable)
    }

    /// Queue the feedback for the UI drainer. A closed receiver (UI gone) is the
    /// only failure — reported as `Unavailable` so the supervisor surfaces it.
    async fn inject_feedback(&self, feedback: &Feedback) -> Result<(), ControlError> {
        self.feedback
            .send(feedback.clone())
            .map_err(|_| ControlError::Unavailable)
    }

    async fn set_phase(&self, _session: &SessionId, _phase: Phase) -> Result<(), ControlError> {
        Err(ControlError::Unavailable)
    }

    async fn pause(&self, _session: &SessionId) -> Result<(), ControlError> {
        Err(ControlError::Unavailable)
    }

    async fn resume(&self, _session: &SessionId) -> Result<(), ControlError> {
        Err(ControlError::Unavailable)
    }
}

/// Bridges the MCP actor's [`ApprovalGate`] to the cockpit's held-approval keystone:
/// the **same** [`PendingApprovals`] registry the plan/danger hooks use. On a held
/// verb it registers a pending approval, notifies the app (so the operator sees an
/// approve/deny affordance), and awaits the decision — resolved by the engine loop's
/// `route_approval` when the operator acts — or denies on timeout.
pub struct KeystoneApprovalGate {
    pending: Arc<PendingApprovals>,
    notifier: Arc<dyn ApprovalNotifier>,
    /// `None` = await the operator indefinitely (no "elapsed" deny — an MCP-verb
    /// approval is a human decision and must not be rushed); `Some(d)` bounds it.
    timeout: Option<Duration>,
}

impl KeystoneApprovalGate {
    pub fn new(
        pending: Arc<PendingApprovals>,
        notifier: Arc<dyn ApprovalNotifier>,
        timeout: Option<Duration>,
    ) -> Self {
        Self {
            pending,
            notifier,
            timeout,
        }
    }
}

#[async_trait]
impl ApprovalGate for KeystoneApprovalGate {
    async fn request(&self, session: &SessionId, what: &str) -> ApprovalDecision {
        let rx = self.pending.register(session.clone());
        // No plan markdown for an actor verb — the description carries the context.
        self.notifier.approval_requested(session, what, None);
        // Bounded (`Some`) → race the budget; unbounded (`None`) → await the operator
        // forever (the `Err`/elapsed arm is then unreachable — no rushed deny).
        let outcome = match self.timeout {
            Some(budget) => tokio::time::timeout(budget, rx).await,
            None => Ok(rx.await),
        };
        match outcome {
            Ok(Ok(Decision::Approve)) => ApprovalDecision::Approve,
            Ok(Ok(Decision::Deny { reason })) => ApprovalDecision::Deny { reason },
            // Sender dropped (superseded / cancelled) → treat as a deny.
            Ok(Err(_)) => ApprovalDecision::Deny {
                reason: "approval cancelled".into(),
            },
            // Only reachable for a bounded hold → drop the waiter and deny.
            Err(_) => {
                self.pending.cancel(session);
                ApprovalDecision::Deny {
                    reason: "approval window elapsed".into(),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moonlight_domain::review::FeedbackOrigin;

    #[test]
    fn observe_only_reports_observe_level() {
        assert_eq!(ObserveOnlyControl.level(), ControlLevel::Observe);
    }

    struct NopNotifier;
    impl ApprovalNotifier for NopNotifier {
        fn approval_requested(&self, _: &SessionId, _: &str, _: Option<&str>) {}
    }

    #[tokio::test]
    async fn keystone_gate_resolves_when_the_operator_approves() {
        let pending = Arc::new(PendingApprovals::new());
        let gate = KeystoneApprovalGate::new(
            pending.clone(),
            Arc::new(NopNotifier),
            Some(Duration::from_secs(5)),
        );
        let sid = SessionId::new("s1");

        let task = {
            let gate = gate;
            let sid = sid.clone();
            tokio::spawn(async move { gate.request(&sid, "run_with_coverage").await })
        };
        // Let the task register its pending approval, then resolve it as the operator.
        for _ in 0..1000 {
            if pending.is_pending(&sid) {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(pending.resolve(&sid, Decision::Approve));
        assert_eq!(task.await.unwrap(), ApprovalDecision::Approve);
    }

    #[tokio::test]
    async fn keystone_gate_denies_on_timeout() {
        let pending = Arc::new(PendingApprovals::new());
        let gate = KeystoneApprovalGate::new(
            pending,
            Arc::new(NopNotifier),
            Some(Duration::from_millis(20)),
        );
        let out = gate.request(&SessionId::new("s1"), "x").await;
        assert!(matches!(out, ApprovalDecision::Deny { .. }));
    }

    #[tokio::test]
    async fn steer_control_queues_feedback_for_the_ui() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let steer = SteerControl::new(tx);
        assert_eq!(steer.level(), ControlLevel::Steer);

        let fb = Feedback {
            session_id: SessionId::new("s1"),
            message: "cap retries at 3".into(),
            origin: FeedbackOrigin::HunkRejection,
        };
        steer.inject_feedback(&fb).await.expect("queued");
        assert_eq!(rx.recv().await.unwrap(), fb, "feedback reaches the UI drainer");
    }

    #[tokio::test]
    async fn steer_inject_fails_when_the_ui_drainer_is_gone() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        drop(rx); // UI side gone
        let steer = SteerControl::new(tx);
        let fb = Feedback {
            session_id: SessionId::new("s1"),
            message: "x".into(),
            origin: FeedbackOrigin::HunkRejection,
        };
        assert!(matches!(
            steer.inject_feedback(&fb).await,
            Err(ControlError::Unavailable)
        ));
    }
}
