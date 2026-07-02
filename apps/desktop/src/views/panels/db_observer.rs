//! DB overview — the database tool window (right dock).
//!
//! A JetBrains-style data-source tree: the operator **adds** databases manually (a SQLite
//! file or a Postgres connection); each is a first-level node showing its name, expanding
//! to Tables/Views → columns (type + PK/FK) and indexes. Nothing is opened automatically.
//! Clicking a table opens its rows in a center [`db_grid`](super::db_grid) data editor;
//! the "SQL" action opens a center [`db_console`](super::db_console) bound to that source.
//! Schemas load lazily off the UI thread; the source list persists in `.moonlight-local`.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use gpui::prelude::*;
use gpui::{
    div, px, App, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla, PathPromptOptions,
    SharedString, Window,
};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::input::{Input, InputState};
use gpui_component::Sizable;

use super::db_source::{self, DataSource, PgConfig, Schema, TableMeta};
use crate::views::center_requests::OpenRequest;
use crate::views::theme;

pub struct DbObserverPanel {
    /// The operator's data sources (loaded from `.moonlight-local` on first render).
    sources: Vec<DataSource>,
    /// Loaded schema per source, keyed by [`DataSource::key`].
    schemas: HashMap<String, Schema>,
    /// Schema-load error per source key.
    errors: HashMap<String, String>,
    /// Source keys whose schema load is in flight.
    loading: HashSet<String>,
    /// Expanded tree node ids.
    open: HashSet<u64>,
    /// The selected source key — where a new SQL console binds.
    selected: Option<String>,
    /// First render loads the persisted source list.
    needs_init: bool,
    /// Postgres connect form: visibility + DSN input (lazy — needs a `Window`).
    connect_open: bool,
    dsn_input: Option<Entity<InputState>>,
    focus_handle: FocusHandle,
}

impl DbObserverPanel {
    /// An empty overview (workspace-owned); the persisted sources load on first render.
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            sources: Vec::new(),
            schemas: HashMap::new(),
            errors: HashMap::new(),
            loading: HashSet::new(),
            open: HashSet::new(),
            selected: None,
            needs_init: true,
            connect_open: false,
            dsn_input: None,
            focus_handle: cx.focus_handle(),
        }
    }

    fn project_root(&self, cx: &App) -> Option<PathBuf> {
        cx.try_global::<crate::views::workspace::ShellDeps>()
            .map(|d| d.focus.read(cx).root())
    }

    /// Add a data source (persist + select + expand so its schema loads).
    pub fn add_source(&mut self, source: DataSource, cx: &mut Context<Self>) {
        let key = source.key();
        if let Some(root) = self.project_root(cx) {
            self.sources = db_source::add_source(&root, &source);
        } else if !self.sources.iter().any(|s| s.key() == key) {
            self.sources.push(source.clone());
        }
        self.selected = Some(key.clone());
        self.open.insert(src_node_id(&key));
        self.load_schema_for(source, cx);
        cx.notify();
    }

    /// Remove a data source (forget its schema + persisted entry).
    fn remove_source(&mut self, source: &DataSource, cx: &mut Context<Self>) {
        let key = source.key();
        if let Some(root) = self.project_root(cx) {
            self.sources = db_source::remove_source(&root, source);
        } else {
            self.sources.retain(|s| s.key() != key);
        }
        self.schemas.remove(&key);
        self.errors.remove(&key);
        self.loading.remove(&key);
        if self.selected.as_deref() == Some(key.as_str()) {
            self.selected = self.sources.first().map(DataSource::key);
        }
        cx.notify();
    }

    /// Load a source's schema off the UI thread.
    fn load_schema_for(&mut self, source: DataSource, cx: &mut Context<Self>) {
        let key = source.key();
        if !self.loading.insert(key.clone()) {
            return; // already loading
        }
        self.errors.remove(&key);
        cx.spawn(async move |weak, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { db_source::load_schema(&source) })
                .await;
            let _ = weak.update(cx, |this, cx| {
                this.loading.remove(&key);
                match result {
                    Ok(schema) => {
                        this.errors.remove(&key);
                        this.schemas.insert(key, schema);
                    }
                    Err(e) => {
                        this.errors.insert(key, e);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle(&mut self, id: u64, cx: &mut Context<Self>) {
        if !self.open.insert(id) {
            self.open.remove(&id);
        }
        cx.notify();
    }

    fn select(&mut self, key: String, cx: &mut Context<Self>) {
        self.selected = Some(key);
        cx.notify();
    }

    /// Emit a center-open request (data editor / SQL console).
    fn emit(&self, req: OpenRequest, cx: &mut Context<Self>) {
        if let Some(center) = cx
            .try_global::<crate::views::workspace::ShellDeps>()
            .map(|d| d.center.clone())
        {
            center.update(cx, |_c, cx| cx.emit(req));
        }
    }

    fn open_table(&mut self, source: DataSource, table: TableMeta, cx: &mut Context<Self>) {
        self.emit(OpenRequest::DbTable { source, table }, cx);
    }

    fn open_console(&mut self, source: DataSource, cx: &mut Context<Self>) {
        self.selected = Some(source.key());
        self.emit(OpenRequest::DbConsole { source }, cx);
    }

    /// Pick a SQLite file to add as a data source.
    fn pick_sqlite(&self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(SharedString::from("Add SQLite Database")),
        });
        cx.spawn(async move |weak, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(path) = paths.into_iter().next() {
                    let _ = weak.update(cx, |this, cx| {
                        this.add_source(DataSource::Sqlite(path), cx);
                    });
                }
            }
        })
        .detach();
    }

    fn toggle_connect(&mut self, cx: &mut Context<Self>) {
        self.connect_open = !self.connect_open;
        cx.notify();
    }
}

