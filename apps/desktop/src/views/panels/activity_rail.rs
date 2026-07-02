//! The left **activity rail** — a slim icon strip on the window's left edge, the
//! noir cousin of VS Code's activity bar.
//!
//! It answers the shell's discoverability gap: collapsed docks used to have *no
//! visible affordance* to bring them back. The rail gives every major surface a
//! permanent, glanceable toggle — and a home for future features (search, git,
//! DB observer) as the shell grows. Buttons follow the moonlight-noir language:
//! a quiet muted glyph when idle, and a **lit** state (accent glyph + tint wash +
//! a glowing phosphor tick on the edge) when the surface is open/frontmost.
//!
//! Like [`status_bar`](super::status_bar) and [`spaces`](super::spaces) this is
//! **window chrome, not a dock [`Panel`](gpui_component::dock::Panel)**: the
//! [`Workspace`] renders it beside the `DockArea` from a per-frame [`RailSnapshot`]
//! (dock open-states read off `gpui_component::dock::Dock::is_open`), so the
//! handlers run in `Context<Workspace>` and call its toggle methods.

use gpui::prelude::*;
use gpui::{div, linear_color_stop, linear_gradient, px, ClickEvent, Context, Window};
use gpui_component::tooltip::Tooltip;

use crate::views::theme;
use crate::views::workspace::{BottomTool, LeftTool, Workspace};

/// Rail width — slim enough to stay chrome, wide enough for a 28px hit target.
const RAIL_W: f32 = 38.;

/// Per-frame snapshot of what each rail button reflects (dock open-states +
/// which center surface is frontmost), owned so handlers stay `'static`.
pub struct RailSnapshot {
    /// The Overview (global fleet grid) is the frontmost center surface.
    pub overview_active: bool,
    /// The left dock is open, and which **tool window** it fronts (Project ⇄
    /// Commit — each has its own stripe button; lit = open + frontmost).
    pub left_open: bool,
    pub left_tool: LeftTool,
    /// The Structure outline is visible (left dock open + outline shown). Its
    /// stripe button toggles the outline independently of the tree.
    pub structure_open: bool,
    /// The bottom dock is open, and which **tool window** it fronts (each bottom
    /// tool — Terminal, Run — has its own stripe button; lit = open + frontmost).
    pub bottom_open: bool,
    pub bottom_tool: BottomTool,
    /// The Run window's status lamp (live blue / verdict green-red), shown on its
    /// stripe button even while the dock is closed. `None` until something ran.
    pub run_lamp: Option<gpui::Hsla>,
    /// The Problems window's lamp (red = errors, amber = warnings only), visible
    /// even while the dock is closed. `None` when the open files are clean.
    pub problems_lamp: Option<gpui::Hsla>,
}

