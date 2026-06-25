//! Plan-review panel — shows a plan the agent proposed and lets the operator
//! **approve or reject** it (the plan-validation gate, G2).
//!
//! Opened as a center tab when `EngineEvent::PlanProposed` fires (the agent called
//! `ExitPlanMode`). When that plan came through the held-hook keystone the session
//! is *paused* on its `ExitPlanMode` call: **Approve** lets it leave plan mode and
//! start working; **Reject** denies the call with feedback so the agent revises.
//! The decision travels as a [`Command`] to the engine, which resolves the held
//! hook (see `main::route_approval`).
//!
//! A rehydrated tab (restored layout) starts non-pending — there is no live hook to
//! resolve — so it shows the plan without action buttons until a fresh proposal.

use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    div, px, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    MouseDownEvent, Task, WeakEntity, Window,
};
use gpui_component::dock::{Panel, PanelEvent, PanelInfo, PanelState, PanelView, TabPanel};

use super::CloseTab;
use gpui_component::input::{Input, InputState};
use gpui_component::text::TextView;

use moonlight_domain::ids::SessionId;
use moonlight_domain::phase::Phase;
use moonlight_domain::session::SessionStatus;
use moonlight_engine::{Command, EngineEvent};
use tokio::sync::broadcast;
use tokio::sync::mpsc::UnboundedSender;

use crate::views::theme;
use crate::views::center_requests::OpenRequest;
use crate::views::workspace::ShellDeps;

/// Default rejection feedback when the operator rejects without typing a reason.
const DEFAULT_REJECT_REASON: &str = "Plan rejected by operator — please revise and re-propose.";

pub struct PlanReviewPanel {
    session: SessionId,
    plan: String,
    /// Sends the operator's verdict to the engine; `None` for static/test views.
    commands: Option<UnboundedSender<Command>>,
    /// Whether a held approval is outstanding for this session (buttons are live).
    pending: bool,
    /// When the operator clicks **Reject**, this holds the reason input they fill in
    /// before the rejection is sent; `None` while the three top-level buttons show.
    reject_input: Option<Entity<InputState>>,
    focus_handle: FocusHandle,
    /// The tab panel this review lives in (captured in [`Panel::on_added_to`]), so
    /// the tab bar's "×" can close this tab — see [`super::tab_title`].
    tab_panel: Option<WeakEntity<TabPanel>>,
    /// Open right-click tab menu, rendered from the panel body (see [`super::TabMenuHost`]).
    tab_menu: Option<super::TabMenu>,
    /// Keeps the bus subscription alive for the panel's lifetime.
    _subscription: Option<Task<()>>,
}

