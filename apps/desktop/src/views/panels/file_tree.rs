//! File-tree panel — the JetBrains-style project explorer.
//!
//! Lazily-expanded recursive tree rooted at the active [`ProjectSpace`] root. A
//! header lets the operator **open a folder**, switch among **recent** projects,
//! and toggle **follow-focused-session**. Rows are decorated with **git status**
//! (color + letter per file; a dot on folders that contain changes) — the core
//! supervision signal: what did the agent change?
//!
//! Filesystem reads are defensive (untrusted, may race/fail): unreadable dirs are
//! skipped, entries sorted dirs-first then case-insensitively by name, and children
//! load lazily on first expand. Git status is computed off the UI thread.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::prelude::*;
use gpui::EventEmitter;
use gpui::{
    div, px, App, Context, Entity, FocusHandle, Focusable, Hsla, PathPromptOptions, SharedString,
    Task, Window,
};
use gpui_component::dock::{Panel, PanelEvent};

use crate::git::status::{load_status, GitFileStatus, RepoStatus};
use crate::views::center_requests::OpenRequest;
use crate::views::project_space::ProjectSpace;
use crate::views::theme;
use crate::views::workspace::ShellDeps;

/// How often to re-sync the tree + git status with disk (catches files and
/// edits created/deleted/renamed by agents or the terminal).
const POLL: Duration = Duration::from_secs(5);

/// One directory entry, as read from the filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

/// A node in the expandable tree. `children == None` means "not yet loaded".
#[derive(Debug, Clone)]
struct Node {
    entry: Entry,
    expanded: bool,
    children: Option<Vec<Node>>,
}

impl Node {
    fn new(entry: Entry) -> Self {
        Self {
            entry,
            expanded: false,
            children: None,
        }
    }
}

/// Read a directory's immediate children, sorted dirs-first then by name
/// (case-insensitive). Defensive: returns an empty vec on any I/O error, skips
/// entries whose name is not valid UTF-8. Pure — directly unit-testable.
pub fn read_dir_sorted(dir: &Path) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = Vec::new();
    for item in read.flatten() {
        let path = item.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
            continue;
        };
        // Prefer the cheap file_type; fall back to a stat only if needed.
        let is_dir = match item.file_type() {
            Ok(ft) => ft.is_dir(),
            Err(_) => path.is_dir(),
        };
        entries.push(Entry { name, path, is_dir });
    }
    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    entries
}

/// Theme color for a git status (decoration).
fn git_color(status: GitFileStatus) -> Hsla {
    match status {
        GitFileStatus::Modified => theme::git_modified(),
        GitFileStatus::Added => theme::git_added(),
        GitFileStatus::Untracked => theme::git_untracked(),
        GitFileStatus::Deleted => theme::git_deleted(),
        GitFileStatus::Renamed => theme::git_modified(),
        GitFileStatus::Conflicted => theme::git_conflict(),
    }
}

/// A flattened, render-ready row produced by walking the expanded tree.
struct Row {
    depth: usize,
    path: PathBuf,
    name: String,
    is_dir: bool,
    expanded: bool,
}

pub struct FileTreePanel {
    root: PathBuf,
    /// Top-level nodes (children of `root`), each lazily expandable.
    nodes: Vec<Node>,
    selected: Option<PathBuf>,
    /// Shared project space — drives the header actions (open/recent/follow).
    space: Option<Entity<ProjectSpace>>,
    /// Working-tree status for the current repo, if `root` is inside one.
    git: Option<RepoStatus>,
    /// Whether the recent-projects list is expanded in the header.
    show_recent: bool,
    focus_handle: FocusHandle,
    /// Keeps the periodic git-refresh task alive for the view's lifetime.
    _git_tick: Option<Task<()>>,
}

