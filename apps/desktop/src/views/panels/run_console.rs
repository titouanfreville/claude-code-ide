//! The **Run tool window** — the JetBrains Run view: its own **onglet bar** with one
//! tab per run target (`cargo run`, `cargo test`, … each with its own captured logs,
//! status lamp, and ✕), a **left control strip** (↻ Restart / ⏹ Stop / ⌫ Clear /
//! ⤒ top / ⤓ end / 🔍 search), and the active run's console body. This is *not*
//! another terminal: children run with piped stdio, runs are concurrent, and
//! re-running a target reuses its tab.
//!
//! Console behaviors:
//! - **Follow-mode scrolling** — the tail stays in view while `follow` is on; any
//!   upward wheel disengages it (free scrolling), **⤓** re-engages, **⤒** jumps to
//!   the top. Lines don't wrap (a console), so the body scrolls both axes and every
//!   line is exactly [`LINE_H`] tall — which makes jump-to-line math exact.
//! - **Search** — `Cmd+F` (or 🔍) opens a search row: case-insensitive substring
//!   over the active run's lines, match count, ‹ › navigation (Enter / Shift+Enter),
//!   matching lines tinted, the current one stronger; `Esc` closes.
//!
//! Everything renders from the shared [`RunRegistry`](crate::run::RunRegistry) — the
//! same state the MCP run verbs drive — so a run started by a CC session pops its
//! own onglet here, live, and vice versa. The registry is GPUI-free, so the panel
//! **polls**: a 100ms loop compares the registry's `seq` and `cx.notify()`s on
//! change (the same dirty-flag cadence the terminal's emulator uses).

