//! The bottom **status bar** — the "moonlight noir" instrument strip pinned under
//! the dock, surfacing the frontmost file/session context, the active project +
//! theme, the Claude-observability cluster, and a notifications affordance.
//!
//! ## Design: a phosphor cockpit on the night floor
//!
//! The bar sits on [`theme::surface_void`] — one elevation step *below* the sunken
//! gutter — separated from the dock by a 1px "moonrise" hairline (a gradient that
//! brightens toward the center, like a horizon line). Everything live is rendered
//! as **light, not just hue**: status dots, gauge fills and the unread bell carry a
//! static [`theme::glow`] bloom. Quota/context usage render as real micro-gauges
//! (3px tracks with glowing fills) instead of ASCII bars, and every glyph is a
//! monochrome geometric character tinted via tokens — no color-bitmap emoji.
//!
//! Like [`space_tab_bar`](super::spaces::space_tab_bar) this is **window chrome, not a
//! dock [`Panel`](gpui_component::dock::Panel)**: it is rendered by [`Workspace`] as
//! the last child of its root `v_flex`, so it survives every layout/space switch and
//! needs no `DOCK_VERSION` bump. It is a pure render fn over a `'static`
//! [`StatusSnapshot`] the workspace builds each frame from the shared
//! [`ActiveContext`](crate::views::active_context::ActiveContext), the
//! [`ProjectSpace`](crate::views::project_space::ProjectSpace), and (later) the
//! Claude-obs read-model — keeping its click handlers `'static`.
//!
//! The Claude-obs cluster (model, quotas, ctx tokens, session time, skills, subagents,
//! persona) renders `—` placeholders today: that data does not yet exist in the
//! engine (see the status-bar plan, Phase 3). The notification button is a stub.

use gpui::prelude::*;
use gpui::{div, linear_color_stop, linear_gradient, px, Context, Hsla};

use moonlight_domain::ids::SessionId;
use moonlight_domain::phase::Phase;
use moonlight_domain::session::SessionStatus;

use crate::views::active_context::Eol;
use crate::views::notifications::NotificationKind;
use crate::views::theme;
use crate::views::workspace::Workspace;

/// Height of the bar — a touch taller than the top tab bar so the obs cluster reads
/// comfortably on one line.
const BAR_HEIGHT: f32 = 30.;

/// Width of a micro-gauge track (the quota / ctx instruments).
const GAUGE_W: f32 = 30.;

/// The context-sensitive left zone: a file's path + caret/metadata, a session's
/// name : state · phase, or nothing when the fleet grid is frontmost.
pub enum LeftZone {
    Empty,
    File {
        rel_path: String,
        caret: String,
        eol: Eol,
        encoding: &'static str,
        indent: String,
    },
    Session {
        label: String,
        status: SessionStatus,
        phase: Phase,
    },
}

/// One render-ready notification row (icon + color resolved from its kind).
pub struct NotifRow {
    pub id: u64,
    pub icon: &'static str,
    pub color: Hsla,
    pub text: String,
    pub read: bool,
    /// The session this notification concerns, for click-to-navigate.
    pub session: Option<SessionId>,
}

/// Render-ready per-session Claude-obs (the bar's right zone). Mirrors the default OMC
/// HUD stats (no financial figures): model, output style, session time, context %.
pub struct ObsView {
    pub model: String,
    pub time: String,
    /// Context-window usage %, when known (raw tokens ÷ the model's window).
    pub ctx_pct: Option<u8>,
    pub persona: String,
    /// Cumulative tokens consumed this session (AGY pay-as-you-go readout); `0` hides it.
    pub tokens: u64,
}

/// The account usage quota — shown account-wide (independent of the selected session).
#[derive(Default)]
pub struct QuotaView {
    pub five_h: Option<u8>,
    /// Compact countdown to the 5h-window reset (e.g. `1h47m`), when known — the
    /// approximate time until the rolling 5-hour split refills.
    pub five_h_reset: Option<String>,
    pub weekly: Option<u8>,
    pub sonnet: Option<u8>,
}

/// Everything the bar renders, snapshotted into owned `'static` values by
/// [`Workspace::render`] so the bar's listeners don't borrow the model.
pub struct StatusSnapshot {
    pub left: LeftZone,
    pub project: String,
    pub theme_name: &'static str,
    pub notifications_open: bool,
    pub notifications: Vec<NotifRow>,
    pub unread: usize,
    /// Account quota (5h / weekly / Sonnet-weekly), shown always.
    pub quota: QuotaView,
    /// Per-session obs for the selected session, or `None` when none is frontmost.
    pub obs: Option<ObsView>,
    /// Whether the frontmost session's backend feeds the Claude-observability cluster
    /// (model + account quota + ctx). `false` for an Antigravity (`agy`) session — those
    /// stats are Claude-specific and would be misleading, so the bar shows an "AGY"
    /// marker instead.
    pub obs_native: bool,
}

