//! Trait ports — the contracts adapters implement. The engine and UI depend on
//! these, never on concrete adapters.

pub mod control;
pub mod detection;
pub mod locks;
pub mod mcp;
pub mod notifier;
pub mod store;

pub use control::{ControlLevel, ControlPort};
pub use detection::{DetectionEvent, DetectionSource};
pub use locks::FileLockMediator;
pub use mcp::{ActorRequest, ActorResult, McpActor, PermissionRequest, PolicyDecisionPoint};
pub use notifier::{Notification, Notifier};
pub use store::{
    AuditStore, BaselineStore, ManagedSession, ManagedSessionStore, ManagedStateUpdate,
    SessionStore,
};
