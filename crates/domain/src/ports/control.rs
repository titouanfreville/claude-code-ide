//! The control surface into Claude Code — the linchpin port (architecture Decision A).
//!
//! Spike 0 verdict (2026-06-02): L0 fully achievable, L2 substantial, L1 sufficient.
//! The concrete adapter (SDK-primary, hooks/JSONL secondary) is chosen at the
//! composition root; if only L0 is available the product degrades to observe-only.

use async_trait::async_trait;

use crate::errors::ControlError;
use crate::ids::SessionId;
use crate::phase::Phase;
use crate::review::Feedback;

/// The capability level a concrete control adapter provides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ControlLevel {
    /// Read-only observation.
    Observe,
    /// Can inject input / feedback into a session.
    Steer,
    /// Can force plan mode and gate/deny permissions.
    Govern,
}

/// Write-side control over Claude Code sessions. (Reads come via [`super::detection`].)
#[async_trait]
pub trait ControlPort: Send + Sync {
    /// The capability level this adapter actually provides (negotiated at startup).
    fn level(&self) -> ControlLevel;

    /// Spawn a new session with a pre-filled prompt, attached path, and starting phase. (FR9)
    async fn spawn(
        &self,
        prompt: &str,
        attached_path: Option<&str>,
        starting_phase: Phase,
    ) -> Result<SessionId, ControlError>;

    /// Inject corrective feedback / a redirection into a session (L1+). (FR18-20)
    async fn inject_feedback(&self, feedback: &Feedback) -> Result<(), ControlError>;

    /// Request the session move to a phase for its *next* turn (L2; e.g. resume in plan). (FR13-14)
    async fn set_phase(&self, session: &SessionId, phase: Phase) -> Result<(), ControlError>;

    /// Pause / resume a session (governor + safety halts).
    async fn pause(&self, session: &SessionId) -> Result<(), ControlError>;
    async fn resume(&self, session: &SessionId) -> Result<(), ControlError>;
}
