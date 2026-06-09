//! The **Problems** tool window — JetBrains' Problems view: language-server
//! diagnostics grouped by file, with severity glyphs and click-to-open.
//!
//! Sourced from the shared [`LspPool`](crate::lsp::LspPool): every server the
//! Structure outline starts also publishes `textDocument/publishDiagnostics`,
//! which the client's reader thread stores live (no request needed). Scope note:
//! servers only diagnose documents the IDE has `didOpen`ed — i.e. **files opened
//! in a code editor tab** — so this is "problems in open files", not a whole-
//! workspace sweep (that needs watched-files support, a later slice).
//!
//! A workspace-owned bottom tool like the terminal / Run / Services: 1.5s poll on
//! the pool's dirty counter; the snapshot (and a re-render) only rebuilds when a
//! server actually published.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{div, px, Context, Window};
use moonlight_domain::session::SessionStatus;

use crate::lsp::{Diagnostic, LspPool};
use crate::views::center_requests::OpenRequest;
use crate::views::chrome_requests::ChromeRequest;
use crate::views::theme;
use crate::views::workspace::ShellDeps;

/// Problem totals across all files — the stripe button's lamp + the bar summary.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub errors: usize,
    pub warnings: usize,
    pub others: usize,
}

impl Counts {
    fn tally(files: &[(PathBuf, Vec<Diagnostic>)]) -> Self {
        let mut c = Self::default();
        for d in files.iter().flat_map(|(_, rows)| rows) {
            match d.severity {
                1 => c.errors += 1,
                2 => c.warnings += 1,
                _ => c.others += 1,
            }
        }
        c
    }

    /// The lamp color for the stripe button: red while anything errors, amber on
    /// warnings only, `None` when clean (no lamp — silence is the good state).
    pub fn lamp(&self) -> Option<gpui::Hsla> {
        if self.errors > 0 {
            Some(theme::status_color(SessionStatus::Errored))
        } else if self.warnings > 0 {
            Some(theme::status_color(SessionStatus::WaitingInput))
        } else {
            None
        }
    }
}

pub struct ProblemsPanel {
    pool: Arc<LspPool>,
    /// The pool's dirty counter at the last rebuild (skip unchanged polls).
    seen_seq: Option<u64>,
    files: Vec<(PathBuf, Vec<Diagnostic>)>,
    counts: Counts,
}

impl ProblemsPanel {
    pub fn new(pool: Arc<LspPool>, cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this, cx| loop {
            let alive = this.update(cx, |panel: &mut Self, cx| {
                let seq = panel.pool.diagnostics_seq();
                if panel.seen_seq != Some(seq) {
                    panel.seen_seq = Some(seq);
                    panel.files = panel.pool.diagnostics();
                    panel.counts = Counts::tally(&panel.files);
                    cx.notify();
                }
            });
            if alive.is_err() {
                break; // panel dropped
            }
            cx.background_executor()
                .timer(Duration::from_millis(1500))
                .await;
        })
        .detach();
        Self {
            pool,
            seen_seq: None,
            files: Vec::new(),
            counts: Counts::default(),
        }
    }

    /// Current totals (the workspace reads these for the stripe lamp).
    pub fn counts(&self) -> Counts {
        self.counts
    }
}

/// Severity → (glyph, color): ✘ error red, ⚠ warning amber, ℹ info blue, ➤ hint.
fn severity_style(severity: u8) -> (&'static str, gpui::Hsla) {
    match severity {
        1 => ("✘", theme::status_color(SessionStatus::Errored)),
        2 => ("⚠", theme::status_color(SessionStatus::WaitingInput)),
        3 => ("ℹ", theme::status_color(SessionStatus::Running)),
        _ => ("➤", theme::text_muted()),
    }
}

/// `path` for display: relative to `root` when under it (JetBrains shows
/// project-relative paths), else the absolute path.
fn display_path(path: &std::path::Path, root: &std::path::Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

impl Render for ProblemsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = cx
            .try_global::<ShellDeps>()
            .map(|d| d.focus.read(cx).root())
            .unwrap_or_default();
        let c = self.counts;
        let summary = if self.files.is_empty() {
            "no problems".to_string()
        } else {
            format!("{} error{} · {} warning{}", c.errors,
                if c.errors == 1 { "" } else { "s" },
                c.warnings,
                if c.warnings == 1 { "" } else { "s" })
        };

        div()
            .id("problems-panel")
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::surface_base())
            // The tool window's bar: title + live summary + the uniform hide ✕.
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(28.))
                    .px_2()
                    .gap_2()
                    .bg(theme::surface_sunken())
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .text_size(theme::text_xs())
                    .text_color(theme::text_muted())
                    .child("Problems")
                    .child(div().text_color(theme::tree_glyph()).child(summary))
                    .child(div().flex_1())
                    .child(super::tool_hide_button(
                        "problems-hide",
                        ChromeRequest::HideBottomDock,
                    )),
            )
            .child(if self.files.is_empty() {
                empty_state().into_any_element()
            } else {
                div()
                    .id("problems-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .py_1()
                    .flex()
                    .flex_col()
                    .children(self.files.iter().enumerate().flat_map(|(fi, (path, rows))| {
                        let mut out: Vec<gpui::AnyElement> =
                            vec![file_header(&display_path(path, &root), rows).into_any_element()];
                        out.extend(rows.iter().enumerate().map(|(ri, d)| {
                            problem_row(fi * 10_000 + ri, path.clone(), d, cx).into_any_element()
                        }));
                        out
                    }))
                    .into_any_element()
            })
    }
}

