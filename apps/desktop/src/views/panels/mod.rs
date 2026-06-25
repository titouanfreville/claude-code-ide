//! Edge-rail dockable panels — the IDE-classic "plugins".
//!
//! Each panel is a `gpui_component::dock::Panel`: a self-contained view the
//! [`workspace`](super::workspace) mounts into a dock (left / bottom / right /
//! center) and the operator can drag, tab, collapse, or zoom (JetBrains-style
//! reveal-on-demand). Adding a new dockable feature = adding a `Panel` here and
//! registering it in the workspace.
//!
//! - [`file_tree`] — project explorer, follows the focused session (FR8).
//! - [`terminal`] — the operator's own manual ANSI terminal (not Claude-related).
//! - [`code_editor`] — opens a file as a center tab (syntax-highlighted viewer).
//! - [`session_monitor`] — "focus mode" view of one session (center tab).
//! - [`plan_review`] — shows a plan the agent proposed (center tab; plan-review gate).
//! - [`code_review`] — per-file diff + full-review + verdict (center tab; review gate).
//! - [`structure`] — outline of the active file (JetBrains "Structure"; rail panel).
//! - [`db_observer`] — database overview tool window: data-source tree (DB → tables → columns).
//! - [`db_grid`] — data editor: a table's rows in a center tab (sortable, paginated).
//! - [`db_console`] — SQL console bound to a data source (scratch query center tab).
//! - [`db_source`] — read-only data-source drivers (SQLite + Postgres).

pub mod activity_rail;
pub mod code_editor;
pub mod code_review;
pub mod commit;
pub mod db_console;
pub mod db_grid;
pub mod db_observer;
pub mod db_source;
pub mod file_tree;
pub mod git_panel;
pub mod http_panel;
pub mod mcp_authorize;
pub mod outline;
pub mod plan_review;
pub mod problems;
pub mod run_console;
pub mod services;
pub mod session_monitor;
pub mod spaces;
pub mod status_bar;
pub mod structure;
pub mod terminal;
pub mod toolbar;

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    actions, anchored, deferred, div, px, pulsating_between, AnyElement, Animation, AnimationExt,
    App, Context, DismissEvent, Entity, FocusHandle, Hsla, MouseButton, MouseDownEvent, Pixels,
    Point, SharedString, Subscription, WeakEntity, Window,
};
use gpui_component::dock::{Panel, PanelView, TabPanel};
use gpui_component::menu::PopupMenu;
use moonlight_domain::session::AttentionKind;

use crate::views::theme;

/// A softly blinking amber **input caret** (`▮`) — the "this session is waiting for *you*
/// to type" beacon, mirroring a terminal cursor. Shared by the space-tab bar and the dock
/// session tabs so the two surfaces signal `NeedsInput` identically. Motion is reserved
/// for `NeedsInput`: a blink always means "needs you now", which is why the steady error /
/// stuck dot stays motionless. `id` must be unique per call site (it keys the animation
/// state) — callers seed it with the tab index or the panel's entity id.
pub fn needs_input_caret(id: impl Into<gpui::ElementId>) -> impl IntoElement {
    let amber = theme::attention_color(AttentionKind::NeedsInput);
    div()
        .flex_none()
        .text_color(amber)
        .text_size(px(12.))
        .child("▮")
        .with_animation(
            id,
            // ~1.1s breath; eased 0.25↔1.0 opacity reads as a calm cursor blink rather
            // than a hard on/off — alive and waiting, not alarming.
            Animation::new(Duration::from_millis(1100))
                .repeat()
                .with_easing(pulsating_between(0.25, 1.0)),
            |el, delta| el.opacity(delta),
        )
}

actions!(moonlight_tab, [CloseTab]);

/// An open per-tab right-click menu: the built [`PopupMenu`], the window position to
/// anchor it at, and a subscription that clears the panel's slot when the menu
/// dismisses (item chosen, Escape, or outside click). Stored in the panel because
/// gpui-component's tab bar can't host the menu itself — see [`TabMenuHost`].
pub struct TabMenu {
    menu: Entity<PopupMenu>,
    at: Point<Pixels>,
    _dismiss: Subscription,
}