impl FileTreePanel {
    /// Build a tree rooted at the project space (or cwd) and re-root whenever the
    /// active root changes. `space` is `None` only in tests.
    pub fn new(space: Option<Entity<ProjectSpace>>, cx: &mut Context<Self>) -> Self {
        let root = space
            .as_ref()
            .map(|s| s.read(cx).root())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));

        if let Some(space) = space.as_ref() {
            cx.observe(space, |this, space, cx| {
                let new_root = space.read(cx).root();
                if new_root != this.root {
                    this.reroot(new_root);
                    this.refresh_git(cx);
                    cx.notify();
                }
            })
            .detach();
        }

        // Periodically re-sync the tree + git status so agent/terminal edits
        // (new/deleted/renamed files, content changes) surface without a reroot.
        let tick = cx.spawn(async move |weak, cx| loop {
            cx.background_executor().timer(POLL).await;
            let keep = weak
                .update(cx, |this, cx| {
                    this.refresh_tree();
                    this.refresh_git(cx);
                    cx.notify();
                })
                .is_ok();
            if !keep {
                break; // view dropped
            }
        });

        let mut this = Self {
            root: root.clone(),
            nodes: Vec::new(),
            selected: None,
            space,
            git: None,
            show_recent: false,
            focus_handle: cx.focus_handle(),
            _git_tick: Some(tick),
        };
        this.reroot(root);
        this.refresh_git(cx);
        this
    }

    /// Point the tree at a new root, loading its top level and resetting expansion.
    fn reroot(&mut self, root: PathBuf) {
        self.nodes = read_dir_sorted(&root).into_iter().map(Node::new).collect();
        self.root = root;
        self.selected = None;
    }

    /// Re-read the directory structure from disk, preserving which folders are
    /// expanded (and their already-loaded children). Catches files an agent or
    /// the terminal created/deleted/renamed. Drops the selection if its path is
    /// gone. Cheap: only expanded directories are re-`read_dir`'d.
    fn refresh_tree(&mut self) {
        let root = self.root.clone();
        self.nodes = reconcile_dir(&root, std::mem::take(&mut self.nodes));
        if let Some(sel) = &self.selected {
            if !sel.exists() {
                self.selected = None;
            }
        }
    }

    /// Force a full refresh — tree listing + git decorations — and repaint.
    /// Wired to the header ⟳ button.
    fn refresh_all(&mut self, cx: &mut Context<Self>) {
        self.refresh_tree();
        self.refresh_git(cx);
        cx.notify();
    }

    /// Recompute git status for the current root off the UI thread, then refresh.
    fn refresh_git(&self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        cx.spawn(async move |weak, cx| {
            let status = cx
                .background_executor()
                .spawn(async move { load_status(&root) })
                .await;
            let _ = weak.update(cx, |this, cx| {
                this.git = status;
                cx.notify();
            });
        })
        .detach();
    }

    /// Open the native folder picker and set the chosen directory as the root.
    fn open_folder(&self, cx: &mut Context<Self>) {
        let Some(space) = self.space.clone() else {
            return;
        };
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(SharedString::from("Open Folder")),
        });
        cx.spawn(async move |_weak, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(path) = paths.into_iter().next() {
                    space.update(cx, |s, cx| {
                        s.open_root(path);
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    /// Switch to a recent project root.
    fn open_recent(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.show_recent = false;
        if let Some(space) = self.space.clone() {
            space.update(cx, |s, cx| {
                s.open_root(path);
                cx.notify();
            });
        }
    }

    /// Flip the follow-focused-session toggle.
    fn toggle_follow(&mut self, cx: &mut Context<Self>) {
        if let Some(space) = self.space.clone() {
            space.update(cx, |s, cx| {
                let now = s.follow_focus();
                s.set_follow_focus(!now);
                cx.notify();
            });
        }
    }

    /// Handle a click on a row: toggle (and lazily load) directories; open files
    /// as a center editor tab beside the session monitor.
    fn on_row_click(&mut self, path: PathBuf, is_dir: bool, cx: &mut Context<Self>) {
        self.selected = Some(path.clone());
        if is_dir {
            toggle_node(&mut self.nodes, &path);
        } else if let Some(center) = cx.try_global::<ShellDeps>().map(|d| d.center.clone()) {
            // SQLite files open the DB observer; everything else opens the editor.
            let request = if is_sqlite(&path) {
                OpenRequest::Db(path.clone())
            } else {
                OpenRequest::File(path.clone())
            };
            center.update(cx, |_c, cx| cx.emit(request));
        }
        cx.notify();
    }

    /// Walk the expanded tree into a flat list of rows for rendering.
    fn flatten(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        push_rows(&self.nodes, 0, &mut rows);
        rows
    }

    fn root_label(&self) -> String {
        self.root
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| self.root.display().to_string())
    }
}

/// Recursively collect render rows for expanded nodes (depth-first, pre-order).
fn push_rows(nodes: &[Node], depth: usize, rows: &mut Vec<Row>) {
    for node in nodes {
        rows.push(Row {
            depth,
            path: node.entry.path.clone(),
            name: node.entry.name.clone(),
            is_dir: node.entry.is_dir,
            expanded: node.expanded,
        });
        if node.expanded {
            if let Some(children) = &node.children {
                push_rows(children, depth + 1, rows);
            }
        }
    }
}

/// Re-read `dir` from disk, carrying over expansion state (and, recursively, the
/// loaded children) from `old` nodes whose path still exists. New entries appear
/// collapsed; vanished entries drop out. An expanded directory recurses so its
/// own contents re-sync too; a collapsed/unloaded one keeps its state untouched.
fn reconcile_dir(dir: &Path, old: Vec<Node>) -> Vec<Node> {
    // Index the previous nodes by path for O(1) carry-over lookup.
    let mut old_by_path: std::collections::HashMap<PathBuf, Node> =
        old.into_iter().map(|n| (n.entry.path.clone(), n)).collect();

    read_dir_sorted(dir)
        .into_iter()
        .map(|entry| match old_by_path.remove(&entry.path) {
            // Existing, expanded directory with loaded children: recurse to
            // refresh its contents while keeping it expanded.
            Some(mut prev) if entry.is_dir && prev.expanded && prev.children.is_some() => {
                let children = prev.children.take().unwrap();
                let path = entry.path.clone();
                Node {
                    entry,
                    expanded: true,
                    children: Some(reconcile_dir(&path, children)),
                }
            }
            // Existing but collapsed / not-yet-loaded: keep prior state as-is.
            Some(prev) => Node {
                entry,
                expanded: prev.expanded,
                children: prev.children,
            },
            // Brand-new entry.
            None => Node::new(entry),
        })
        .collect()
}

/// Find the node at `path` and toggle its expansion, lazily loading children on
/// first expand. Returns `true` if a matching directory node was found.
fn toggle_node(nodes: &mut [Node], path: &Path) -> bool {
    for node in nodes.iter_mut() {
        if node.entry.path == path {
            if node.expanded {
                node.expanded = false;
            } else {
                if node.children.is_none() {
                    node.children = Some(
                        read_dir_sorted(&node.entry.path)
                            .into_iter()
                            .map(Node::new)
                            .collect(),
                    );
                }
                node.expanded = true;
            }
            return true;
        }
        if node.expanded {
            if let Some(children) = node.children.as_mut() {
                if toggle_node(children, path) {
                    return true;
                }
            }
        }
    }
    false
}

impl Focusable for FileTreePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for FileTreePanel {}

impl Panel for FileTreePanel {
    fn panel_name(&self) -> &'static str {
        "FileTree"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from(self.root_label())
    }

    fn title_suffix(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        // The uniform tool-window hide ✕ — collapses the left dock to its stripe.
        Some(super::tool_hide_button(
            "file-tree-hide",
            crate::views::chrome_requests::ChromeRequest::HideLeftDock,
        ))
    }
}

impl Render for FileTreePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.flatten();
        let selected = self.selected.clone();
        let (follow, recent) = self
            .space
            .as_ref()
            .map(|s| {
                let s = s.read(cx);
                (s.follow_focus(), s.recent().to_vec())
            })
            .unwrap_or((true, Vec::new()));

        div()
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::surface_raised())
            .text_color(theme::text_primary())
            .child(self.header(follow, &recent, cx))
            .child(
                div()
                    .id("file-tree-scroll")
                    .flex()
                    .flex_col()
                    .size_full()
                    .overflow_y_scroll()
                    .py_1()
                    .children(rows.into_iter().enumerate().map(|(i, row)| {
                        let is_selected = selected.as_deref() == Some(row.path.as_path());
                        let deco = self.row_decoration(&row);
                        file_row(i, row, is_selected, deco, cx)
                    })),
            )
    }
}

