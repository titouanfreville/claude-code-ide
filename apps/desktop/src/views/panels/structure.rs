//! Structure panel — a JetBrains-style **collapsible outline** of the active file.
//!
//! The symbol tree comes from the [`outline`](super::outline) provider chain
//! (tree-sitter for supported grammars, keyword heuristic otherwise; the LSP tier is
//! overlaid when available). The panel tracks the frontmost editor via the shared
//! [`ActiveEditor`], renders the hierarchy as an expand/collapse tree, and on a symbol
//! click moves that editor's cursor to the symbol's line. The parsed outline is cached
//! by `(path, text-hash)` so toggling a node doesn't re-parse the buffer.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    div, px, App, Context, Entity, EventEmitter, FocusHandle, Focusable, SharedString, Window,
};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::input::Position;

use super::outline::{self, OutlineLang, Symbol, SymbolKind};
use crate::lsp::LspPool;
use crate::views::active_editor::ActiveEditor;
use crate::views::theme;

/// Memoized outline for one buffer snapshot, so re-renders (e.g. collapse toggles)
/// don't re-parse unchanged text.
struct OutlineCache {
    path: PathBuf,
    text_hash: u64,
    symbols: Vec<Symbol>,
}

pub struct StructurePanel {
    active_editor: Option<Entity<ActiveEditor>>,
    focus_handle: FocusHandle,
    /// Ids of collapsed nodes (default: everything expanded).
    collapsed: HashSet<u64>,
    cache: Option<OutlineCache>,
    /// Pool of language servers for the richest outline tier.
    lsp_pool: Arc<LspPool>,
    /// The latest LSP outline and the `(path, text-hash)` it was computed for; shown in
    /// place of the tree-sitter baseline while it matches the active buffer.
    lsp: Option<(PathBuf, u64, Vec<Symbol>)>,
    /// The `(path, text-hash)` of an in-flight LSP request, to avoid duplicate spawns.
    lsp_inflight: Option<(PathBuf, u64)>,
}

/// A flattened, render-ready outline row (collapsed subtrees omitted).
struct Row {
    id: u64,
    depth: usize,
    kind: SymbolKind,
    name: String,
    line: usize,
    has_children: bool,
    collapsed: bool,
}

impl StructurePanel {
    pub fn new(active_editor: Option<Entity<ActiveEditor>>, cx: &mut Context<Self>) -> Self {
        if let Some(ae) = active_editor.as_ref() {
            // On a new active file, kick off its LSP request and re-render.
            cx.observe(ae, |this, _ae, cx| {
                this.request_lsp(cx);
                cx.notify();
            })
            .detach();
        }
        Self {
            active_editor,
            focus_handle: cx.focus_handle(),
            collapsed: HashSet::new(),
            cache: None,
            // The shared shell pool when available, so this panel's servers are the
            // ones feeding the Problems window too; own pool in static/test views.
            lsp_pool: cx
                .try_global::<crate::views::workspace::ShellDeps>()
                .map(|d| d.lsp_pool.clone())
                .unwrap_or_else(|| Arc::new(LspPool::new())),
            lsp: None,
            lsp_inflight: None,
        }
    }

    /// The active file's `(path, text-hash)` key, or `None` when no editor is active.
    fn current_key(&self, cx: &App) -> Option<(PathBuf, u64)> {
        let ae = self.active_editor.as_ref()?.read(cx);
        let path = ae.path()?.clone();
        Some((path, hash_str(ae.text())))
    }

    /// The symbols to display: the LSP outline when it matches the active buffer, else
    /// the tree-sitter baseline.
    fn displayed_symbols(&mut self, cx: &App) -> Vec<Symbol> {
        if let (Some((path, hash)), Some((lp, lh, syms))) = (self.current_key(cx), &self.lsp) {
            if *lp == path && *lh == hash {
                return syms.clone();
            }
        }
        self.outline(cx)
    }

