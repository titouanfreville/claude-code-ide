//! Code-editor panel — opens a file beside the session monitor (center dock tab).
//!
//! Built on `gpui-component`'s code editor (`InputState::code_editor`) so it gets
//! syntax highlighting (built-in tree-sitter grammars), line numbers, scrolling,
//! and ⌘F search for free.
//!
//! **Editability is phase-gated** by the shared [`EditGate`](crate::views::edit_gate::EditGate): a file is read-only
//! exactly while the focused session is implementing it (`AutoImplement` + the file
//! under the session's root), and editable + saveable otherwise. Edits write back to
//! disk on **⌘S**. Loads are bounded/defensive (oversized or binary files show a
//! notice).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    actions, div, App, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable,
    MouseDownEvent, Subscription, WeakEntity, Window,
};
use gpui_component::dock::{Panel, PanelEvent, PanelInfo, PanelState, PanelView, TabPanel};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::PopupMenu;
use gpui_component::Placement;

use super::CloseTab;
use crate::views::active_context::{ActiveContext, Eol, Indent};
use crate::views::editor_commands::EditorCommand;
use crate::views::theme;
use crate::views::workspace::ShellDeps;

actions!(
    moonlight_editor,
    [
        SaveFile,
        FormatDocument,
        SplitRight,
        SplitLeft,
        SplitUp,
        SplitDown,
        CopyPath
    ]
);

/// Skip files larger than this (avoid stalling the UI / huge buffers).
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Idle delay before an edit refreshes the outline, so fast typing doesn't re-parse /
/// re-query the language server per keystroke (it settles once typing pauses).
const OUTLINE_DEBOUNCE: Duration = Duration::from_millis(200);
/// How many leading bytes to scan for a NUL when sniffing binary content.
const BINARY_SNIFF_BYTES: usize = 8192;

pub struct CodeEditorPanel {
    path: PathBuf,
    /// The code editor state, or `None` when the file couldn't be shown (`error`).
    state: Option<Entity<InputState>>,
    /// Why the file isn't shown (too large / binary / unreadable), if applicable.
    error: Option<String>,
    /// Whether the operator may currently edit (from the phase `EditGate`).
    editable: bool,
    /// Unsaved edits pending a ⌘S write.
    dirty: bool,
    /// Whether this is the frontmost center tab (set in `set_active`). Gates the
    /// status-bar context announces so a background editor never steals the bar.
    active: bool,
    /// Detected line-ending + indent of the buffer (shown in the status bar),
    /// refreshed on every text change.
    eol: Eol,
    indent: Indent,
    /// Last caret `(line, column)` pushed to the status bar, to dedupe announces
    /// (the editor's `InputState` notifies on every cursor move).
    last_caret: Option<(usize, usize)>,
    /// Fallback focus target for the error state (the editor owns focus otherwise).
    focus_handle: FocusHandle,
    /// The tab panel this editor lives in (captured in [`Panel::on_added_to`]), so
    /// the tab bar's "×" can close this file — see [`super::tab_title`].
    tab_panel: Option<WeakEntity<TabPanel>>,
    /// Open right-click tab menu, rendered from the panel body (see [`super::TabMenuHost`]).
    tab_menu: Option<super::TabMenu>,
    /// Generation counter for the debounced outline push: each edit bumps it, and only
    /// the timer whose captured generation still matches gets to refresh the outline
    /// (coalesces fast typing into one refresh per pause).
    outline_gen: u32,
    /// Holds the gate-observe + input-change subscriptions for the tab's lifetime.
    _subs: Vec<Subscription>,
}

