//! Held-approval registry — the keystone that lets a synchronous Claude Code hook
//! *wait* for a human decision.
//!
//! A `PreToolUse` hook is synchronous, but Claude Code waits for it (≈60s budget),
//! so the control server can **hold** the connection open while the operator
//! decides in the cockpit. The flow:
//!
//! 1. The server classifies a tool call as needing approval (`gate::needs_hold`).
//! 2. It [`PendingApprovals::register`]s the session, getting back a oneshot
//!    receiver, and notifies the app via [`ApprovalNotifier`] (→ the cockpit shows
//!    an approve/reject affordance).
//! 3. It `await`s the receiver (with a timeout); meanwhile CC's hook is still
//!    blocked, so the session has not acted.
//! 4. The operator approves/denies → the app calls [`PendingApprovals::resolve`]
//!    → the oneshot fires → the hook returns allow/deny **in the same turn**.
//!
//! Keyed by [`SessionId`]: a session is single-threaded and its hook is blocking,
//! so at most one approval is outstanding per session at any time. Registering a
//! second one for the same session supersedes the first (the old waiter then sees
//! its sender dropped and resolves as a deny — see the server's timeout/closed arm).
//!
//! Sends are synchronous (`oneshot::Sender::send` needs no runtime), so the app's
//! GPUI-side command loop can resolve an approval even though the server awaits on
//! a separate tokio runtime thread.

use std::collections::HashMap;
use std::sync::Mutex;

use moonlight_domain::ids::SessionId;
use tokio::sync::oneshot;

/// The operator's verdict on a held action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Let the held action proceed.
    Approve,
    /// Block it; `reason` is returned to the session as the hook's deny reason
    /// (so the agent gets steered, e.g. to revise a rejected plan).
    Deny { reason: String },
}

/// Lets the control server tell the running app that a session is waiting on an
/// operator decision. Implemented in the composition root (it publishes the
/// matching engine events onto the bus); kept as a trait here so `control` stays
/// free of any dependency on the engine/event types.
pub trait ApprovalNotifier: Send + Sync {
    /// A session's action is held pending approval. `what` is a short
    /// human-readable description (e.g. "approve plan", "run `rm -rf …`"); `plan`
    /// carries the proposed plan markdown when the held action is `ExitPlanMode`;
    /// `mcp_tool` carries the full `mcp__server__tool` name when the held action is an
    /// external MCP tool a frozen phase blocked (so the cockpit can offer per-tool /
    /// per-server "always allow"). Both are `None` for a plain danger-zone hold.
    fn approval_requested(
        &self,
        session: &SessionId,
        what: &str,
        plan: Option<&str>,
        mcp_tool: Option<&str>,
    );
}

/// Registry of in-flight held approvals, keyed by session.
#[derive(Default)]
pub struct PendingApprovals {
    waiters: Mutex<HashMap<SessionId, oneshot::Sender<Decision>>>,
}

impl PendingApprovals {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a pending approval for `session` and return the receiver to await.
    /// Any prior pending approval for the same session is superseded (its sender is
    /// dropped, so its waiter resolves as closed → deny).
    pub fn register(&self, session: SessionId) -> oneshot::Receiver<Decision> {
        let (tx, rx) = oneshot::channel();
        self.waiters
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(session, tx);
        rx
    }

    /// Resolve the pending approval for `session`, if any. Returns `true` when a
    /// waiter was present and notified (the receiver may already be gone if the
    /// hold timed out, in which case the send is dropped and this still reports the
    /// waiter was consumed).
    pub fn resolve(&self, session: &SessionId, decision: Decision) -> bool {
        let Some(tx) = self
            .waiters
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(session)
        else {
            return false;
        };
        // The receiver may have dropped (timeout) — that's fine; we still consumed
        // the waiter, so report success.
        let _ = tx.send(decision);
        true
    }

    /// Drop any pending approval for `session` without resolving it (e.g. the
    /// session ended). The waiter sees the sender close and resolves as a deny.
    pub fn cancel(&self, session: &SessionId) {
        self.waiters
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(session);
    }

    /// Whether an approval is currently outstanding for `session`.
    pub fn is_pending(&self, session: &SessionId) -> bool {
        self.waiters
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid() -> SessionId {
        SessionId::new("s1")
    }

    #[tokio::test]
    async fn approve_resolves_the_waiter() {
        let pending = PendingApprovals::new();
        let rx = pending.register(sid());
        assert!(pending.is_pending(&sid()));

        assert!(pending.resolve(&sid(), Decision::Approve));
        assert_eq!(rx.await.unwrap(), Decision::Approve);
        // Consumed: no longer pending.
        assert!(!pending.is_pending(&sid()));
    }

    #[tokio::test]
    async fn deny_carries_the_reason() {
        let pending = PendingApprovals::new();
        let rx = pending.register(sid());

        pending.resolve(
            &sid(),
            Decision::Deny {
                reason: "no".into(),
            },
        );
        assert_eq!(
            rx.await.unwrap(),
            Decision::Deny {
                reason: "no".into()
            }
        );
    }

    #[test]
    fn resolve_unknown_session_reports_false() {
        let pending = PendingApprovals::new();
        assert!(!pending.resolve(&sid(), Decision::Approve));
    }

    #[tokio::test]
    async fn cancel_drops_the_waiter_so_it_sees_closed() {
        let pending = PendingApprovals::new();
        let rx = pending.register(sid());
        pending.cancel(&sid());
        assert!(!pending.is_pending(&sid()));
        // Sender dropped → receiver errors (the server maps this to a deny).
        assert!(rx.await.is_err());
    }

    #[tokio::test]
    async fn re_registering_supersedes_the_prior_waiter() {
        let pending = PendingApprovals::new();
        let first = pending.register(sid());
        let second = pending.register(sid());
        // The first waiter's sender was dropped when the second registered.
        assert!(first.await.is_err());
        // The live one resolves normally.
        pending.resolve(&sid(), Decision::Approve);
        assert_eq!(second.await.unwrap(), Decision::Approve);
    }
}