/// The calm empty state — explains the "open files" scope instead of looking broken.
fn empty_state() -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_1()
        .size_full()
        .p_4()
        .text_color(theme::text_muted())
        .child(div().text_size(px(22.)).child("✓"))
        .child(div().text_size(px(11.)).child("No problems"))
        .child(
            div()
                .text_size(px(10.))
                .text_color(theme::tree_glyph())
                .child("language servers report diagnostics for files opened in an editor tab"),
        )
}

/// One file's group header: path + its per-severity counts.
fn file_header(rel: &str, rows: &[Diagnostic]) -> impl IntoElement {
    let errors = rows.iter().filter(|d| d.severity == 1).count();
    let warnings = rows.iter().filter(|d| d.severity == 2).count();
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_2()
        .py(px(3.))
        .bg(theme::surface_sunken())
        .text_size(theme::text_xs())
        .child(div().text_color(theme::text_secondary()).child(rel.to_string()))
        .when(errors > 0, |d| {
            d.child(
                div()
                    .text_color(theme::status_color(SessionStatus::Errored))
                    .child(format!("✘ {errors}")),
            )
        })
        .when(warnings > 0, |d| {
            d.child(
                div()
                    .text_color(theme::status_color(SessionStatus::WaitingInput))
                    .child(format!("⚠ {warnings}")),
            )
        })
}

/// One diagnostic: severity glyph · line:col · message (· source). Click opens
/// the file in a code-editor tab via the center channel.
fn problem_row(
    id: usize,
    path: PathBuf,
    d: &Diagnostic,
    cx: &mut Context<ProblemsPanel>,
) -> impl IntoElement {
    let (glyph, color) = severity_style(d.severity);
    let source = d.source.clone();
    div()
        .id(("problem-row", id))
        .flex()
        .flex_row()
        .items_start()
        .gap_2()
        .pl_4()
        .pr_2()
        .py(px(2.))
        .cursor_pointer()
        .hover(|s| s.bg(theme::row_hover()))
        .text_size(theme::text_xs())
        .on_click(cx.listener(move |_this, _ev, _w, cx| {
            let center = cx.global::<ShellDeps>().center.clone();
            let path = path.clone();
            center.update(cx, |_, cx| cx.emit(OpenRequest::File(path)));
        }))
        .child(div().flex_none().w(px(12.)).text_color(color).child(glyph))
        .child(
            div()
                .flex_none()
                .text_color(theme::tree_glyph())
                .child(format!("{}:{}", d.line, d.col)),
        )
        .child(
            div()
                .min_w_0()
                .text_color(theme::text_primary())
                .child(d.message.clone()),
        )
        .when_some(source, |el, src| {
            el.child(
                div()
                    .flex_none()
                    .text_color(theme::text_muted())
                    .child(format!("[{src}]")),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(severity: u8) -> Diagnostic {
        Diagnostic {
            severity,
            line: 1,
            col: 1,
            message: "m".into(),
            source: None,
        }
    }

    #[test]
    fn counts_tally_and_lamp_prioritize_errors() {
        let files = vec![
            (PathBuf::from("/a.rs"), vec![diag(1), diag(2), diag(3)]),
            (PathBuf::from("/b.rs"), vec![diag(2)]),
        ];
        let c = Counts::tally(&files);
        assert_eq!((c.errors, c.warnings, c.others), (1, 2, 1));
        assert_eq!(c.lamp(), Some(theme::status_color(SessionStatus::Errored)));

        let warn_only = Counts { errors: 0, warnings: 1, others: 0 };
        assert_eq!(
            warn_only.lamp(),
            Some(theme::status_color(SessionStatus::WaitingInput))
        );
        assert_eq!(Counts::default().lamp(), None);
    }

    #[test]
    fn display_path_is_project_relative_when_under_root() {
        let root = std::path::Path::new("/w/proj");
        assert_eq!(
            display_path(std::path::Path::new("/w/proj/src/main.rs"), root),
            "src/main.rs"
        );
        assert_eq!(
            display_path(std::path::Path::new("/elsewhere/x.rs"), root),
            "/elsewhere/x.rs"
        );
    }
}