/// Coarse file category, used to pick a tree icon glyph + color by file type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileKind {
    Rust,
    Go,
    Js,
    Ts,
    Python,
    Data,   // json / yaml / toml
    Markup, // html / xml / svg
    Css,
    Shell,
    Sql,
    Doc, // md / txt
    Image,
    Lock, // *.lock / Cargo.lock
    Other,
}

/// Whether `path` looks like a SQLite database (opens the DB observer, not the editor).
fn is_sqlite(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "sqlite" | "sqlite3" | "db"
    )
}

/// Classify a file by name/extension for its tree icon.
fn file_kind(path: &Path) -> FileKind {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if name.ends_with(".lock") || name == "cargo.lock" {
        return FileKind::Lock;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => FileKind::Rust,
        "go" => FileKind::Go,
        "js" | "jsx" | "mjs" | "cjs" => FileKind::Js,
        "ts" | "tsx" => FileKind::Ts,
        "py" => FileKind::Python,
        "json" | "yaml" | "yml" | "toml" => FileKind::Data,
        "html" | "htm" | "xml" | "svg" => FileKind::Markup,
        "css" | "scss" | "less" => FileKind::Css,
        "sh" | "bash" | "zsh" | "fish" => FileKind::Shell,
        "sql" => FileKind::Sql,
        "md" | "markdown" | "txt" => FileKind::Doc,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "ico" => FileKind::Image,
        _ => FileKind::Other,
    }
}