impl CodeEditorPanel {
    /// Open `path` in a fresh editor tab.
    pub fn open(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Status-bar metadata, detected once from the buffer (refreshed on format).
        let mut eol = Eol::Lf;
        let mut indent = Indent::Spaces(4);
        let (state, error) = match read_text(&path) {
            Ok(text) => {
                eol = Eol::detect(&text);
                indent = Indent::detect(&text);
                let language = lang_from_ext(&path);
                let state = cx.new(|cx| {
                    InputState::new(window, cx)
                        .code_editor(language)
                        .line_number(true)
                        .soft_wrap(false)
                });
                state.update(cx, |s, cx| s.set_value(text, window, cx));
                (Some(state), None)
            }
            Err(e) => (None, Some(e)),
        };

        // Editability follows the focused session's phase via the shared gate.
        let gate = cx.try_global::<ShellDeps>().map(|d| d.edit_gate.clone());
        let editable = gate
            .as_ref()
            .map(|g| g.read(cx).editable_for(&path))
            .unwrap_or(true);

        let mut subs = Vec::new();
        if let Some(gate) = gate {
            subs.push(cx.observe(&gate, |this, gate, cx| {
                let now = gate.read(cx).editable_for(&this.path);
                if now != this.editable {
                    this.editable = now;
                    cx.notify();
                }
            }));
        }
        if let Some(state) = state.as_ref() {
            subs.push(cx.subscribe(state, |this, _state, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    if !this.dirty {
                        this.dirty = true;
                        cx.notify();
                    }
                    // Refresh the outline as the frontmost buffer is edited (debounced).
                    if this.active {
                        this.schedule_outline_push(cx);
                    }
                }
            }));
            // Caret movement: `InputState` notifies on every cursor move (its blink
            // lives in a separate entity, so this does NOT fire on blink). While this
            // tab is frontmost, push the new caret to the status bar, deduped.
            subs.push(cx.observe(state, |this, state, cx| {
                if !this.active {
                    return;
                }
                let pos = state.read(cx).cursor_position();
                let caret = (pos.line as usize + 1, pos.character as usize + 1);
                if this.last_caret != Some(caret) {
                    this.announce_context(cx);
                }
            }));
        }

        // Status-bar metadata commands (e.g. an EOL click) targeted at the active
        // editor — only the frontmost tab acts (the dock keeps one active per panel).
        if let Some(ec) = cx
            .try_global::<ShellDeps>()
            .map(|d| d.editor_commands.clone())
        {
            subs.push(cx.subscribe(&ec, |this, _ec, cmd: &EditorCommand, cx| {
                if !this.active {
                    return;
                }
                match cmd {
                    EditorCommand::SetEol(eol) => this.set_eol(*eol, cx),
                }
            }));
        }

        Self {
            path,
            state,
            error,
            editable,
            dirty: false,
            active: false,
            eol,
            indent,
            last_caret: None,
            focus_handle: cx.focus_handle(),
            tab_panel: None,
            tab_menu: None,
            outline_gen: 0,
            _subs: subs,
        }
    }

    /// Rebuild from a persisted layout: open the stashed path, or show a notice
    /// when none was recorded (keeps a reloaded tab benign instead of invalid).
    pub fn restore(path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        match path {
            Some(path) => Self::open(path, window, cx),
            None => Self {
                path: PathBuf::new(),
                state: None,
                error: Some("no file".to_string()),
                editable: false,
                dirty: false,
                active: false,
                eol: Eol::Lf,
                indent: Indent::Spaces(4),
                last_caret: None,
                focus_handle: cx.focus_handle(),
                tab_panel: None,
                tab_menu: None,
                outline_gen: 0,
                _subs: Vec::new(),
            },
        }
    }

    /// Write the buffer back to disk (⌘S). No-op when clean or unshowable.
    fn save(&mut self, _: &SaveFile, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.dirty {
            return;
        }
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let text = state.read(cx).value();
        let body = self.eol.apply(&text);
        match std::fs::write(&self.path, body.as_bytes()) {
            Ok(()) => {
                self.dirty = false;
                cx.notify();
                tracing::info!(path = %self.path.display(), "saved file");
            }
            Err(err) => {
                tracing::warn!(error = %err, path = %self.path.display(), "save failed");
            }
        }
    }

    /// Reformat the file with its language formatter (rustfmt/prettier/…). Gated to
    /// editable files; flushes the buffer first so the formatter sees current text,
    /// runs in place, then reloads. No-op (logged) when read-only or no formatter.
    fn format_document(&mut self, _: &FormatDocument, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editable {
            tracing::info!(path = %self.path.display(), "format skipped: read-only");
            return;
        }
        let Some(state) = self.state.clone() else {
            return;
        };
        // Flush the current buffer so the on-disk file the formatter rewrites is current.
        let text = state.read(cx).value();
        if let Err(err) = std::fs::write(&self.path, text.as_bytes()) {
            tracing::warn!(error = %err, path = %self.path.display(), "format: pre-write failed");
            return;
        }
        let Some(mut cmd) = formatter_command(&self.path) else {
            tracing::info!(path = %self.path.display(), "no formatter for this file type");
            return;
        };
        match cmd.output() {
            Ok(out) if out.status.success() => match std::fs::read_to_string(&self.path) {
                Ok(formatted) => {
                    self.eol = Eol::detect(&formatted);
                    self.indent = Indent::detect(&formatted);
                    state.update(cx, |s, cx| s.set_value(formatted, window, cx));
                    self.dirty = false;
                    if self.active {
                        self.announce_context(cx);
                    }
                    cx.notify();
                    tracing::info!(path = %self.path.display(), "formatted");
                }
                Err(err) => tracing::warn!(error = %err, "format: reload failed"),
            },
            Ok(out) => tracing::warn!(
                stderr = %String::from_utf8_lossy(&out.stderr),
                "formatter exited non-zero"
            ),
            Err(err) => tracing::warn!(error = %err, "formatter failed to run"),
        }
    }

    /// Open a second view of this file in a new pane split off toward `placement`
    /// — the menu equivalent of dragging the tab to that edge. Duplicates the file
    /// (VS Code "Split" semantics) rather than moving the tab, so the original pane
    /// stays put. No-op for an unshowable (error) tab, which has nothing to split.
    fn split(&mut self, placement: Placement, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.is_none() {
            return;
        }
        let Some(tab_panel) = self.tab_panel.as_ref().and_then(|w| w.upgrade()) else {
            return;
        };
        let path = self.path.clone();
        let new_panel: Arc<dyn PanelView> = Arc::new(cx.new(|cx| Self::open(path, window, cx)));
        tab_panel.update(cx, |tp, cx| {
            tp.add_panel_at(new_panel, placement, None, window, cx);
        });
    }

    fn split_right(&mut self, _: &SplitRight, window: &mut Window, cx: &mut Context<Self>) {
        self.split(Placement::Right, window, cx);
    }

    fn split_left(&mut self, _: &SplitLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.split(Placement::Left, window, cx);
    }

    fn split_up(&mut self, _: &SplitUp, window: &mut Window, cx: &mut Context<Self>) {
        self.split(Placement::Top, window, cx);
    }

    fn split_down(&mut self, _: &SplitDown, window: &mut Window, cx: &mut Context<Self>) {
        self.split(Placement::Bottom, window, cx);
    }

    fn copy_path(&mut self, _: &CopyPath, _window: &mut Window, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.path.display().to_string()));
    }

    fn file_name(&self) -> String {
        self.path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| self.path.display().to_string())
    }

    /// Push this editor's path + caret + detected metadata to the shared
    /// [`ActiveContext`](crate::views::active_context::ActiveContext) so the bottom
    /// status bar reflects it. Cheap — reads only the cursor position, not the
    /// buffer. No-op for an unshowable (error) tab.
    fn announce_context(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let pos = state.read(cx).cursor_position();
        let line = pos.line as usize + 1;
        let column = pos.character as usize + 1;
        self.last_caret = Some((line, column));
        let Some(ac) = cx
            .try_global::<ShellDeps>()
            .map(|d| d.active_context.clone())
        else {
            return;
        };
        let ctx = ActiveContext::File {
            path: self.path.clone(),
            line,
            column,
            eol: self.eol,
            indent: self.indent,
        };
        ac.update(cx, |a, cx| {
            *a = ctx;
            cx.notify();
        });
    }

    /// Schedule a debounced outline refresh after the buffer settles. Each edit bumps
    /// `outline_gen`; only the timer whose captured generation is still current (no edit
    /// landed after it) pushes, so fast typing collapses to one refresh per pause.
    fn schedule_outline_push(&mut self, cx: &mut Context<Self>) {
        self.outline_gen = self.outline_gen.wrapping_add(1);
        let generation = self.outline_gen;
        cx.spawn(async move |weak, cx| {
            cx.background_executor().timer(OUTLINE_DEBOUNCE).await;
            let _ = weak.update(cx, |this, cx| {
                if this.active && this.outline_gen == generation {
                    this.push_outline(cx);
                }
            });
        })
        .detach();
    }

    /// Push the current buffer text into the shared `ActiveEditor` so the Structure
    /// panel re-outlines it live (text-only — path + handle are bound on activation).
    fn push_outline(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let Some(ae) = cx
            .try_global::<ShellDeps>()
            .map(|d| d.active_editor.clone())
        else {
            return;
        };
        let text = state.read(cx).value();
        ae.update(cx, |a, cx| {
            a.set_text(text);
            cx.notify();
        });
    }

    /// Set the file's line-ending as a save-time attribute (status-bar EOL click):
    /// flip [`Self::eol`] and mark dirty so the next ⌘S rewrites with the new ending.
    /// The buffer text is left untouched (EOL is a file attribute, not buffer content).
    fn set_eol(&mut self, eol: Eol, cx: &mut Context<Self>) {
        if self.state.is_none() || self.eol == eol {
            return;
        }
        self.eol = eol;
        self.dirty = true;
        self.announce_context(cx);
        cx.notify();
    }
}