/// Map a notification kind to its glyph + color. All glyphs are monochrome text
/// presentation — color comes from the token, never from an emoji bitmap.
/// (Presentation lives here, not in the gpui-free
/// [`Notifications`](crate::views::notifications) store.)
pub fn kind_style(kind: NotificationKind) -> (&'static str, Hsla) {
    match kind {
        NotificationKind::Review => ("◎", theme::status_color(SessionStatus::WaitingInput)),
        NotificationKind::Approval => ("◈", theme::accent()),
        NotificationKind::Advance => ("≫", theme::status_color(SessionStatus::Running)),
        NotificationKind::Error => ("✕", theme::status_color(SessionStatus::Errored)),
        NotificationKind::Phase => ("↗", theme::accent()),
        NotificationKind::Input => ("◐", theme::status_color(SessionStatus::WaitingInput)),
        NotificationKind::Compact => ("⊜", theme::accent()),
    }
}

/// Render the bottom status bar from `snap`. `cx` is the workspace context so the
/// notifications button can toggle workspace state.
pub fn status_bar(snap: StatusSnapshot, cx: &mut Context<Workspace>) -> impl IntoElement {
    // Right cluster: the full Claude-observability cluster (account quota + model + ctx)
    // for a Claude / no-session bar; for an AGY session we render a custom agy_zone containing
    // the Gemini model, active session duration (computed from the transcript), and a subtle,
    // glowing "AGY" token marker to differentiate it from Claude sessions.
    let right_cluster = if snap.obs_native {
        obs_zone(snap.quota, snap.obs).into_any_element()
    } else {
        agy_zone(snap.obs).into_any_element()
    };
    div()
        .id("status-bar")
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .w_full()
        .h(px(BAR_HEIGHT))
        .px_2()
        .gap_2()
        .bg(theme::surface_void())
        .text_color(theme::text_muted())
        .text_size(theme::text_sm())
        // The moonrise hairline — replaces a flat border-top.
        .child(moon_hairline())
        // LEFT — the frontmost file/session context.
        .child(left_zone(snap.left, cx))
        // Spacer pushes project/theme/obs/notifications to the right edge.
        .child(div().flex_1())
        // CENTER-RIGHT — active project + color theme. The crescent is the one
        // brand moment in the bar, so it alone takes the accent.
        .child(meta_item("⬡", theme::text_muted(), snap.project))
        .child(meta_item("☾", theme::accent(), snap.theme_name.to_string()))
        .child(sep())
        // RIGHT — Claude obs cluster for a Claude/no session; AGY's model marker otherwise.
        .child(right_cluster)
        // FAR RIGHT — notifications.
        .child(bell(
            snap.unread,
            snap.notifications_open,
            snap.notifications,
            cx,
        ))
}

