//! Persistence ports. Backed by SQLite; the audit store is append-only.

use async_trait::async_trait;

use crate::agent::AgentKind;
use crate::audit::AuditEntry;
use crate::continuity::{BaselineMetric, SessionSummary};
use crate::errors::StoreError;
use crate::ids::{SessionId, Timestamp};
use crate::phase::Phase;
use crate::session::{Mode, Session, SessionStatus};
use crate::trust::TrustTier;

/// Restart-survivable session state (FR42) + continuity summaries (FR43).
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn upsert(&self, session: &Session) -> Result<(), StoreError>;
    async fn get(&self, id: &SessionId) -> Result<Option<Session>, StoreError>;
    async fn all(&self) -> Result<Vec<Session>, StoreError>;
    async fn save_summary(&self, summary: &SessionSummary) -> Result<(), StoreError>;
    async fn latest_summary(&self, id: &SessionId) -> Result<Option<SessionSummary>, StoreError>;
}

/// Append-only audit log (FR33-34). Revert appends a compensating entry.
#[async_trait]
pub trait AuditStore: Send + Sync {
    async fn append(&self, entry: &AuditEntry) -> Result<(), StoreError>;
    async fn recent(
        &self,
        session: &SessionId,
        limit: usize,
    ) -> Result<Vec<AuditEntry>, StoreError>;
}

/// Usage baseline samples (FR45).
#[async_trait]
pub trait BaselineStore: Send + Sync {
    async fn record(&self, sample: &BaselineMetric) -> Result<(), StoreError>;
    async fn all(&self) -> Result<Vec<BaselineMetric>, StoreError>;
}

/// Durable identity of an **app-managed** session — one MoonlightCode launched
/// (or took over) into an embedded terminal. *Presence in the managed store* is
/// what distinguishes a managed session from a merely-observed one across a
/// restart: a restored monitor can re-resume its terminal (`claude --resume`)
/// instead of coming back as a read-only observed transcript (FR42).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedSession {
    pub id: SessionId,
    /// Working directory the session's terminal is rooted at, if known.
    pub root: Option<String>,
    /// Last observed session title (CC's custom `/rename` name or its auto
    /// `ai-title`), persisted so a rehydrated session shows its name instead of the
    /// raw id. Refreshed by the supervisor on `TitleObserved`.
    pub title: Option<String>,
    pub mode: Mode,
    pub phase: Phase,
    /// Which agent CLI drives this session (`claude` / `agy`). Persisted so a restart
    /// relaunches the same backend. Defaults to [`AgentKind::ClaudeCode`] for records
    /// written before backend selection existed (the migration backfills them).
    pub agent: AgentKind,
    /// The backend's own conversation id, when it differs from [`id`](Self::id).
    /// Claude pins our chosen id (`--session-id`), so its conversation *is* `id` and
    /// this stays `None`. Antigravity has no such flag — it mints its own
    /// `conversationId` after launch — so we discover it (root + launch-time
    /// correlation) and persist it here. That makes a restart resume the right AGY
    /// conversation (`agy --conversation <cid>`) and lets per-session data (model /
    /// usage) key exactly instead of guessing by root. `None` until observed.
    pub conversation_id: Option<String>,
    /// The operator's trust tier for this session, persisted so a manual "trust this
    /// session" (or a project-trust default seeded at launch) survives restart/reset
    /// instead of reseeding to the default-deny [`TrustTier::Observed`]. Backfilled to
    /// `Observed` for records written before trust was persisted.
    pub trust_tier: TrustTier,
    pub adopted: bool,
    pub paused: bool,
    /// Whether the operator pinned the phase (manual override of auto-advance).
    pub phase_pinned: bool,
    /// Whether the operator masked this session from the default fleet view
    /// (soft-hide; see [`Session::hidden`]).
    pub hidden: bool,
    /// When MoonlightCode first launched/took over this session.
    pub created_at: Timestamp,
    /// Last time the supervisor refreshed this row from observed activity.
    pub last_seen: Timestamp,
}

