//! Cross-panel "open this in the center" channel.
//!
//! The file tree and the session grid live in different dock panels and can't
//! reach the center `DockArea` directly. They instead **emit** an [`OpenRequest`]
//! on this shared GPUI [`Entity`] (held in `ShellDeps`); the [`Workspace`]
//! `subscribe_in`s to it and opens/activates the matching center tab. Using an
//! event (not polled state) hands the workspace a `Window`, which `add_panel`
//! requires. UI-local — never the engine bus.

use std::path::PathBuf;

use gpui::EventEmitter;
use moonlight_domain::agent::AgentKind;
use moonlight_domain::ids::SessionId;
use moonlight_domain::phase::Phase;
use moonlight_domain::session::Session;

use super::panels::db_source::{DataSource, TableMeta};

/// Something the operator asked to open as a center tab.
#[derive(Clone)]
pub enum OpenRequest {
    /// View a file in a code-editor tab.
    File(PathBuf),
    /// Zoom into a session (focus mode); carries the snapshot so the monitor has
    /// initial state before the next bus update.
    Session(Session),
    /// Review a plan the agent proposed (plan-review gate). Carries the plan
    /// markdown so the tab can render it immediately, and the emitting session's repo
    /// `root` so the tab opens in *that* project's space — not whatever is live.
    PlanReview {
        session: SessionId,
        plan: String,
        root: Option<PathBuf>,
    },
    /// Review a session's changes (code-review gate). Carries the repo root so the
    /// tab can load the per-file diff, and the latest assistant `summary` ("what
    /// this covers", T4) if one was observed before review opened.
    CodeReview {
        session: SessionId,
        root: Option<PathBuf>,
        summary: Option<String>,
    },
    /// Start a new **managed session**: open a focus tab whose embedded terminal runs
    /// the chosen `agent` CLI (`claude --session-id <id> …` / `agy …`) in the focused
    /// project (the "＋ Session" action). `phase` is the operator's chosen starting phase
    /// (Plan / Discovery / Auto); its `cc_permission_mode()` drives the launch flag.
    /// `agent` selects the backend (`claude` / `agy`). The pinned `id` means the tab and
    /// the grid tile (discovered from JSONL) are the same session, so clicking the tile
    /// later reuses this tab.
    NewManagedSession {
        id: SessionId,
        phase: Phase,
        agent: AgentKind,
    },
    /// Open (or bring to front) a session's focus tab **by id** — the status-bar
    /// notification click-to-navigate. Unlike [`OpenRequest::Session`] the caller only
    /// has the id, so the workspace reconstructs the monitor from the managed store
    /// (resume it if managed, else show its read-only transcript). `root` is the
    /// session's repo (resolved by the caller) so the tab lands in its own space.
    SessionById {
        id: SessionId,
        root: Option<PathBuf>,
    },
    /// Browse a SQLite database file read-only (opens it in the DB overview tool window).
    Db(PathBuf),
    /// Open a table/view's rows as a data-editor center tab (from the DB overview tree).
    DbTable {
        source: DataSource,
        table: TableMeta,
    },
    /// Open a SQL console center tab bound to a data source (from the DB overview tree).
    DbConsole { source: DataSource },
    /// Open the HTTP request builder (Postman-style center tab; one shared tab).
    Http,
}

impl OpenRequest {
    /// Stable identity for dedup: re-opening the same file/session activates its
    /// existing tab instead of spawning a duplicate.
    pub fn key(&self) -> String {
        match self {
            OpenRequest::File(path) => format!("file:{}", path.display()),
            OpenRequest::Session(s) => format!("session:{}", s.id.as_str()),
            OpenRequest::PlanReview { session, .. } => format!("plan:{}", session.as_str()),
            OpenRequest::CodeReview { session, .. } => format!("review:{}", session.as_str()),
            // Same key as `Session` so the discovered tile reuses the managed tab.
            OpenRequest::NewManagedSession { id, .. } => format!("session:{}", id.as_str()),
            // Same key as `Session` so click-to-navigate reuses an already-open tab.
            OpenRequest::SessionById { id, .. } => format!("session:{}", id.as_str()),
            OpenRequest::Db(path) => format!("db:{}", path.display()),
            OpenRequest::DbTable { source, table } => {
                super::panels::db_grid::DbGridPanel::tab_key(source, table)
            }
            OpenRequest::DbConsole { source } => {
                super::panels::db_console::DbConsolePanel::tab_key(source)
            }
            OpenRequest::Http => super::panels::http_panel::HttpPanel::tab_key().to_string(),
        }
    }

    /// The repo root this request belongs to, for the auto-opened session gates
    /// (plan / code review). Used to open the tab in the emitting session's space
    /// rather than the currently-live one. `None` for non-session tabs.
    pub fn target_root(&self) -> Option<PathBuf> {
        match self {
            OpenRequest::PlanReview { root, .. } => root.clone(),
            OpenRequest::CodeReview { root, .. } => root.clone(),
            OpenRequest::SessionById { root, .. } => root.clone(),
            _ => None,
        }
    }
}

/// Event hub for center-open requests. A unit entity whose only job is to relay
/// [`OpenRequest`]s from rail panels to the workspace.
#[derive(Default)]
pub struct CenterRequests;

impl EventEmitter<OpenRequest> for CenterRequests {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_distinguishes_files_and_sessions() {
        let f = OpenRequest::File(PathBuf::from("/tmp/a.rs"));
        assert_eq!(f.key(), "file:/tmp/a.rs");
    }
}