// ── Tree model ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum NodeKind {
    Db,
    Group,
    Table,
    View,
    Column,
    IndexGroup,
    Index,
    Note,
}

struct TreeRow {
    id: u64,
    depth: usize,
    kind: NodeKind,
    label: String,
    /// Db/Table/View rows carry their source (for select / console / open / remove).
    source: Option<DataSource>,
    /// Table/View rows carry the table to open.
    table: Option<TableMeta>,
    /// Column rows: `(type, pk, fk-target)`.
    col: Option<(String, bool, Option<String>)>,
    count: Option<i64>,
    has_children: bool,
    open: bool,
    selected: bool,
}

fn hash_str(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Stable id for a source's top-level node.
fn src_node_id(key: &str) -> u64 {
    hash_str(&format!("src:{key}"))
}

/// Flatten the sources + their (loaded) schemas into render rows. Free function (no
/// panel/context) so it's directly testable.
fn build_tree(
    sources: &[DataSource],
    schemas: &HashMap<String, Schema>,
    errors: &HashMap<String, String>,
    loading: &HashSet<String>,
    open: &HashSet<u64>,
    selected: Option<&str>,
) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    for source in sources {
        let skey = source.key();
        let sid = src_node_id(&skey);
        let sopen = open.contains(&sid);
        rows.push(TreeRow {
            id: sid,
            depth: 0,
            kind: NodeKind::Db,
            label: source.label(),
            source: Some(source.clone()),
            table: None,
            col: None,
            count: None,
            has_children: true,
            open: sopen,
            selected: selected == Some(skey.as_str()),
        });
        if !sopen {
            continue;
        }
        if let Some(err) = errors.get(&skey) {
            rows.push(note_row(&skey, 1, err.clone()));
            continue;
        }
        let Some(schema) = schemas.get(&skey) else {
            let msg = if loading.contains(&skey) {
                "loading…"
            } else {
                "—"
            };
            rows.push(note_row(&skey, 1, msg.to_string()));
            continue;
        };
        push_schema(&mut rows, source, &skey, schema, open);
    }
    rows
}

