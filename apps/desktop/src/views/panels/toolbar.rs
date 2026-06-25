//! The **main toolbar** — JetBrains New-UI style — hosted in the window's *custom
//! titlebar*, at the macOS traffic-light level (reclaiming the row the native title
//! used to waste). Built on gpui-component's [`TitleBar`], which makes the titlebar
//! transparent, leaves the 80px inset for the traffic lights, and handles
//! drag-to-move / double-click-zoom / the Linux+Windows window controls for us.
//!
//! Layout (after the traffic lights):
//! ```text
//! ● ● ●   ⌹ project ▾   ⎇ branch ▾   …flex…   ▶ run ▾   ＋ Session   ⌗ Explorer
//! ```
//! - **Project selector** — the active space's name; click opens a dropdown of the
//!   other open spaces + "＋ Open Project…" (the space switcher, JetBrains "project
//!   widget"). Each row calls back into the [`Workspace`].
//! - **Branch selector** — the active repo's git branch; click lists local branches
//!   to check out. Hidden when the space isn't a git repo.
//! - **Run widget** — a split `▶ | target ▾`: ▶ launches the active run target in the
//!   operator terminal; the chip's ▾ picks among detected targets (cargo / npm).
//! - **＋ Session** / **Explorer toggle** — the operator action tools.
//!
//! Like [`status_bar`](super::status_bar) and [`spaces`](super::spaces) this is
//! **window chrome, not a dock `Panel`**: the [`Workspace`] renders it from a
//! per-frame [`ToolbarSnapshot`], so handlers run in `Context<Workspace>`. Dropdowns
//! are `deferred` overlays (raised paint priority) anchored under their chip, so they
//! paint over the tab bar / dock below.

use gpui::prelude::*;
use gpui::{deferred, div, px, Context, SharedString};
use gpui_component::TitleBar;
use moonlight_domain::session::SessionStatus;

use crate::views::project_space::SpaceId;
use crate::views::theme;
use crate::views::workspace::Workspace;

/// The play-button green — the "session Done" status hue doubles as the run/go color.
fn run_green() -> gpui::Hsla {
    theme::status_color(SessionStatus::Done)
}

/// The stop-button red — the "Errored" status hue doubles as the stop color.
fn stop_red() -> gpui::Hsla {
    theme::status_color(SessionStatus::Errored)
}

/// One open space, for the project selector dropdown.
pub struct SpaceRow {
    pub id: SpaceId,
    pub label: String,
    pub active: bool,
}

/// One run target, for the run-widget dropdown.
pub struct RunRow {
    pub id: String,
    pub label: String,
    pub glyph: &'static str,
    pub active: bool,
}

/// Everything the toolbar draws, snapshotted into owned values so its handlers stay
/// `'static` (decoupled from the borrowed models).
pub struct ToolbarSnapshot {
    /// Active space name (or a dash on the overview).
    pub project: String,
    /// Active repo's git branch, or `None` when the space isn't a git repo.
    pub branch: Option<String>,
    pub space_menu_open: bool,
    pub branch_menu_open: bool,
    pub run_menu_open: bool,
    /// Explorer (left dock) open — lights the explorer toggle.
    pub left_open: bool,
    /// Open spaces for the project dropdown.
    pub spaces: Vec<SpaceRow>,
    /// Local branches for the branch dropdown (only filled while it's open).
    pub branches: Vec<String>,
    /// Active run target's label + kind glyph, or `None` when no config is detected.
    pub run_label: Option<String>,
    pub run_glyph: &'static str,
    /// A run is live in the Run console — the toolbar's ▶ becomes a red ⏹ (JetBrains
    /// behavior: stop replaces play while running).
    pub run_running: bool,
    /// Detected run targets for the run dropdown (only filled while it's open).
    pub run_configs: Vec<RunRow>,
    /// Auto-phasing opt-in is on — lights the ⟳ toggle (the agent may move its own
    /// workflow phase without a cockpit approval).
    pub auto_phase: bool,
}

