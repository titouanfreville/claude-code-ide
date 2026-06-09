//! Token economy: per-session telemetry, rate-limit headroom, and governor actions.

use serde::{Deserialize, Serialize};

use crate::ids::SessionId;

/// Snapshot of a session's token/cost telemetry (feeds the HUD).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenStat {
    pub session_id: SessionId,
    /// Context window fullness, 0.0–1.0.
    pub context_fraction: f32,
    /// Output tokens consumed so far this session.
    pub tokens_spent: u64,
    /// Tokens estimated saved by RTK compression.
    pub tokens_saved: u64,
    pub model: String,
}

/// Global rate-limit headroom the governor watches.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RateHeadroom {
    /// Remaining fraction of the rate limit, 0.0–1.0.
    pub remaining_fraction: f32,
}

impl RateHeadroom {
    pub fn is_low(self) -> bool {
        self.remaining_fraction < 0.15
    }
}

/// An action the fleet governor may take to control spend (FR40). Must never
/// throttle the operator's own UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GovernorAction {
    /// Pause a low-trust session until headroom recovers.
    Pause { session_id: SessionId },
    /// Suggest down-shifting a session to a cheaper model.
    DownShift {
        session_id: SessionId,
        to_model: String,
    },
    /// No action needed.
    None,
}
