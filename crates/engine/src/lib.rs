//! MoonlightCode core engine: session supervisor, phase state machine, event bus,
//! fleet governor. Depends on `domain` ports only.
//!
//! This module defines the **UI ↔ engine boundary** types and (Slice 1, Step 2)
//! re-exports the event [`bus`] and session [`supervisor`]. The governor lands
//! in a later slice (see architecture build order).

pub mod bus;
pub mod supervisor;

pub use bus::EventBus;
pub use supervisor::SessionSupervisor;

use moonlight_domain::economy::GovernorAction;
use moonlight_domain::ids::SessionId;
use moonlight_domain::phase::Phase;
use moonlight_domain::review::Feedback;
use moonlight_domain::session::{AttentionKind, Session, SessionStatus};
use moonlight_domain::trust::TrustTier;

/// Facts and requests the engine publishes; the UI's only inbound channel.
/// Past-tense variants are facts; imperative variants are requests for the operator.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// A session row was created, or a non-status field (e.g. title) changed —
    /// carries the full record so the UI can add or replace a tile. Thin
    /// single-field updates use `SessionStateChanged` / `PhaseTransitioned`.
    SessionUpserted {
        session: Session,
    },
    SessionStateChanged {
        session: SessionId,
        status: SessionStatus,
    },
    /// A session was forgotten (record deleted, dropped from the fleet) — the UI
    /// removes its tile. The transcript on disk is untouched.
    SessionRemoved {
        session: SessionId,
    },
    PhaseTransitioned {
        session: SessionId,
        phase: Phase,
    },
    ReviewReady {
        session: SessionId,
    },
    /// A **pinned** session reached a done-checkpoint and the workflow wants to
    /// advance, but the operator's manual pin holds it (A ≫ B). Surfaces a
    /// "may I advance to `to`?" affordance; approving emits `Command::AdvancePhase`.
    PhaseAdvanceRequested {
        session: SessionId,
        to: Phase,
    },
    /// The agent proposed a plan; the operator can review it before work proceeds
    /// (plan-review gate). Carries the full plan markdown.
    PlanProposed {
        session: SessionId,
        plan: String,
    },
    /// The agent's latest end-of-turn prose — the "what this covers" summary shown
    /// on the code-review gate (T4). A pass-through of the detection observation.
    SummaryObserved {
        session: SessionId,
        summary: String,
    },
    ApprovalRequested {
        session: SessionId,
        what: String,
    },
    AuditAppended {
        session: SessionId,
        summary: String,
    },
    /// A session's **attention overlay** changed — the louder "this one wants you /
    /// did not complete" signal layered over the resting status (⚠ on the tile + the
    /// space-tab dot). `Some(kind)` raises it (the IDE detecting an abnormal end, or
    /// the agent self-reporting `Stuck`); `None` clears it. Transient like status — not
    /// persisted; the UI folds it into a per-session overlay.
    SessionAlert {
        session: SessionId,
        alert: Option<AttentionKind>,
    },
    GovernorActed {
        action: GovernorAction,
    },
}

/// Operator intents sent from the UI to the engine. The UI never mutates engine
/// state directly — it emits `Command`s.
#[derive(Debug, Clone)]
pub enum Command {
    RejectHunk {
        feedback: Feedback,
    },
    ApproveAction {
        session: SessionId,
    },
    DenyAction {
        session: SessionId,
        reason: String,
    },
    SpawnSession {
        prompt: String,
        attached_path: Option<String>,
    },
    Steer {
        session: SessionId,
        message: String,
    },
    /// Opt a session into (or out of) MoonlightCode governance. Only adopted
    /// sessions are gated by the PDP/hooks.
    ToggleAdoption {
        session: SessionId,
    },
    /// Set a session's adoption to an explicit value (idempotent). Used to
    /// **auto-adopt** sessions the app creates or imports, so they are gated
    /// immediately without a manual opt-in.
    SetAdopted {
        session: SessionId,
        adopted: bool,
    },
    /// Set a session's trust tier (operator override). Modulates the PDP's
    /// autonomy for actor verbs (and, later, graduated danger handling).
    SetTrust {
        session: SessionId,
        tier: TrustTier,
    },
    /// Pause/resume a session (operator safety halt). A paused adopted session has
    /// every tool denied by the gate until resumed.
    TogglePause {
        session: SessionId,
    },
    /// Soft-hide / unhide a session — mask a "not relevant anymore" session from the
    /// default fleet view (recoverable via "show hidden"). Persists for managed
    /// sessions; the transcript and governance are untouched. Republishes the full
    /// row so the grid re-renders with the new `hidden` flag.
    SetHidden {
        session: SessionId,
        hidden: bool,
    },
    /// Set a session's workflow phase — the operator's authority over the state
    /// machine. The selector picks Plan / Discovery / Auto; this can also set the
    /// engine-only Test/Review/Commit that no automatic transition reaches today. An
    /// operator pick takes precedence over any future automatic progression.
    SetPhase {
        session: SessionId,
        phase: Phase,
    },
    /// Advance a session to the next workflow phase ([`Phase::next`]) and return
    /// it to **auto** mode (clears any pin). Operator-confirmed advancement —
    /// emitted by the Test "tests done" button, the Commit "new cycle" button, the
    /// code-review Approve (Review→Commit), and the pinned-advance approval.
    AdvancePhase {
        session: SessionId,
    },
    /// Pin / unpin a session's phase. Pinning freezes auto-advance and shields the
    /// phase from detection reconcile; unpinning resumes auto in place. (The
    /// implicit pin on a manual pick rides on `SetPhase`; this is the explicit
    /// lock/unlock affordance.)
    SetPhasePinned {
        session: SessionId,
        pinned: bool,
    },
    /// Reload the managed fleet from the durable store (FR42). Re-seeds any
    /// persisted managed session missing from the live in-memory fleet and
    /// republishes it so the cockpit grid (which only grows from live deltas)
    /// shows it again. Runs once at boot and on the operator's manual "refresh".
    /// Idempotent: sessions already live are left untouched.
    RehydrateFleet,
    /// Forget a session entirely: drop it from the live fleet **and** delete its
    /// managed record, then announce [`EngineEvent::SessionRemoved`] so the grid
    /// drops its tile. Used when ↻ Reset replaces a session with a fresh id (CC's
    /// `/clear` continues the terminal under a NEW session id, so the old record
    /// would forever point at the dead pre-clear conversation).
    ForgetSession {
        session: SessionId,
    },
    /// Raise or clear a session's **attention overlay** (the ⚠ "did not complete / is
    /// stuck" signal). Emitted by the agent's `report_blocked` MCP verb (→ `Stuck`) and
    /// by the IDE's abnormal-end detection (→ `Incomplete`); `None` clears it. The
    /// supervisor just republishes it as [`EngineEvent::SessionAlert`] for the UI.
    FlagSession {
        session: SessionId,
        alert: Option<AttentionKind>,
    },
}