impl PlanReviewPanel {
    /// Live panel: tracks pending-approval state by folding the engine bus.
    /// `pending` seeds whether an approval is currently outstanding (true when the
    /// tab is opened from a fresh proposal; false on layout rehydration).
    pub fn new(
        session: SessionId,
        plan: String,
        pending: bool,
        commands: Option<UnboundedSender<Command>>,
        rx: broadcast::Receiver<EngineEvent>,
        cx: &mut Context<Self>,
    ) -> Self {
        let watched = session.clone();
        let subscription = cx.spawn(async move |weak, cx| {
            let mut rx = rx;
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        let applied = weak.update(cx, |this, cx| {
                            if this.apply_event(&watched, &event) {
                                cx.notify();
                            }
                        });
                        if applied.is_err() {
                            break; // panel dropped
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        Self {
            session,
            plan,
            commands,
            pending,
            reject_input: None,
            focus_handle: cx.focus_handle(),
            tab_panel: None,
            tab_menu: None,
            _subscription: Some(subscription),
        }
    }

    /// Fold a bus event into the panel's pending state. Returns whether it changed
    /// (so the view only re-renders when relevant to this session).
    fn apply_event(&mut self, watched: &SessionId, event: &EngineEvent) -> bool {
        match event {
            // A fresh proposal for this session re-arms the buttons (re-proposal
            // after a reject) and refreshes the plan text.
            EngineEvent::PlanProposed { session, plan } if session == watched => {
                self.plan = plan.clone();
                self.pending = true;
                self.reject_input = None;
                true
            }
            // The session resumed (our decision was applied, or it moved on) — the
            // hold is no longer outstanding.
            EngineEvent::SessionStateChanged { session, status }
                if session == watched && !status_is_waiting(*status) =>
            {
                let was = self.pending || self.reject_input.is_some();
                self.pending = false;
                self.reject_input = None;
                was
            }
            // The session was forgotten/removed (e.g. ↻ Reset, or it ended): no hold can
            // still be outstanding, so disarm the buttons rather than leave a stale
            // Approve/Reject that would resolve nothing.
            EngineEvent::SessionRemoved { session } if session == watched => {
                let was = self.pending || self.reject_input.is_some();
                self.pending = false;
                self.reject_input = None;
                was
            }
            _ => false,
        }
    }

    fn decide(&mut self, decision: Command, cx: &mut Context<Self>) {
        if let Some(tx) = &self.commands {
            let _ = tx.send(decision);
        }
        self.pending = false;
        cx.notify();
    }

    fn approve(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.decide(
            Command::ApproveAction {
                session: self.session.clone(),
            },
            cx,
        );
        // Approving = CC's native option 1 (accept + auto). See [`select_native_option`].
        // (Spawned before we close the tab — the task drives the session's terminal
        // independently of this panel's lifetime.)
        self.select_native_option("1", cx);
        // A verdict is final: return the operator to the session that proposed the plan.
        self.close_and_return_to_session(window, cx);
    }

    /// After a final verdict (approve/reject), front the related session's focus tab
    /// and close this plan-review tab — the operator lands back on the session that
    /// proposed the plan. (Refine is *not* final, so it keeps the tab open.)
    ///
    /// The navigate + close are **deferred to the next tick**: this runs from the
    /// approve/reject click listener, i.e. *inside* `PlanReviewPanel`'s own `update`.
    /// Removing this very panel (and emitting `SessionById`, which switches spaces and
    /// rebuilds the center) synchronously re-enters the entity and aborts the process
    /// with gpui's "cannot update PlanReviewPanel while it is already being updated".
    /// Spawning lets the current update unwind before we touch the dock.
    fn close_and_return_to_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.session.clone();
        let tab_panel = self.tab_panel.clone();
        // `Entity<PlanReviewPanel>` is itself a `PanelView` — hand ourselves to the tab
        // panel to remove the tab (mirrors the tab bar's "×").
        let me: Arc<dyn PanelView> = Arc::new(cx.entity());
        cx.spawn_in(window, async move |_, cx| {
            let _ = cx.update(|window, cx| {
                // Front (or reopen) the session's focus tab. `SessionById` dedups by id,
                // so an already-open session tab is simply brought forward; root is
                // resolved from the managed store (None is fine for a plan-gated session).
                if let Some(deps) = cx.try_global::<ShellDeps>() {
                    let center = deps.center.clone();
                    center.update(cx, |_, cx| {
                        cx.emit(OpenRequest::SessionById { id: session, root: None });
                    });
                }
                if let Some(tp) = tab_panel.as_ref().and_then(|w| w.upgrade()) {
                    tp.update(cx, |tp, cx| tp.remove_panel(me, window, cx));
                }
            });
        })
        .detach();
    }

    /// **Refine with ultraplan** = CC's native option 3. Resolving the held hook with
    /// "allow" (same as approve) lets CC render its continuation prompt; we then drive
    /// the dialog to option 3 so CC re-plans via `/ultraplan` instead of proceeding.
    fn refine(&mut self, cx: &mut Context<Self>) {
        self.decide(
            Command::ApproveAction {
                session: self.session.clone(),
            },
            cx,
        );
        self.select_native_option("3", cx);
    }

    /// Drive CC's post-plan continuation dialog to a numbered choice. The held hook
    /// emits *no output* = "allow", so CC leaves plan mode and renders its **own**
    /// interactive prompt in the embedded terminal: 1 = accept + auto · 2 = step-by-step
    /// · 3 = refine w/ ultraplan · 4 = reject. We send the digit explicitly (robust to
    /// the default-highlight moving as CC adds options) followed by Enter to confirm.
    ///
    /// Rather than racing a fixed delay (the hook round-trip — cockpit → engine → held
    /// oneshot → hook returns → CC re-renders — is variable and often outlasts it), we
    /// **poll the embedded terminal until the continuation menu is actually on screen**,
    /// then inject. This is robust whether the held hook is still resolving or CC
    /// already showed the menu on its own. A bounded fallback injects anyway, so a CC
    /// wording change can't strand the session.
    fn select_native_option(&self, option: &'static str, cx: &mut Context<Self>) {
        let Some(term) = cx
            .try_global::<ShellDeps>()
            .and_then(|d| d.session_io.terminal(&self.session))
        else {
            tracing::warn!(session = %self.session.as_str(),
                "plan: no embedded terminal to drive CC continuation — open the session tab");
            return;
        };
        cx.spawn(async move |_, cx| {
            let mut waited = 0u64;
            let mut matched = false;
            // Give the hook round-trip a moment to start, then poll for the menu.
            while waited < 6000 {
                cx.background_executor()
                    .timer(Duration::from_millis(150))
                    .await;
                waited += 150;
                let Ok(screen) = term.update(cx, |t, _| t.visible_text()) else {
                    return; // terminal gone
                };
                if is_continuation_menu(&screen) {
                    matched = true;
                    break;
                }
            }
            tracing::info!(
                option,
                waited_ms = waited,
                matched,
                "plan: driving CC continuation menu"
            );
            let _ = term.update(cx, |t, _| t.send_text(&format!("{option}\r")));
        })
        .detach();
    }

    /// Clicking **Reject** opens an inline reason field (rather than denying with a
    /// canned message). The operator types why, then confirms with [`Self::send_reject`].
    fn begin_reject(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Why are you rejecting this plan? (sent back to the agent)")
        });
        input.focus_handle(cx).focus(window, cx);
        self.reject_input = Some(input);
        cx.notify();
    }

