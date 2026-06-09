//! Append-only audit trail. Revert is a *new compensating entry*, never a mutation.

use serde::{Deserialize, Serialize};

use crate::ids::{SessionId, Timestamp};
use crate::trust::McpVerb;

/// What an audit entry records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditAction {
    /// An autonomous MCP verb was executed.
    VerbExecuted { verb: McpVerb, summary: String },
    /// An action was denied (by the PDP or a human).
    Denied { what: String, reason: String },
    /// An action was approved by the operator.
    Approved { what: String },
    /// A compensating revert of a prior entry.
    Reverted { reverts: String },
    /// Feedback was injected into the session.
    FeedbackInjected { message: String },
    /// A phase transition occurred.
    PhaseChanged { to: crate::phase::Phase },
}

/// One append-only audit record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    /// ULID (time-ordered) — formatted as string at the persistence boundary.
    pub id: String,
    pub session_id: SessionId,
    pub at: Timestamp,
    pub action: AuditAction,
    /// Whether this entry can still be one-click reverted.
    pub revertible: bool,
}
