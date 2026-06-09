//! Sentinel domain errors. Typed enums cross every port boundary (no `anyhow`).

use thiserror::Error;

use crate::ids::SessionId;

/// Errors from the control surface into Claude Code (force-plan, gate, inject).
#[derive(Debug, Error)]
pub enum ControlError {
    #[error("session not found: {0}")]
    SessionNotFound(SessionId),
    #[error("control surface unavailable (degraded to observe-only)")]
    Unavailable,
    #[error("capability not supported at the current control level: {0}")]
    Unsupported(String),
    #[error("control transport failure: {0}")]
    Transport(String),
}

/// Errors from persistence/store adapters.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("backend failure: {0}")]
    Backend(String),
    #[error("data corrupt or unparseable: {0}")]
    Corrupt(String),
}

/// Errors from trust/permission evaluation.
#[derive(Debug, Error)]
pub enum TrustError {
    #[error("policy evaluation failed: {0}")]
    Evaluation(String),
}

/// Top-level domain error aggregating the above for app-boundary use.
#[derive(Debug, Error)]
pub enum DomainError {
    #[error(transparent)]
    Control(#[from] ControlError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Trust(#[from] TrustError),
}
