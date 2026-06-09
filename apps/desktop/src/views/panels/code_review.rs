//! Code-review gate panel — the end-of-work review surface (G4 + G5) and the home
//! of the **per-hunk rejection-as-feedback** primitive (FR18/FR23).
//!
//! Opened as a center tab when `EngineEvent::ReviewReady` fires (a session declared
//! it is done). Shows the **per-file diff** of everything the session changed
//! against `HEAD` (G4, via the `git` module), split into individual **hunks** the
//! operator can **Accept** or **Reject**. Rejecting a hunk opens an inline reason
//! field; sending it writes structured corrective feedback into the session's
//! embedded terminal (the signature steering loop — the engine can't reach the
//! embedded PTY, so the cockpit delivers it and the engine records the audit via
//! `Command::RejectHunk`). A **Run full-review** action shells the local
//! `full-review` over the repo (G5/T16, manual), and **Approve** advances the
//! workflow Review → Commit.
//!
//! Delivery is "Option A": feedback reaches sessions that have a live embedded
//! terminal (the managed/resumed case). A terminal-less session shows the feedback
//! was captured but flags that it can't be pushed yet (a future `Steer` adapter /
//! hooks channel lifts that limit).

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command as ProcCommand;

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    div, px, App, Context, Entity, EventEmitter, FocusHandle, Focusable, MouseDownEvent, WeakEntity,
    Window,
};
use gpui_component::dock::{Panel, PanelEvent, PanelInfo, PanelState, TabPanel};
use gpui_component::input::{Input, InputState};

use super::CloseTab;

use moonlight_domain::ids::SessionId;
use moonlight_domain::review::{Feedback, FeedbackOrigin, ReviewHunk};
use moonlight_engine::Command;
use tokio::sync::mpsc::UnboundedSender;

use crate::git::diff::{changed_files, file_diff, parse_hunks, restage_file, split_file_diff, ChangedFile};
use crate::git::status::GitFileStatus;
use crate::views::theme;

/// Feedback used when the operator rejects a hunk without typing a reason.
const DEFAULT_HUNK_REASON: &str = "Reconsider this change — it was rejected in review.";

/// The operator's verdict on a single hunk (visual state for the review pass).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HunkVerdict {
    Accepted,
    Rejected,
}

pub struct CodeReviewPanel {
    session: SessionId,
    /// Repo root of the session under review (its `attached_path`), if known.
    root: Option<PathBuf>,
    /// The agent's "what this covers" summary (latest assistant prose), if observed
    /// before review opened (T4). Shown as a band above the diff.
    summary: Option<String>,
    /// Sends the operator's verdict to the engine; `None` for static/test views.
    commands: Option<UnboundedSender<Command>>,
    /// Why the diff couldn't be loaded (not a repo, git missing) or a delivery note.
    error: Option<String>,
    files: Vec<ChangedFile>,
    selected: Option<usize>,
    /// Unified diff of the selected file (raw; colorized at render time). Kept for
    /// the no-textual-hunks fallback.
    diff: String,
    /// The selected file's diff split into reviewable hunks.
    hunks: Vec<ReviewHunk>,
    /// The selected file's diff preamble (`diff --git`/`---`/`+++`), kept so accepted
    /// hunks can be reassembled into a valid patch and staged ([`restage_file`]).
    diff_header: String,
    /// Per-hunk verdicts for the current file (index → verdict), reset on file switch.
    decisions: HashMap<usize, HunkVerdict>,
    /// When the operator is rejecting hunk `idx`, the reason field they fill in.
    reject_input: Option<(usize, Entity<InputState>)>,
    /// Output of the last full-review run, if any.
    review_output: Option<String>,
    review_running: bool,
    focus_handle: FocusHandle,
    /// The tab panel this review lives in (captured in [`Panel::on_added_to`]), so
    /// the tab bar's "×" can close this tab — see [`super::tab_title`].
    tab_panel: Option<WeakEntity<TabPanel>>,
    /// Open right-click tab menu, rendered from the panel body (see [`super::TabMenuHost`]).
    tab_menu: Option<super::TabMenu>,
}

impl CodeReviewPanel {
    pub fn new(
        session: SessionId,
        root: Option<PathBuf>,
        summary: Option<String>,
        commands: Option<UnboundedSender<Command>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut panel = Self {
            session,
            root,
            summary,
            commands,
            error: None,
            files: Vec::new(),
            selected: None,
            diff: String::new(),
            hunks: Vec::new(),
            diff_header: String::new(),
            decisions: HashMap::new(),
            reject_input: None,
            review_output: None,
            review_running: false,
            focus_handle: cx.focus_handle(),
            tab_panel: None,
            tab_menu: None,
        };
        panel.reload(cx);
        panel
    }

