//! The **Commit tool window** — JetBrains' Commit view, the hybrid the operator
//! chose: this left-dock panel lists the repo's changed files with their
//! **index-faithful staged ⇄ unstaged split** and owns the commit action; the
//! rich per-hunk review stays a center tab, opened from here ("Review ▸").
//!
//! Coherence with the review flow is the point: the review tab's per-hunk
//! **Accept stages hunks** (`git apply --cached`), so accepted work surfaces in
//! this panel's *Staged* group and **Commit commits exactly the index** — the
//! operator's accepted set, never a blind `-a`.
//!
//! A dock `Panel` like the file tree (the stripe's ⎘ swaps the left dock between
//! Project and Commit). Status refreshes on a 2s poll + immediately after every
//! stage/unstage/commit.

use std::path::PathBuf;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{div, px, App, Context, Entity, EventEmitter, FocusHandle, Focusable, Window};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::input::{Input, InputState};
use gpui_component::Sizable;

use crate::git::commit::{commit, load_entries, stage, unstage, CommitEntry};
use crate::git::status::GitFileStatus;
use crate::views::center_requests::OpenRequest;
use crate::views::chrome_requests::ChromeRequest;
use crate::views::project_space::ProjectSpace;
use crate::views::theme;
use crate::views::workspace::ShellDeps;

pub struct CommitPanel {
    /// The active project (root provider); the panel follows space switches.
    focus: Option<Entity<ProjectSpace>>,
    entries: Vec<CommitEntry>,
    /// The root the current `entries` were loaded from (drop stale lists on switch).
    loaded_root: Option<PathBuf>,
    /// Commit message input, created lazily on first render (needs a `Window`).
    message: Option<Entity<InputState>>,
    /// Outcome of the last commit / stage op: `Ok(summary)` or `Err(stderr)`.
    last_result: Option<Result<String, String>>,
    focus_handle: FocusHandle,
}

impl CommitPanel {
    pub fn new(focus: Option<Entity<ProjectSpace>>, cx: &mut Context<Self>) -> Self {
        // 2s status poll (same cadence as the Services probe): cheap `git status`
        // against the active root; notify only when the list actually changed.
        cx.spawn(async move |this, cx| loop {
            let alive = this.update(cx, |panel: &mut Self, cx| panel.refresh(cx));
            if alive.is_err() {
                break; // panel dropped
            }
            cx.background_executor()
                .timer(Duration::from_secs(2))
                .await;
        })
        .detach();
        Self {
            focus,
            entries: Vec::new(),
            loaded_root: None,
            message: None,
            last_result: None,
            focus_handle: cx.focus_handle(),
        }
    }

    fn root(&self, cx: &App) -> Option<PathBuf> {
        Some(self.focus.as_ref()?.read(cx).root())
    }

    /// Reload the entries for the active root; notify on change (or root switch).
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let root = self.root(cx);
        let next = root.as_ref().and_then(|r| load_entries(r)).unwrap_or_default();
        if next != self.entries || root != self.loaded_root {
            self.entries = next;
            self.loaded_root = root;
            cx.notify();
        }
    }

    /// Run a stage/unstage op, surface its error, record it on the shared git
    /// console, and refresh immediately.
    fn index_op(
        &mut self,
        verb: &str,
        op: impl FnOnce(&std::path::Path, &std::path::Path) -> Result<(), String>,
        rel: &std::path::Path,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.root(cx) else { return };
        let result = op(&root, rel).map(|()| String::new());
        record_op(format!("{verb} {}", rel.display()), &result, cx);
        if let Err(err) = result {
            self.last_result = Some(Err(err));
        }
        self.refresh(cx);
        cx.notify();
    }

    /// Commit the index with the typed message; clear it on success.
    fn do_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root(cx) else { return };
        let Some(input) = self.message.clone() else { return };
        let msg = input.read(cx).value().trim().to_string();
        if msg.is_empty() || self.staged_count() == 0 {
            return;
        }
        let result = commit(&root, &msg);
        record_op("commit", &result, cx);
        self.last_result = Some(result);
        if matches!(self.last_result, Some(Ok(_))) {
            input.update(cx, |state, cx| state.set_value("", window, cx));
        }
        self.refresh(cx);
        cx.notify();
    }

    fn staged_count(&self) -> usize {
        self.entries.iter().filter(|e| e.staged.is_some()).count()
    }

    /// The frontmost session id, if a session tab is focused — the "Review ▸"
    /// action opens the per-hunk review for it (the hybrid's center half).
    fn focused_session(cx: &App) -> Option<moonlight_domain::ids::SessionId> {
        match cx.try_global::<ShellDeps>()?.active_context.read(cx) {
            crate::views::active_context::ActiveContext::Session { id, .. } => Some(id.clone()),
            _ => None,
        }
    }
}

