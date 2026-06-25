//! Concrete adapters the [`ActorService`](crate::ActorService) depends on:
//! a [`SessionPolicyView`] folded from the engine bus, and an [`AuditSink`] backed
//! by the durable SQLite store. The composition root wires these; everything here
//! is testable without a live MCP transport.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use moonlight_domain::audit::{AuditAction, AuditEntry};
use moonlight_domain::ids::{SessionId, Timestamp};
use moonlight_domain::ports::mcp::{AuditSink, SessionPolicySnapshot, SessionPolicyView};
use moonlight_domain::ports::store::{ManagedSession, ManagedSessionStore};
use moonlight_domain::trust::TrustTier;
use moonlight_engine::EngineEvent;

/// A live [`SessionPolicyView`] kept in sync by folding the engine bus. Trust tier
/// isn't persisted, so the policy snapshot is read from the *live* fleet facts
/// (`SessionUpserted` carries phase + trust + attached path) rather than the store —
/// the actor always gates against the operator's current intent.
///
/// The composition root subscribes to the bus and calls [`apply`](Self::apply) per
/// event; the actor reads via [`snapshot`](SessionPolicyView::snapshot).
#[derive(Default)]
pub struct BusPolicyView {
    snapshots: RwLock<HashMap<SessionId, SessionPolicySnapshot>>,
}

impl BusPolicyView {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed snapshots from the durable managed records so the actor can resolve a
    /// session **even if it never sees that session's live `SessionUpserted`** — the
    /// upsert may have been published before this view's bus fold subscribed, or
    /// dropped on a `Lagged` burst (e.g. the supervisor's boot `hydrate_from_store`).
    /// Mirrors that hydrate, off the same [`ManagedSession`] source of truth.
    ///
    /// **Merge-only**: an entry already present from a live bus fact is never
    /// overwritten. Trust tier isn't persisted, so a seed restores the default-deny
    /// [`TrustTier::Observed`] (a live upsert later raises it to the operator's tier);
    /// seeding over a live entry would wrongly downgrade that tier.
    pub fn seed_from_managed(&self, records: &[ManagedSession]) {
        let mut map = self.snapshots.write().unwrap_or_else(|p| p.into_inner());
        for m in records {
            map.entry(m.id.clone()).or_insert_with(|| SessionPolicySnapshot {
                phase: m.phase,
                trust_tier: TrustTier::Observed,
                root: m.root.clone().map(PathBuf::from),
            });
        }
    }

    /// Fold one engine fact into the policy map.
    pub fn apply(&self, event: &EngineEvent) {
        let mut map = self.snapshots.write().unwrap_or_else(|p| p.into_inner());
        match event {
            EngineEvent::SessionUpserted { session } => {
                map.insert(
                    session.id.clone(),
                    SessionPolicySnapshot {
                        phase: session.phase,
                        trust_tier: session.trust_tier,
                        root: session.attached_path.clone().map(PathBuf::from),
                    },
                );
            }
            EngineEvent::PhaseTransitioned { session, phase } => {
                if let Some(snap) = map.get_mut(session) {
                    snap.phase = *phase;
                }
            }
            _ => {}
        }
    }
}

impl SessionPolicyView for BusPolicyView {
    fn snapshot(&self, session: &SessionId) -> Option<SessionPolicySnapshot> {
        self.snapshots
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(session)
            .cloned()
    }
}

/// An [`AuditSink`] that appends to the durable [`ManagedSessionStore`] audit log
/// (FR33). Stamps a time-ordered `<millis>-<seq>` id like the supervisor, so actor
/// entries interleave correctly with engine entries. Best-effort: a write failure is
/// logged, never propagated (it must not fail the verb).
pub struct StoreAuditSink {
    store: Arc<dyn ManagedSessionStore>,
    seq: AtomicU64,
}

impl StoreAuditSink {
    pub fn new(store: Arc<dyn ManagedSessionStore>) -> Self {
        Self {
            store,
            seq: AtomicU64::new(0),
        }
    }
}

impl AuditSink for StoreAuditSink {
    fn record(&self, session: &SessionId, action: AuditAction, revertible: bool) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let at = now();
        let entry = AuditEntry {
            id: format!("{:013}-{:06}", at.as_millis(), seq),
            session_id: session.clone(),
            at,
            action,
            revertible,
        };
        if let Err(err) = self.store.append_audit(&entry) {
            tracing::warn!(session = %session, error = %err, "actor audit append failed");
        }
    }
}