use gpui::prelude::*;
use gpui::{
    actions, div, point, px, App, Context, FocusHandle, Focusable, KeyBinding, MouseButton,
    ScrollHandle, SharedString, Subscription, Window,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::tooltip::Tooltip;
use gpui_component::Sizable;

use moonlight_domain::session::SessionStatus;

use crate::run::{RunRegistry, RunSnapshot, RunStatus, RunTab};
use crate::views::theme;

actions!(moonlight_run, [ToggleSearch, CloseSearch]);

/// Key context for the Run window's bindings (`Cmd+F`, `Esc`).
const RUN_CONTEXT: &str = "RunConsole";

/// Bind the Run window's keys. Called once from `init_shell`.
pub fn init_keybindings(cx: &mut App) {
    cx.bind_keys(vec![
        // `secondary-` is ⌘ on macOS, Ctrl elsewhere (see `init_shell`).
        KeyBinding::new("secondary-f", ToggleSearch, Some(RUN_CONTEXT)),
        KeyBinding::new("escape", CloseSearch, Some(RUN_CONTEXT)),
    ]);
}

/// Poll cadence for the registry's seq counter (matches the terminal's dirty poll).
const POLL_MS: u64 = 100;

/// Fixed console line height — lines are `whitespace_nowrap` + exactly this tall,
/// so search-jump scroll offsets are exact (`line index × LINE_H`).
const LINE_H: f32 = 16.;

pub struct RunConsolePanel {
    registry: RunRegistry,
    scroll: ScrollHandle,
    focus_handle: FocusHandle,
    /// Last registry seq folded into a frame — repaint only on change.
    last_seq: u64,
    /// The selected onglet (a run id); `None` until something runs.
    active: Option<u64>,
    /// Last `last_started` folded in — a *newly* started run fronts its tab once,
    /// without stealing the selection back on every poll.
    last_started_seen: Option<u64>,
    /// Tail-follow: new output keeps the bottom in view. Wheel-up disengages;
    /// ⤓ re-engages.
    follow: bool,
    search_open: bool,
    search: gpui::Entity<InputState>,
    query: String,
    /// Which match ‹ › is on (an index into the recomputed match list).
    current_match: usize,
    _subs: Vec<Subscription>,
}

impl RunConsolePanel {
    pub fn new(registry: RunRegistry, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Dirty-poll the registry: new output / status flips repaint the console,
        // a newly started run fronts its onglet, and (while following) the tail
        // stays in view.
        cx.spawn(async move |this, cx| loop {
            let Some(panel) = this.upgrade() else { break };
            panel.update(cx, |panel, cx| {
                let seq = panel.registry.seq();
                if seq != panel.last_seq {
                    panel.last_seq = seq;
                    // Front the onglet of a run that just started (operator or CC).
                    let started = panel.registry.last_started();
                    if started != panel.last_started_seen {
                        panel.last_started_seen = started;
                        panel.active = started.or(panel.active);
                        panel.follow = true;
                    }
                    // The selected onglet may have been closed via MCP/remove.
                    if panel
                        .active
                        .is_some_and(|id| panel.registry.snapshot(id).is_none())
                    {
                        panel.active = panel.registry.last_started();
                    }
                    if panel.follow {
                        panel.scroll.scroll_to_bottom();
                    }
                    cx.notify();
                }
            });
            cx.background_executor()
                .timer(std::time::Duration::from_millis(POLL_MS))
                .await;
        })
        .detach();

        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search output…"));
        let mut subs = Vec::new();
        // Search input: live query on Change; Enter / Shift+Enter walk the matches.
        subs.push(
            cx.subscribe(&search, |this, state, event: &InputEvent, cx| match event {
                InputEvent::Change => {
                    this.query = state.read(cx).value().to_string();
                    this.current_match = 0;
                    cx.notify();
                }
                InputEvent::PressEnter { shift, .. } => this.step_match(!shift, cx),
                _ => {}
            }),
        );

        Self {
            registry,
            scroll: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            last_seq: 0,
            active: None,
            last_started_seen: None,
            follow: true,
            search_open: false,
            search,
            query: String::new(),
            current_match: 0,
            _subs: subs,
        }
    }

    /// Relaunch the active onglet's command in its root (↻ — kills a live run first,
    /// JetBrains "Rerun").
    fn restart(&mut self, cx: &mut Context<Self>) {
        if let Some(snap) = self.active.and_then(|id| self.registry.snapshot(id)) {
            let _ = self.registry.start(&snap.label, &snap.command, snap.root);
            self.follow = true;
            cx.notify();
        }
    }

    /// The line indices matching the current query (case-insensitive substring).
    fn matches(&self, snap: &RunSnapshot) -> Vec<usize> {
        if !self.search_open || self.query.is_empty() {
            return Vec::new();
        }
        let needle = self.query.to_lowercase();
        snap.logs
            .iter()
            .enumerate()
            .filter(|(_, l)| l.text.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect()
    }

    /// Walk to the next/previous match and bring it into view (2 context lines
    /// above; exact math thanks to the fixed line height).
    fn step_match(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(snap) = self.active.and_then(|id| self.registry.snapshot(id)) else {
            return;
        };
        let matches = self.matches(&snap);
        if matches.is_empty() {
            return;
        }
        self.current_match = if forward {
            (self.current_match + 1) % matches.len()
        } else {
            (self.current_match + matches.len() - 1) % matches.len()
        };
        let line = matches[self.current_match].saturating_sub(2);
        self.follow = false;
        self.scroll
            .set_offset(point(px(0.), -px(line as f32 * LINE_H)));
        cx.notify();
    }

    fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = true;
        window.focus(&self.search.focus_handle(cx), cx);
        cx.notify();
    }

    fn on_toggle_search(&mut self, _: &ToggleSearch, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_open {
            self.on_close_search(&CloseSearch, window, cx);
        } else {
            self.open_search(window, cx);
        }
    }

    fn on_close_search(&mut self, _: &CloseSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = false;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// The status lamp + chip colors: running = the session "Running" blue,
    /// exit 0 = Done green, anything else = Errored red, stopped = muted.
    /// Also colors the workspace's rail Run-button lamp.
    pub(crate) fn status_color(status: &RunStatus) -> gpui::Hsla {
        match status {
            RunStatus::Running => theme::status_color(SessionStatus::Running),
            RunStatus::Exited(0) => theme::status_color(SessionStatus::Done),
            RunStatus::Exited(_) | RunStatus::Failed(_) => {
                theme::status_color(SessionStatus::Errored)
            }
            RunStatus::Killed => theme::text_muted(),
        }
    }
}

impl Focusable for RunConsolePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RunConsolePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = self.registry.tabs();
        // Selection sanity per frame: closed onglet → fall back to the newest run.
        let active_id = self
            .active
            .filter(|id| tabs.iter().any(|t| t.id == *id))
            .or_else(|| tabs.last().map(|t| t.id));
        let active = active_id.and_then(|id| self.registry.snapshot(id));
        let matches = active.as_ref().map(|s| self.matches(s)).unwrap_or_default();
        let current = matches
            .get(self.current_match.min(matches.len().saturating_sub(1)))
            .copied();

        div()
            .id("run-console")
            .key_context(RUN_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_toggle_search))
            .on_action(cx.listener(Self::on_close_search))
            // Clicking anywhere in the window focuses it, arming Cmd+F.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev, window, cx| window.focus(&this.focus_handle, cx)),
            )
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::surface_base())
            .child(onglet_bar(&tabs, active_id, cx))
            .when(self.search_open, |d| {
                d.child(search_row(self, &matches, cx))
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .child(control_strip(active.as_ref(), cx))
                    .child(log_body(
                        active.as_ref(),
                        &self.scroll,
                        &matches,
                        current,
                        cx,
                    )),
            )
    }
}