/// Render the activity rail. `cx` is the workspace context so buttons can toggle
/// docks (which needs the `Window`) and switch the center to the fleet.
///
/// JetBrains-stripe grouping: **project tools top** (Fleet, Project files,
/// Commit°, Structure), **runtime tools bottom** (Terminal, Run, Problems°,
/// Git°, Services°, Debug°). `°` = reserved slots — greyed placeholders claiming
/// the position their tool window will occupy (Commit/Problems/Git/Services are
/// scoped follow-up slices; Debug is the planned DAP phase 3).
pub fn activity_rail(snap: RailSnapshot, cx: &mut Context<Workspace>) -> impl IntoElement {
    div()
        .id("activity-rail")
        .relative()
        .flex()
        .flex_col()
        .items_center()
        .flex_none()
        .w(px(RAIL_W))
        .h_full()
        .py_2()
        .gap_1()
        .bg(theme::surface_void())
        // The rail's right edge carries the vertical moonrise hairline — the same
        // horizon line the status bar draws along its top, framing the dock.
        .child(edge_hairline(EdgeSide::Right))
        // ── Project tools (top): the surfaces that live in the upper area ──────────
        .child(rail_btn(
            "rail-fleet",
            grid_icon(snap.overview_active).into_any_element(),
            "Sessions overview (toggle)",
            snap.overview_active,
            cx.listener(|this, _ev: &ClickEvent, _w, cx| this.toggle_overview(cx)),
        ))
        .child(rail_btn(
            "rail-explorer",
            folder_icon(snap.left_open && snap.left_tool == LeftTool::Project).into_any_element(),
            "Project files",
            snap.left_open && snap.left_tool == LeftTool::Project,
            cx.listener(|this, _ev: &ClickEvent, window, cx| {
                this.toggle_left_tool(LeftTool::Project, window, cx)
            }),
        ))
        .child(rail_btn(
            "rail-commit",
            commit_icon(snap.left_open && snap.left_tool == LeftTool::Commit).into_any_element(),
            "Commit",
            snap.left_open && snap.left_tool == LeftTool::Commit,
            cx.listener(|this, _ev: &ClickEvent, window, cx| {
                this.toggle_left_tool(LeftTool::Commit, window, cx)
            }),
        ))
        .child(rail_btn(
            "rail-structure",
            structure_icon(snap.structure_open).into_any_element(),
            "Structure (toggle)",
            snap.structure_open,
            cx.listener(|this, _ev: &ClickEvent, window, cx| this.toggle_structure(window, cx)),
        ))
        // The flexible gap separates the **project** tools (top) from the **runtime**
        // tools (bottom) — terminal + run now, problems / git / services / debug to
        // come — so each tool sits in the region its surface opens in.
        .child(div().flex_1())
        // ── Runtime tools (bottom): one stripe button per bottom tool window ──────
        .child(rail_btn(
            "rail-terminal",
            terminal_icon(snap.bottom_open && snap.bottom_tool == BottomTool::Terminal)
                .into_any_element(),
            "Terminal",
            snap.bottom_open && snap.bottom_tool == BottomTool::Terminal,
            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                this.toggle_bottom_tool(BottomTool::Terminal, cx)
            }),
        ))
        .child(rail_btn(
            "rail-run",
            run_icon(
                snap.bottom_open && snap.bottom_tool == BottomTool::Run,
                snap.run_lamp,
            )
            .into_any_element(),
            "Run",
            snap.bottom_open && snap.bottom_tool == BottomTool::Run,
            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                this.toggle_bottom_tool(BottomTool::Run, cx)
            }),
        ))
        .child(rail_btn(
            "rail-problems",
            problems_icon(
                snap.bottom_open && snap.bottom_tool == BottomTool::Problems,
                snap.problems_lamp,
            )
            .into_any_element(),
            "Problems",
            snap.bottom_open && snap.bottom_tool == BottomTool::Problems,
            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                this.toggle_bottom_tool(BottomTool::Problems, cx)
            }),
        ))
        .child(rail_btn(
            "rail-git",
            branch_icon(snap.bottom_open && snap.bottom_tool == BottomTool::Git).into_any_element(),
            "Git",
            snap.bottom_open && snap.bottom_tool == BottomTool::Git,
            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                this.toggle_bottom_tool(BottomTool::Git, cx)
            }),
        ))
        .child(rail_btn(
            "rail-services",
            gear_icon(snap.bottom_open && snap.bottom_tool == BottomTool::Services)
                .into_any_element(),
            "Services",
            snap.bottom_open && snap.bottom_tool == BottomTool::Services,
            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                this.toggle_bottom_tool(BottomTool::Services, cx)
            }),
        ))
        .child(reserved_btn(
            "rail-debug",
            debug_icon().into_any_element(),
            "Debug — planned (DAP)",
        ))
}

/// Render the **right stripe** — the mirrored edge rail for right-dock tools
/// (DB observer today; JetBrains keeps databases/notifications on this side).
pub fn right_stripe(db_open: bool, cx: &mut Context<Workspace>) -> impl IntoElement {
    div()
        .id("right-stripe")
        .relative()
        .flex()
        .flex_col()
        .items_center()
        .flex_none()
        .w(px(RAIL_W))
        .h_full()
        .py_2()
        .gap_1()
        .bg(theme::surface_void())
        .child(edge_hairline(EdgeSide::Left))
        .child(rail_btn(
            "stripe-db",
            db_icon(db_open).into_any_element(),
            "Databases",
            db_open,
            cx.listener(|this, _ev: &ClickEvent, window, cx| this.toggle_db_observer(window, cx)),
        ))
}

/// A tool glyph's color: lit (the tool is open/active) = accent, idle = muted.
fn tool_color(lit: bool) -> gpui::Hsla {
    if lit {
        theme::accent()
    } else {
        theme::text_muted()
    }
}

