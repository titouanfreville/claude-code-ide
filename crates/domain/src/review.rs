//! Review surface: hunks, per-hunk decisions, and the rejection-as-feedback primitive.

use serde::{Deserialize, Serialize};

use crate::ids::{HunkId, SessionId};

/// A single reviewable change within a session's pending diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewHunk {
    pub id: HunkId,
    pub session_id: SessionId,
    pub file_path: String,
    /// Unified-diff text for this hunk (rendered with gutter markers in the UI).
    pub diff: String,
}

/// The operator's decision on a hunk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HunkDecision {
    Accept,
    /// Reject — carries optional one-line steering that becomes injected feedback.
    Reject {
        feedback: Option<Feedback>,
    },
}

/// Structured corrective feedback delivered back into a session (the signature
/// primitive). Produced by a rejected hunk, a denied danger-zone action, or a
/// failed gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feedback {
    pub session_id: SessionId,
    /// One-line intent, e.g. "exponential backoff, cap 3 retries".
    pub message: String,
    pub origin: FeedbackOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeedbackOrigin {
    HunkRejection,
    DangerZoneDenial,
    FailedGate,
    /// Free-form operator redirection mid-flight (the `Steer` command). Not tied
    /// to a rejected hunk or a denied gate — the operator is nudging direction.
    OperatorSteer,
}