/// A category-shaped glyph (BMP symbols that render reliably in the mono font).
fn kind_glyph(kind: FileKind) -> &'static str {
    match kind {
        FileKind::Data => "◇",
        FileKind::Markup | FileKind::Css => "◈",
        FileKind::Doc => "≡",
        FileKind::Shell => "»",
        FileKind::Sql => "▤",
        FileKind::Image => "▣",
        FileKind::Lock => "▪",
        // All code-ish kinds share a filled disc, distinguished by color.
        _ => "●",
    }
}

/// Icon color by kind, drawn from the theme's ANSI palette (no hardcoded colors).
fn kind_color(kind: FileKind) -> Hsla {
    let idx = match kind {
        FileKind::Rust => 11,  // bright yellow → rust-orange-ish
        FileKind::Go => 14,    // bright cyan
        FileKind::Js => 3,     // yellow
        FileKind::Ts => 12,    // bright blue
        FileKind::Python => 2, // green
        FileKind::Data => 13,  // bright magenta
        FileKind::Markup => 9, // bright red
        FileKind::Css => 12,   // bright blue
        FileKind::Shell => 10, // bright green
        FileKind::Sql => 6,    // cyan
        FileKind::Image => 5,  // magenta
        FileKind::Doc | FileKind::Lock | FileKind::Other => {
            return theme::text_muted();
        }
    };
    theme::ansi_base(idx).unwrap_or_else(theme::text_muted)
}

/// Resolved git decoration for one row.
#[derive(Default, Clone, Copy)]
struct RowDeco {
    color: Option<Hsla>,
    letter: Option<&'static str>,
    dir_dot: bool,
}

impl FileTreePanel {
    fn row_decoration(&self, row: &Row) -> RowDeco {
        let Some(git) = self.git.as_ref() else {
            return RowDeco::default();
        };
        if row.is_dir {
            RowDeco {
                dir_dot: git.dir_has_changes(&row.path),
                ..RowDeco::default()
            }
        } else if let Some(status) = git.status_for(&row.path) {
            RowDeco {
                color: Some(git_color(status)),
                letter: Some(status.letter()),
                dir_dot: false,
            }
        } else {
            RowDeco::default()
        }
    }