    /// Back out of the reject reason field, restoring the three top-level buttons.
    fn cancel_reject(&mut self, cx: &mut Context<Self>) {
        self.reject_input = None;
        cx.notify();
    }

    /// Send the rejection with the typed reason (falling back to the default when the
    /// field is empty). Denies the held hook, so CC blocks `ExitPlanMode` and revises.
    fn send_reject(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let reason = self
            .reject_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_string())
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| DEFAULT_REJECT_REASON.to_string());
        self.reject_input = None;
        self.decide(
            Command::DenyAction {
                session: self.session.clone(),
                reason,
            },
            cx,
        );
        // A verdict is final: return the operator to the session that proposed the plan.
        self.close_and_return_to_session(window, cx);
    }
}

/// Whether a status keeps the session in the "waiting on the operator" state.
fn status_is_waiting(status: SessionStatus) -> bool {
    matches!(status, SessionStatus::WaitingInput)
}

/// Heuristic: does the visible terminal text show Claude Code's post-`ExitPlanMode`
/// continuation menu? These phrases are specific to that menu (option 1 ends in
/// "…auto-accept edits", the keep-planning option says "keep planning"), so they don't
/// false-match the plan markdown shown while CC is still blocked on the hook. Tune here
/// if a CC version rewords the prompt.
fn is_continuation_menu(screen: &str) -> bool {
    let s = screen.to_ascii_lowercase();
    s.contains("auto-accept") || s.contains("keep planning")
}

#[cfg(test)]
mod tests {
    use super::is_continuation_menu;

    #[test]
    fn detects_continuation_menu_not_plan_text() {
        // The rendered menu (with option 1's "auto-accept edits") is detected.
        assert!(is_continuation_menu(
            "Would you like to proceed?\n 1. Yes, and auto-accept edits\n 3. No, keep planning"
        ));
        // Matches on the keep-planning option alone too.
        assert!(is_continuation_menu(" 2. Yes, and manually approve\n 3. No, keep planning"));
        // Plain plan markdown (numbered steps) must NOT look like the menu.
        assert!(!is_continuation_menu(
            "## Plan\n1. Refactor the parser\n2. Add tests\n3. Update docs"
        ));
    }
}

/// First 8 chars of the session id (uuids are long); enough to identify a tab.
fn short_id(id: &SessionId) -> String {
    let s = id.as_str();
    s.get(..8).unwrap_or(s).to_string()
}

impl Focusable for PlanReviewPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl super::TabMenuHost for PlanReviewPanel {
    fn tab_menu_slot(&mut self) -> &mut Option<super::TabMenu> {
        &mut self.tab_menu
    }
}

impl EventEmitter<PanelEvent> for PlanReviewPanel {}

