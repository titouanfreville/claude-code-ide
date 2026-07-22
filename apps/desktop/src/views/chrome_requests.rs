//! Cross-panel "hide this tool window" channel — the chrome twin of
//! [`center_requests`](super::center_requests).
//!
//! Tool windows (file tree, structure, terminal, …) render a uniform hide "✕" in
//! their header, but dock open-state lives on the [`Workspace`] / `DockArea` —
//! out of a panel's reach. The button **emits** a [`ChromeRequest`] on this shared
//! GPUI [`Entity`] (held in `ShellDeps`); the workspace `subscribe_in`s and flips
//! the matching dock/tool flag (an event hands it the `Window` the dock APIs
//! need). UI-local — never the engine bus.

use gpui::EventEmitter;

/// A chrome-level ask raised from inside a tool window — mostly the uniform header ✕
/// hides, plus the DB tool's request to open the (window-level) Add-Data-Source modal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChromeRequest {
    /// Hide the left dock (Project files + Structure).
    HideLeftDock,
    /// Hide just the Structure outline (the tree keeps the full dock height).
    HideStructure,
    /// Hide the bottom dock (Terminal / Run — whichever tool is frontmost).
    HideBottomDock,
    /// Hide the right dock (DB observer).
    HideRightDock,
    /// Open the Add-Data-Source modal (raised by the DB tool's `+` button; the modal is
    /// hosted by the Workspace so it centers over the whole window, not the narrow dock).
    OpenAddDataSource,
}

/// Event hub for chrome requests. A unit entity whose only job is to relay
/// [`ChromeRequest`]s from tool-window headers to the workspace.
#[derive(Default)]
pub struct ChromeRequests;

impl EventEmitter<ChromeRequest> for ChromeRequests {}