/// Build the in-place formatter command for a file, or `None` if none is known.
/// (Formatters are optional external tools; absence degrades gracefully.)
fn formatter_command(path: &Path) -> Option<Command> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let cmd = match ext.as_str() {
        "rs" => {
            let mut c = Command::new("rustfmt");
            c.arg(path);
            c
        }
        "go" => {
            let mut c = Command::new("gofmt");
            c.arg("-w").arg(path);
            c
        }
        "py" => {
            let mut c = Command::new("black");
            c.arg("-q").arg(path);
            c
        }
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "json" | "css" | "scss" | "less" | "html"
        | "htm" | "md" | "markdown" | "yaml" | "yml" => {
            let mut c = Command::new("prettier");
            c.arg("--write").arg(path);
            c
        }
        _ => return None,
    };
    Some(cmd)
}

/// Read a file for display, rejecting oversized or binary content.
fn read_text(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!("file too large ({} KB)", meta.len() / 1024));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.iter().take(BINARY_SNIFF_BYTES).any(|&b| b == 0) {
        return Err("binary file".to_string());
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Map a file extension to a gpui-component built-in language name. Unknown
/// extensions fall back to plain text (`"text"`), which highlights nothing.
fn lang_from_ext(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => "rust",
        "go" => "go",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "ts" => "typescript",
        "tsx" => "tsx",
        "py" => "python",
        "rb" => "ruby",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        "scala" | "sc" => "scala",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => "cpp",
        "cs" => "csharp",
        "ex" | "exs" => "elixir",
        "lua" => "lua",
        "php" => "php",
        "zig" => "zig",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "sql" => "sql",
        "graphql" | "gql" => "graphql",
        "proto" => "proto",
        "html" | "htm" => "html",
        "css" | "scss" | "less" => "css",
        "svelte" => "svelte",
        "astro" => "astro",
        "sh" | "bash" | "zsh" => "bash",
        "cmake" => "cmake",
        "diff" | "patch" => "diff",
        "md" | "markdown" => "markdown",
        _ => "text",
    }
}

