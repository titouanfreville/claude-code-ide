//! Per-space open-tab persistence (the "reopen what was open at shutdown" sidecar).
//!
//! The dock layout (`layout.json`) records the *shape* of the docks, but its center
//! tabs are not attributed to a project-space, so the workspace deliberately drops
//! them on restore (see [`crate::views::workspace`]). This sidecar fills that gap: it
//! records, per space, the dedup **keys** of the center tabs that were open
//! (`session:<id>`, `file:<path>`, `db:<path>`, `plan:<id>`, `review:<id>` — the same
//! keys [`crate::views::center_requests::OpenRequest::key`] produces). On startup the
//! workspace rebuilds its per-space tab tracking from this file: the active space's
//! tabs are reopened (+ resumed) eagerly, the rest lazily on first switch.
//!
//! Stored next to the dock layout under Application Support. Best-effort and
//! version-gated: a missing/old/corrupt file simply yields no restored tabs.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Bump when the sidecar shape changes incompatibly; a mismatched file is ignored
/// (the app then starts with a clean center, same as no file).
pub const OPEN_TABS_VERSION: u32 = 2;

/// The full set of open center tabs at shutdown, grouped by space.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct OpenTabsState {
    pub version: u32,
    /// The space that was active at shutdown (`None` = the overview). Stored for
    /// robustness; the live restore decides eager-vs-lazy off the workspace's own
    /// `current_space`.
    pub active: Option<String>,
    pub spaces: Vec<SpaceTabs>,
}

/// One space's open tabs. `space` is the [`crate::views::project_space::SpaceId`]
/// string (its root path), or `None` for the overview space.
#[derive(Debug, Serialize, Deserialize)]
pub struct SpaceTabs {
    pub space: Option<String>,
    /// Ordered dedup keys of the open center tabs (best-effort order).
    pub tabs: Vec<String>,
    /// The dedup key of the **session** tab that was last frontmost in this space
    /// (`session:<id>`), restored as the active tab on return. `None` when no session
    /// tab was active. Defaulted so a v1 file (pre-field) still deserializes.
    #[serde(default)]
    pub active_tab: Option<String>,
}

/// The open-tabs sidecar, beside `layout.json` in the platform state directory.
fn path() -> Option<PathBuf> {
    crate::support::support_path("open_tabs.json")
}

/// Load the sidecar, or `None` when it is missing, unreadable, unparseable, or its
/// version no longer matches (the caller then restores no tabs).
pub fn load() -> Option<OpenTabsState> {
    let json = std::fs::read_to_string(path()?).ok()?;
    let state: OpenTabsState = serde_json::from_str(&json).ok()?;
    (state.version == OPEN_TABS_VERSION).then_some(state)
}

/// Persist the sidecar (creating its directory). Best-effort: logs and returns on
/// any failure rather than disturbing app quit.
pub fn save(state: &OpenTabsState) {
    let Some(path) = path() else {
        return; // No home dir → nothing to persist.
    };
    if let Some(parent) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            tracing::warn!(error = %err, "failed to create open-tabs dir");
            return;
        }
    }
    match serde_json::to_string_pretty(state) {
        Ok(json) => {
            if let Err(err) = std::fs::write(&path, json) {
                tracing::warn!(error = %err, "failed to save open tabs");
            }
        }
        Err(err) => tracing::warn!(error = %err, "failed to serialize open tabs"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json_including_overview() {
        let state = OpenTabsState {
            version: OPEN_TABS_VERSION,
            active: Some("/repo/a".to_string()),
            spaces: vec![
                SpaceTabs {
                    space: Some("/repo/a".to_string()),
                    tabs: vec![
                        "session:abc".to_string(),
                        "file:/repo/a/main.rs".to_string(),
                    ],
                    active_tab: Some("session:abc".to_string()),
                },
                SpaceTabs {
                    space: None,
                    tabs: vec!["db:/tmp/x.sqlite".to_string()],
                    active_tab: None,
                },
            ],
        };

        let json = serde_json::to_string(&state).unwrap();
        let back: OpenTabsState = serde_json::from_str(&json).unwrap();

        assert_eq!(back.version, OPEN_TABS_VERSION);
        assert_eq!(back.active.as_deref(), Some("/repo/a"));
        assert_eq!(back.spaces.len(), 2);
        assert_eq!(back.spaces[0].space.as_deref(), Some("/repo/a"));
        assert_eq!(back.spaces[0].tabs.len(), 2);
        assert_eq!(back.spaces[0].active_tab.as_deref(), Some("session:abc"));
        assert_eq!(back.spaces[1].active_tab, None);
        assert_eq!(back.spaces[1].space, None);
        assert_eq!(back.spaces[1].tabs, vec!["db:/tmp/x.sqlite".to_string()]);
    }

    #[test]
    fn version_gate_rejects_mismatch() {
        let json = r#"{"version":999,"active":null,"spaces":[]}"#;
        let state: OpenTabsState = serde_json::from_str(json).unwrap();
        assert_ne!(state.version, OPEN_TABS_VERSION);
    }
}