/// Build the main toolbar inside a styled [`TitleBar`]. The single child is a
/// full-width row split left (selectors) / right (action tools).
pub fn main_toolbar(snap: ToolbarSnapshot, cx: &mut Context<Workspace>) -> TitleBar {
    TitleBar::new()
        .bg(theme::surface_void())
        .border_color(theme::border_subtle())
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .w_full()
                .h_full()
                .pl_2()
                .pr_1()
                .text_size(theme::text_sm())
                // ── Left: project + branch selectors ──────────────────────────────
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .child(project_selector(&snap, cx))
                        .when(snap.branch.is_some(), |d| d.child(branch_selector(&snap, cx))),
                )
                // ── Right: run widget + session + explorer ────────────────────────
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .child(run_widget(&snap, cx))
                        .child(new_session_btn(cx))
                        .child(auto_phase_toggle(snap.auto_phase, cx))
                        .child(explorer_toggle(snap.left_open, cx)),
                ),
        )
}

// ─────────────────────────────────────────────────────────────────────────────
// Project selector
// ─────────────────────────────────────────────────────────────────────────────

fn project_selector(snap: &ToolbarSnapshot, cx: &mut Context<Workspace>) -> impl IntoElement {
    let open = snap.space_menu_open;
    let chip = chip("tb-project", "⌹", &snap.project, open, true).on_click(
        cx.listener(|this, _ev, _w, cx| this.toggle_space_menu(cx)),
    );
    div()
        .relative()
        .child(chip)
        .when(open, |d| d.child(project_menu(&snap.spaces, cx)))
}

fn project_menu(spaces: &[SpaceRow], cx: &mut Context<Workspace>) -> impl IntoElement {
    let mut list = menu_panel();
    for s in spaces {
        let id = s.id.clone();
        list = list.child(
            menu_row(SharedString::from(format!("tb-space-{}", s.id.as_str())), s.active)
                .child(div().w(px(12.)).text_color(theme::accent()).child(if s.active {
                    "✓"
                } else {
                    ""
                }))
                .child(div().flex_1().child(s.label.clone()))
                .on_click(cx.listener(move |this, _ev, _w, cx| {
                    this.select_space(id.clone(), cx);
                    this.close_toolbar_menus(cx);
                })),
        );
    }
    list = list.child(menu_divider()).child(
        menu_row("tb-open-project", false)
            .child(div().w(px(12.)).text_color(theme::text_muted()).child("＋"))
            .child(div().flex_1().text_color(theme::text_secondary()).child("Open Project…"))
            .on_click(cx.listener(|this, _ev, _w, cx| {
                this.close_toolbar_menus(cx);
                this.new_space(cx);
            })),
    );
    dropdown(list)
}

// ─────────────────────────────────────────────────────────────────────────────
// Branch selector
// ─────────────────────────────────────────────────────────────────────────────

fn branch_selector(snap: &ToolbarSnapshot, cx: &mut Context<Workspace>) -> impl IntoElement {
    let open = snap.branch_menu_open;
    let branch = snap.branch.clone().unwrap_or_default();
    let chip = chip("tb-branch", "⎇", &branch, open, true).on_click(
        cx.listener(|this, _ev, _w, cx| this.toggle_branch_menu(cx)),
    );
    div()
        .relative()
        .child(chip)
        .when(open, |d| d.child(branch_menu(&branch, &snap.branches, cx)))
}

fn branch_menu(current: &str, branches: &[String], cx: &mut Context<Workspace>) -> impl IntoElement {
    let mut list = menu_panel();
    if branches.is_empty() {
        list = list.child(
            div()
                .px_2()
                .py_1()
                .text_color(theme::text_muted())
                .child("No branches"),
        );
    }
    for b in branches {
        let is_current = b == current;
        let name = b.clone();
        list = list.child(
            menu_row(SharedString::from(format!("tb-branch-{b}")), is_current)
                .child(div().w(px(12.)).text_color(theme::accent()).child(if is_current {
                    "✓"
                } else {
                    ""
                }))
                .child(div().flex_1().child(b.clone()))
                .on_click(cx.listener(move |this, _ev, _w, cx| {
                    this.close_toolbar_menus(cx);
                    this.checkout_branch(name.clone(), cx);
                })),
        );
    }
    dropdown(list)
}