    /// (Re)load the changed-file list for the session's repo and select the first.
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.diff.clear();
        self.hunks.clear();
        self.diff_header.clear();
        self.decisions.clear();
        self.reject_input = None;
        let Some(root) = self.root.clone() else {
            self.error = Some("no repo path for this session".to_string());
            return;
        };
        match changed_files(&root) {
            Some(files) => {
                self.error = None;
                self.files = files;
                if !self.files.is_empty() {
                    self.select(0, cx);
                }
            }
            None => {
                self.error = Some("not a git repository (or git unavailable)".to_string());
                self.files.clear();
            }
        }
    }

    /// Load and show the diff of file `idx`, split into hunks. Resets per-file
    /// verdict state (hunk indices are file-local).
    fn select(&mut self, idx: usize, cx: &mut Context<Self>) {
        let (Some(root), Some(file)) = (self.root.clone(), self.files.get(idx).cloned()) else {
            return;
        };
        self.selected = Some(idx);
        self.diff = file_diff(&root, &file).unwrap_or_else(|| "(diff unavailable)".to_string());
        let (header, _) = split_file_diff(&self.diff);
        self.diff_header = header;
        self.hunks = parse_hunks(&self.diff, &self.session, &file.display());
        self.decisions.clear();
        self.reject_input = None;
        self.error = None;
        cx.notify();
    }

    /// Stage exactly the accepted hunks of the selected file into the git index
    /// (idempotent; recomputed on every accept/reject). Surfaces a git error in the
    /// panel rather than failing silently.
    fn restage_selected(&mut self) {
        let (Some(root), Some(file)) = (
            self.root.clone(),
            self.selected.and_then(|i| self.files.get(i).cloned()),
        ) else {
            return;
        };
        let accepted: Vec<String> = self
            .hunks
            .iter()
            .enumerate()
            .filter(|(i, _)| self.decisions.get(i) == Some(&HunkVerdict::Accepted))
            .map(|(_, h)| h.diff.clone())
            .collect();
        let untracked = file.status == GitFileStatus::Untracked;
        match restage_file(&root, &file.display(), &self.diff_header, &accepted, untracked) {
            Ok(()) => self.error = None,
            Err(e) => self.error = Some(format!("staging failed: {e}")),
        }
    }

    /// Mark a hunk accepted (visual only in v1 — selective staging is a follow-on;
    /// the steering loop is reject→feedback, which is what changes agent behavior).
    fn accept_hunk(&mut self, idx: usize, cx: &mut Context<Self>) {
        self.decisions.insert(idx, HunkVerdict::Accepted);
        self.restage_selected();
        cx.notify();
    }

    /// Open the inline reason field for rejecting hunk `idx`.
    fn begin_hunk_reject(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("What's wrong with this hunk? (sent to the agent)")
        });
        input.focus_handle(cx).focus(window, cx);
        self.reject_input = Some((idx, input));
        cx.notify();
    }

    /// Back out of the reason field, restoring the hunk's Accept/Reject controls.
    fn cancel_reject(&mut self, cx: &mut Context<Self>) {
        self.reject_input = None;
        cx.notify();
    }

    /// Send the rejection for hunk `idx`: emit `Command::RejectHunk`, which the
    /// engine delivers through the `SteerControl` port → the UI drainer writes it
    /// into the session's embedded terminal (Option C). The panel optimistically
    /// marks the hunk rejected.
    fn send_hunk_reject(&mut self, idx: usize, cx: &mut Context<Self>) {
        let reason = self
            .reject_input
            .as_ref()
            .filter(|(i, _)| *i == idx)
            .map(|(_, input)| input.read(cx).value().trim().to_string())
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| DEFAULT_HUNK_REASON.to_string());
        let file = self
            .hunks
            .get(idx)
            .map(|h| h.file_path.clone())
            .unwrap_or_default();
        let message = format!("Review feedback — {file}: {reason}");
        self.reject_input = None;
        self.send(Command::RejectHunk {
            feedback: Feedback {
                session_id: self.session.clone(),
                message,
                origin: FeedbackOrigin::HunkRejection,
            },
        });
        self.decisions.insert(idx, HunkVerdict::Rejected);
        // A rejected hunk is not staged — recompute the index from the accepted set
        // (un-stages it if it had been accepted before).
        self.restage_selected();
        cx.notify();
    }

    /// Run the local `full-review` over the session's repo in the background and
    /// show its output (G5). Shells `claude -p "/full-review"` in the repo root.
    fn run_full_review(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            self.error = Some("no repo path — cannot run full-review".to_string());
            cx.notify();
            return;
        };
        if self.review_running {
            return;
        }
        self.review_running = true;
        self.review_output = None;
        cx.notify();

        cx.spawn(async move |weak, cx| {
            let output = cx
                .background_executor()
                .spawn(async move { run_full_review_blocking(&root) })
                .await;
            let _ = weak.update(cx, |this, cx| {
                this.review_output = Some(output);
                this.review_running = false;
                cx.notify();
            });
        })
        .detach();
    }

    fn send(&mut self, command: Command) {
        if let Some(tx) = &self.commands {
            let _ = tx.send(command);
        }
    }

    fn approve(&mut self, cx: &mut Context<Self>) {
        // Resolve any held approval / resume the session, then advance the workflow
        // Review → Commit — approving the review *is* the operator confirmation for
        // this gate (returns the session to auto mode).
        self.send(Command::ApproveAction {
            session: self.session.clone(),
        });
        self.send(Command::AdvancePhase {
            session: self.session.clone(),
        });
        cx.notify();
    }
}