/// Record an op on the shared git console (no-op in static/test views).
fn record_op(label: impl Into<String>, result: &Result<String, String>, cx: &App) {
    if let Some(deps) = cx.try_global::<ShellDeps>() {
        deps.git_console.record(label, result);
    }
}

fn git_color(status: GitFileStatus) -> gpui::Hsla {
    match status {
        GitFileStatus::Modified => theme::git_modified(),
        GitFileStatus::Added => theme::git_added(),
        GitFileStatus::Untracked => theme::git_untracked(),
        GitFileStatus::Deleted => theme::git_deleted(),
        GitFileStatus::Renamed => theme::git_modified(),
        GitFileStatus::Conflicted => theme::git_conflict(),
    }
}

impl Focusable for CommitPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for CommitPanel {}

impl Panel for CommitPanel {
    fn panel_name(&self) -> &'static str {
        "Commit"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        gpui::SharedString::from("Commit")
    }

    fn title_suffix(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        Some(super::tool_hide_button(
            "commit-hide",
            ChromeRequest::HideLeftDock,
        ))
    }
}

impl Render for CommitPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // First render: stand the message input up (needs the Window).
        if self.message.is_none() {
            self.message =
                Some(cx.new(|cx| InputState::new(window, cx).placeholder("Commit message")));
            self.refresh(cx); // don't show an empty panel until the first poll tick
        }
        let staged: Vec<_> = self
            .entries
            .iter()
            .filter_map(|e| e.staged.map(|s| (e.rel.clone(), s)))
            .collect();
        let changes: Vec<_> = self
            .entries
            .iter()
            .filter_map(|e| e.unstaged.map(|s| (e.rel.clone(), s)))
            .collect();
        let can_commit = !staged.is_empty();
        let review_session = Self::focused_session(cx);

        div()
            .id("commit-panel")
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::surface_base())
            .text_size(theme::text_xs())
            // ── Changes groups ──────────────────────────────────────────────
            .child(
                div()
                    .id("commit-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .py_1()
                    .flex()
                    .flex_col()
                    .child(group_header(format!("Staged ({})", staged.len())))
                    .children(staged.iter().enumerate().map(|(i, (rel, s))| {
                        file_row(("staged", i), rel, *s, RowAction::Unstage, cx)
                    }))
                    .when(staged.is_empty(), |d| d.child(empty_hint("nothing staged — stage files below, or Accept hunks in a review tab")))
                    .child(group_header(format!("Changes ({})", changes.len())))
                    .children(changes.iter().enumerate().map(|(i, (rel, s))| {
                        file_row(("change", i), rel, *s, RowAction::Stage, cx)
                    }))
                    .when(changes.is_empty() && !self.entries.is_empty(), |d| {
                        d.child(empty_hint("working tree clean — everything is staged"))
                    })
                    .when(self.entries.is_empty(), |d| {
                        d.child(empty_hint("no changes in the working tree"))
                    }),
            )
            // ── Review handoff (the hybrid's center half) ───────────────────
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .border_t_1()
                    .border_color(theme::border_subtle())
                    .child(match review_session {
                        Some(session) => div()
                            .id("commit-open-review")
                            .px_2()
                            .py(px(2.))
                            .rounded(theme::radius_sm())
                            .cursor_pointer()
                            .text_color(theme::accent())
                            .hover(|d| d.bg(theme::row_hover()))
                            .child("Review changes ▸")
                            .on_click(cx.listener(move |this, _ev, _w, cx| {
                                let Some(root) = this.root(cx) else { return };
                                let session = session.clone();
                                let center = cx.global::<ShellDeps>().center.clone();
                                center.update(cx, |_, cx| {
                                    cx.emit(OpenRequest::CodeReview {
                                        session,
                                        root: Some(root),
                                        summary: None,
                                    })
                                });
                            }))
                            .into_any_element(),
                        None => div()
                            .text_color(theme::text_muted())
                            .child("focus a session tab to open its per-hunk review")
                            .into_any_element(),
                    }),
            )
            // ── Message + commit ────────────────────────────────────────────
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .border_t_1()
                    .border_color(theme::border_subtle())
                    .children(self.message.as_ref().map(|input| Input::new(input).small()))
                    .child({
                        let mut btn = div()
                            .id("commit-button")
                            .flex()
                            .items_center()
                            .justify_center()
                            .py(px(4.))
                            .rounded(theme::radius_sm())
                            .child(format!("Commit ({} staged)", staged.len()));
                        if can_commit {
                            btn = btn
                                .cursor_pointer()
                                .bg(theme::tint(theme::accent(), 0.18))
                                .text_color(theme::accent())
                                .hover(|d| d.bg(theme::tint(theme::accent(), 0.30)))
                                .on_click(cx.listener(|this, _ev, window, cx| {
                                    this.do_commit(window, cx)
                                }));
                        } else {
                            btn = btn
                                .text_color(theme::text_muted())
                                .bg(theme::surface_sunken());
                        }
                        btn
                    })
                    .children(self.last_result.as_ref().map(|res| match res {
                        Ok(summary) => div()
                            .text_size(theme::text_2xs())
                            .text_color(theme::git_added())
                            .child(summary.clone()),
                        Err(err) => div()
                            .text_size(theme::text_2xs())
                            .text_color(theme::git_deleted())
                            .child(err.clone()),
                    })),
            )
    }
}