/// Implemented by every closable center panel so [`tab_title`]'s right-click handler
/// can stash an open menu in the panel's own state, and [`tab_menu_overlay`] (called
/// from the panel's `render`) can draw it. The library's `TabBar`/`Tab` attaches its
/// own interactive hitbox and offers no per-tab context-menu hook, so — mirroring
/// gpui-component's own `tree.rs` — we capture the right-click on the tab title
/// (`on_mouse_down(MouseButton::Right)`, which *does* fire when nested) and render
/// the [`PopupMenu`] from the panel body as a deferred, window-anchored overlay.
pub trait TabMenuHost: Panel + Sized {
    fn tab_menu_slot(&mut self) -> &mut Option<TabMenu>;
}

/// A center tab's title: the label + a per-tab close "×" + a right-click menu.
/// Returned from each panel's [`gpui_component::dock::Panel::title`], which
/// gpui-component calls **once per tab** when rendering the tab bar — so the "×"
/// shows on *every* tab, not just the active one. (The library's `title_suffix`
/// hook only renders for the active tab, which is why the close affordance used to
/// be stuck at the far right of the bar.)
///
/// - **Close ×** removes *this specific* panel via [`TabPanel::remove_panel`], which
///   also bypasses the library's `closable`/lock gating.
/// - **Right-click** builds a [`PopupMenu`] and stashes it in the panel (via
///   [`TabMenuHost`]); the panel body renders it (see [`tab_menu_overlay`]). The
///   shared `Close` item dispatches [`CloseTab`] to `focus`; `extend_menu` appends
///   panel items (Split, Copy Path) that dispatch to the same `focus`, so the
///   panel's existing `on_action` handlers pick them up. Each tab targets its own
///   panel, so an *inactive* tab's menu still acts on that tab.
///
/// `id_seed` (the panel's entity id) keeps the interactive title element's id stable
/// and unique per tab.
pub fn tab_title<P: TabMenuHost>(
    label: impl Into<SharedString>,
    label_color: Option<Hsla>,
    id_seed: u64,
    tab_panel: Option<WeakEntity<TabPanel>>,
    panel: Arc<dyn PanelView>,
    focus: FocusHandle,
    cx: &mut Context<P>,
    extend_menu: impl Fn(PopupMenu) -> PopupMenu + 'static,
) -> AnyElement {
    let label: SharedString = label.into();
    // A session may tint its tab label with its operator-chosen color. (Dock tabs are
    // already height-clipped + nowrapped by gpui-component's own tab chrome, so no
    // truncation is needed here — the space-tab nav handles its own; see `spaces.rs`.)
    let labelled = || {
        let mut e = div().child(label.clone());
        if let Some(color) = label_color {
            e = e.text_color(color);
        }
        e
    };

    // Not yet added to a TabPanel (no captured handle): render the label alone.
    let Some(tab_panel) = tab_panel else {
        return labelled().into_any_element();
    };

    let close_tp = tab_panel.clone();
    let close_panel = panel.clone();
    let extend = Rc::new(extend_menu);

    div()
        .id(SharedString::from(format!("moonlight-tab-title-{id_seed}")))
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .child(labelled())
        .child(
            div()
                .id("tab-close")
                .px_1()
                .cursor_pointer()
                .text_color(theme::text_muted())
                .hover(|d| d.text_color(theme::text_primary()))
                .child("×")
                .on_click(move |_ev, window, cx| {
                    // Don't let the click also re-activate the tab we're closing.
                    cx.stop_propagation();
                    if let Some(tp) = close_tp.upgrade() {
                        let panel = close_panel.clone();
                        tp.update(cx, |tp, cx| tp.remove_panel(panel, window, cx));
                    }
                }),
        )
        // The tab bar can't host a context menu, but a nested right mouse-down DOES
        // fire (mirrors gpui-component `tree.rs`). Build the menu and stash it; the
        // panel body renders it via `tab_menu_overlay`.
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                let at = ev.position;
                let focus = focus.clone();
                let extend = extend.clone();
                let menu = PopupMenu::build(window, cx, move |menu, _w, _c| {
                    let menu = menu
                        .action_context(focus.clone())
                        .menu("Close", Box::new(CloseTab));
                    extend(menu)
                });
                let dismiss = cx.subscribe(&menu, |this, _menu, _ev: &DismissEvent, cx| {
                    *this.tab_menu_slot() = None;
                    cx.notify();
                });
                *this.tab_menu_slot() = Some(TabMenu {
                    menu,
                    at,
                    _dismiss: dismiss,
                });
                cx.notify();
            }),
        )
        .into_any_element()
}

