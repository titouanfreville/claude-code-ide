//! File-lock negotiation between concurrent autonomous sessions (FR52).
//! A safety net for shared resources that escape per-session git-worktree isolation.

use async_trait::async_trait;

use crate::errors::ControlError;
use crate::ids::SessionId;

/// Mediates acquire / wait / release of a named shared resource across sessions.
#[async_trait]
pub trait FileLockMediator: Send + Sync {
    /// Try to acquire `resource` for `session`. Returns true if acquired, false if
    /// it is held by another session (caller should wait/retry).
    async fn try_acquire(&self, session: &SessionId, resource: &str) -> Result<bool, ControlError>;

    /// Release a previously acquired resource.
    async fn release(&self, session: &SessionId, resource: &str) -> Result<(), ControlError>;
}
