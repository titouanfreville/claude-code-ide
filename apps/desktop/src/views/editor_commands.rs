//! Cross-panel command channel: the bottom [`status_bar`](super::panels::status_bar)
//! → the **frontmost** [`CodeEditorPanel`](super::panels::code_editor::CodeEditorPanel).
//!
//! The status bar holds only the *resolved* [`ActiveContext`](super::active_context)
//! (path, caret, eol, indent) — not a handle to the active editor — so a click on a
//! metadata widget (e.g. EOL) can't reach into the editor directly. Instead the bar
//! emits an [`EditorCommand`] on this shared hub; every `CodeEditorPanel` subscribes
//! and the one that is currently active (`Panel::set_active(true)` — the dock keeps
//! exactly one active per tab panel) handles it. UI-local; never crosses the engine bus.

use gpui::EventEmitter;

use super::active_context::Eol;

/// A command targeted at whichever code editor is currently active.
#[derive(Debug, Clone)]
pub enum EditorCommand {
    /// Set the active editor's line-ending. Applied as a **save-time attribute** — the
    /// buffer text is untouched; the new ending is written on the next ⌘S.
    SetEol(Eol),
}

/// The event hub the status bar emits [`EditorCommand`]s on. A tiny shared
/// [`Entity`](gpui::Entity) in [`ShellDeps`](super::workspace::ShellDeps).
#[derive(Default)]
pub struct EditorCommands;

impl EventEmitter<EditorCommand> for EditorCommands {}
