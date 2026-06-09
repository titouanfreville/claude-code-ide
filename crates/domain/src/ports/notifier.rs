//! OS notification port (FR5). Batching/DND policy lives in the adapter, not the UI.

use async_trait::async_trait;

use crate::errors::ControlError;
use crate::ids::SessionId;

/// A notification the operator should (maybe) see. The adapter decides whether
/// to surface, batch (DND/focus), or suppress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub session: SessionId,
    pub kind: NotificationKind,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationKind {
    NeedsInput,
    Completed,
    Errored,
}

#[async_trait]
pub trait Notifier: Send + Sync {
    async fn notify(&self, n: &Notification) -> Result<(), ControlError>;
}