fn push_schema(
    rows: &mut Vec<TreeRow>,
    source: &DataSource,
    skey: &str,
    schema: &Schema,
    open: &HashSet<u64>,
) {
    let groups: [(&str, &Vec<TableMeta>, NodeKind); 2] = [
        ("Tables", &schema.tables, NodeKind::Table),
        ("Views", &schema.views, NodeKind::View),
    ];
    for (glabel, objs, kind) in groups {
        if objs.is_empty() {
            continue;
        }
        let gid = hash_str(&format!("{skey}/{glabel}"));
        let gopen = open.contains(&gid);
        rows.push(TreeRow {
            id: gid,
            depth: 1,
            kind: NodeKind::Group,
            label: glabel.to_string(),
            source: None,
            table: None,
            col: None,
            count: Some(objs.len() as i64),
            has_children: true,
            open: gopen,
            selected: false,
        });
        if !gopen {
            continue;
        }
        for t in objs {
            let tid = hash_str(&format!("{skey}/{glabel}/{}", t.name));
            let topen = open.contains(&tid);
            rows.push(TreeRow {
                id: tid,
                depth: 2,
                kind,
                label: t.name.clone(),
                source: Some(source.clone()),
                table: Some(t.clone()),
                col: None,
                count: t.row_count,
                has_children: !t.columns.is_empty() || !t.indexes.is_empty(),
                open: topen,
                selected: false,
            });
            if !topen {
                continue;
            }
            for c in &t.columns {
                rows.push(TreeRow {
                    id: hash_str(&format!("{skey}/{glabel}/{}/col/{}", t.name, c.name)),
                    depth: 3,
                    kind: NodeKind::Column,
                    label: c.name.clone(),
                    source: None,
                    table: None,
                    col: Some((c.ty.clone(), c.pk, c.fk.clone())),
                    count: None,
                    has_children: false,
                    open: false,
                    selected: false,
                });
            }
            if !t.indexes.is_empty() {
                let iid = hash_str(&format!("{skey}/{glabel}/{}/__idx", t.name));
                let iopen = open.contains(&iid);
                rows.push(TreeRow {
                    id: iid,
                    depth: 3,
                    kind: NodeKind::IndexGroup,
                    label: "Indexes".to_string(),
                    source: None,
                    table: None,
                    col: None,
                    count: Some(t.indexes.len() as i64),
                    has_children: true,
                    open: iopen,
                    selected: false,
                });
                if iopen {
                    for ix in &t.indexes {
                        rows.push(TreeRow {
                            id: hash_str(&format!("{skey}/{glabel}/{}/idx/{ix}", t.name)),
                            depth: 4,
                            kind: NodeKind::Index,
                            label: ix.clone(),
                            source: None,
                            table: None,
                            col: None,
                            count: None,
                            has_children: false,
                            open: false,
                            selected: false,
                        });
                    }
                }
            }
        }
    }
}

fn note_row(skey: &str, depth: usize, label: String) -> TreeRow {
    TreeRow {
        id: hash_str(&format!("{skey}/note/{label}")),
        depth,
        kind: NodeKind::Note,
        label,
        source: None,
        table: None,
        col: None,
        count: None,
        has_children: false,
        open: false,
        selected: false,
    }
}

/// Accent colour for a tree node kind (from the theme's ANSI palette).
fn kind_color(kind: NodeKind) -> Hsla {
    let idx = match kind {
        NodeKind::Db => 14,        // bright cyan — the data source
        NodeKind::Table => 2,      // green — base data
        NodeKind::View => 6,       // cyan — derived
        NodeKind::IndexGroup => 8, // grey
        _ => return theme::text_muted(),
    };
    theme::ansi_base(idx).unwrap_or_else(theme::text_muted)
}

impl Focusable for DbObserverPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for DbObserverPanel {}

impl Panel for DbObserverPanel {
    fn panel_name(&self) -> &'static str {
        "DbObserver"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from("Databases")
    }

    fn title_suffix(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        // The right-dock tool window's uniform hide ✕ (stripe button brings it back).
        Some(super::tool_hide_button(
            "db-observer-hide",
            crate::views::chrome_requests::ChromeRequest::HideRightDock,
        ))
    }
}

impl Render for DbObserverPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // First render: load the persisted source list.
        if self.needs_init {
            self.needs_init = false;
            self.sources = self
                .project_root(cx)
                .map(|r| db_source::load_sources(&r))
                .unwrap_or_default();
        }
        // Lazily fetch schemas for expanded sources we haven't loaded yet.
        let to_load: Vec<DataSource> = self
            .sources
            .iter()
            .filter(|s| {
                let k = s.key();
                self.open.contains(&src_node_id(&k))
                    && !self.schemas.contains_key(&k)
                    && !self.errors.contains_key(&k)
                    && !self.loading.contains(&k)
            })
            .cloned()
            .collect();
        for s in to_load {
            self.load_schema_for(s, cx);
        }
        // Stand up the DSN input the first time the connect form shows.
        if self.connect_open && self.dsn_input.is_none() {
            self.dsn_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder("postgres://user:pass@host:5432/dbname")
            }));
        }

        div()
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::surface_raised())
            .text_color(theme::text_primary())
            .child(self.render_header(cx))
            .children(self.connect_open.then(|| self.render_connect_bar(cx)))
            .child(self.render_tree(cx))
    }
}

