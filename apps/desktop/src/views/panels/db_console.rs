//! SQL console — a scratch query editor bound to a data source (center tab).
//!
//! Opened from the [`db_observer`](super::db_observer) tree's "SQL Console" action. It's
//! the JetBrains "console as a file": a SQL editor whose **Run** executes against the
//! bound connection (read-only `SELECT`/`WITH`) and renders the result in the shared
//! [`db_grid`](super::db_grid) grid. Not persisted to disk — a scratch buffer per tab.

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    div, px, AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, MouseDownEvent,
    WeakEntity, Window,
};
use gpui_component::dock::{Panel, PanelEvent, PanelInfo, PanelState, TabPanel};
use gpui_component::input::{Input, InputState};
use gpui_component::Sizable;

use super::db_grid::{self, header_cell, toolbar_button};
use super::db_source::{self, DataSource, Page};
use super::CloseTab;
use crate::views::theme;

pub struct DbConsolePanel {
    source: DataSource,
    /// SQL editor buffer (stood up lazily — needs a `Window`).
    input: Option<Entity<InputState>>,
    result: Option<Page>,
    error: Option<String>,
    loading: bool,
    focus_handle: FocusHandle,
    tab_panel: Option<WeakEntity<TabPanel>>,
    tab_menu: Option<super::TabMenu>,
}

impl DbConsolePanel {
    pub fn new(source: DataSource, cx: &mut Context<Self>) -> Self {
        Self {
            source,
            input: None,
            result: None,
            error: None,
            loading: false,
            focus_handle: cx.focus_handle(),
            tab_panel: None,
            tab_menu: None,
        }
    }

    /// Rebuild from persisted layout state (the dumped `source`); the buffer starts empty.
    /// Center DB tabs are wiped to home on layout restore, so a corrupt/missing source
    /// degrades to an empty source rather than failing the whole layout load.
    pub fn restore(info: &PanelInfo, cx: &mut Context<Self>) -> Self {
        let source = match info {
            PanelInfo::Panel(val) => val
                .get("source")
                .cloned()
                .and_then(|v| serde_json::from_value::<DataSource>(v).ok())
                .unwrap_or_else(|| DataSource::Sqlite(std::path::PathBuf::new())),
            _ => DataSource::Sqlite(std::path::PathBuf::new()),
        };
        Self::new(source, cx)
    }

    /// Run the editor's query against the bound source, off the UI thread.
    fn run(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.input.clone() else {
            return;
        };
        let sql = input.read(cx).value().trim().to_string();
        if sql.is_empty() {
            return;
        }
        let src = self.source.clone();
        self.loading = true;
        self.error = None;
        cx.spawn(async move |weak, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { db_source::run_query(&src, &sql) })
                .await;
            let _ = weak.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(page) => {
                        this.result = Some(page);
                        this.error = None;
                    }
                    Err(e) => {
                        this.error = Some(e);
                        this.result = None;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Dedup key shared with [`OpenRequest::DbConsole`](crate::views::center_requests) —
    /// one console per data source.
    pub fn tab_key(source: &DataSource) -> String {
        format!("dbconsole:{}", source.key())
    }
}

impl Focusable for DbConsolePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl super::TabMenuHost for DbConsolePanel {
    fn tab_menu_slot(&mut self) -> &mut Option<super::TabMenu> {
        &mut self.tab_menu
    }
}

impl EventEmitter<PanelEvent> for DbConsolePanel {}

impl Panel for DbConsolePanel {
    fn panel_name(&self) -> &'static str {
        "DbConsole"
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        super::tab_title(
            format!("SQL · {}", self.source.label()),
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
            "key": Self::tab_key(&self.source),
        }));
        state
    }
}

impl Render for DbConsolePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.input.is_none() {
            self.input = Some(cx.new(|cx| {
                InputState::new(window, cx)
                    .multi_line(true)
                    .auto_grow(3, 12)
                    .placeholder("SELECT … (read-only) — Run to execute against this connection")
            }));
        }
        let dismiss = cx.listener(|this, _: &MouseDownEvent, _w, cx| {
            this.tab_menu = None;
            cx.notify();
        });

        // Result region: error, grid (static headers), or a hint.
        let results = if let Some(err) = &self.error {
            div()
                .p_2()
                .text_size(theme::text_xs())
                .text_color(theme::git_deleted())
                .child(err.clone())
                .into_any_element()
        } else if let Some(page) = &self.result {
            let headers: Vec<AnyElement> = page
                .columns
                .iter()
                .enumerate()
                .map(|(i, col)| header_cell(i, col, "").into_any_element())
                .collect();
            db_grid::grid(page, 0, headers).into_any_element()
        } else {
            div()
                .p_3()
                .text_size(theme::text_xs())
                .text_color(theme::text_muted())
                .child("Run a read-only query to see results")
                .into_any_element()
        };

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
            // Toolbar: bound connection + Run.
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .w_full()
                    .px_2()
                    .py(px(4.))
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .child(
                        div()
                            .text_size(theme::text_xs())
                            .text_color(theme::text_secondary())
                            .child(format!("→ {}", self.source.label())),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(theme::text_muted())
                            .child("read-only · SELECT / WITH"),
                    )
                    .when(self.loading, |d| {
                        d.child(
                            div()
                                .text_size(theme::text_xs())
                                .text_color(theme::text_muted())
                                .child("running…"),
                        )
                    })
                    .child(div().flex_1())
                    .child(toolbar_button(
                        "db-run",
                        "Run ▸",
                        cx.listener(|this, _ev, _w, cx| this.run(cx)),
                    )),
            )
            // SQL editor.
            .child(
                div()
                    .flex_none()
                    .px_2()
                    .py(px(4.))
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .children(self.input.as_ref().map(|i| Input::new(i).small())),
            )
            // Results.
            .child(div().flex_1().min_h(px(0.)).overflow_hidden().child(results))
            .children(super::tab_menu_overlay(self.tab_menu.as_ref(), dismiss, window))
    }
}
