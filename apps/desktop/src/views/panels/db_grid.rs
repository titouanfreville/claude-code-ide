//! DB data editor — a table's rows in a center tab (the JetBrains "data editor").
//!
//! Opened from the [`db_observer`](super::db_observer) tree when a table/view is clicked.
//! Holds one `(DataSource, TableMeta)` and renders a sortable, paginated grid. The grid
//! widgets here ([`grid`], [`header_cell`], [`cell_color`], the button helpers) are shared
//! with the SQL [`db_console`](super::db_console), so both render results identically.

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    div, px, AnyElement, App, ClickEvent, Context, Div, EventEmitter, FocusHandle, Focusable, Hsla,
    Stateful, WeakEntity, Window,
};
use gpui_component::dock::{Panel, PanelEvent, PanelInfo, PanelState, TabPanel};

use super::db_source::{self, Cell, ColumnMeta, DataSource, Page, SortDir, TableMeta};
use super::CloseTab;
use crate::views::theme;

/// Fixed grid column width (cells ellipsize).
pub const COL_W: f32 = 184.;
/// Row-number gutter width.
pub const GUTTER_W: f32 = 56.;

// ── Shared grid rendering (used by the data editor + the SQL console) ─────────

/// Display colour for a typed cell (numbers cyan, NULL/blob muted, text default).
pub fn cell_color(cell: &Cell) -> Hsla {
    match cell {
        Cell::Null | Cell::Blob(_) => theme::text_muted(),
        Cell::Int(_) | Cell::Real(_) => theme::ansi_base(6).unwrap_or_else(theme::text_primary),
        Cell::Bool(_) => theme::ansi_base(3).unwrap_or_else(theme::text_primary),
        Cell::Text(_) => theme::text_primary(),
    }
}

/// A styled, id'd header cell (caller adds `.on_click` for the sortable data editor;
/// the console leaves it static). `marker` is the active-sort glyph (or empty).
pub fn header_cell(i: usize, col: &ColumnMeta, marker: &str) -> Stateful<Div> {
    div()
        .id(("db-col", i))
        .w(px(COL_W))
        .flex_none()
        .px_2()
        .py(px(3.))
        .border_r_1()
        .border_color(theme::border_subtle())
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .overflow_hidden()
                .text_size(theme::text_xs())
                .text_color(theme::text_secondary())
                .child(format!("{}{}", col.name, marker)),
        )
        .when(!col.ty.is_empty(), |d| {
            d.child(
                div()
                    .text_size(px(9.))
                    .text_color(theme::text_muted())
                    .child(col.ty.clone()),
            )
        })
}

/// The scrollable grid: a sticky header (pre-built `header_cells`) above the typed body.
/// Header + body share one horizontal scroll so columns stay aligned; `offset` numbers
/// the row-gutter from the page's absolute position.
pub fn grid(page: &Page, offset: usize, header_cells: Vec<AnyElement>) -> impl IntoElement {
    let ncol = page.columns.len();
    let content_w = px(GUTTER_W + ncol as f32 * COL_W);

    let header = div()
        .flex()
        .flex_row()
        .flex_none()
        .w(content_w)
        .bg(theme::surface_raised())
        .border_b_1()
        .border_color(theme::border_strong())
        .child(div().w(px(GUTTER_W)).flex_none())
        .children(header_cells);

    let body = div()
        .id("db-grid-body")
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.))
        .overflow_y_scroll()
        .children(page.rows.iter().enumerate().map(|(r, row)| {
            let mut line = div()
                .flex()
                .flex_row()
                .w(content_w)
                .when(r % 2 == 1, |d| d.bg(theme::tint(theme::surface_raised(), 0.4)))
                .hover(|d| d.bg(theme::row_hover()))
                .child(
                    div()
                        .w(px(GUTTER_W))
                        .flex_none()
                        .px_2()
                        .py(px(2.))
                        .flex()
                        .justify_end()
                        .border_r_1()
                        .border_color(theme::border_subtle())
                        .font_family(theme::mono_font())
                        .text_size(px(10.))
                        .text_color(theme::tree_glyph())
                        .child((offset + r + 1).to_string()),
                );
            for cell in row {
                let c = div()
                    .w(px(COL_W))
                    .flex_none()
                    .px_2()
                    .py(px(2.))
                    .flex()
                    .when(cell.is_numeric(), |d| d.justify_end())
                    .border_r_1()
                    .border_color(theme::border_subtle());
                let mut text = div()
                    .overflow_hidden()
                    .font_family(theme::mono_font())
                    .text_size(theme::text_sm())
                    .text_color(cell_color(cell))
                    .child(cell.display());
                if cell.is_null() {
                    text = text.italic();
                }
                line = line.child(c.child(text));
            }
            line
        }));

    div()
        .id("db-grid")
        .flex_1()
        .min_h(px(0.))
        .overflow_x_scroll()
        .child(div().flex().flex_col().min_w(content_w).child(header).child(body))
}