/// **Project files** — a folder: a tab + a faintly-filled body. A recognizable *tool*,
/// not a panel-position hint.
fn folder_icon(lit: bool) -> impl IntoElement {
    let c = tool_color(lit);
    div()
        .relative()
        .w(px(16.))
        .h(px(13.))
        // The folder tab.
        .child(
            div()
                .absolute()
                .top(px(1.))
                .left_0()
                .w(px(7.))
                .h(px(3.))
                .rounded_t(px(2.))
                .bg(c),
        )
        // The folder body.
        .child(
            div()
                .absolute()
                .top(px(3.))
                .left_0()
                .right_0()
                .bottom(px(1.))
                .rounded(px(2.))
                .bg(theme::tint(c, 0.20))
                .border_1()
                .border_color(c),
        )
}

/// **Terminal** — a window with a `❯` prompt. The runtime/shell tool.
fn terminal_icon(lit: bool) -> impl IntoElement {
    let c = tool_color(lit);
    div()
        .relative()
        .w(px(16.))
        .h(px(13.))
        .rounded(px(3.))
        .border_1()
        .border_color(c)
        .flex()
        .items_center()
        .pl(px(3.))
        .child(div().text_size(px(8.)).text_color(c).child("❯"))
}

/// **Run** — a window with a play triangle: the JetBrains Run tool window. Carries
/// the run-status lamp on its corner (live blue / verdict green-red) so a finished
/// or failing run is glanceable even with the dock closed.
fn run_icon(lit: bool, lamp: Option<gpui::Hsla>) -> impl IntoElement {
    let c = tool_color(lit);
    div()
        .relative()
        .w(px(16.))
        .h(px(13.))
        .rounded(px(3.))
        .border_1()
        .border_color(c)
        .flex()
        .items_center()
        .justify_center()
        .child(div().text_size(px(7.)).text_color(c).child("▶"))
        .when_some(lamp, |d, color| {
            d.child(
                div()
                    .absolute()
                    .top(px(-2.))
                    .right(px(-2.))
                    .w(px(5.))
                    .h(px(5.))
                    .rounded_full()
                    .bg(color)
                    .shadow(theme::glow(color)),
            )
        })
}

/// **Sessions overview** — a 2×2 grid (the fleet tiles), accent when frontmost.
fn grid_icon(lit: bool) -> impl IntoElement {
    let c = tool_color(lit);
    let cell = move || div().w(px(5.)).h(px(5.)).rounded(px(1.)).bg(c);
    let row = move || {
        div()
            .flex()
            .flex_row()
            .gap(px(2.))
            .child(cell())
            .child(cell())
    };
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(row())
        .child(row())
}

/// **Structure** — an indented outline: three rows, the lower two stepped in
/// (the symbol tree's silhouette). Echoes the panel's own "≣" empty-state glyph.
fn structure_icon(lit: bool) -> impl IntoElement {
    let c = tool_color(lit);
    let bar =
        move |indent: f32, w: f32| div().ml(px(indent)).w(px(w)).h(px(2.)).rounded_full().bg(c);
    div()
        .flex()
        .flex_col()
        .gap(px(2.5))
        .child(bar(0., 12.))
        .child(bar(4., 8.))
        .child(bar(4., 8.))
}

/// **Commit** — a VCS node: a circle on its vertical history line. Lit when the
/// Commit tool fronts the left dock.
fn commit_icon(lit: bool) -> impl IntoElement {
    let c = tool_color(lit);
    div()
        .flex()
        .flex_col()
        .items_center()
        .child(div().w(px(1.5)).h(px(3.)).bg(c))
        .child(
            div()
                .w(px(7.))
                .h(px(7.))
                .rounded_full()
                .border_1()
                .border_color(c),
        )
        .child(div().w(px(1.5)).h(px(3.)).bg(c))
}

/// **Debug** (reserved) — the run triangle in a circular bezel: "run, but under
/// the debugger". Distinct from the Run window's square chrome.
fn debug_icon() -> impl IntoElement {
    let c = theme::text_muted();
    div()
        .w(px(15.))
        .h(px(15.))
        .rounded_full()
        .border_1()
        .border_color(c)
        .flex()
        .items_center()
        .justify_center()
        .child(div().text_size(px(7.)).text_color(c).child("▶"))
}