    /// The header: root label (toggles recent list), Open / refresh / follow.
    fn header(&self, follow: bool, recent: &[PathBuf], cx: &mut Context<Self>) -> impl IntoElement {
        let follow_color = if follow {
            theme::accent()
        } else {
            theme::text_muted()
        };

        let mut header = div()
            .flex()
            .flex_col()
            .w_full()
            .border_b_1()
            .border_color(theme::border_subtle())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .w_full()
                    .px_2()
                    .py(px(4.))
                    .child(
                        // Root name — clicking toggles the recent-projects list.
                        div()
                            .id("file-tree-root")
                            .flex_1()
                            .overflow_hidden()
                            .cursor_pointer()
                            .text_size(px(12.))
                            .text_color(theme::text_primary())
                            .child(format!("▾ {}", self.root_label()))
                            .on_click(cx.listener(|this, _ev, _w, cx| {
                                this.show_recent = !this.show_recent;
                                cx.notify();
                            })),
                    )
                    .child(header_button(
                        "ft-open",
                        "Open…",
                        cx.listener(|this, _ev, _w, cx| {
                            this.open_folder(cx);
                        }),
                    ))
                    .child(header_button(
                        "ft-refresh",
                        "⟳",
                        cx.listener(|this, _ev, _w, cx| {
                            this.refresh_all(cx);
                        }),
                    ))
                    .child(
                        div()
                            .id("ft-follow")
                            .px_2()
                            .py(px(2.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .text_size(px(11.))
                            .text_color(follow_color)
                            .bg(theme::tint(follow_color, 0.14))
                            .child("⇄ follow")
                            .on_click(cx.listener(|this, _ev, _w, cx| this.toggle_follow(cx))),
                    ),
            );

        if self.show_recent && !recent.is_empty() {
            header = header.child(div().flex().flex_col().w_full().pb_1().children(
                recent.iter().enumerate().map(|(i, path)| {
                    let p = path.clone();
                    let label = path.display().to_string();
                    div()
                        .id(("ft-recent", i))
                        .w_full()
                        .px_3()
                        .py(px(2.))
                        .cursor_pointer()
                        .text_size(px(11.))
                        .text_color(theme::text_muted())
                        .overflow_hidden()
                        .hover(|d| d.bg(theme::row_hover()))
                        .child(label)
                        .on_click(cx.listener(move |this, _ev, _w, cx| {
                            this.open_recent(p.clone(), cx);
                        }))
                }),
            ));
        }

        header
    }
}

/// A small ghost button for the tree header.
fn header_button(
    id: &'static str,
    label: &'static str,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .px_2()
        .py(px(2.))
        .rounded(px(6.))
        .cursor_pointer()
        .text_size(px(11.))
        .text_color(theme::text_muted())
        .hover(|d| d.bg(theme::row_hover()))
        .child(label)
        .on_click(on_click)
}

/// Render one tree row: indentation + disclosure glyph + name + git decoration.
fn file_row(
    i: usize,
    row: Row,
    selected: bool,
    deco: RowDeco,
    cx: &mut Context<FileTreePanel>,
) -> impl IntoElement {
    // Icon column: dirs show the disclosure triangle; files show a type-colored
    // category glyph (the "icon set").
    let (icon_glyph, icon_color) = if row.is_dir {
        let tri = if row.expanded { "▾" } else { "▸" };
        (tri, theme::tree_glyph())
    } else {
        let kind = file_kind(&row.path);
        (kind_glyph(kind), kind_color(kind))
    };
    let depth = row.depth;
    let indent = px(8. + depth as f32 * 14.);
    // Git color overrides the default name color when present.
    let name_color = deco.color.unwrap_or(if row.is_dir {
        theme::text_primary()
    } else {
        theme::text_muted()
    });
    let path = row.path.clone();
    let is_dir = row.is_dir;

    div()
        .id(("file-row", i))
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .w_full()
        .pl(indent)
        .pr_2()
        .py(px(2.))
        .cursor_pointer()
        .when(selected, |d| d.bg(theme::row_selected()))
        .hover(|d| d.bg(theme::row_hover()))
        .on_click(cx.listener(move |this, _ev, _window, cx| {
            this.on_row_click(path.clone(), is_dir, cx);
        }))
        // Indent guides — a faint vertical line under each ancestor's disclosure
        // column, so deep nesting stays readable (JetBrains-style).
        .children((0..depth).map(|lvl| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(8. + lvl as f32 * 14. + 6.))
                .w(px(1.))
                .bg(theme::border_subtle())
        }))
        // Selected: a crisp accent bar hugging the left edge (clearer than the wash
        // alone, echoing the activity rail's lit tick).
        .when(selected, |d| {
            d.child(
                div()
                    .absolute()
                    .left_0()
                    .top(px(3.))
                    .bottom(px(3.))
                    .w(px(2.))
                    .rounded_full()
                    .bg(theme::accent()),
            )
        })
        .child(
            div()
                .w(px(12.))
                .text_color(icon_color)
                .text_size(px(11.))
                .child(icon_glyph),
        )
        .child(
            div()
                .flex_1()
                .overflow_hidden()
                .text_color(name_color)
                .text_size(px(13.))
                .child(row.name),
        )
        // A dot for directories that contain changes.
        .when(deco.dir_dot, |d| {
            d.child(
                div()
                    .text_color(theme::git_modified())
                    .text_size(px(11.))
                    .child("●"),
            )
        })
        // A status letter for changed files (M/A/D/?/R/U).
        .when_some(deco.letter, |d, letter| {
            d.child(
                div()
                    .text_color(name_color)
                    .text_size(px(11.))
                    .child(letter),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn read_dir_sorted_puts_dirs_first_then_alpha() {
        let tmp = std::env::temp_dir().join(format!("mlc-filetree-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("zeta_dir")).unwrap();
        fs::create_dir_all(tmp.join("alpha_dir")).unwrap();
        fs::write(tmp.join("b_file.txt"), b"x").unwrap();
        fs::write(tmp.join("A_file.txt"), b"x").unwrap();

        let entries = read_dir_sorted(&tmp);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        // Dirs first (alpha), then files (case-insensitive alpha).
        assert_eq!(
            names,
            vec!["alpha_dir", "zeta_dir", "A_file.txt", "b_file.txt"]
        );
        assert!(entries[0].is_dir && !entries[2].is_dir);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn file_kind_classifies_by_name_and_ext() {
        assert_eq!(file_kind(Path::new("src/main.rs")), FileKind::Rust);
        assert_eq!(file_kind(Path::new("app.tsx")), FileKind::Ts);
        assert_eq!(file_kind(Path::new("data.JSON")), FileKind::Data);
        assert_eq!(file_kind(Path::new("README.md")), FileKind::Doc);
        assert_eq!(file_kind(Path::new("logo.png")), FileKind::Image);
        assert_eq!(file_kind(Path::new("Cargo.lock")), FileKind::Lock);
        // Only a real `.lock` extension is a lockfile; `pnpm-lock.yaml` is yaml → Data.
        assert_eq!(file_kind(Path::new("pnpm-lock.yaml")), FileKind::Data);
        assert_eq!(file_kind(Path::new("Makefile")), FileKind::Other);
    }

    #[test]
    fn reconcile_dir_syncs_disk_while_keeping_expansion() {
        let tmp = std::env::temp_dir().join(format!("mlc-reconcile-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("open_dir")).unwrap();
        fs::create_dir_all(tmp.join("closed_dir")).unwrap();
        fs::write(tmp.join("open_dir").join("a.txt"), b"x").unwrap();
        fs::write(tmp.join("keep.txt"), b"x").unwrap();
        fs::write(tmp.join("gone.txt"), b"x").unwrap();

        // Initial tree: expand+load open_dir, leave closed_dir collapsed.
        let mut nodes: Vec<Node> = read_dir_sorted(&tmp).into_iter().map(Node::new).collect();
        assert!(toggle_node(&mut nodes, &tmp.join("open_dir")));

        // Mutate disk the way an agent would: add a new file inside the expanded
        // dir, remove a top-level file.
        fs::write(tmp.join("open_dir").join("b.txt"), b"x").unwrap();
        fs::write(tmp.join("new.txt"), b"x").unwrap();
        fs::remove_file(tmp.join("gone.txt")).unwrap();

        let synced = reconcile_dir(&tmp, nodes);
        let by_name = |name: &str| synced.iter().find(|n| n.entry.name == name);

        // Deleted file dropped, new file appeared.
        assert!(by_name("gone.txt").is_none());
        assert!(by_name("new.txt").is_some());
        assert!(by_name("keep.txt").is_some());

        // Collapsed dir stayed collapsed + unloaded.
        let closed = by_name("closed_dir").unwrap();
        assert!(!closed.expanded && closed.children.is_none());

        // Expanded dir stayed expanded and its children re-synced (a.txt + b.txt).
        let open = by_name("open_dir").unwrap();
        assert!(open.expanded);
        let child_names: Vec<&str> = open
            .children
            .as_ref()
            .unwrap()
            .iter()
            .map(|n| n.entry.name.as_str())
            .collect();
        assert_eq!(child_names, vec!["a.txt", "b.txt"]);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn read_dir_sorted_is_empty_for_missing_dir() {
        let missing = PathBuf::from("/this/path/should/not/exist/mlc");
        assert!(read_dir_sorted(&missing).is_empty());
    }

    #[test]
    fn toggle_node_lazily_loads_children() {
        let tmp = std::env::temp_dir().join(format!("mlc-toggle-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("sub")).unwrap();
        fs::write(tmp.join("sub").join("inner.txt"), b"x").unwrap();

        let mut nodes: Vec<Node> = read_dir_sorted(&tmp).into_iter().map(Node::new).collect();
        let sub = tmp.join("sub");
        assert!(nodes.iter().all(|n| n.children.is_none()));

        // First toggle expands + loads children.
        assert!(toggle_node(&mut nodes, &sub));
        let node = nodes.iter().find(|n| n.entry.path == sub).unwrap();
        assert!(node.expanded);
        assert_eq!(node.children.as_ref().unwrap().len(), 1);

        // Second toggle collapses (children stay cached).
        assert!(toggle_node(&mut nodes, &sub));
        let node = nodes.iter().find(|n| n.entry.path == sub).unwrap();
        assert!(!node.expanded && node.children.is_some());

        let _ = fs::remove_dir_all(&tmp);
    }
}