/// Blocking full-review invocation (run off the UI thread). Captures stdout+stderr
/// so a failure surfaces in the panel rather than vanishing.
fn run_full_review_blocking(root: &PathBuf) -> String {
    match ProcCommand::new("claude")
        .arg("-p")
        .arg("/full-review")
        .current_dir(root)
        .output()
    {
        Ok(out) => {
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            if !out.status.success() {
                let err = String::from_utf8_lossy(&out.stderr);
                text.push_str(&format!("\n[full-review exited with {}]\n{err}", out.status));
            }
            if text.trim().is_empty() {
                "(full-review produced no output)".to_string()
            } else {
                text
            }
        }
        Err(e) => format!("failed to launch full-review (`claude -p`): {e}"),
    }
}

/// First 8 chars of the session id — enough to identify a tab.
fn short_id(id: &SessionId) -> String {
    let s = id.as_str();
    s.get(..8).unwrap_or(s).to_string()
}

/// Color a diff line by its leading marker (added / removed / hunk header).
fn diff_line_color(line: &str) -> gpui::Hsla {
    if line.starts_with("@@") {
        theme::accent()
    } else if line.starts_with('+') {
        theme::status_color(moonlight_domain::session::SessionStatus::Running)
    } else if line.starts_with('-') {
        theme::status_color(moonlight_domain::session::SessionStatus::Errored)
    } else {
        theme::text_secondary()
    }
}

/// Render one diff line (blank lines keep a space so rows don't collapse).
fn diff_line(line: &str) -> impl IntoElement {
    div()
        .font_family(theme::mono_font())
        .text_size(px(12.))
        .text_color(diff_line_color(line))
        .child(if line.is_empty() {
            " ".to_string()
        } else {
            line.to_string()
        })
}

impl Focusable for CodeReviewPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl super::TabMenuHost for CodeReviewPanel {
    fn tab_menu_slot(&mut self) -> &mut Option<super::TabMenu> {
        &mut self.tab_menu
    }
}

impl EventEmitter<PanelEvent> for CodeReviewPanel {}

impl Panel for CodeReviewPanel {
    fn panel_name(&self) -> &'static str {
        "CodeReview"
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        super::tab_title(
            format!("Review · {}", short_id(&self.session)),
            None,
            cx.entity_id().as_u64(),
            self.tab_panel.clone(),
            Arc::new(cx.entity()),
            self.focus_handle(cx),
            cx,
            |menu| menu,
        )
    }

    /// Capture the tab panel so the tab bar's "×" can close this tab.
    fn on_added_to(
        &mut self,
        tab_panel: WeakEntity<TabPanel>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.tab_panel = Some(tab_panel);
    }

    /// Persist the session id + repo root so a rehydrated tab can reload the diff.
    fn dump(&self, _cx: &App) -> PanelState {
        let mut state = PanelState::new(self);
        state.info = PanelInfo::panel(serde_json::json!({
            "session": self.session.as_str(),
            "root": self.root.as_ref().map(|p| p.to_string_lossy().into_owned()),
            "summary": self.summary,
        }));
        state
    }
}