impl Panel for PlanReviewPanel {
    fn panel_name(&self) -> &'static str {
        "PlanReview"
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        super::tab_title(
            format!("Plan · {}", short_id(&self.session)),
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

    /// Persist which session this tab reviews; the plan text is not persisted
    /// (it is re-emitted live), so a rehydrated tab shows a placeholder.
    fn dump(&self, _cx: &App) -> PanelState {
        let mut state = PanelState::new(self);
        state.info = PanelInfo::panel(serde_json::json!({ "session": self.session.as_str() }));
        state
    }
}

impl PlanReviewPanel {
    /// A pill button for the action bar.
    fn action_button(
        id: &'static str,
        label: &'static str,
        color: gpui::Hsla,
        tint: f32,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(id)
            .cursor_pointer()
            .px_3()
            .py(px(5.))
            .rounded(px(8.))
            .bg(theme::tint(color, tint))
            .text_color(color)
            .text_size(px(12.))
            .child(label)
            .on_click(cx.listener(move |this, _ev, window, cx| on_click(this, window, cx)))
    }

    /// The action bar (live) or the review-only footer (no hold). While the operator
    /// is composing a rejection reason, the bar is replaced by the reason field.
    fn footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.pending {
            return div()
                .text_size(px(11.))
                .text_color(theme::text_muted())
                .child("Review-only — no decision is currently awaited for this session.")
                .into_any_element();
        }

        // Reason-entry mode: show the input + Send/Cancel instead of the three buttons.
        if let Some(input) = &self.reject_input {
            let send = Self::action_button(
                "plan-reject-send",
                "✕ Send rejection",
                theme::status_color(SessionStatus::Errored),
                0.16,
                |this, window, cx| this.send_reject(window, cx),
                cx,
            );
            let cancel = Self::action_button(
                "plan-reject-cancel",
                "Cancel",
                theme::text_muted(),
                0.12,
                |this, _w, cx| this.cancel_reject(cx),
                cx,
            );
            return div()
                .flex()
                .flex_col()
                .gap_2()
                .child(Input::new(input))
                .child(div().flex().flex_row().items_center().gap_2().child(send).child(cancel))
                .into_any_element();
        }

        let approve = Self::action_button(
            "plan-approve",
            "✓ Approve plan",
            theme::accent(),
            0.18,
            |this, window, cx| this.approve(window, cx),
            cx,
        );
        let refine = Self::action_button(
            "plan-refine",
            "✦ Refine with ultraplan",
            theme::phase_color(Phase::Plan),
            0.16,
            |this, _w, cx| this.refine(cx),
            cx,
        );
        let reject = Self::action_button(
            "plan-reject",
            "✕ Reject",
            theme::status_color(SessionStatus::Errored),
            0.16,
            |this, window, cx| this.begin_reject(window, cx),
            cx,
        );

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(approve)
            .child(refine)
            .child(reject)
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(theme::text_muted())
                    .child("The session is paused on this plan, waiting for you."),
            )
            .into_any_element()
    }
}

impl Render for PlanReviewPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dismiss = cx.listener(|this, _: &MouseDownEvent, _w, cx| {
            this.tab_menu = None;
            cx.notify();
        });
        div()
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
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(7.))
                    .child(
                        div()
                            .text_color(theme::phase_color(Phase::Plan))
                            .text_size(theme::text_base())
                            .child("◴"),
                    )
                    .child(
                        div()
                            .text_size(theme::text_lg())
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Plan review"),
                    )
                    .child(
                        div()
                            .font_family(theme::mono_font())
                            .text_size(theme::text_sm())
                            .text_color(theme::text_muted())
                            .child(short_id(&self.session)),
                    ),
            )
            .child(
                // The plan, rendered as real markdown (headings/lists/code/bold)
                // via gpui-component's `TextView` so it reads as a document.
                div()
                    .id("plan-body")
                    .flex_1()
                    .overflow_y_scroll()
                    .rounded(theme::radius_md())
                    .border_1()
                    .border_color(theme::border_subtle())
                    .bg(theme::surface_raised())
                    .px_4()
                    .py_3()
                    .child(TextView::markdown("plan-body-md", self.plan.clone())),
            )
            .child(self.footer(cx))
            .children(super::tab_menu_overlay(self.tab_menu.as_ref(), dismiss, window))
    }
}
