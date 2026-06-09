//! Shared "which code editor is frontmost" signal.
//!
//! The Structure (outline) panel lives in a different dock than the editor tabs, so
//! it can't reach the active editor directly. Each [`CodeEditorPanel`] announces
//! itself here when it becomes active (`Panel::set_active`); the Structure panel
//! `cx.observe`s this entity, re-computes the outline for the current file, and
//! (on a symbol click) drives the editor's cursor through the weak handle. UI-local.
//!
//! [`CodeEditorPanel`]: crate::views::panels::code_editor::CodeEditorPanel

use std::path::PathBuf;

use gpui::{SharedString, WeakEntity};
use gpui_component::input::InputState;

/// The frontmost editor's path, text snapshot, and a weak handle to its state.
#[derive(Default)]
pub struct ActiveEditor {
    path: Option<PathBuf>,
    text: SharedString,
    editor: Option<WeakEntity<InputState>>,
}

impl ActiveEditor {
    /// Record the now-active editor. The caller must `cx.notify()` so observers
    /// (the Structure panel) re-render.
    pub fn set(&mut self, path: PathBuf, text: SharedString, editor: WeakEntity<InputState>) {
        self.path = Some(path);
        self.text = text;
        self.editor = Some(editor);
    }

    /// Update just the active editor's text snapshot (path + handle unchanged), for
    /// live outline refresh while the buffer is edited. Caller must `cx.notify()`.
    pub fn set_text(&mut self, text: SharedString) {
        self.text = text;
    }

    pub fn path(&self) -> Option<&PathBuf> {
        self.path.as_ref()
    }

    pub fn text(&self) -> &SharedString {
        &self.text
    }

    /// Weak handle to the active editor's `InputState`, for cursor navigation.
    pub fn editor(&self) -> Option<&WeakEntity<InputState>> {
        self.editor.as_ref()
    }
}