/// The Run window's own tab bar: one onglet per run target (lamp + label + ✕).
fn onglet_bar(
    tabs: &[RunTab],
    active_id: Option<u64>,
    cx: &mut Context<RunConsolePanel>,
) -> impl IntoElement {
    let mut bar = div()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .h(px(28.))
        .px_1()
        .bg(theme::surface_sunken())
        .border_b_1()
        .border_color(theme::border_subtle())
        .text_size(theme::text_xs());

    if tabs.is_empty() {
        bar = bar.child(
            div()
                .px_2()
                .text_color(theme::text_muted())
                .child("Run — no targets launched yet"),
        );
    }
    for tab in tabs {
        bar = bar.child(run_onglet(tab, active_id == Some(tab.id), cx));
    }
    // Far right: the uniform tool-window hide ✕ (collapses the bottom dock).
    bar.child(div().flex_1()).child(super::tool_hide_button(
        "run-console-hide",
        crate::views::chrome_requests::ChromeRequest::HideBottomDock,
    ))
}

/// One run onglet: status lamp + target label + a hover-revealed ✕ (kills the run
/// and closes the tab). Active = raised + accent text, like the space tabs.
fn run_onglet(
    tab: &RunTab,
    is_active: bool,
    cx: &mut Context<RunConsolePanel>,
) -> impl IntoElement {
    let id = tab.id;
    let lamp = RunConsolePanel::status_color(&tab.status);
    let group: SharedString = format!("run-onglet-grp-{id}").into();
    let mut onglet = div()
        .id(SharedString::from(format!("run-onglet-{id}")))
        .group(group.clone())
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .px_2()
        .py(px(2.))
        .rounded(theme::radius_sm())
        .cursor_pointer()
        .max_w(px(180.))
        .overflow_hidden();
    onglet = if is_active {
        onglet
            .bg(theme::surface_raised())
            .text_color(theme::accent())
    } else {
        onglet
            .text_color(theme::text_muted())
            .hover(|d| d.bg(theme::row_hover()).text_color(theme::text_secondary()))
    };
    onglet
        .child(
            div()
                .w(px(6.))
                .h(px(6.))
                .flex_none()
                .rounded_full()
                .bg(lamp)
                .when(tab.status.is_running(), |d| d.shadow(theme::glow(lamp))),
        )
        .child(div().overflow_hidden().child(tab.label.clone()))
        .child(
            // ✕ — kill + close this onglet; hidden at rest, shown on hover/active.
            div()
                .id(SharedString::from(format!("run-onglet-close-{id}")))
                .px(px(2.))
                .text_color(theme::text_muted())
                .when(!is_active, |d| {
                    d.opacity(0.).group_hover(group, |s| s.opacity(1.))
                })
                .hover(|d| d.text_color(theme::text_primary()))
                .child("✕")
                .on_click(cx.listener(move |this, _ev, _w, cx| {
                    cx.stop_propagation();
                    this.registry.remove(id);
                    if this.active == Some(id) {
                        this.active = this.registry.last_started();
                    }
                    cx.notify();
                })),
        )
        .on_click(cx.listener(move |this, _ev, _w, cx| {
            this.active = Some(id);
            cx.notify();
        }))
}

