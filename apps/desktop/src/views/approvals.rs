//! A tiny UI-side registry of outstanding operator approvals, keyed by session.
//!
//! An approval hold (an external MCP tool a frozen phase blocked, a sensitive phase
//! change, a danger-zone command) is announced **once** on the engine bus as
//! [`ApprovalRequested`](moonlight_engine::EngineEvent::ApprovalRequested). The session's
//! [`SessionMonitor`](super::panels::session_monitor) turns that into its bottom-right
//! authorization popup — but only if it is mounted at that instant. A monitor opened
//! *later* (clicking the bell notification, or switching back to the session's space)
//! missed the one-shot event and would show **no** popup, even though the hold is still
//! blocking the session on the control server. That is the "clicking the notification
//! doesn't reopen it" gap.
//!
//! This registry keeps the last-announced, still-outstanding hold per session so a
//! freshly-built monitor can recover its popup. The [`Workspace`](super::workspace)'s
//! bus loop is the single writer: it [`set`](Approvals::set)s on `ApprovalRequested` and
//! [`clear`](Approvals::clear)s when the hold resolves (the session resumes / changes
//! phase / is removed) — mirroring the monitor's own popup-clearing so the retained copy
//! never outlives the live one. The monitor also clears on the operator's own verdict,
//! closing the tiny window between the click and the resolving event. Cloning shares the
//! same registry (an `Arc<Mutex<…>>` handle, like [`SessionIo`](super::session_io)).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use moonlight_domain::ids::SessionId;

/// A still-outstanding approval hold, in the shape the popup needs to rebuild itself.
#[derive(Clone)]
pub struct ApprovalHold {
    /// Short human description of the held action (e.g. `"RequestPhase auto"`, a
    /// danger-zone command). Shown for a non-tool hold.
    pub what: String,
    /// The full `mcp__server__tool` name when the hold is an external MCP tool a frozen
    /// phase blocked; `None` for a plain action / phase-change hold.
    pub tool: Option<String>,
}

/// Shared, cloneable registry of outstanding holds keyed by session.
#[derive(Clone, Default)]
pub struct Approvals {
    inner: Arc<Mutex<HashMap<SessionId, ApprovalHold>>>,
}

impl Approvals {
    /// Record (or replace) the outstanding hold for `session`.
    pub fn set(&self, session: SessionId, hold: ApprovalHold) {
        if let Ok(mut m) = self.inner.lock() {
            m.insert(session, hold);
        }
    }

    /// Clear any outstanding hold for `session` (it resolved, or the session ended).
    pub fn clear(&self, session: &SessionId) {
        if let Ok(mut m) = self.inner.lock() {
            m.remove(session);
        }
    }

    /// The outstanding hold for `session`, if one is still pending.
    pub fn get(&self, session: &SessionId) -> Option<ApprovalHold> {
        self.inner.lock().ok()?.get(session).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid() -> SessionId {
        SessionId::new("s1")
    }

    #[test]
    fn set_get_clear_round_trip() {
        let a = Approvals::default();
        assert!(a.get(&sid()).is_none());

        a.set(
            sid(),
            ApprovalHold {
                what: "RequestPhase auto".into(),
                tool: None,
            },
        );
        let held = a.get(&sid()).expect("hold retained");
        assert_eq!(held.what, "RequestPhase auto");
        assert!(held.tool.is_none());

        a.clear(&sid());
        assert!(a.get(&sid()).is_none());
    }

    #[test]
    fn set_replaces_the_prior_hold() {
        let a = Approvals::default();
        a.set(
            sid(),
            ApprovalHold {
                what: "first".into(),
                tool: None,
            },
        );
        a.set(
            sid(),
            ApprovalHold {
                what: "second".into(),
                tool: Some("mcp__srv__tool".into()),
            },
        );
        let held = a.get(&sid()).unwrap();
        assert_eq!(held.what, "second");
        assert_eq!(held.tool.as_deref(), Some("mcp__srv__tool"));
    }

    #[test]
    fn clones_share_one_registry() {
        let a = Approvals::default();
        let b = a.clone();
        b.set(
            sid(),
            ApprovalHold {
                what: "held".into(),
                tool: None,
            },
        );
        // The write through `b` is visible through `a` (same underlying map).
        assert!(a.get(&sid()).is_some());
    }
}
