//! A tiny UI-side registry mapping a session id to its embedded terminal, so a
//! panel that doesn't own the terminal (e.g. the Plan-review tab) can still drive
//! the session's Claude Code TUI — the engine can't reach the embedded PTY, only
//! the UI can. Used to actuate CC's native plan-continuation prompt: on plan
//! approval we send Enter to pick option 1 (accept + auto).
//!
//! Entries are weak, so a closed/dropped session monitor's terminal simply fails to
//! upgrade (no manual de-registration needed). Cloning shares the same registry.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use gpui::WeakEntity;
use moonlight_domain::ids::SessionId;

use super::panels::terminal::TerminalPanel;

#[derive(Clone, Default)]
pub struct SessionIo {
    inner: Arc<Mutex<HashMap<SessionId, WeakEntity<TerminalPanel>>>>,
}

impl SessionIo {
    /// Record (or replace) the embedded terminal for `id`.
    pub fn register(&self, id: SessionId, term: WeakEntity<TerminalPanel>) {
        if let Ok(mut map) = self.inner.lock() {
            map.insert(id, term);
        }
    }

    /// The embedded terminal for `id`, if one is registered and still alive.
    pub fn terminal(&self, id: &SessionId) -> Option<WeakEntity<TerminalPanel>> {
        self.inner.lock().ok()?.get(id).cloned()
    }
}
