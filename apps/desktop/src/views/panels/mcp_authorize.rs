//! MCP authorization panel — the once / always / refuse gate for an **external MCP
//! tool** a frozen phase (Discovery / Plan) would otherwise block.
//!
//! Opened as a center tab when `EngineEvent::ApprovalRequested { authorize_tool:
//! Some(tool), .. }` fires: the session is *paused* on the held hook. The four
//! affordances each resolve that hook:
//! - **Allow once** → `Command::ApproveAction` (this call only).
//! - **Always allow this tool** → `Command::AuthorizeAlwaysTool { pattern: <tool> }`.
//! - **Always allow this server** → `AuthorizeAlwaysTool { pattern: "mcp__<server>__*" }`.
//! - **Refuse** → `Command::DenyAction`.
//!
//! "Always" vouches the pattern in the runtime overlay (immediate effect) and persists
//! it to `~/.moonlight/config.json` (see `main::route_approval` / `persist_always_allow`).
//! A rehydrated tab (restored layout) is never produced — the panel does not persist
//! across restarts, since a stale authorization prompt has no live hook to resolve.

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    div, px, App, Context, EventEmitter, FocusHandle, Focusable, FontWeight, MouseDownEvent, Task,
    WeakEntity, Window,
};
use gpui_component::dock::{Panel, PanelEvent, PanelInfo, PanelState, PanelView, TabPanel};
use tokio::sync::broadcast;
use tokio::sync::mpsc::UnboundedSender;

use super::CloseTab;
use moonlight_domain::ids::SessionId;
use moonlight_domain::session::SessionStatus;
use moonlight_engine::{Command, EngineEvent};

use crate::views::center_requests::OpenRequest;
use crate::views::theme;
use crate::views::workspace::ShellDeps;

/// Reason returned to the agent when the operator refuses an MCP authorization.
const REFUSE_REASON: &str =
    "External tool call refused by operator in this phase — request a phase change or use a different approach.";

pub struct McpAuthorizePanel {
    session: SessionId,
    /// The full `mcp__server__tool` name awaiting authorization.
    tool: String,
    /// Sends the operator's verdict to the engine; `None` for static/test views.
    commands: Option<UnboundedSender<Command>>,
    /// Whether a held authorization is outstanding for this session (buttons are live).
    pending: bool,
    focus_handle: FocusHandle,
    tab_panel: Option<WeakEntity<TabPanel>>,
    tab_menu: Option<super::TabMenu>,
    /// Keeps the bus subscription alive for the panel's lifetime.
    _subscription: Option<Task<()>>,
}

/// The `mcp__server__*` glob for an external MCP tool name (the "always allow this
/// server" pattern). Falls back to the exact name when it isn't the expected
/// `mcp__server__tool` shape.
fn server_glob(tool: &str) -> String {
    let parts: Vec<&str> = tool.split("__").collect();
    if parts.len() >= 3 && parts[0] == "mcp" {
        format!("{}__{}__*", parts[0], parts[1])
    } else {
        tool.to_string()
    }
}