/// A small toolbar text button.
pub fn toolbar_button(
    id: &'static str,
    label: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
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
        .on_click(move |ev, w, cx| on_click(ev, w, cx))
}

/// A pagination chevron button; muted + non-interactive when `disabled`.
pub fn page_button(
    id: &'static str,
    glyph: &'static str,
    disabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let mut b = div()
        .id(id)
        .px_2()
        .py(px(1.))
        .rounded(px(4.))
        .text_size(theme::text_sm())
        .text_color(if disabled {
            theme::border_strong()
        } else {
            theme::text_secondary()
        })
        .child(glyph);
    if !disabled {
        b = b
            .cursor_pointer()
            .hover(|d| d.bg(theme::row_hover()))
            .on_click(move |ev, w, cx| on_click(ev, w, cx));
    }
    b
}

// ── The data-editor panel ────────────────────────────────────────────────────

pub struct DbGridPanel {
    source: DataSource,
    table: TableMeta,
    page: Page,
    offset: usize,
    sort: Option<(usize, SortDir)>,
    loading: bool,
    error: Option<String>,
    /// First render kicks off the page load (entity/weak handle exists by then).
    needs_load: bool,
    focus_handle: FocusHandle,
    tab_panel: Option<WeakEntity<TabPanel>>,
    tab_menu: Option<super::TabMenu>,
}

impl DbGridPanel {
    pub fn new(source: DataSource, table: TableMeta, cx: &mut Context<Self>) -> Self {
        Self {
            source,
            table,
            page: Page::default(),
            offset: 0,
            sort: None,
            loading: false,
            error: None,
            needs_load: true,
            focus_handle: cx.focus_handle(),
            tab_panel: None,
            tab_menu: None,
        }
    }

    /// Rebuild from persisted layout state (the dumped `source` + `table`). Center DB
    /// tabs are wiped to home on layout restore, so a corrupt/missing source degrades to
    /// an empty source rather than failing the whole layout load.
    pub fn restore(info: &PanelInfo, cx: &mut Context<Self>) -> Self {
        let (source, table) = match info {
            PanelInfo::Panel(val) => (
                val.get("source")
                    .cloned()
                    .and_then(|v| serde_json::from_value::<DataSource>(v).ok())
                    .unwrap_or_else(|| DataSource::Sqlite(std::path::PathBuf::new())),
                val.get("table")
                    .cloned()
                    .and_then(|v| serde_json::from_value::<TableMeta>(v).ok())
                    .unwrap_or_default(),
            ),
            _ => (DataSource::Sqlite(std::path::PathBuf::new()), TableMeta::default()),
        };
        Self::new(source, table, cx)
    }

    fn fetch_page(&mut self, cx: &mut Context<Self>) {
        let src = self.source.clone();
        let table = self.table.clone();
        let sort = self.sort;
        let offset = self.offset;
        self.loading = true;
        cx.spawn(async move |weak, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    db_source::load_page(&src, &table, sort, offset, db_source::PAGE_LIMIT)
                })
                .await;
            let _ = weak.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(page) => {
                        this.error = None;
                        this.page = page;
                    }
                    Err(e) => this.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn sort_by(&mut self, col: usize, cx: &mut Context<Self>) {
        self.sort = Some(match self.sort {
            Some((c, dir)) if c == col => (col, dir.toggled()),
            _ => (col, SortDir::Asc),
        });
        self.offset = 0;
        self.fetch_page(cx);
    }

    fn page_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.offset = offset;
        self.fetch_page(cx);
    }

    /// Dedup key shared with [`OpenRequest::DbTable`](crate::views::center_requests).
    pub fn tab_key(source: &DataSource, table: &TableMeta) -> String {
        let ns = table.schema.as_deref().unwrap_or("");
        format!("dbtable:{}:{ns}.{}", source.key(), table.name)
    }
}

impl Focusable for DbGridPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl super::TabMenuHost for DbGridPanel {
    fn tab_menu_slot(&mut self) -> &mut Option<super::TabMenu> {
        &mut self.tab_menu
    }
}

impl EventEmitter<PanelEvent> for DbGridPanel {}

