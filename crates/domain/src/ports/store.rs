//! Persistence ports. Backed by SQLite; the audit store is append-only.

use async_trait::async_trait;

use crate::agent::AgentKind;
use crate::audit::AuditEntry;
use crate::changes::{Baseline, FileTouch, ReviewComment, TouchedPath};
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

/// Per-session ledger of the files an agent wrote, plus the operator's review
/// comments on them (FR22). **Synchronous** for the same reason as
/// [`ManagedSessionStore`]: local SQLite, so the hook path can record inline
/// without an async hop while Claude Code waits on its verdict.
pub trait SessionChangeStore: Send + Sync {
    /// Record one observed write. Idempotent per `(session, path)`: the **first**
    /// call stores [`FileTouch::baseline`], later ones only advance the last-touch
    /// timestamp and the counter. Returns whether this call established the
    /// baseline (i.e. it was the session's first touch of that file).
    fn record_touch(&self, touch: &FileTouch) -> Result<bool, StoreError>;
    /// Every file `session` has written, most recently touched first — names and
    /// counts only. A baseline is a whole file's contents, so listing a session's
    /// changes never loads them.
    fn touched_paths(&self, session: &SessionId) -> Result<Vec<TouchedPath>, StoreError>;
    /// The pre-image of **one** file, fetched only when something is about to show
    /// it. `None` when this session never touched `path`.
    fn baseline(&self, session: &SessionId, path: &str) -> Result<Option<Baseline>, StoreError>;
    /// How many files each session has written, for every session at once — one
    /// query behind a whole grid of badges.
    fn touched_counts(&self) -> Result<Vec<(SessionId, u32)>, StoreError>;
    /// Drop a session's ledger, its comments, and its review state (the session was
    /// forgotten, or the operator closed the pass).
    fn forget_session_changes(&self, session: &SessionId) -> Result<(), StoreError>;

    /// Mark a file reviewed as of `at`, or clear the mark with `None`.
    ///
    /// The timestamp is the whole mechanism: [`TouchedPath::reviewed`] is
    /// `at >= last_touch_at`, so a later write by the session un-marks the file
    /// without anyone having to notice it happened. Returns whether a ledger row
    /// was found to mark.
    fn mark_reviewed(
        &self,
        session: &SessionId,
        path: &str,
        at: Option<Timestamp>,
    ) -> Result<bool, StoreError>;

    /// The paths this session's review is hiding — files, and directory prefixes
    /// standing for everything beneath them.
    ///
    /// Kept per session rather than per project: "ignore for this review" is a
    /// judgement about the pass being made now, and it dies with the pass. A
    /// generated file worth skipping today can be the whole point of tomorrow's
    /// review.
    fn ignored_paths(&self, session: &SessionId) -> Result<Vec<String>, StoreError>;
    /// Hide `path` from this session's review, or bring it back.
    fn set_ignored(&self, session: &SessionId, path: &str, ignored: bool)
        -> Result<(), StoreError>;
    /// Bring everything this session's review is hiding back.
    fn clear_ignored(&self, session: &SessionId) -> Result<(), StoreError>;

    /// Persist one review comment (written as soon as the operator adds it, so a
    /// closed tab doesn't lose it).
    fn add_comment(&self, comment: &ReviewComment) -> Result<(), StoreError>;
    /// All of `session`'s comments, oldest first.
    fn comments(&self, session: &SessionId) -> Result<Vec<ReviewComment>, StoreError>;
    /// Rewrite an existing comment in place — its body, its anchor, whether it
    /// counts as delivered, and whether it is resolved. Editing a comment that was
    /// already sent is how an operator corrects one that landed wrong; clearing its
    /// `sent_at` is what puts it back in the next batch, and stamping
    /// `resolved_at` is what takes it off the diff without erasing it.
    fn update_comment(&self, comment: &ReviewComment) -> Result<bool, StoreError>;
    fn delete_comment(&self, id: &str) -> Result<(), StoreError>;
    /// Stamp `ids` as delivered — called once the batched review reaches the session.
    fn mark_comments_sent(&self, ids: &[String], at: Timestamp) -> Result<(), StoreError>;
}