/// **Database** — a cylinder: an elliptical lid over a banded body.
fn db_icon(lit: bool) -> impl IntoElement {
    let c = tool_color(lit);
    div()
        .relative()
        .w(px(14.))
        .h(px(14.))
        .rounded_b(px(5.))
        .rounded_t(px(2.))
        .border_1()
        .border_color(c)
        .bg(theme::tint(c, 0.15))
        // The lid seam + a mid band suggest the stacked-platters silhouette.
        .child(
            div()
                .absolute()
                .top(px(2.))
                .left_0()
                .right_0()
                .h(px(1.))
                .bg(c),
        )
        .child(
            div()
                .absolute()
                .top(px(7.))
                .left_0()
                .right_0()
                .h(px(1.))
                .bg(theme::tint(c, 0.6)),
        )
}

/// **Services** — the gear: the app's long-lived background services
/// (control server, MCP host). Lit when the Services window is frontmost.
fn gear_icon(lit: bool) -> impl IntoElement {
    div()
        .text_size(px(12.))
        .text_color(tool_color(lit))
        .child("⚙")
}

/// **Git** — the branch glyph, lit when the Git window is frontmost.
fn branch_icon(lit: bool) -> impl IntoElement {
    div()
        .text_size(px(11.))
        .text_color(tool_color(lit))
        .child("⎇")
}

/// **Problems** — the warning triangle, carrying the diagnostics lamp on its
/// corner (red = errors, amber = warnings) so trouble shows even dock-closed.
fn problems_icon(lit: bool, lamp: Option<gpui::Hsla>) -> impl IntoElement {
    div()
        .relative()
        .text_size(px(11.))
        .text_color(tool_color(lit))
        .child("⚠")
        .when_some(lamp, |d, color| {
            d.child(
                div()
                    .absolute()
                    .top(px(-2.))
                    .right(px(-4.))
                    .w(px(5.))
                    .h(px(5.))
                    .rounded_full()
                    .bg(color)
                    .shadow(theme::glow(color)),
            )
        })
}

/// One rail button: a centered icon in a 28px hit target. Lit = a faint accent wash
/// with a glowing phosphor tick hugging the left edge; idle wakes on hover. The icon
/// carries its own lit/idle color (see [`panel_icon`]/[`grid_icon`]).
fn rail_btn(
    id: &'static str,
    icon: gpui::AnyElement,
    label: &'static str,
    lit: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let mut btn = div()
        .id(id)
        .relative()
        .flex()
        .items_center()
        .justify_center()
        .w(px(28.))
        .h(px(28.))
        .rounded(theme::radius_sm())
        .cursor_pointer();
    btn = if lit {
        btn.bg(theme::tint(theme::accent(), 0.10)).child(
            // The phosphor tick — the "this surface is on" lamp.
            div()
                .absolute()
                .left(px(-5.))
                .w(px(2.))
                .h(px(14.))
                .rounded_full()
                .bg(theme::accent())
                .shadow(theme::glow(theme::accent())),
        )
    } else {
        btn.hover(|d| d.bg(theme::row_hover()))
    };
    btn.child(icon)
        .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
        .on_click(on_click)
}

/// A **reserved** stripe slot: the tool's icon at half presence, no handler — a
/// visible claim on where the tool window will land (tooltip says "planned").
fn reserved_btn(id: &'static str, icon: gpui::AnyElement, label: &'static str) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .w(px(28.))
        .h(px(28.))
        .rounded(theme::radius_sm())
        .opacity(0.4)
        .child(icon)
        .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
}

/// Which edge of the stripe carries the hairline (the side facing the dock).
enum EdgeSide {
    /// Left rail → hairline on its right edge.
    Right,
    /// Right stripe → hairline on its left edge.
    Left,
}

/// The 1px dock-facing hairline: transparent at top/bottom, moonlight at the
/// center — two mirrored vertical gradients (gpui gradients take two stops).
fn edge_hairline(side: EdgeSide) -> gpui::Div {
    let dark = theme::tint(theme::accent(), 0.0);
    let lit = theme::tint(theme::accent(), 0.35);
    let d = div().absolute().top_0().w(px(1.)).h_full();
    let d = match side {
        EdgeSide::Right => d.right_0(),
        EdgeSide::Left => d.left_0(),
    };
    d.flex()
        .flex_col()
        .child(div().flex_1().w_full().bg(linear_gradient(
            180.,
            linear_color_stop(dark, 0.),
            linear_color_stop(lit, 1.),
        )))
        .child(div().flex_1().w_full().bg(linear_gradient(
            180.,
            linear_color_stop(lit, 0.),
            linear_color_stop(dark, 1.),
        )))
}
