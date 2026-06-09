//! Read-side detection: fuse Claude Code hooks + transcript JSONL into session
//! state events. Adapters MUST treat all Claude Code artifacts as untrusted input.

use async_trait::async_trait;

use crate::errors::ControlError;
use crate::ids::SessionId;
use crate::phase::Phase;
use crate::session::{AttentionKind, SessionStatus};

/// A detected change in a session's observable state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectionEvent {
    StatusChanged {
        session: SessionId,
        status: SessionStatus,
    },
    /// The detector raised or cleared an **attention overlay** observed from the
    /// transcript — today: a session handed a turn (operator prompt / tool result)
    /// that then went silent far past the working window is flagged `Incomplete` (it
    /// stopped without finishing). `None` clears it when the session comes back to
    /// life. Distinct from `StatusChanged` because it is the louder ⚠ overlay, not a
    /// resting status (see [`AttentionKind`]).
    Alert {
        session: SessionId,
        alert: Option<AttentionKind>,
    },
    PhaseObserved {
        session: SessionId,
        phase: Phase,
    },
    TitleObserved {
        session: SessionId,
        title: String,
    },
    /// The session's working directory (observed from a transcript `cwd`).
    WorkspaceObserved {
        session: SessionId,
        path: String,
    },
    /// The agent proposed a plan (observed from an `ExitPlanMode` tool call) —
    /// the operator can review it before the session acts (plan-review gate).
    PlanProposed {
        session: SessionId,
        plan: String,
    },
    /// The agent's latest end-of-turn prose (the final assistant *text*, not a tool
    /// call) — surfaced as the "what this covers" summary on the review gate (T4).
    SummaryObserved {
        session: SessionId,
        summary: String,
    },
    /// A session appeared (was started, possibly outside MoonlightCode).
    Discovered {
        session: SessionId,
    },
    /// A session ended.
    Ended {
        session: SessionId,
    },
}

/// Streams detection events. Implemented by the hooks+JSONL fusion adapter.
#[async_trait]
pub trait DetectionSource: Send + Sync {
    /// Drain the next batch of detected events (adapter buffers internally).
    /// Returns an empty vec when nothing new; never blocks the engine loop indefinitely.
    async fn poll(&self) -> Result<Vec<DetectionEvent>, ControlError>;
}