/// The search row (Cmd+F / 🔍): input + `k/n` count + ‹ › navigation + ✕.
fn search_row(
    panel: &RunConsolePanel,
    matches: &[usize],
    cx: &mut Context<RunConsolePanel>,
) -> impl IntoElement {
    let count = if panel.query.is_empty() {
        String::new()
    } else if matches.is_empty() {
        "0 results".to_string()
    } else {
        format!(
            "{}/{}",
            panel.current_match.min(matches.len() - 1) + 1,
            matches.len()
        )
    };
    div()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .h(px(30.))
        .px_2()
        .bg(theme::surface_sunken())
        .border_b_1()
        .border_color(theme::border_subtle())
        .text_size(theme::text_xs())
        .child(div().text_color(theme::text_muted()).child("⌕"))
        .child(div().w(px(220.)).child(Input::new(&panel.search).small()))
        .child(div().text_color(theme::text_muted()).child(count))
        .child(strip_btn(
            "run-search-prev",
            "‹",
            "Previous match (Shift+Enter)",
            cx.listener(|this, _ev, _w, cx| this.step_match(false, cx)),
        ))
        .child(strip_btn(
            "run-search-next",
            "›",
            "Next match (Enter)",
            cx.listener(|this, _ev, _w, cx| this.step_match(true, cx)),
        ))
        .child(div().flex_1())
        .child(strip_btn(
            "run-search-close",
            "✕",
            "Close search (Esc)",
            cx.listener(|this, _ev, window, cx| this.on_close_search(&CloseSearch, window, cx)),
        ))
}

/// The left vertical control strip (JetBrains run-console style): ↻ Restart,
/// ⏹ Stop (live runs), ⌫ Clear, then scroll ⤒/⤓, then 🔍.
fn control_strip(
    active: Option<&RunSnapshot>,
    cx: &mut Context<RunConsolePanel>,
) -> impl IntoElement {
    let mut strip = div()
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .gap_1()
        .w(px(26.))
        .py_1()
        .bg(theme::surface_sunken())
        .border_r_1()
        .border_color(theme::border_subtle());
    let Some(active) = active else {
        return strip;
    };
    let id = active.id;
    let running = active.status.is_running();

    strip = strip.child(strip_btn(
        "run-restart",
        "↻",
        "Restart",
        cx.listener(|this, _ev, _w, cx| this.restart(cx)),
    ));
    if running {
        strip = strip.child(strip_btn(
            "run-stop",
            "⏹",
            "Stop",
            cx.listener(move |this, _ev, _w, cx| {
                this.registry.stop(id);
                cx.notify();
            }),
        ));
    }
    strip = strip
        .child(strip_btn(
            "run-clear",
            "⌫",
            "Clear output",
            cx.listener(move |this, _ev, _w, cx| {
                this.registry.clear_logs(id);
                cx.notify();
            }),
        ))
        .child(strip_divider())
        .child(strip_btn(
            "run-scroll-top",
            "⤒",
            "Scroll to top",
            cx.listener(|this, _ev, _w, cx| {
                this.follow = false;
                this.scroll.set_offset(gpui::Point::default());
                cx.notify();
            }),
        ))
        .child(strip_btn(
            "run-scroll-end",
            "⤓",
            "Scroll to end (follow)",
            cx.listener(|this, _ev, _w, cx| {
                this.follow = true;
                this.scroll.scroll_to_bottom();
                cx.notify();
            }),
        ))
        .child(strip_divider())
        .child(strip_btn(
            "run-search",
            "⌕",
            "Search (Cmd+F)",
            cx.listener(|this, _ev, window, cx| this.on_toggle_search(&ToggleSearch, window, cx)),
        ));
    strip
}