impl DbObserverPanel {
    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .w_full()
            .px_2()
            .py(px(4.))
            .border_b_1()
            .border_color(theme::border_subtle())
            .child(
                div()
                    .text_size(theme::text_xs())
                    .text_color(theme::text_muted())
                    .child("DATA SOURCES"),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .child(add_button(
                        "db-add-file",
                        "+ File",
                        cx.listener(|this, _e, _w, cx| this.pick_sqlite(cx)),
                    ))
                    .child(add_button(
                        "db-add-conn",
                        "+ Connect",
                        cx.listener(|this, _e, _w, cx| this.toggle_connect(cx)),
                    )),
            )
    }

    fn render_connect_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .w_full()
            .px_2()
            .py(px(6.))
            .border_b_1()
            .border_color(theme::border_subtle())
            .bg(theme::surface_base())
            .child(
                div()
                    .flex_1()
                    .children(self.dsn_input.as_ref().map(|i| Input::new(i).small())),
            )
            .child(add_button(
                "db-do-connect",
                "Connect",
                cx.listener(|this, _e, _w, cx| {
                    if let Some(input) = this.dsn_input.clone() {
                        let dsn = input.read(cx).value().trim().to_string();
                        if !dsn.is_empty() {
                            let cfg = PgConfig {
                                label: db_source::pg_label(&dsn),
                                dsn,
                            };
                            this.connect_open = false;
                            this.add_source(DataSource::Postgres(cfg), cx);
                        }
                    }
                }),
            ))
    }

    fn render_tree(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = build_tree(
            &self.sources,
            &self.schemas,
            &self.errors,
            &self.loading,
            &self.open,
            self.selected.as_deref(),
        );
        let mut tree = div()
            .id("db-tree")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .py_1();

        if rows.is_empty() {
            tree = tree.child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .size_full()
                    .p_4()
                    .text_color(theme::text_muted())
                    .child(div().text_size(px(22.)).child("⛁"))
                    .child(div().text_size(theme::text_xs()).child("No data sources"))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(theme::tree_glyph())
                            .child("Add a SQLite file or connect to Postgres"),
                    ),
            );
        }

        tree.children(
            rows.into_iter()
                .enumerate()
                .map(|(i, row)| self.render_row(i, row, cx)),
        )
    }

    fn render_row(&self, i: usize, row: TreeRow, cx: &mut Context<Self>) -> impl IntoElement {
        let TreeRow {
            id,
            depth,
            kind,
            label,
            source,
            table,
            col,
            count,
            has_children,
            open,
            selected,
        } = row;

        let chevron = if has_children {
            div()
                .id(("db-chevron", i))
                .w(px(14.))
                .flex_none()
                .text_size(px(9.))
                .text_color(theme::text_muted())
                .cursor_pointer()
                .child(if open { "▾" } else { "▸" })
                .on_click(cx.listener(move |this, _e, _w, cx| this.toggle(id, cx)))
                .into_any_element()
        } else {
            div().w(px(14.)).flex_none().into_any_element()
        };

        let mut name = div()
            .id(("db-name", i))
            .flex()
            .flex_row()
            .items_center()
            .gap_1p5()
            .flex_1()
            .overflow_hidden();

        // Category chip for DB / table / view rows.
        if matches!(kind, NodeKind::Db | NodeKind::Table | NodeKind::View) {
            let kc = kind_color(kind);
            let chip = match kind {
                NodeKind::Db => "DB",
                NodeKind::View => "VIEW",
                _ => "TABLE",
            };
            name = name.child(
                div()
                    .flex_none()
                    .px(px(4.))
                    .py(px(1.))
                    .rounded(px(3.))
                    .bg(theme::tint(kc, 0.14))
                    .text_color(kc)
                    .text_size(px(9.))
                    .child(chip),
            );
        }

        name = name.child(
            div()
                .flex_1()
                .overflow_hidden()
                .text_size(theme::text_sm())
                .text_color(match kind {
                    NodeKind::Group => theme::text_secondary(),
                    NodeKind::Index | NodeKind::IndexGroup | NodeKind::Note => theme::text_muted(),
                    _ => theme::text_primary(),
                })
                .child(label.clone()),
        );

        if let Some((ty, pk, fk)) = col {
            if pk {
                name = name.child(badge(
                    "PK",
                    theme::ansi_base(3).unwrap_or_else(theme::text_muted),
                ));
            }
            if let Some(t) = fk {
                name = name.child(badge(
                    &format!("→{t}"),
                    theme::ansi_base(13).unwrap_or_else(theme::text_muted),
                ));
            }
            if !ty.is_empty() {
                name = name.child(
                    div()
                        .flex_none()
                        .text_size(px(10.))
                        .text_color(theme::text_muted())
                        .child(ty),
                );
            }
        }

        if let Some(n) = count {
            name = name.child(
                div()
                    .flex_none()
                    .text_size(px(10.))
                    .text_color(theme::tree_glyph())
                    .child(n.to_string()),
            );
        }

        // Row click behaviour: a DB row selects + toggles; a table/view opens the editor.
        match (kind, source.clone(), table.clone()) {
            (NodeKind::Db, Some(src), _) => {
                let key = src.key();
                name = name
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _e, _w, cx| {
                        this.select(key.clone(), cx);
                        this.toggle(id, cx);
                    }));
            }
            (NodeKind::Table | NodeKind::View, Some(src), Some(t)) => {
                name =
                    name.cursor_pointer()
                        .on_click(cx.listener(move |this, _e, _w, cx| {
                            this.open_table(src.clone(), t.clone(), cx)
                        }));
            }
            _ => {}
        }

        // DB rows carry inline actions: open a SQL console, remove the source.
        let actions =
            (kind == NodeKind::Db).then(|| {
                let src_console = source.clone().unwrap();
                let src_remove = source.clone().unwrap();
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .flex_none()
                    .child(
                        div()
                            .id(("db-sql", i))
                            .px(px(4.))
                            .py(px(1.))
                            .rounded(px(3.))
                            .cursor_pointer()
                            .text_size(px(9.))
                            .text_color(theme::accent())
                            .hover(|d| d.bg(theme::tint(theme::accent(), 0.14)))
                            .child("SQL")
                            .on_click(cx.listener(move |this, _e, _w, cx| {
                                this.open_console(src_console.clone(), cx)
                            })),
                    )
                    .child(
                        div()
                            .id(("db-rm", i))
                            .px(px(3.))
                            .cursor_pointer()
                            .text_size(px(11.))
                            .text_color(theme::text_muted())
                            .hover(|d| d.text_color(theme::git_deleted()))
                            .child("✕")
                            .on_click(cx.listener(move |this, _e, _w, cx| {
                                this.remove_source(&src_remove, cx)
                            })),
                    )
            });

        div()
            .id(("db-row", i))
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .w_full()
            .pl(px(6. + depth as f32 * 13.))
            .pr_2()
            .py(px(2.))
            .when(selected, |d| d.bg(theme::row_selected()))
            .hover(|d| d.bg(theme::row_hover()))
            .children((0..depth).map(|lvl| {
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(6. + lvl as f32 * 13. + 7.))
                    .w(px(1.))
                    .bg(theme::border_subtle())
            }))
            .child(chevron)
            .child(name)
            .children(actions)
    }
}