/// The 1px top hairline: transparent at the edges, moonlight at the center —
/// two mirrored gradients meeting in the middle (gpui gradients take two stops).
fn moon_hairline() -> gpui::Div {
    let dark = theme::tint(theme::accent(), 0.0);
    let lit = theme::tint(theme::accent(), 0.45);
    div()
        .absolute()
        .top_0()
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

/// The left zone, switching on what is frontmost.
fn left_zone(left: LeftZone, cx: &mut Context<Workspace>) -> gpui::Div {
    let row = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .overflow_hidden();
    match left {
        LeftZone::Empty => row.child(div().text_color(theme::text_muted()).child("◇ no file")),
        LeftZone::File {
            rel_path,
            caret,
            eol,
            encoding,
            indent,
        } => row
            .child(div().text_color(theme::tree_glyph()).child("◇"))
            .child(
                div()
                    .font_family(theme::mono_font())
                    .text_color(theme::text_secondary())
                    .max_w(px(440.))
                    .overflow_hidden()
                    .child(rel_path),
            )
            .child(sep())
            .child(item(caret))
            .child(eol_item(eol, cx))
            .child(item(encoding.to_string()))
            .child(item(indent)),
        LeftZone::Session {
            label,
            status,
            phase,
        } => row
            .child(status_dot(status))
            .child(
                div()
                    .text_color(theme::text_secondary())
                    .max_w(px(360.))
                    .overflow_hidden()
                    .child(label),
            )
            .child(
                div()
                    .text_size(theme::text_2xs())
                    .text_color(theme::tint(theme::status_color(status), 0.9))
                    .child(status_label(status).to_uppercase()),
            )
            .child(phase_chip(phase)),
    }
}

/// A 7px phosphor status dot — lit (glowing) when the session is in a state that
/// wants eyes on it; a quiet filled dot otherwise.
fn status_dot(status: SessionStatus) -> gpui::Div {
    let color = theme::status_color(status);
    let lit = matches!(
        status,
        SessionStatus::Running | SessionStatus::WaitingInput | SessionStatus::Errored
    );
    div()
        .w(px(7.))
        .h(px(7.))
        .flex_none()
        .rounded_full()
        .bg(color)
        .when(lit, |d| d.shadow(theme::glow(color)))
}

/// The Claude-obs cluster — the default-OMC-HUD stat set (no financial figures):
/// **5h/wk/sn quota gauges** (account-wide, always shown) · model · persona ·
/// session time · **context gauge** (for the selected session, else `—`).
fn obs_zone(quota: QuotaView, obs_view: Option<ObsView>) -> gpui::Div {
    let mut row = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        // Account quota — independent of which session is frontmost.
        .child(gauge("5h", quota.five_h));
    // The approximate reset of the rolling 5-hour window, beside its gauge.
    if let Some(reset) = quota.five_h_reset {
        row = row.child(reset_chip(&reset));
    }
    row = row
        .child(gauge("wk", quota.weekly))
        .child(gauge("sn", quota.sonnet))
        .child(sep());
    row = match obs_view {
        Some(o) => row
            .child(obs("⌬", o.model))
            .child(obs("✦", o.persona))
            .child(obs("◷", o.time))
            .child(gauge("ctx", o.ctx_pct)),
        None => row.child(obs("⌬", "—")).child(gauge("ctx", None)),
    };
    row
}

/// A custom, gorgeous observability cluster for Google Antigravity (Gemini) sessions.
/// Displays the active model, session duration, and a glowing AGY brand badge.
fn agy_zone(obs_view: Option<ObsView>) -> gpui::Div {
    let mut row = div().flex().flex_row().items_center().gap_3();

    // A subtle glowing "AGY" marker to show it's powered by Google Antigravity
    row = row.child(
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1_5()
            .child(
                div()
                    .w(px(6.))
                    .h(px(6.))
                    .rounded_full()
                    .bg(theme::accent())
                    .shadow(theme::glow(theme::accent())),
            )
            .child(
                div()
                    .text_size(theme::text_xs())
                    .text_color(theme::text_muted())
                    .child("AGY"),
            ),
    );

    row = row.child(sep());

    row = match obs_view {
        Some(o) => {
            let mut r = row.child(obs("⌬", o.model));
            if o.time != "0s" && !o.time.is_empty() && o.time != "—" {
                r = r.child(obs("◷", o.time));
            }
            // Context usage as a % gauge — same notation as Claude, against Gemini's 1M
            // window (see `obs::GEMINI_CONTEXT_WINDOW`). Beside it, the cumulative token
            // consumption (`Σ`, the pay-as-you-go readout); both AGY-only, from its
            // language-server RPC, hidden until a live/cached figure exists.
            r = r.child(gauge("ctx", o.ctx_pct));
            if o.tokens > 0 {
                r = r.child(obs("Σ", fmt_count(o.tokens)));
            }
            r
        }
        None => row.child(obs("⌬", "—")),
    };

    row
}

/// Compact a token count: `936 → "936"`, `124_446 → "124k"`, `1_180_465 → "1.18M"`.
fn fmt_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.2}M", n as f64 / 1_000_000.)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}

/// A micro-gauge instrument: tiny uppercase label, a 3px track whose fill glows in
/// the utilization color, and the percent value. `gauge("5h", None)` renders
/// `5H —` while the figure isn't available yet.
fn gauge(label: &str, pct: Option<u8>) -> gpui::Div {
    let row = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .child(tiny_label(label));
    match pct {
        Some(p) => {
            let color = quota_color(p);
            let fill_w = (GAUGE_W * f32::from(p.min(100)) / 100.).max(2.);
            row.child(
                div()
                    .w(px(GAUGE_W))
                    .h(px(3.))
                    .flex_none()
                    .rounded_full()
                    .bg(theme::surface_raised())
                    .child(
                        div()
                            .w(px(fill_w))
                            .h_full()
                            .rounded_full()
                            .bg(color)
                            .shadow(theme::glow(color)),
                    ),
            )
            .child(
                div()
                    .font_family(theme::mono_font())
                    .text_size(theme::text_xs())
                    .text_color(color)
                    .child(format!("{p}%")),
            )
        }
        None => row.child(div().text_color(theme::text_muted()).child("—")),
    }
}