impl CodeReviewPanel {
    fn file_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("review-files")
            .w(px(220.))
            .flex_none()
            .overflow_y_scroll()
            .border_r_1()
            .border_color(theme::border_subtle())
            .flex()
            .flex_col()
            .children(self.files.iter().enumerate().map(|(i, f)| {
                let selected = self.selected == Some(i);
                let label = f.display();
                div()
                    .id(("review-file", i))
                    .cursor_pointer()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py(px(3.))
                    .when(selected, |d| d.bg(theme::tint(theme::accent(), 0.12)))
                    .child(
                        div()
                            .w(px(12.))
                            .text_size(px(11.))
                            .text_color(theme::text_muted())
                            .child(f.status.letter()),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(if selected {
                                theme::text_primary()
                            } else {
                                theme::text_secondary()
                            })
                            .child(label),
                    )
                    .on_click(cx.listener(move |this, _ev, _window, cx| this.select(i, cx)))
            }))
    }

    /// A small pill button.
    fn pill(
        id: (&'static str, usize),
        label: &'static str,
        color: gpui::Hsla,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(id)
            .cursor_pointer()
            .px_2()
            .py(px(2.))
            .rounded(px(6.))
            .bg(theme::tint(color, 0.16))
            .text_color(color)
            .text_size(px(11.))
            .child(label)
            .on_click(cx.listener(move |this, _ev, window, cx| on_click(this, window, cx)))
    }

    /// One hunk: header (index + verdict / controls), the colorized diff body, and —
    /// while rejecting — the inline reason field.
    fn hunk_card(&self, i: usize, hunk: &ReviewHunk, cx: &mut Context<Self>) -> gpui::AnyElement {
        let verdict = self.decisions.get(&i).copied();
        let rejecting = self.reject_input.as_ref().is_some_and(|(idx, _)| *idx == i);

        let controls = match verdict {
            Some(HunkVerdict::Accepted) => div()
                .text_size(px(11.))
                .text_color(theme::status_color(
                    moonlight_domain::session::SessionStatus::Running,
                ))
                .child("✓ accepted · staged")
                .into_any_element(),
            Some(HunkVerdict::Rejected) => div()
                .text_size(px(11.))
                .text_color(theme::status_color(
                    moonlight_domain::session::SessionStatus::Errored,
                ))
                .child("✕ rejected → feedback sent")
                .into_any_element(),
            None if rejecting => div().into_any_element(),
            None => div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(Self::pill(
                    ("hunk-accept", i),
                    "✓ Accept",
                    theme::status_color(moonlight_domain::session::SessionStatus::Running),
                    move |this, _w, cx| this.accept_hunk(i, cx),
                    cx,
                ))
                .child(Self::pill(
                    ("hunk-reject", i),
                    "✕ Reject",
                    theme::status_color(moonlight_domain::session::SessionStatus::Errored),
                    move |this, window, cx| this.begin_hunk_reject(i, window, cx),
                    cx,
                ))
                .into_any_element(),
        };

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .mb_1()
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(theme::text_muted())
                    .child(format!("Hunk {}", i + 1)),
            )
            .child(div().flex_1())
            .child(controls);

        let body = div()
            .flex()
            .flex_col()
            .children(hunk.diff.lines().map(diff_line));

        let mut card = div()
            .id(("hunk", i))
            .rounded(px(8.))
            .border_1()
            .border_color(theme::border_subtle())
            .bg(theme::surface_base())
            .p_2()
            .flex()
            .flex_col()
            .child(header)
            .child(body);

        if rejecting {
            if let Some((_, input)) = self.reject_input.as_ref() {
                let send = Self::pill(
                    ("hunk-reject-send", i),
                    "✕ Send rejection",
                    theme::status_color(moonlight_domain::session::SessionStatus::Errored),
                    move |this, _w, cx| this.send_hunk_reject(i, cx),
                    cx,
                );
                let cancel = Self::pill(
                    ("hunk-reject-cancel", i),
                    "Cancel",
                    theme::text_muted(),
                    move |this, _w, cx| this.cancel_reject(cx),
                    cx,
                );
                card = card.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .mt_2()
                        .child(Input::new(input))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .child(send)
                                .child(cancel),
                        ),
                );
            }
        }

        card.into_any_element()
    }

    /// The hunk list (or the raw-diff fallback when a file has no textual hunks).
    fn hunk_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div()
            .id("review-hunks")
            .flex_1()
            .overflow_y_scroll()
            .bg(theme::surface_raised())
            .p_2()
            .flex()
            .flex_col()
            .gap_2();

        if self.hunks.is_empty() {
            let text = if self.diff.trim().is_empty() {
                "(no textual changes for this file)".to_string()
            } else {
                self.diff.clone()
            };
            col = col.child(
                div()
                    .p_1()
                    .flex()
                    .flex_col()
                    .children(text.lines().map(diff_line)),
            );
        } else {
            col = col.children(
                self.hunks
                    .iter()
                    .enumerate()
                    .map(|(i, h)| self.hunk_card(i, h, cx))
                    .collect::<Vec<_>>(),
            );
        }
        col
    }

    /// "What this covers" band — the agent's latest summary (T4), above the diff.
    fn summary_band(summary: &str) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .rounded(px(8.))
            .border_1()
            .border_color(theme::border_subtle())
            .bg(theme::surface_raised())
            .px_3()
            .py_2()
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(theme::text_muted())
                    .child("What this covers"),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme::text_secondary())
                    .child(summary.to_string()),
            )
    }

    fn action_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let run_label = if self.review_running {
            "Running full-review…"
        } else {
            "Run full-review"
        };
        let run = div()
            .id("run-full-review")
            .cursor_pointer()
            .px_3()
            .py(px(5.))
            .rounded(px(8.))
            .bg(theme::tint(theme::accent(), 0.16))
            .text_color(theme::accent())
            .text_size(px(12.))
            .child(run_label)
            .on_click(cx.listener(|this, _ev, _window, cx| this.run_full_review(cx)));

        let approve = div()
            .id("review-approve")
            .cursor_pointer()
            .px_3()
            .py(px(5.))
            .rounded(px(8.))
            .bg(theme::tint(
                theme::status_color(moonlight_domain::session::SessionStatus::Running),
                0.16,
            ))
            .text_color(theme::status_color(
                moonlight_domain::session::SessionStatus::Running,
            ))
            .text_size(px(12.))
            .child("✓ Approve")
            .on_click(cx.listener(|this, _ev, _window, cx| this.approve(cx)));

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(run)
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(theme::text_muted())
                    .child("Reject a hunk to steer the agent; Approve advances Review → Commit."),
            )
            .child(div().flex_1())
            .child(approve)
    }
}