    /// Ask the language server for `documentSymbol` on the active buffer (off the UI
    /// thread) and overlay the result. No-op when a server-less language, already have
    /// the result, or a matching request is in flight.
    fn request_lsp(&mut self, cx: &mut Context<Self>) {
        let Some((path, hash)) = self.current_key(cx) else {
            return;
        };
        let lang = outline::outline_lang(&path);
        if matches!(lang, OutlineLang::Other) {
            return; // no LSP server mapping for unknown languages
        }
        let key = (path.clone(), hash);
        if self
            .lsp
            .as_ref()
            .is_some_and(|(p, h, _)| *p == key.0 && *h == key.1)
        {
            return; // already have this outline
        }
        if self.lsp_inflight.as_ref() == Some(&key) {
            return; // a request for this exact buffer is already running
        }
        let Some(text) = self
            .active_editor
            .as_ref()
            .map(|ae| ae.read(cx).text().to_string())
        else {
            return;
        };

        self.lsp_inflight = Some(key);
        let pool = self.lsp_pool.clone();
        let req_path = path.clone();
        cx.spawn(async move |weak, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { pool.document_symbols(&req_path, &text, lang) })
                .await;
            let _ = weak.update(cx, |this, cx| {
                this.lsp_inflight = None;
                // Keep the tree-sitter baseline if the server returned nothing (e.g.
                // still warming up) rather than flashing an empty outline.
                if let Some(syms) = result.filter(|s| !s.is_empty()) {
                    this.lsp = Some((path, hash, syms));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The outline for the active file, recomputed only when the path or text changes.
    fn outline(&mut self, cx: &App) -> Vec<Symbol> {
        let Some(ae) = self.active_editor.as_ref() else {
            return Vec::new();
        };
        let ae = ae.read(cx);
        let Some(path) = ae.path().cloned() else {
            return Vec::new();
        };
        let text = ae.text();
        let text_hash = hash_str(text);
        if let Some(c) = &self.cache {
            if c.path == path && c.text_hash == text_hash {
                return c.symbols.clone();
            }
        }
        let symbols = outline::symbols(outline::outline_lang(&path), text);
        self.cache = Some(OutlineCache {
            path,
            text_hash,
            symbols: symbols.clone(),
        });
        symbols
    }

    /// Toggle a node's collapsed state.
    fn toggle(&mut self, id: u64, cx: &mut Context<Self>) {
        if !self.collapsed.insert(id) {
            self.collapsed.remove(&id);
        }
        cx.notify();
    }

    /// Jump the active editor's cursor to `line` (best-effort via the weak handle).
    fn goto(&self, line: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ae) = self.active_editor.as_ref() else {
            return;
        };
        let Some(weak) = ae.read(cx).editor().cloned() else {
            return;
        };
        if let Some(state) = weak.upgrade() {
            state.update(cx, |s, cx| {
                s.set_cursor_position(
                    Position {
                        line: line as u32,
                        character: 0,
                    },
                    window,
                    cx,
                );
            });
        }
    }
}

/// Flatten the symbol tree into render rows, skipping collapsed subtrees. A node's id
/// is a stable hash of its ancestor-name path, so collapse state survives re-parses and
/// line shifts.
fn flatten(
    syms: &[Symbol],
    depth: usize,
    parent_sig: &str,
    collapsed: &HashSet<u64>,
    out: &mut Vec<Row>,
) {
    for s in syms {
        let sig = format!("{parent_sig}/{}:{}", s.kind.label(), s.name);
        let id = hash_str(&sig);
        let has_children = !s.children.is_empty();
        let is_collapsed = collapsed.contains(&id);
        out.push(Row {
            id,
            depth,
            kind: s.kind,
            name: s.name.clone(),
            line: s.line,
            has_children,
            collapsed: is_collapsed,
        });
        if has_children && !is_collapsed {
            flatten(&s.children, depth + 1, &sig, collapsed, out);
        }
    }
}

fn hash_str(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// A color per symbol kind (drawn from the theme's ANSI palette, no hardcoded hues),
/// so the outline reads by category at a glance — like syntax-highlighted symbols.
fn kind_color(kind: SymbolKind) -> gpui::Hsla {
    use SymbolKind::*;
    let idx = match kind {
        Function | Method | Macro => 12, // bright blue — callables
        Struct | Class => 2,             // green — data types
        Enum => 13,                      // bright magenta
        Trait | Interface => 3,          // yellow — contracts
        Impl | Module => 14,             // bright cyan — containers
        Type | Constant => 6,            // cyan
        Field | Variable | Other => return theme::text_muted(),
    };
    theme::ansi_base(idx).unwrap_or_else(theme::text_muted)
}

impl Focusable for StructurePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for StructurePanel {}

impl Panel for StructurePanel {
    fn panel_name(&self) -> &'static str {
        "Structure"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from("Structure")
    }

    fn title_suffix(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        // The uniform tool-window hide ✕ — collapses just the outline (the tree
        // keeps the full dock height); the stripe button brings it back.
        Some(super::tool_hide_button(
            "structure-hide",
            crate::views::chrome_requests::ChromeRequest::HideStructure,
        ))
    }
}

impl Render for StructurePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Ensure the active buffer's LSP outline is being fetched (guarded against
        // duplicates), then show the best tier available right now.
        self.request_lsp(cx);
        let symbols = self.displayed_symbols(cx);
        let mut rows = Vec::new();
        flatten(&symbols, 0, "", &self.collapsed, &mut rows);

        let body = div()
            .id("structure-scroll")
            .flex()
            .flex_col()
            .size_full()
            .overflow_y_scroll()
            .py_1();

        let body = if rows.is_empty() {
            body.child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .size_full()
                    .p_4()
                    .text_color(theme::text_muted())
                    .child(div().text_size(px(22.)).child("≣"))
                    .child(div().text_size(px(11.)).child("No symbols"))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(theme::tree_glyph())
                            .child("Open a code file to see its outline"),
                    ),
            )
        } else {
            body.children(rows.into_iter().enumerate().map(|(i, row)| {
                let Row {
                    id,
                    depth,
                    kind,
                    name,
                    line,
                    has_children,
                    collapsed,
                } = row;
                let kc = kind_color(kind);

                // Disclosure triangle (clickable) for parents; a spacer aligns leaves.
                let chevron = if has_children {
                    div()
                        .id(("structure-chevron", i))
                        .w(px(14.))
                        .flex_none()
                        .text_size(px(9.))
                        .text_color(theme::text_muted())
                        .cursor_pointer()
                        .child(if collapsed { "▸" } else { "▾" })
                        .on_click(cx.listener(move |this, _ev, _window, cx| this.toggle(id, cx)))
                        .into_any_element()
                } else {
                    div().w(px(14.)).flex_none().into_any_element()
                };

                div()
                    .id(("structure-row", i))
                    .relative()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .w_full()
                    .pl(px(6. + depth as f32 * 12.))
                    .pr_2()
                    .py(px(2.))
                    .hover(|d| d.bg(theme::row_hover()))
                    // Indent guides — faint vertical lines under each ancestor's
                    // chevron, matching the file tree's hierarchy cues.
                    .children((0..depth).map(|lvl| {
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(px(6. + lvl as f32 * 12. + 7.))
                            .w(px(1.))
                            .bg(theme::border_subtle())
                    }))
                    .child(chevron)
                    // Name area: clicking it navigates (separate element from the
                    // chevron, so a disclosure click never also jumps the cursor).
                    .child(
                        div()
                            .id(("structure-name", i))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .flex_1()
                            .overflow_hidden()
                            .cursor_pointer()
                            .on_click(
                                cx.listener(move |this, _ev, window, cx| {
                                    this.goto(line, window, cx)
                                }),
                            )
                            .child(
                                // A small color-coded kind chip (fn/struct/enum/…),
                                // far more glanceable than uniform grey labels.
                                div()
                                    .flex_none()
                                    .px(px(4.))
                                    .py(px(1.))
                                    .rounded(px(3.))
                                    .bg(theme::tint(kc, 0.14))
                                    .text_color(kc)
                                    .text_size(px(9.))
                                    .child(kind.label()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .overflow_hidden()
                                    .text_size(px(13.))
                                    .text_color(theme::text_primary())
                                    .child(name),
                            ),
                    )
            }))
        };

        div()
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::surface_raised())
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatten_respects_collapse_and_emits_children() {
        let tree = vec![Symbol {
            name: "Foo".into(),
            kind: SymbolKind::Impl,
            line: 0,
            children: vec![Symbol::leaf("bar", SymbolKind::Method, 1)],
        }];

        // Expanded: parent + child.
        let mut rows = Vec::new();
        flatten(&tree, 0, "", &HashSet::new(), &mut rows);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].name, "bar");
        assert_eq!(rows[1].depth, 1);

        // Collapse the parent → only the parent row remains.
        let parent_id = rows[0].id;
        let collapsed: HashSet<u64> = [parent_id].into_iter().collect();
        let mut rows = Vec::new();
        flatten(&tree, 0, "", &collapsed, &mut rows);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Foo");
        assert!(rows[0].has_children);
    }
}