/// The 5h-window reset countdown (`↻1h47m`) — a muted, mono chip beside the 5h gauge.
fn reset_chip(remaining: &str) -> gpui::Div {
    div()
        .font_family(theme::mono_font())
        .text_size(theme::text_2xs())
        .text_color(theme::text_muted())
        .child(format!("↻{remaining}"))
}

/// A tiny uppercase instrument label (the `5H` / `WK` / `CTX` captions).
fn tiny_label(label: &str) -> gpui::Div {
    div()
        .font_family(theme::mono_font())
        .text_size(theme::text_2xs())
        .text_color(theme::text_muted())
        .child(label.to_uppercase())
}

/// Quota color by utilization: calm under 70%, amber to 90%, red above.
fn quota_color(pct: u8) -> Hsla {
    if pct >= 90 {
        theme::status_color(SessionStatus::Errored)
    } else if pct >= 70 {
        theme::status_color(SessionStatus::WaitingInput)
    } else {
        theme::status_color(SessionStatus::Done)
    }
}

/// The notifications button (with an unread badge) + the inbox popover. The signal
/// glyph lights up — accent + glow — while anything is unread.
fn bell(unread: usize, open: bool, rows: Vec<NotifRow>, cx: &mut Context<Workspace>) -> gpui::Div {
    let lit = unread > 0;
    div()
        .relative()
        .child(
            div()
                .id("status-bell")
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .px_1()
                .cursor_pointer()
                .rounded(theme::radius_sm())
                .hover(|d| d.bg(theme::row_hover()))
                .child(if lit {
                    div()
                        .rounded_full()
                        .text_color(theme::accent())
                        .shadow(theme::glow(theme::accent()))
                        .child("◉")
                } else {
                    div().text_color(theme::text_muted()).child("○")
                })
                .when(lit, |d| {
                    d.child(
                        div()
                            .px_1()
                            .rounded_full()
                            .bg(theme::status_color(SessionStatus::Errored))
                            .text_color(theme::text_primary())
                            .text_size(theme::text_2xs())
                            .font_family(theme::mono_font())
                            .child(unread.to_string()),
                    )
                })
                .on_click(cx.listener(|this, _ev, _w, cx| this.toggle_notifications(cx))),
        )
        .when(open, |d| d.child(notif_popover(rows, cx)))
}

/// The inbox popover: a header (Mark all read / Clear) over a newest-first list.
/// Each row marks itself read on click; unread rows carry a phosphor tick in their
/// kind color. Positioned above the bell (bottom bar).
fn notif_popover(rows: Vec<NotifRow>, cx: &mut Context<Workspace>) -> gpui::Div {
    let mut list = div()
        .flex()
        .flex_col()
        .gap(px(1.))
        .p_1()
        .max_h(px(280.))
        .overflow_hidden();
    if rows.is_empty() {
        list = list.child(
            div()
                .p_2()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .text_color(theme::text_muted())
                .child(
                    div()
                        .text_color(theme::tint(theme::accent(), 0.6))
                        .child("☾"),
                )
                .child("All quiet"),
        );
    } else {
        for r in rows {
            let id = r.id;
            let session = r.session.clone();
            let text_color = if r.read {
                theme::text_muted()
            } else {
                theme::text_secondary()
            };
            let tick_color = if r.read {
                theme::tint(r.color, 0.0)
            } else {
                r.color
            };
            list = list.child(
                div()
                    .id(("notif", id as usize))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded(theme::radius_sm())
                    .cursor_pointer()
                    .hover(|d| d.bg(theme::row_hover()))
                    // The phosphor tick — lit in the kind color while unread,
                    // fully transparent (but space-keeping) once read.
                    .child(
                        div()
                            .w(px(2.))
                            .h(px(12.))
                            .flex_none()
                            .rounded_full()
                            .bg(tick_color),
                    )
                    .child(div().text_color(r.color).child(r.icon))
                    .child(div().flex_1().text_color(text_color).child(r.text))
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.open_notification(id, session.clone(), cx)
                    })),
            );
        }
    }
    div()
        .absolute()
        .bottom(px(26.))
        .right_0()
        .w(px(300.))
        .rounded(theme::radius_md())
        .overflow_hidden()
        .bg(theme::surface_overlay())
        .border_1()
        .border_color(theme::border_subtle())
        .shadow(theme::overlay_shadow())
        .text_size(theme::text_sm())
        // The popover carries its own moonrise hairline along the top edge.
        .child(
            div()
                .w_full()
                .h(px(1.))
                .flex()
                .flex_row()
                .child(div().flex_1().h_full().bg(linear_gradient(
                    90.,
                    linear_color_stop(theme::tint(theme::accent(), 0.0), 0.),
                    linear_color_stop(theme::tint(theme::accent(), 0.45), 1.),
                )))
                .child(div().flex_1().h_full().bg(linear_gradient(
                    90.,
                    linear_color_stop(theme::tint(theme::accent(), 0.45), 0.),
                    linear_color_stop(theme::tint(theme::accent(), 0.0), 1.),
                ))),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .px_2()
                .py_1()
                .border_b_1()
                .border_color(theme::border_subtle())
                .child(
                    div()
                        .text_size(theme::text_2xs())
                        .text_color(theme::text_secondary())
                        .child("NOTIFICATIONS"),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .child(
                            div()
                                .id("notif-mark-all")
                                .cursor_pointer()
                                .text_color(theme::text_muted())
                                .hover(|d| d.text_color(theme::accent()))
                                .child("Mark all read")
                                .on_click(cx.listener(|this, _ev, _w, cx| {
                                    this.mark_all_notifications_read(cx)
                                })),
                        )
                        .child(
                            div()
                                .id("notif-clear")
                                .cursor_pointer()
                                .text_color(theme::text_muted())
                                .hover(|d| d.text_color(theme::accent()))
                                .child("Clear")
                                .on_click(
                                    cx.listener(|this, _ev, _w, cx| this.clear_notifications(cx)),
                                ),
                        ),
                ),
        )
        .child(list)
}

