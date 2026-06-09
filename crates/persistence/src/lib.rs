//! Persistence adapter: a SQLite-backed [`Store`] implementing the domain
//! [`ManagedSessionStore`](moonlight_domain::ports::ManagedSessionStore) port —
//! managed-session identity (restart-survivable) plus an append-only audit log of
//! autonomous actions. Forward-only migrations live in [`migrations`] and are run
//! on open. The store is synchronous (local SQLite) and `Send + Sync`, so it can
//! be shared as an `Arc<dyn ManagedSessionStore>` across the engine and UI.

mod migrations;
mod store;

pub use store::Store;