impl McpAuthorizePanel {
    /// Live panel: tracks pending-authorization state by folding the engine bus.
    pub fn new(
        session: SessionId,
        tool: String,
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
            tool,
            commands,
            pending,
            focus_handle: cx.focus_handle(),
            tab_panel: None,
            tab_menu: None,
            _subscription: Some(subscription),
        }
    }

    /// Fold a bus event into the panel's pending state. Returns whether it changed.
    fn apply_event(&mut self, watched: &SessionId, event: &EngineEvent) -> bool {
        match event {
            // A fresh authorization for this session re-arms the buttons and refreshes
            // the tool name (a later external tool the freeze blocked).
            EngineEvent::ApprovalRequested {
                session,
                authorize_tool: Some(tool),
                ..
            } if session == watched => {
                self.tool = tool.clone();
                self.pending = true;
                true
            }
            // The session resumed (our decision was applied, or it moved on) — the hold
            // is no longer outstanding.
            EngineEvent::SessionStateChanged { session, status }
                if session == watched && !status_is_waiting(*status) =>
            {
                let was = self.pending;
                self.pending = false;
                was
            }
            // The session was forgotten/removed — no hold can still be outstanding.
            EngineEvent::SessionRemoved { session } if session == watched => {
                let was = self.pending;
                self.pending = false;
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

    fn allow_once(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.decide(
            Command::ApproveAction {
                session: self.session.clone(),
            },
            cx,
        );
        self.close_and_return_to_session(window, cx);
    }

    fn always_allow(&mut self, pattern: String, window: &mut Window, cx: &mut Context<Self>) {
        self.decide(
            Command::AuthorizeAlwaysTool {
                session: self.session.clone(),
                pattern,
            },
            cx,
        );
        self.close_and_return_to_session(window, cx);
    }

    fn refuse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.decide(
            Command::DenyAction {
                session: self.session.clone(),
                reason: REFUSE_REASON.to_string(),
            },
            cx,
        );
        self.close_and_return_to_session(window, cx);
    }

    /// After a verdict, front the related session's focus tab and close this tab — the
    /// operator lands back on the session that requested the tool. Deferred to the next
    /// tick: this runs inside the panel's own `update`, and removing the panel (plus the
    /// `SessionById` space switch) would otherwise re-enter the entity and panic. Mirrors
    /// [`super::plan_review::PlanReviewPanel::close_and_return_to_session`].
    fn close_and_return_to_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.session.clone();
        let tab_panel = self.tab_panel.clone();
        let me: Arc<dyn PanelView> = Arc::new(cx.entity());
        cx.spawn_in(window, async move |_, cx| {
            let _ = cx.update(|window, cx| {
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
}

/// Whether a status keeps the session in the "waiting on the operator" state.
fn status_is_waiting(status: SessionStatus) -> bool {
    matches!(status, SessionStatus::WaitingInput)
}

/// First 8 chars of the session id (uuids are long); enough to identify a tab.
fn short_id(id: &SessionId) -> String {
    let s = id.as_str();
    s.get(..8).unwrap_or(s).to_string()
}

impl Focusable for McpAuthorizePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl super::TabMenuHost for McpAuthorizePanel {
    fn tab_menu_slot(&mut self) -> &mut Option<super::TabMenu> {
        &mut self.tab_menu
    }
}

impl EventEmitter<PanelEvent> for McpAuthorizePanel {}

impl Panel for McpAuthorizePanel {
    fn panel_name(&self) -> &'static str {
        "McpAuthorize"
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        super::tab_title(
            format!("Authorize · {}", short_id(&self.session)),
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

    /// Not persisted for restore: a stale authorization prompt has no live hook to
    /// resolve, so `panel_state_key` deliberately has no `McpAuthorize` arm (the tab is
    /// dropped on restart). `dump` still records the session for completeness.
    fn dump(&self, _cx: &App) -> PanelState {
        let mut state = PanelState::new(self);
        state.info = PanelInfo::panel(serde_json::json!({ "session": self.session.as_str() }));
        state
    }
}

impl McpAuthorizePanel {
    /// A pill button for the action bar.
    fn action_button(
        id: &'static str,
        label: String,
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

    /// The action bar (live) or the review-only footer (no hold).
    fn footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.pending {
            return div()
                .text_size(px(11.))
                .text_color(theme::text_muted())
                .child("Review-only — no authorization is currently awaited for this session.")
                .into_any_element();
        }

        let glob = server_glob(&self.tool);
        let tool = self.tool.clone();

        let allow_once = Self::action_button(
            "mcp-allow-once",
            "✓ Allow once".to_string(),
            theme::accent(),
            0.18,
            |this, window, cx| this.allow_once(window, cx),
            cx,
        );
        let always_tool = Self::action_button(
            "mcp-always-tool",
            "✓ Always allow this tool".to_string(),
            theme::accent(),
            0.12,
            move |this, window, cx| {
                let pattern = this.tool.clone();
                this.always_allow(pattern, window, cx)
            },
            cx,
        );
        let glob_for_btn = glob.clone();
        let always_server = Self::action_button(
            "mcp-always-server",
            format!("✓ Always allow server · {glob}"),
            theme::accent(),
            0.12,
            move |this, window, cx| this.always_allow(glob_for_btn.clone(), window, cx),
            cx,
        );
        let refuse = Self::action_button(
            "mcp-refuse",
            "✕ Refuse".to_string(),
            theme::status_color(SessionStatus::Errored),
            0.16,
            |this, window, cx| this.refuse(window, cx),
            cx,
        );

        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .flex_wrap()
                    .gap_2()
                    .child(allow_once)
                    .child(always_tool)
                    .child(always_server)
                    .child(refuse),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(theme::text_muted())
                    .child(format!(
                        "The session is paused waiting for you to authorize `{tool}`."
                    )),
            )
            .into_any_element()
    }
}

impl Render for McpAuthorizePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dismiss = cx.listener(|this, _: &MouseDownEvent, _w, cx| {
            this.tab_menu = None;
            cx.notify();
        });
        let tool = self.tool.clone();
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
                            .text_color(theme::accent())
                            .text_size(theme::text_base())
                            .child("◈"),
                    )
                    .child(
                        div()
                            .text_size(theme::text_lg())
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Authorize external tool"),
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
                div()
                    .flex_1()
                    .rounded(theme::radius_md())
                    .border_1()
                    .border_color(theme::border_subtle())
                    .bg(theme::surface_raised())
                    .px_4()
                    .py_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(theme::text_sm())
                            .text_color(theme::text_muted())
                            .child(
                                "A frozen phase blocks this external MCP tool. Authorize it for \
                                 this one call, always for the tool, or always for its whole \
                                 server — or refuse.",
                            ),
                    )
                    .child(
                        div()
                            .font_family(theme::mono_font())
                            .text_size(theme::text_base())
                            .child(tool),
                    ),
            )
            .child(self.footer(cx))
            .children(super::tab_menu_overlay(self.tab_menu.as_ref(), dismiss, window))
    }
}

#[cfg(test)]
mod tests {
    use super::server_glob;

    #[test]
    fn server_glob_covers_the_whole_server() {
        assert_eq!(
            server_glob("mcp__phoenix__run_select_query"),
            "mcp__phoenix__*"
        );
        assert_eq!(server_glob("mcp__rustrover__execute_sql_query"), "mcp__rustrover__*");
        // A tool with extra `__` in its leaf still globs at the server boundary.
        assert_eq!(server_glob("mcp__phoenix__open_snowflake_request"), "mcp__phoenix__*");
        // A non-MCP-shaped name falls back to itself (never an over-broad glob).
        assert_eq!(server_glob("weird"), "weird");
        assert_eq!(server_glob("mcp__only"), "mcp__only");
    }
}