impl ManagedSession {
    /// Reconstruct a live [`Session`] read-model from this persisted record — the
    /// single source of truth for fleet rehydration (the engine supervisor on boot
    /// and the UI grid's pull-seed both use it, so they can't drift). Restored **at
    /// rest**: status [`SessionStatus::Idle`] (detection promotes it to `Running` if
    /// the session is actually live). The operator-set fields — phase, mode, trust
    /// tier, adopted, paused, the phase pin, root, last-seen — come straight from the
    /// record.
    pub fn to_session(&self) -> Session {
        Session {
            id: self.id.clone(),
            title: self.title.clone(),
            status: SessionStatus::Idle,
            phase: self.phase,
            mode: self.mode,
            trust_tier: self.trust_tier,
            attached_path: self.root.clone(),
            pinned: false,
            adopted: self.adopted,
            paused: self.paused,
            phase_pinned: self.phase_pinned,
            hidden: self.hidden,
            last_activity: self.last_seen,
        }
    }
}

/// Mutable slice of a managed session refreshed by the supervisor as it observes
/// activity. Applied as an UPDATE-only operation so it never resurrects a row for
/// a session that was never managed (keeps the table managed-only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedStateUpdate {
    pub id: SessionId,
    /// Last observed title (see [`ManagedSession::title`]).
    pub title: Option<String>,
    pub phase: Phase,
    pub mode: Mode,
    /// Operator trust tier (see [`ManagedSession::trust_tier`]).
    pub trust_tier: TrustTier,
    pub adopted: bool,
    pub paused: bool,
    pub phase_pinned: bool,
    /// Soft-hide flag (see [`ManagedSession::hidden`]).
    pub hidden: bool,
    pub last_seen: Timestamp,
}

/// Restart-survivable store of managed-session identity + an append-only audit
/// log of autonomous actions (FR33-34, FR42). **Synchronous**: it is backed by a
/// local SQLite file (sub-millisecond), so the supervisor can persist inline
/// without an async hop and the engine never blocks meaningfully on it.
pub trait ManagedSessionStore: Send + Sync {
    /// Insert or replace the managed record for `session` (preserving its original
    /// `created_at` on conflict). Called when the app launches/takes over a session.
    fn upsert_managed(&self, session: &ManagedSession) -> Result<(), StoreError>;
    /// Refresh the mutable state of an *existing* managed row. Returns whether a row
    /// was updated (`false` ⇒ the session is not managed, so nothing was written).
    fn update_managed_state(&self, update: &ManagedStateUpdate) -> Result<bool, StoreError>;
    /// Persist the backend's discovered conversation id on an *existing* managed row
    /// (Antigravity correlation — see [`ManagedSession::conversation_id`]). UPDATE-only:
    /// returns whether a row was touched (`false` ⇒ the session is not managed).
    fn set_conversation_id(
        &self,
        id: &SessionId,
        conversation_id: &str,
    ) -> Result<bool, StoreError>;
    /// Fetch the managed record for `id`, if the session is managed.
    fn managed(&self, id: &SessionId) -> Result<Option<ManagedSession>, StoreError>;
    /// All managed sessions, oldest first (`created_at`).
    fn all_managed(&self) -> Result<Vec<ManagedSession>, StoreError>;
    /// Forget a managed session (e.g. it was permanently closed).
    fn remove_managed(&self, id: &SessionId) -> Result<(), StoreError>;
    /// Append one audit entry (append-only; never updates an existing row).
    fn append_audit(&self, entry: &AuditEntry) -> Result<(), StoreError>;
    /// The most recent `limit` audit entries for `session`, newest first.
    fn recent_audit(
        &self,
        session: &SessionId,
        limit: usize,
    ) -> Result<Vec<AuditEntry>, StoreError>;
}