impl Render for CodeReviewPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(div().text_size(px(15.)).child("Code review"))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme::text_muted())
                    .child(format!(
                        "session {} · {} file(s)",
                        short_id(&self.session),
                        self.files.len()
                    )),
            );

        let body = match &self.error {
            Some(err) if self.files.is_empty() => div()
                .flex_1()
                .p_3()
                .text_size(px(12.))
                .text_color(theme::text_muted())
                .child(err.clone())
                .into_any_element(),
            _ => div()
                .flex_1()
                .flex()
                .flex_row()
                .min_h(px(0.))
                .border_1()
                .border_color(theme::border_subtle())
                .rounded(px(8.))
                .overflow_hidden()
                .child(self.file_list(cx))
                .child(self.hunk_list(cx))
                .into_any_element(),
        };

        let mut root = div()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                super::close_this_tab(&this.tab_panel, cx.entity(), window, cx)
            }))
            .flex()
            .flex_col()
            .gap_3()
            .size_full()
            .bg(theme::surface_base())
            .text_color(theme::text_primary())
            .p_4()
            .child(header)
            .children(self.summary.as_ref().map(|s| Self::summary_band(s)))
            .child(body);

        // A delivery note (e.g. "no live terminal") shown while files are present.
        if let (Some(err), false) = (&self.error, self.files.is_empty()) {
            root = root.child(
                div()
                    .text_size(px(11.))
                    .text_color(theme::status_color(
                        moonlight_domain::session::SessionStatus::Errored,
                    ))
                    .child(err.clone()),
            );
        }

        root = root.child(self.action_bar(cx));

        if let Some(output) = &self.review_output {
            root = root.child(
                div()
                    .id("review-output")
                    .max_h(px(220.))
                    .overflow_y_scroll()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(theme::border_subtle())
                    .bg(theme::surface_raised())
                    .p_3()
                    .flex()
                    .flex_col()
                    .children(output.lines().map(|line| {
                        div()
                            .text_size(px(11.))
                            .text_color(theme::text_secondary())
                            .child(if line.is_empty() {
                                " ".to_string()
                            } else {
                                line.to_string()
                            })
                    })),
            );
        }

        let dismiss = cx.listener(|this, _: &MouseDownEvent, _w, cx| {
            this.tab_menu = None;
            cx.notify();
        });
        root.children(super::tab_menu_overlay(self.tab_menu.as_ref(), dismiss, window))
    }
}