/// Wall-clock epoch millis (the audit sink stamps occurrence time).
fn now() -> Timestamp {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Timestamp::from_millis(ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    use moonlight_domain::phase::Phase;
    use moonlight_domain::session::{Mode, Session, SessionStatus};
    use moonlight_domain::trust::TrustTier;
    use moonlight_persistence::Store;

    fn session(id: &str, phase: Phase, tier: TrustTier, path: Option<&str>) -> Session {
        Session {
            id: SessionId::new(id),
            title: None,
            status: SessionStatus::Running,
            phase,
            mode: Mode::Auto,
            trust_tier: tier,
            attached_path: path.map(str::to_string),
            pinned: false,
            adopted: true,
            paused: false,
            phase_pinned: false,
            hidden: false,
            last_activity: Timestamp::from_millis(0),
        }
    }

    #[test]
    fn bus_policy_view_folds_upsert_and_phase_transition() {
        let view = BusPolicyView::new();
        assert!(view.snapshot(&SessionId::new("s1")).is_none());

        view.apply(&EngineEvent::SessionUpserted {
            session: session("s1", Phase::AutoImplement, TrustTier::Standard, Some("/repo")),
        });
        let snap = view.snapshot(&SessionId::new("s1")).expect("tracked");
        assert_eq!(snap.phase, Phase::AutoImplement);
        assert_eq!(snap.trust_tier, TrustTier::Standard);
        assert_eq!(snap.root, Some(PathBuf::from("/repo")));

        // A thin PhaseTransitioned updates phase but keeps tier + root.
        view.apply(&EngineEvent::PhaseTransitioned {
            session: SessionId::new("s1"),
            phase: Phase::Review,
        });
        let snap = view.snapshot(&SessionId::new("s1")).unwrap();
        assert_eq!(snap.phase, Phase::Review);
        assert_eq!(snap.trust_tier, TrustTier::Standard);
        assert_eq!(snap.root, Some(PathBuf::from("/repo")));

        // A phase transition for an unknown session is ignored (no insert).
        view.apply(&EngineEvent::PhaseTransitioned {
            session: SessionId::new("ghost"),
            phase: Phase::Commit,
        });
        assert!(view.snapshot(&SessionId::new("ghost")).is_none());
    }

    fn managed(id: &str, phase: Phase, root: Option<&str>) -> ManagedSession {
        ManagedSession {
            id: SessionId::new(id),
            root: root.map(str::to_string),
            title: None,
            mode: Mode::Auto,
            phase,
            agent: moonlight_domain::AgentKind::ClaudeCode,
            adopted: true,
            paused: false,
            phase_pinned: false,
            hidden: false,
            created_at: Timestamp::from_millis(0),
            last_seen: Timestamp::from_millis(0),
        }
    }

    #[test]
    fn seed_from_managed_populates_missing_and_never_clobbers_live() {
        let view = BusPolicyView::new();

        // A live upsert already carries the operator's real (raised) trust tier.
        view.apply(&EngineEvent::SessionUpserted {
            session: session("live", Phase::AutoImplement, TrustTier::Trusted, Some("/live")),
        });

        view.seed_from_managed(&[
            // New session: seeded at the default-deny Observed tier, from the record.
            managed("seeded", Phase::Plan, Some("/seeded")),
            // Same id as the live one: must NOT downgrade its tier or phase.
            managed("live", Phase::Commit, Some("/stale")),
        ]);

        let seeded = view.snapshot(&SessionId::new("seeded")).expect("seeded");
        assert_eq!(seeded.phase, Phase::Plan);
        assert_eq!(seeded.trust_tier, TrustTier::Observed);
        assert_eq!(seeded.root, Some(PathBuf::from("/seeded")));

        // The live entry is untouched (merge-only) — tier/phase/root all preserved.
        let live = view.snapshot(&SessionId::new("live")).unwrap();
        assert_eq!(live.phase, Phase::AutoImplement);
        assert_eq!(live.trust_tier, TrustTier::Trusted);
        assert_eq!(live.root, Some(PathBuf::from("/live")));
    }

    #[test]
    fn store_audit_sink_appends_to_the_real_log() {
        let store: Arc<dyn ManagedSessionStore> =
            Arc::new(Store::open_in_memory().expect("in-memory store"));
        let sink = StoreAuditSink::new(store.clone());
        let sid = SessionId::new("s1");

        sink.record(
            &sid,
            AuditAction::VerbExecuted {
                verb: moonlight_domain::trust::McpVerb::RunWithCoverage,
                summary: "test result: ok. 5 passed".into(),
            },
            true,
        );
        sink.record(
            &sid,
            AuditAction::Denied {
                what: "OpenReview".into(),
                reason: "tier too low".into(),
            },
            false,
        );

        let entries = store.recent_audit(&sid, 10).expect("read back");
        assert_eq!(entries.len(), 2);
        // Newest-first, and the ids are distinct + time-ordered.
        assert!(entries[0].id != entries[1].id);
        assert!(entries
            .iter()
            .any(|e| matches!(&e.action, AuditAction::VerbExecuted { summary, .. }
                if summary.contains("5 passed"))));
    }
}