impl Focusable for CodeEditorPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        // Route focus into the editor when present, so an activated tab takes keys.
        self.state
            .as_ref()
            .map(|s| s.read(cx).focus_handle(cx))
            .unwrap_or_else(|| self.focus_handle.clone())
    }
}

impl super::TabMenuHost for CodeEditorPanel {
    fn tab_menu_slot(&mut self) -> &mut Option<super::TabMenu> {
        &mut self.tab_menu
    }
}

impl EventEmitter<PanelEvent> for CodeEditorPanel {}

impl Panel for CodeEditorPanel {
    fn panel_name(&self) -> &'static str {
        "CodeEditor"
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut label = self.file_name();
        if self.dirty {
            label = format!("● {label}");
        }
        if !self.editable {
            label = format!("{label} · read-only");
        }
        // Splittable (real file) tabs get Split entries; error tabs have nothing to split.
        let splittable = self.state.is_some();
        super::tab_title(
            label,
            None,
            cx.entity_id().as_u64(),
            self.tab_panel.clone(),
            Arc::new(cx.entity()),
            self.focus_handle(cx),
            cx,
            move |menu| {
                let menu = menu.separator().menu("Copy Path", Box::new(CopyPath));
                if splittable {
                    menu.separator()
                        .menu("Split Right", Box::new(SplitRight))
                        .menu("Split Down", Box::new(SplitDown))
                } else {
                    menu
                }
            },
        )
    }

    /// Capture the tab panel so the tab bar's "×" can close this file.
    fn on_added_to(
        &mut self,
        tab_panel: WeakEntity<TabPanel>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.tab_panel = Some(tab_panel);
    }

    /// Add "Split …" entries to the tab's "…" secondary menu (gpui-component renders
    /// this dropdown in the tab bar). The split actions are routed to *this* panel
    /// via `action_context` so the `on_action` handlers in `render` pick them up;
    /// the dock's own Zoom/Close items still bubble up to the parent `TabPanel`.
    /// Skipped for an unshowable (error) tab, which has nothing to split.
    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        if self.state.is_none() {
            return menu;
        }
        menu.action_context(self.focus_handle(cx))
            .menu("Split Right", Box::new(SplitRight))
            .menu("Split Left", Box::new(SplitLeft))
            .menu("Split Down", Box::new(SplitDown))
            .menu("Split Up", Box::new(SplitUp))
    }

    /// When this tab becomes frontmost, announce it to the shared `ActiveEditor`
    /// so the Structure panel outlines this file and can drive its cursor.
    fn set_active(&mut self, active: bool, _window: &mut Window, cx: &mut Context<Self>) {
        self.active = active;
        if !active {
            return;
        }
        // Announce to the Structure panel (ActiveEditor) so it outlines this file.
        if let (Some(state), Some(ae)) = (
            self.state.clone(),
            cx.try_global::<ShellDeps>()
                .map(|d| d.active_editor.clone()),
        ) {
            let path = self.path.clone();
            let text = state.read(cx).value();
            let weak = state.downgrade();
            ae.update(cx, |a, cx| {
                a.set(path, text, weak);
                cx.notify();
            });
        }
        // Announce to the bottom status bar (ActiveContext).
        self.announce_context(cx);
    }

    /// Persist which file this tab shows, so a saved layout can reopen it.
    fn dump(&self, _cx: &App) -> PanelState {
        let mut state = PanelState::new(self);
        state.info = PanelInfo::panel(serde_json::json!({
            "path": self.path.to_string_lossy(),
        }));
        state
    }
}