/// A tiny pill badge (PK / FK markers).
fn badge(text: &str, color: Hsla) -> impl IntoElement {
    div()
        .flex_none()
        .px(px(4.))
        .py(px(1.))
        .rounded(px(3.))
        .bg(theme::tint(color, 0.14))
        .text_color(color)
        .text_size(px(9.))
        .child(text.to_string())
}

/// A small header "+ …" button.
fn add_button(
    id: &'static str,
    label: &'static str,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .px_2()
        .py(px(2.))
        .rounded(theme::radius_sm())
        .cursor_pointer()
        .text_size(theme::text_xs())
        .text_color(theme::text_muted())
        .hover(|d| d.bg(theme::row_hover()))
        .child(label)
        .on_click(move |e, w, cx| on_click(e, w, cx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::panels::db_source::{ColumnMeta, TableMeta};

    fn schema() -> Schema {
        Schema {
            tables: vec![TableMeta {
                name: "users".into(),
                schema: None,
                row_count: Some(3),
                columns: vec![ColumnMeta {
                    name: "id".into(),
                    ty: "INTEGER".into(),
                    not_null: true,
                    pk: true,
                    fk: None,
                }],
                indexes: vec!["idx_users_name".into()],
            }],
            views: vec![],
        }
    }

    fn tree_for(source: &DataSource, open: &[u64]) -> Vec<TreeRow> {
        let mut schemas = HashMap::new();
        schemas.insert(source.key(), schema());
        build_tree(
            std::slice::from_ref(source),
            &schemas,
            &HashMap::new(),
            &HashSet::new(),
            &open.iter().copied().collect(),
            None,
        )
    }

    #[test]
    fn collapsed_source_shows_only_db_node() {
        let src = DataSource::Sqlite("/tmp/a.sqlite".into());
        let rows = tree_for(&src, &[]);
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0].kind, NodeKind::Db));
    }

    #[test]
    fn expanded_source_shows_tables_group_and_table() {
        let src = DataSource::Sqlite("/tmp/a.sqlite".into());
        let key = src.key();
        let open = [src_node_id(&key), hash_str(&format!("{key}/Tables"))];
        let rows = tree_for(&src, &open);
        assert!(rows
            .iter()
            .any(|r| matches!(r.kind, NodeKind::Group) && r.label == "Tables"));
        assert!(rows
            .iter()
            .any(|r| matches!(r.kind, NodeKind::Table) && r.label == "users"));
    }
}
