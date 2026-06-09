//! Continuity: restart-survivable session summaries and the usage baseline.

use serde::{Deserialize, Serialize};

use crate::ids::{SessionId, Timestamp};

/// A "what was I doing" summary regenerated when the operator returns (FR43).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: SessionId,
    pub generated_at: Timestamp,
    pub text: String,
}

/// One sample of the usage baseline captured before/while building, so the
/// primary success metrics (lower tokens, more concurrency) are provable (FR45).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaselineMetric {
    pub at: Timestamp,
    pub parallel_sessions: u32,
    /// Total minutes sessions sat idle-blocked in this sample window.
    pub idle_blocked_minutes: f32,
    pub tokens_spent: u64,
}
