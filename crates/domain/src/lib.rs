//! MoonlightCode domain — pure entities, trait ports, and sentinel errors.
//!
//! This crate has NO I/O, NO async runtime, and NO UI. Adapters (in sibling
//! crates) implement the trait ports defined under [`ports`]; the desktop app
//! wires concrete adapters into these ports at its composition root.

pub mod agent;
pub mod audit;
pub mod changes;
pub mod continuity;
pub mod economy;
pub mod errors;
pub mod ids;
pub mod phase;
pub mod ports;
pub mod review;
pub mod session;
pub mod trust;

// Convenience re-exports for the most-used types.
pub use agent::{AgentKind, McpInjection};
pub use audit::{AuditAction, AuditEntry};
pub use changes::{
    Baseline, BaselineGap, ChangeTool, DiffSide, FileTouch, ReviewComment, TouchedPath,
};
pub use economy::{GovernorAction, RateHeadroom, TokenStat};
pub use errors::{ControlError, DomainError, StoreError, TrustError};
pub use ids::{HunkId, SessionId, Timestamp};
pub use phase::Phase;
pub use review::{Feedback, HunkDecision, ReviewHunk};
pub use session::{AttentionKind, Mode, Session, SessionStatus};
pub use trust::{DangerClass, McpVerb, PermissionOutcome, TrustTier};
