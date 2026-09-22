//! UI read-model for Claude-observability — a `SessionId → SessionObs` map the
//! [`Workspace`](super::workspace) polls from disk (`<support>/obs/`, written by the
//! `moonlight statusline` subcommand — see [`crate::obs`]) and the bottom status bar
//! reads for the selected session. A tiny shared [`Entity`](gpui::Entity) in
//! [`ShellDeps`](super::workspace::ShellDeps); never crosses the engine bus.

use std::collections::HashMap;

use moonlight_domain::ids::SessionId;

use crate::obs::{Quota, SessionObs};

/// Latest per-session observability snapshots (keyed by session id) + the global
/// account usage quota (5h / weekly / Sonnet-weekly), polled from disk.
#[derive(Default)]
pub struct ObsStore {
    sessions: HashMap<SessionId, SessionObs>,
    quota: Option<Quota>,
}

impl ObsStore {
    /// Replace the map from a fresh disk scan ([`crate::obs::load_all`]). Returns
    /// whether anything changed, so the caller only `cx.notify()`s on real updates.
    pub fn apply(&mut self, loaded: Vec<(String, SessionObs)>) -> bool {
        let next: HashMap<SessionId, SessionObs> = loaded
            .into_iter()
            .map(|(id, obs)| (SessionId::new(id), obs))
            .collect();
        if next == self.sessions {
            return false;
        }
        self.sessions = next;
        true
    }

    /// Replace the account quota (from [`crate::obs::quota`]). Returns whether it
    /// changed.
    pub fn set_quota(&mut self, quota: Option<Quota>) -> bool {
        if quota == self.quota {
            return false;
        }
        self.quota = quota;
        true
    }

    /// The snapshot for `id`, if one has been observed.
    pub fn get(&self, id: &SessionId) -> Option<&SessionObs> {
        self.sessions.get(id)
    }

    /// The latest account usage quota, if known.
    pub fn quota(&self) -> Option<&Quota> {
        self.quota.as_ref()
    }
}