impl Render for CodeEditorPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editable = self.editable;
        let dismiss = cx.listener(|this, _: &MouseDownEvent, _w, cx| {
            this.tab_menu = None;
            cx.notify();
        });
        div()
            .key_context("CodeEditor")
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::format_document))
            .on_action(cx.listener(Self::split_right))
            .on_action(cx.listener(Self::split_left))
            .on_action(cx.listener(Self::split_up))
            .on_action(cx.listener(Self::split_down))
            .on_action(cx.listener(Self::copy_path))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                super::close_this_tab(&this.tab_panel, cx.entity(), window, cx)
            }))
            .size_full()
            .bg(theme::surface_base())
            .when_some(self.error.clone(), |d, err| {
                d.child(
                    div()
                        .p_3()
                        .text_color(theme::text_muted())
                        .child(format!("{}: {err}", self.file_name())),
                )
            })
            .when_some(self.state.clone(), |d, state| {
                d.child(
                    Input::new(&state)
                        .disabled(!editable)
                        .h_full()
                        // Right-click *in the editor body*: a richer menu than the tab's.
                        // The Input dispatches every item to its own focus handle, which
                        // bubbles to this panel's render root — so the native find/clipboard
                        // actions AND our Split*/CopyPath `on_action` handlers all fire.
                        .context_menu(|menu, _window, _cx| {
                            use gpui_component::input;
                            menu.menu("Find…", Box::new(input::Search))
                                .separator()
                                .menu("Cut", Box::new(input::Cut))
                                .menu("Copy", Box::new(input::Copy))
                                .menu("Paste", Box::new(input::Paste))
                                .menu("Select All", Box::new(input::SelectAll))
                                .separator()
                                .menu("Split Right", Box::new(SplitRight))
                                .menu("Split Down", Box::new(SplitDown))
                                .separator()
                                .menu("Copy Path", Box::new(CopyPath))
                        }),
                )
            })
            .children(super::tab_menu_overlay(
                self.tab_menu.as_ref(),
                dismiss,
                window,
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn lang_from_ext_maps_known_extensions() {
        assert_eq!(lang_from_ext(Path::new("a/b/main.rs")), "rust");
        assert_eq!(lang_from_ext(Path::new("x.tsx")), "tsx");
        assert_eq!(lang_from_ext(Path::new("y.ts")), "typescript");
        assert_eq!(lang_from_ext(Path::new("s.py")), "python");
        assert_eq!(lang_from_ext(Path::new("data.JSON")), "json");
        assert_eq!(lang_from_ext(Path::new("README.md")), "markdown");
        assert_eq!(lang_from_ext(Path::new("Makefile")), "text");
        assert_eq!(lang_from_ext(Path::new("noext")), "text");
    }

    #[test]
    fn read_text_rejects_binary_and_reads_text() {
        let dir = std::env::temp_dir().join(format!("mlc-editor-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);

        let text_file = dir.join("hello.txt");
        fs::write(&text_file, b"hello\nworld\n").unwrap();
        assert_eq!(read_text(&text_file).unwrap(), "hello\nworld\n");

        let bin_file = dir.join("blob.bin");
        fs::write(&bin_file, [0x00, 0x01, 0x02, 0x00]).unwrap();
        assert_eq!(read_text(&bin_file).unwrap_err(), "binary file");

        let _ = fs::remove_dir_all(&dir);
    }
}