/// Render the panel's open right-click tab menu (if any) as a deferred,
/// window-anchored [`PopupMenu`] with a full-window dismiss backdrop. Call from each
/// panel's `render`:
/// ```ignore
/// let dismiss = cx.listener(|this, _: &MouseDownEvent, _w, cx| {
///     *this.tab_menu_slot() = None;
///     cx.notify();
/// });
/// root.children(super::tab_menu_overlay(self.tab_menu_slot().as_ref(), dismiss, window))
/// ```
pub fn tab_menu_overlay(
    slot: Option<&TabMenu>,
    dismiss: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    window: &Window,
) -> Option<AnyElement> {
    let tm = slot?;
    let menu = tm.menu.clone();
    let at = tm.at;
    let size = window.bounds().size;
    Some(
        deferred(
            anchored().child(
                div()
                    .occlude()
                    .w(size.width)
                    .h(size.height)
                    // Clicking outside the menu dismisses it (item clicks dismiss via
                    // the PopupMenu's own DismissEvent → the panel's subscription).
                    .on_mouse_down(MouseButton::Left, dismiss)
                    .child(
                        anchored()
                            .position(at)
                            .snap_to_window_with_margin(px(8.))
                            .child(menu),
                    ),
            ),
        )
        .with_priority(1)
        .into_any_element(),
    )
}

/// The uniform tool-window **hide** button (JetBrains' "▔ Hide"): a quiet "✕"
/// rendered at the right edge of every tool window's header (dock panels return
/// it from [`Panel::title_suffix`]; the workspace-owned bottom tools append it to
/// their own bars). Clicking emits `request` on the shared chrome channel — the
/// workspace flips the matching dock/tool flag (see
/// [`chrome_requests`](crate::views::chrome_requests)).
pub fn tool_hide_button(
    id: &'static str,
    request: crate::views::chrome_requests::ChromeRequest,
) -> AnyElement {
    div()
        .id(id)
        .flex_none()
        .px_1()
        .cursor_pointer()
        .text_size(px(11.))
        .text_color(theme::text_muted())
        .hover(|d| d.text_color(theme::text_primary()))
        .child("✕")
        .tooltip(|window, cx| {
            gpui_component::tooltip::Tooltip::new("Hide").build(window, cx)
        })
        .on_click(move |_ev, _window, cx| {
            cx.stop_propagation();
            let chrome = cx.global::<crate::views::workspace::ShellDeps>().chrome.clone();
            chrome.update(cx, |_, cx| cx.emit(request));
        })
        .into_any_element()
}

/// Shared `CloseTab` handler body: remove *this* panel from its tab panel. Each
/// closable center panel exposes a thin `on_action` method that calls this.
pub fn close_this_tab<P: Panel>(
    tab_panel: &Option<WeakEntity<TabPanel>>,
    entity: Entity<P>,
    window: &mut Window,
    cx: &mut App,
) {
    if let Some(tp) = tab_panel.as_ref().and_then(|w| w.upgrade()) {
        let panel: Arc<dyn PanelView> = Arc::new(entity);
        tp.update(cx, |tp, cx| tp.remove_panel(panel, window, cx));
    }
}