// ─────────────────────────────────────────────────────────────────────────────
// Run widget
// ─────────────────────────────────────────────────────────────────────────────

fn run_widget(snap: &ToolbarSnapshot, cx: &mut Context<Workspace>) -> impl IntoElement {
    let open = snap.run_menu_open;
    let has_config = snap.run_label.is_some();
    let label = snap.run_label.clone().unwrap_or_else(|| "No run config".to_string());
    let glyph = snap.run_glyph;

    // The action button: ▶ launches the active target; while a run is live it is
    // **replaced** by a red ⏹ that stops it (JetBrains play⇄stop swap).
    let action = if snap.run_running {
        let red = stop_red();
        div()
            .id("tb-run-stop")
            .flex()
            .items_center()
            .justify_center()
            .w(px(22.))
            .h(px(22.))
            .rounded(theme::radius_sm())
            .cursor_pointer()
            .text_color(red)
            .bg(theme::tint(red, 0.10))
            .hover(|d| d.bg(theme::tint(red, 0.22)))
            .child("⏹")
            .on_click(cx.listener(|this, _ev, _w, cx| this.stop_active_target(cx)))
    } else {
        div()
            .id("tb-run-play")
            .flex()
            .items_center()
            .justify_center()
            .w(px(22.))
            .h(px(22.))
            .rounded(theme::radius_sm())
            .text_color(if has_config {
                run_green()
            } else {
                theme::text_muted()
            })
            .when(has_config, |d| {
                d.cursor_pointer()
                    .hover(|d| d.bg(theme::tint(run_green(), 0.16)))
                    .on_click(cx.listener(|this, _ev, _w, cx| this.run_active_target(cx)))
            })
            .child("▶")
    };

    // target chip — kind glyph + label + ▾, opens the target menu.
    let target = chip("tb-run-target", glyph, &label, open, has_config).on_click(
        cx.listener(|this, _ev, _w, cx| this.toggle_run_menu(cx)),
    );

    div()
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(1.))
        .child(action)
        .child(target)
        .when(open, |d| d.child(run_menu(&snap.run_configs, cx)))
}

fn run_menu(configs: &[RunRow], cx: &mut Context<Workspace>) -> impl IntoElement {
    let mut list = menu_panel();
    for c in configs {
        let id = c.id.clone();
        list = list.child(
            menu_row(SharedString::from(format!("tb-run-{}", c.id)), c.active)
                .child(div().w(px(12.)).text_color(theme::accent()).child(if c.active {
                    "✓"
                } else {
                    ""
                }))
                .child(div().text_color(theme::text_muted()).child(c.glyph))
                .child(div().flex_1().child(c.label.clone()))
                .on_click(cx.listener(move |this, _ev, _w, cx| {
                    this.set_run_target(id.clone(), cx);
                    this.close_toolbar_menus(cx);
                })),
        );
    }
    dropdown(list)
}

// ─────────────────────────────────────────────────────────────────────────────
// Action tools
// ─────────────────────────────────────────────────────────────────────────────

fn new_session_btn(cx: &mut Context<Workspace>) -> impl IntoElement {
    div()
        .id("tb-new-session")
        .flex()
        .flex_row()
        .items_center()
        .gap(px(3.))
        .px_2()
        .h(px(24.))
        .rounded(theme::radius_sm())
        .cursor_pointer()
        .text_color(theme::accent())
        .bg(theme::tint(theme::accent(), 0.10))
        .hover(|d| d.bg(theme::tint(theme::accent(), 0.20)))
        .child("＋")
        .child("Session")
        .on_click(cx.listener(|this, _ev, _w, cx| this.new_session(cx)))
}