/// The scrolling log body of the active run: stdout in secondary, stderr tinted
/// error-red; search matches tinted, the current one stronger. Lines never wrap
/// (both-axis scroll) and are exactly [`LINE_H`] tall — search jumps stay exact.
fn log_body(
    active: Option<&RunSnapshot>,
    scroll: &ScrollHandle,
    matches: &[usize],
    current: Option<usize>,
    cx: &mut Context<RunConsolePanel>,
) -> impl IntoElement {
    let err_color = theme::status_color(SessionStatus::Errored);
    let mut body = div()
        .id("run-log-body")
        .flex_1()
        .min_w_0()
        .overflow_scroll()
        .track_scroll(scroll)
        .px_2()
        .py_1()
        .font_family(theme::mono_font())
        .text_size(theme::text_sm())
        .line_height(px(LINE_H))
        // Free scrolling: an upward wheel disengages tail-follow (⤓ re-engages).
        .on_scroll_wheel(cx.listener(|this, ev: &gpui::ScrollWheelEvent, _w, cx| {
            if ev.delta.pixel_delta(px(LINE_H)).y > px(0.) && this.follow {
                this.follow = false;
                cx.notify();
            }
        }));
    let Some(active) = active else {
        return body.child(
            div()
                .pt_4()
                .flex()
                .flex_col()
                .items_center()
                .gap_1()
                .text_color(theme::text_muted())
                .child(
                    div()
                        .text_color(theme::tint(theme::accent(), 0.6))
                        .child("▶"),
                )
                .child("Launch a target from the toolbar — each run gets its own tab here"),
        );
    };
    if active.logs.is_empty() {
        body = body.child(div().pt_2().text_color(theme::text_muted()).child(format!(
            "`{}` — {} (no output yet)",
            active.command,
            active.status.label()
        )));
    } else {
        body = body.children(active.logs.iter().enumerate().map(|(i, line)| {
            let mut row = div()
                .h(px(LINE_H))
                .whitespace_nowrap()
                .text_color(if line.stderr {
                    theme::tint(err_color, 0.85)
                } else {
                    theme::text_secondary()
                })
                .child(line.text.clone());
            if current == Some(i) {
                row = row.bg(theme::tint(theme::accent(), 0.25));
            } else if matches.binary_search(&i).is_ok() {
                row = row.bg(theme::tint(theme::accent(), 0.10));
            }
            row
        }));
    }
    body
}

/// A small square ghost control (control strip + search row).
fn strip_btn(
    id: &'static str,
    glyph: &'static str,
    label: &'static str,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .w(px(20.))
        .h(px(20.))
        .rounded(theme::radius_sm())
        .cursor_pointer()
        .text_color(theme::text_muted())
        .hover(|d| d.bg(theme::row_hover()).text_color(theme::text_primary()))
        .child(glyph)
        .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
        .on_click(on_click)
}

fn strip_divider() -> gpui::Div {
    div()
        .my(px(2.))
        .w(px(14.))
        .h(px(1.))
        .bg(theme::border_subtle())
}
