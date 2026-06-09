//! The **Git tool window** — bottom-dock tool with two views behind chips,
//! JetBrains-style:
//!
//! - **Log**: the repo's recent history (`git log`, 50 commits) — hash · subject
//!   · refs badge · author · relative time. Follows the active space's root.
//! - **Console**: every git operation the IDE itself ran (toolbar checkout,
//!   Commit-tool stage/unstage/commit) with its outcome — errors stop being
//!   toast-only. Fed by the shared [`GitConsole`] sink in `ShellDeps`.
//!
//! Workspace-owned like the other bottom tools: 3s log poll (rebuild only on
//! change) + the console's dirty-seq poll on the same tick.

use std::path::PathBuf;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{div, px, Context, Entity, Window};

use crate::git::log::{load_log, LogEntry};
use crate::views::chrome_requests::ChromeRequest;
use crate::views::git_console::GitOp;
use crate::views::project_space::ProjectSpace;
use crate::views::theme;
use crate::views::workspace::ShellDeps;

/// Which view the window fronts.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GitView {
    Log,
    Console,
}

pub struct GitPanel {
    focus: Option<Entity<ProjectSpace>>,
    view: GitView,
    log: Vec<LogEntry>,
    loaded_root: Option<PathBuf>,
    ops: Vec<GitOp>,
    seen_seq: Option<u64>,
}

impl GitPanel {
    pub fn new(focus: Option<Entity<ProjectSpace>>, cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this, cx| loop {
            let alive = this.update(cx, |panel: &mut Self, cx| panel.refresh(cx));
            if alive.is_err() {
                break; // panel dropped
            }
            cx.background_executor()
                .timer(Duration::from_secs(3))
                .await;
        })
        .detach();
        Self {
            focus,
            view: GitView::Log,
            log: Vec::new(),
            loaded_root: None,
            ops: Vec::new(),
            seen_seq: None,
        }
    }

    /// Reload the log (root-scoped) + console snapshot; notify only on change.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let mut dirty = false;

        let root = self.focus.as_ref().map(|f| f.read(cx).root());
        let next = root.as_ref().and_then(|r| load_log(r, 50)).unwrap_or_default();
        if next != self.log || root != self.loaded_root {
            self.log = next;
            self.loaded_root = root;
            dirty = true;
        }

        if let Some(deps) = cx.try_global::<ShellDeps>() {
            let seq = deps.git_console.seq();
            if self.seen_seq != Some(seq) {
                self.seen_seq = Some(seq);
                self.ops = deps.git_console.ops();
                dirty = true;
            }
        }

        if dirty {
            cx.notify();
        }
    }
}

impl Render for GitPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = self.view;
        div()
            .id("git-panel")
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::surface_base())
            .text_size(theme::text_xs())
            // Bar: view chips + the uniform hide ✕.
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(28.))
                    .px_1()
                    .gap_1()
                    .bg(theme::surface_sunken())
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .child(view_chip("git-chip-log", "Log", view == GitView::Log, cx))
                    .child(view_chip(
                        "git-chip-console",
                        "Console",
                        view == GitView::Console,
                        cx,
                    ))
                    .child(div().flex_1())
                    .child(super::tool_hide_button(
                        "git-hide",
                        ChromeRequest::HideBottomDock,
                    )),
            )
            .child(match view {
                GitView::Log => log_body(&self.log).into_any_element(),
                GitView::Console => console_body(&self.ops).into_any_element(),
            })
    }
}

/// A view chip in the bar (the Run-onglet visual language, minus the lamp).
fn view_chip(
    id: &'static str,
    label: &'static str,
    active: bool,
    cx: &mut Context<GitPanel>,
) -> impl IntoElement {
    let target = match label {
        "Log" => GitView::Log,
        _ => GitView::Console,
    };
    let mut chip = div()
        .id(id)
        .px_2()
        .py(px(2.))
        .rounded(theme::radius_sm())
        .cursor_pointer();
    chip = if active {
        chip.bg(theme::surface_raised()).text_color(theme::accent())
    } else {
        chip.text_color(theme::text_muted())
            .hover(|d| d.bg(theme::row_hover()).text_color(theme::text_secondary()))
    };
    chip.child(label).on_click(cx.listener(move |this, _ev, _w, cx| {
        this.view = target;
        cx.notify();
    }))
}

/// The Log view: one row per commit.
fn log_body(log: &[LogEntry]) -> impl IntoElement {
    let body = div()
        .id("git-log-scroll")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .py_1()
        .flex()
        .flex_col();
    if log.is_empty() {
        return body.child(
            div()
                .p_4()
                .text_color(theme::text_muted())
                .child("no history — not a git repo, or no commits yet"),
        );
    }
    body.children(log.iter().map(|e| {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py(px(2.))
            .hover(|d| d.bg(theme::row_hover()))
            .child(
                div()
                    .flex_none()
                    .text_color(theme::accent())
                    .child(e.hash.clone()),
            )
            .when(!e.refs.is_empty(), |d| {
                d.child(
                    div()
                        .flex_none()
                        .px_1()
                        .rounded(theme::radius_sm())
                        .bg(theme::tint(theme::accent(), 0.12))
                        .text_size(theme::text_2xs())
                        .text_color(theme::text_secondary())
                        .child(e.refs.clone()),
                )
            })
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .text_color(theme::text_primary())
                    .child(e.subject.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(theme::text_muted())
                    .child(e.author.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(theme::text_2xs())
                    .text_color(theme::tree_glyph())
                    .child(e.when.clone()),
            )
    }))
}

/// The Console view: the IDE's recorded git operations, newest last.
fn console_body(ops: &[GitOp]) -> impl IntoElement {
    let body = div()
        .id("git-console-scroll")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .py_1()
        .flex()
        .flex_col();
    if ops.is_empty() {
        return body.child(div().p_4().text_color(theme::text_muted()).child(
            "no operations yet — toolbar checkouts and Commit-tool actions land here",
        ));
    }
    body.children(ops.iter().map(|op| {
        let (glyph, color) = if op.ok {
            ("✓", theme::git_added())
        } else {
            ("✘", theme::git_deleted())
        };
        div()
            .flex()
            .flex_col()
            .px_2()
            .py(px(2.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(div().flex_none().text_color(color).child(glyph))
                    .child(
                        div()
                            .text_color(theme::text_primary())
                            .child(format!("git {}", op.label)),
                    ),
            )
            .when(!op.detail.is_empty(), |d| {
                d.child(
                    div()
                        .pl_4()
                        .text_size(theme::text_2xs())
                        .text_color(if op.ok {
                            theme::text_muted()
                        } else {
                            theme::git_deleted()
                        })
                        .child(op.detail.clone()),
                )
            })
    }))
}