/// A muted metadata pill (caret / encoding / indent) — mono, code-adjacent.
fn item(text: impl Into<String>) -> gpui::Div {
    div()
        .font_family(theme::mono_font())
        .text_size(theme::text_xs())
        .text_color(theme::text_muted())
        .child(text.into())
}

/// The EOL pill — clickable: toggles the active editor's line-ending (LF ↔ CRLF) via
/// the editor-command channel. The change applies on the next ⌘S (save-time attribute).
fn eol_item(eol: Eol, cx: &mut Context<Workspace>) -> gpui::Stateful<gpui::Div> {
    div()
        .id("status-eol")
        .px_1()
        .rounded(theme::radius_sm())
        .cursor_pointer()
        .font_family(theme::mono_font())
        .text_size(theme::text_xs())
        .text_color(theme::text_muted())
        .hover(|d| d.bg(theme::row_hover()).text_color(theme::text_secondary()))
        .child(eol.label().to_string())
        .on_click(cx.listener(move |this, _ev, _w, cx| this.set_editor_eol(eol.toggled(), cx)))
}

/// A thin vertical separator between zones — a real 1px rule, not a glyph.
fn sep() -> gpui::Div {
    div()
        .w(px(1.))
        .h(px(12.))
        .flex_none()
        .bg(theme::border_subtle())
}

/// An icon + value pair for the project / theme center items. The glyph color is
/// caller-chosen so the theme crescent can carry the accent.
fn meta_item(icon: &str, icon_color: Hsla, value: String) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .child(div().text_color(icon_color).child(icon.to_string()))
        .child(div().text_color(theme::text_secondary()).child(value))
}

/// An icon + mono value pair for an obs metric.
fn obs(icon: &str, value: impl Into<String>) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .child(
            div()
                .text_color(theme::text_muted())
                .child(icon.to_string()),
        )
        .child(
            div()
                .font_family(theme::mono_font())
                .text_size(theme::text_xs())
                .text_color(theme::text_secondary())
                .child(value.into()),
        )
}

/// A phase-colored capsule naming the session's workflow phase — tinted wash
/// behind the phase color so the chip reads as a lit instrument, not a label.
fn phase_chip(phase: Phase) -> gpui::Div {
    let color = theme::phase_color(phase);
    div()
        .px_1()
        .rounded_full()
        .bg(theme::tint(color, 0.12))
        .text_size(theme::text_2xs())
        .text_color(color)
        .child(phase.label().to_uppercase())
}

/// Human label for a session status (the phosphor dot carries the color).
fn status_label(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Running => "Running",
        SessionStatus::WaitingInput => "Waiting",
        SessionStatus::Done => "Done",
        SessionStatus::Errored => "Errored",
        SessionStatus::Idle => "Idle",
        SessionStatus::Paused => "Paused",
    }
}
