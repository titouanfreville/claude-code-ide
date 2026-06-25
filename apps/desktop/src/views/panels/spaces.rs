//! Spaces tab bar — the second-row space switcher (GitKraken / IntelliJ
//! project-tabs style), in the moonlight-noir language. It sits **below** the main
//! [`toolbar`](super::toolbar) (which carries the project + branch selectors and the
//! create actions); this bar is the quick **tab strip** of already-open spaces.
//!
//! A space is a project: its own code (the rails root at it) and its own sessions
//! (the fleet scopes to it). The operator switches spaces by clicking a tab and
//! closes one with the per-tab **"×"** ([`ProjectSpace::remove_space`]) — revealed
//! on hover/active so resting tabs stay quiet. The leftmost **"⊞ Overview"** tab is
//! the global view — the whole fleet across every space. New spaces are opened from
//! the toolbar's project selector ("Open Project…"); new sessions from its ＋ Session.
//!
//! Noir specifics: the bar sits on the same [`theme::surface_void`] night floor as
//! the status bar and activity rail, closed by a bottom moonrise hairline. The
//! active tab carries a glowing accent underline; a space whose sessions need the
//! operator (Waiting/Errored) shows a lit **attention dot** in the worst status
//! color, so the fleet is glanceable from any space.
//!
//! This is window chrome, **not** a dock [`Panel`](gpui_component::dock::Panel): it
//! is rendered by [`Workspace`] above the [`DockArea`], so its click handlers run in
//! `Context<Workspace>` and call back into the workspace's space-switch methods. The
//! rails (file tree / structure / terminal) and the fleet `cx.observe` the shared
//! [`ProjectSpace`] and re-root / re-scope when the active space changes.

use gpui::prelude::*;
use gpui::{div, linear_color_stop, linear_gradient, px, Context, FontWeight, Hsla, SharedString};

use crate::views::project_space::SpaceId;
use crate::views::theme;
use crate::views::workspace::Workspace;

/// One space's render-ready tab snapshot (decoupled from the borrowed model so the
/// click handlers are `'static`).
pub struct SpaceTab {
    pub id: SpaceId,
    pub label: String,
    pub active: bool,
    /// Worst "broke / stuck" status color among this space's sessions
    /// (Errored/Incomplete > Stuck), or `None` when none. Shown as a steady dot.
    pub attention: Option<Hsla>,
    /// Any session in this space is awaiting the operator (`NeedsInput`). Shown as the
    /// blinking amber caret — kept separate from `attention` so it is never hidden
    /// behind a higher-severity error dot.
    pub needs_input: bool,
}

/// Render the top space-tab bar: `⊞ Overview │ <space tabs> │ ＋ Session ＋ Space`.
/// `overview_active` is true when no space is selected (the global fleet view).
pub fn space_tab_bar(
    tabs: Vec<SpaceTab>,
    overview_active: bool,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    div()
        .id("space-tab-bar")
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .w_full()
        .h(px(30.))
        .px_1()
        .gap(px(2.))
        .bg(theme::surface_void())
        // The bottom moonrise hairline — mirrors the status bar's top edge so the
        // two bars frame the workspace.
        .child(moon_hairline_bottom())
        // ⊞ Overview — the global, all-spaces fleet.
        .child(
            tab_shell("space-tab-overview", "space-grp-overview", overview_active)
                .child("⊞ Overview")
                .on_click(cx.listener(|this, _ev, _w, cx| this.select_overview(cx))),
        )
        .children(tabs.into_iter().enumerate().map(|(i, tab)| {
            let id = tab.id.clone();
            let close_id = tab.id.clone();
            let group: SharedString = format!("space-grp-{i}").into();
            let mut shell = tab_shell(("space-tab", i), group.clone(), tab.active);
            // Steady attention dot — a session in this space broke / is stuck
            // (Errored/Incomplete/Stuck). NeedsInput is NOT here — it gets the caret.
            if let Some(color) = tab.attention {
                shell = shell.child(
                    div()
                        .w(px(5.))
                        .h(px(5.))
                        .flex_none()
                        .rounded_full()
                        .bg(color)
                        .shadow(theme::glow(color)),
                );
            }
            // Blinking amber caret — a session here is waiting for the operator to type.
            if tab.needs_input {
                shell = shell.child(super::needs_input_caret(("space-tab-caret", i)));
            }
            shell
                .child(
                    div()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(tab.label),
                )
                .child(
                    // Per-tab "×" close — hidden at rest, revealed on tab hover or
                    // while active (stop-propagation so it doesn't also select).
                    div()
                        .id(("space-tab-close", i))
                        .px_1()
                        .text_color(theme::text_muted())
                        .when(!tab.active, |d| {
                            d.opacity(0.).group_hover(group.clone(), |s| s.opacity(1.))
                        })
                        .hover(|d| d.text_color(theme::text_primary()))
                        .child("×")
                        .on_click(cx.listener(move |this, _ev, _w, cx| {
                            cx.stop_propagation();
                            this.remove_space(close_id.clone(), cx);
                        })),
                )
                .on_click(cx.listener(move |this, _ev, _w, cx| {
                    this.select_space(id.clone(), cx)
                }))
        }))
        // Spacer keeps the tabs left-aligned. The create actions (＋ Session, Open
        // Project) now live in the main toolbar / its project selector dropdown.
        .child(div().flex_1())
}

/// Shared shell for a single tab. Active = raised pill + accent text + a glowing
/// moonlit underline; inactive = muted, waking on hover. `group` names the tab's
/// hover scope so the close "×" can reveal itself.
fn tab_shell(
    id: impl Into<gpui::ElementId>,
    group: impl Into<SharedString>,
    active: bool,
) -> gpui::Stateful<gpui::Div> {
    let mut tab = div()
        .id(id)
        .group(group)
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .px_3()
        .py(px(3.))
        .rounded(theme::radius_sm())
        .cursor_pointer()
        .text_size(px(12.))
        .max_w(px(160.))
        .overflow_hidden()
        // Single-line: the name widens the tab up to `max_w`, then truncates — never
        // wraps to a second line (which used to grow the tab past the 30px bar).
        .whitespace_nowrap();
    tab = if active {
        tab.bg(theme::surface_raised())
            .text_color(theme::accent())
            .font_weight(FontWeight::MEDIUM)
            .child(
                // The moonlit underline — the active tab's phosphor lamp.
                div()
                    .absolute()
                    .bottom_0()
                    .left_2()
                    .right_2()
                    .h(px(2.))
                    .rounded_full()
                    .bg(theme::accent())
                    .shadow(theme::glow(theme::accent())),
            )
    } else {
        tab.text_color(theme::text_muted())
            .hover(|d| d.bg(theme::row_hover()).text_color(theme::text_secondary()))
    };
    tab
}

/// The 1px bottom hairline: transparent at the edges, moonlight at the center —
/// two mirrored gradients meeting in the middle (gpui gradients take two stops).
fn moon_hairline_bottom() -> gpui::Div {
    let dark = theme::tint(theme::accent(), 0.0);
    let lit = theme::tint(theme::accent(), 0.45);
    div()
        .absolute()
        .bottom_0()
        .left_0()
        .w_full()
        .h(px(1.))
        .flex()
        .flex_row()
        .child(div().flex_1().h_full().bg(linear_gradient(
            90.,
            linear_color_stop(dark, 0.),
            linear_color_stop(lit, 1.),
        )))
        .child(div().flex_1().h_full().bg(linear_gradient(
            90.,
            linear_color_stop(lit, 0.),
            linear_color_stop(dark, 1.),
        )))
}