impl Panel for DbGridPanel {
    fn panel_name(&self) -> &'static str {
        "DbGrid"
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        super::tab_title(
            format!("{} · {}", self.table.name, self.source.label()),
            None,
            cx.entity_id().as_u64(),
            self.tab_panel.clone(),
            Arc::new(cx.entity()),
            self.focus_handle(cx),
            cx,
            |menu| menu,
        )
    }

    fn on_added_to(
        &mut self,
        tab_panel: WeakEntity<TabPanel>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.tab_panel = Some(tab_panel);
    }

    fn dump(&self, _cx: &App) -> PanelState {
        let mut state = PanelState::new(self);
        state.info = PanelInfo::panel(serde_json::json!({
            "source": serde_json::to_value(&self.source).ok(),
            "table": serde_json::to_value(&self.table).ok(),
            "key": Self::tab_key(&self.source, &self.table),
        }));
        state
    }
}

impl Render for DbGridPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.needs_load {
            self.needs_load = false;
            self.fetch_page(cx);
        }
        let dismiss = cx.listener(|this, _: &gpui::MouseDownEvent, _w, cx| {
            this.tab_menu = None;
            cx.notify();
        });

        // Sortable header cells.
        let header_cells: Vec<AnyElement> = self
            .page
            .columns
            .iter()
            .enumerate()
            .map(|(i, col)| {
                let marker = match self.sort {
                    Some((c, dir)) if c == i => format!("  {}", dir.marker()),
                    _ => String::new(),
                };
                header_cell(i, col, &marker)
                    .cursor_pointer()
                    .hover(|d| d.bg(theme::row_hover()))
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.sort_by(i, cx)))
                    .into_any_element()
            })
            .collect();

        div()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                super::close_this_tab(&this.tab_panel, cx.entity(), window, cx)
            }))
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::surface_base())
            .text_color(theme::text_primary())
            .child(self.render_toolbar(cx))
            .when_some(self.error.clone(), |d, err| {
                d.child(
                    div()
                        .p_2()
                        .text_size(theme::text_xs())
                        .text_color(theme::git_deleted())
                        .child(err),
                )
            })
            .child(grid(&self.page, self.offset, header_cells))
            .child(self.render_pagination(cx))
            .children(super::tab_menu_overlay(self.tab_menu.as_ref(), dismiss, window))
    }
}

impl DbGridPanel {
    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .w_full()
            .px_2()
            .py(px(4.))
            .border_b_1()
            .border_color(theme::border_subtle())
            .child(
                div()
                    .text_size(theme::text_sm())
                    .text_color(theme::text_primary())
                    .child(self.table.name.clone()),
            )
            .when_some(self.page.total, |d, n| {
                d.child(
                    div()
                        .text_size(theme::text_xs())
                        .text_color(theme::text_muted())
                        .child(format!("{n} rows")),
                )
            })
            .when(self.loading, |d| {
                d.child(
                    div()
                        .text_size(theme::text_xs())
                        .text_color(theme::text_muted())
                        .child("loading…"),
                )
            })
            .child(div().flex_1())
            .when(self.sort.is_some(), |d| {
                d.child(toolbar_button(
                    "db-reset-sort",
                    "Reset sort",
                    cx.listener(|this, _ev, _w, cx| {
                        this.sort = None;
                        this.offset = 0;
                        this.fetch_page(cx);
                    }),
                ))
            })
            .child(toolbar_button(
                "db-refresh",
                "Refresh",
                cx.listener(|this, _ev, _w, cx| this.fetch_page(cx)),
            ))
    }

    fn render_pagination(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let limit = db_source::PAGE_LIMIT;
        let shown = self.page.rows.len();
        let offset = self.offset;
        let total = self.page.total;
        let from = if shown == 0 { 0 } else { offset + 1 };
        let to = offset + shown;
        let at_start = offset == 0;
        let at_end = shown < limit || total.is_some_and(|t| to as i64 >= t);
        let label = match total {
            Some(t) => format!("rows {from}–{to} of {t}"),
            None => format!("rows {from}–{to}"),
        };
        let last_offset = match total {
            Some(t) if t > 0 => ((t as usize - 1) / limit) * limit,
            _ => offset,
        };

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .w_full()
            .px_2()
            .py(px(3.))
            .border_t_1()
            .border_color(theme::border_subtle())
            .bg(theme::surface_void())
            .child(page_button("db-first", "⏮", at_start, cx.listener(|this, _e, _w, cx| this.page_to(0, cx))))
            .child(page_button(
                "db-prev",
                "‹",
                at_start,
                cx.listener(move |this, _e, _w, cx| this.page_to(offset.saturating_sub(limit), cx)),
            ))
            .child(
                div()
                    .text_size(theme::text_xs())
                    .text_color(theme::text_muted())
                    .child(label),
            )
            .child(page_button(
                "db-next",
                "›",
                at_end,
                cx.listener(move |this, _e, _w, cx| this.page_to(offset + limit, cx)),
            ))
            .child(page_button(
                "db-last",
                "⏭",
                at_end || total.is_none(),
                cx.listener(move |this, _e, _w, cx| this.page_to(last_offset, cx)),
            ))
    }
}