/// The auto-phasing toggle (⟳ + label): lit when the agent may move its own workflow
/// phase without a cockpit approval. Flips the shared `auto_phase` flag the actor reads.
fn auto_phase_toggle(lit: bool, cx: &mut Context<Workspace>) -> impl IntoElement {
    let color = if lit { theme::accent() } else { theme::text_muted() };
    div()
        .id("tb-autophase")
        .flex()
        .flex_row()
        .items_center()
        .gap(px(3.))
        .h(px(24.))
        .px_2()
        .rounded(theme::radius_sm())
        .text_color(color)
        .text_size(theme::text_2xs())
        .when(lit, |d| d.bg(theme::tint(theme::accent(), 0.10)))
        .when(!lit, |d| d.hover(|d| d.bg(theme::row_hover())))
        .cursor_pointer()
        .child("⟳")
        .child("auto-phase")
        .on_click(cx.listener(|this, _ev, _w, cx| this.toggle_auto_phase(cx)))
}

fn explorer_toggle(lit: bool, cx: &mut Context<Workspace>) -> impl IntoElement {
    let color = if lit { theme::accent() } else { theme::text_muted() };
    div()
        .id("tb-explorer")
        .flex()
        .items_center()
        .justify_center()
        .w(px(24.))
        .h(px(24.))
        .rounded(theme::radius_sm())
        .text_color(color)
        .when(lit, |d| d.bg(theme::tint(theme::accent(), 0.10)))
        .when(!lit, |d| d.hover(|d| d.bg(theme::row_hover())))
        .cursor_pointer()
        .child("⌗")
        .on_click(cx.listener(|this, _ev, window, cx| this.toggle_left_dock(window, cx)))
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared building blocks
// ─────────────────────────────────────────────────────────────────────────────

/// A selector chip: `icon  text  ▾`. Lit (its menu open) wears a faint accent wash;
/// idle wakes on hover. `enabled=false` greys it and drops the caret (e.g. no repo /
/// no run config).
fn chip(
    id: &'static str,
    icon: &'static str,
    text: &str,
    open: bool,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    let fg = if enabled {
        theme::text_secondary()
    } else {
        theme::text_muted()
    };
    let mut c = div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.))
        .px_2()
        .h(px(24.))
        .rounded(theme::radius_sm())
        .text_color(fg)
        .child(div().text_color(theme::tint(theme::accent(), 0.7)).child(icon))
        .child(div().max_w(px(180.)).overflow_hidden().child(text.to_string()));
    if enabled {
        c = c.cursor_pointer().child(
            div()
                .text_size(theme::text_2xs())
                .text_color(theme::text_muted())
                .child("▾"),
        );
    }
    if open {
        c = c.bg(theme::tint(theme::accent(), 0.12)).text_color(theme::text_primary());
    } else if enabled {
        c = c.hover(|d| d.bg(theme::row_hover()));
    }
    c
}

/// The inner column of a dropdown menu (rows are pushed by the caller).
fn menu_panel() -> gpui::Div {
    div().flex().flex_col().gap(px(1.)).p_1()
}

/// One menu row — hover-highlighted; `active` rows carry a faint accent wash.
fn menu_row(id: impl Into<gpui::ElementId>, active: bool) -> gpui::Stateful<gpui::Div> {
    let mut r = div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .rounded(theme::radius_sm())
        .cursor_pointer()
        .text_color(theme::text_secondary())
        .hover(|d| d.bg(theme::row_hover()));
    if active {
        r = r.bg(theme::tint(theme::accent(), 0.08));
    }
    r
}

fn menu_divider() -> gpui::Div {
    div()
        .my(px(2.))
        .h(px(1.))
        .w_full()
        .bg(theme::border_subtle())
}

/// Wrap a menu column in the moonlight-noir popover frame, raised over the bars below
/// as a `deferred` overlay anchored just under its chip (top of the row is the 34px
/// titlebar). Mirrors the status-bar inbox popover styling.
fn dropdown(list: gpui::Div) -> impl IntoElement {
    deferred(
        div()
            .absolute()
            .top(px(30.))
            .left_0()
            .min_w(px(200.))
            .max_w(px(320.))
            .rounded(theme::radius_md())
            .overflow_hidden()
            .bg(theme::surface_overlay())
            .border_1()
            .border_color(theme::border_subtle())
            .shadow(theme::overlay_shadow())
            .text_size(theme::text_sm())
            .child(list),
    )
    .with_priority(1)
}