/// What the row's trailing action does (the +/− affordance).
#[derive(Clone, Copy)]
enum RowAction {
    Stage,
    Unstage,
}

fn group_header(label: String) -> impl IntoElement {
    div()
        .px_2()
        .py(px(3.))
        .bg(theme::surface_sunken())
        .text_color(theme::text_secondary())
        .child(label)
}

fn empty_hint(text: &'static str) -> impl IntoElement {
    div()
        .px_2()
        .py(px(3.))
        .text_size(theme::text_2xs())
        .text_color(theme::tree_glyph())
        .child(text)
}

/// One changed file: status letter (VCS-colored) + path; click opens the file,
/// the trailing +/− stages/unstages it.
fn file_row(
    id: (&'static str, usize),
    rel: &std::path::Path,
    status: GitFileStatus,
    action: RowAction,
    cx: &mut Context<CommitPanel>,
) -> impl IntoElement {
    let color = git_color(status);
    let rel_owned = rel.to_path_buf();
    let open_rel = rel_owned.clone();
    let (glyph, tip): (&'static str, &'static str) = match action {
        RowAction::Stage => ("＋", "Stage"),
        RowAction::Unstage => ("－", "Unstage"),
    };
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_2()
        .py(px(2.))
        .cursor_pointer()
        .hover(|d| d.bg(theme::row_hover()))
        // Click the row → open the file in an editor tab (diff view = review tab).
        .on_click(cx.listener(move |this, _ev, _w, cx| {
            let Some(root) = this.root(cx) else { return };
            let abs = root.join(&open_rel);
            let center = cx.global::<ShellDeps>().center.clone();
            center.update(cx, |_, cx| cx.emit(OpenRequest::File(abs)));
        }))
        .child(
            div()
                .flex_none()
                .w(px(12.))
                .text_color(color)
                .child(status.letter()),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .overflow_hidden()
                .text_color(theme::text_primary())
                .child(rel.to_string_lossy().into_owned()),
        )
        .child(
            div()
                .id((tip, id.1))
                .flex_none()
                .px_1()
                .text_color(theme::text_muted())
                .hover(|d| d.text_color(theme::text_primary()))
                .child(glyph)
                .on_click(cx.listener(move |this, _ev, _w, cx| {
                    cx.stop_propagation();
                    match action {
                        RowAction::Stage => this.index_op("stage", stage, &rel_owned, cx),
                        RowAction::Unstage => this.index_op("unstage", unstage, &rel_owned, cx),
                    }
                })),
        )
}
