//! The session tile — the signature glanceable unit of the grid.
//!
//! Conveys status (badge + traffic-light color + accent rail), phase (chip), and
//! mode at a glance, across many small tiles. Stateless: renders a `&Session`
//! read-model. The visual hierarchy is deliberate — a weighted title leads, the
//! phase/mode meta is quiet, and the path sits at the foot in mono so it reads as
//! an address rather than prose.

use gpui::prelude::*;
use gpui::{div, px, FontWeight, Hsla};
use moonlight_domain::agent::AgentKind;
use moonlight_domain::session::{AttentionKind, Session};

use super::session_meta::SessionMeta;
use super::theme;

/// Render one session as a tile element. `meta` carries the operator's custom name
/// and color (default when unset). `attention` is the louder ⚠ overlay (the agent
/// self-reported `Stuck`, or the IDE detected an `Incomplete`/`Errored` end) — drawn
/// only for the "did not finish correctly" cases (a plain NeedsInput is already the
/// status badge).
pub fn session_tile(
    s: &Session,
    meta: &SessionMeta,
    attention: Option<AttentionKind>,
    agent: AgentKind,
) -> impl IntoElement {
    let status = s.status;
    let scolor = theme::status_color(status);
    let warn = attention.filter(|a| a.is_warning());
    // Operator overrides: custom name wins over the CC title; the color shows as a
    // small dot beside the title (kept distinct from the status traffic-light rail).
    let name = meta
        .display_name()
        .map(str::to_string)
        .unwrap_or_else(|| s.label().to_string());
    let dot = meta.palette_color().map(|c| c.hsla());

    div()
        .flex()
        .flex_row()
        .w(px(256.))
        .h(px(90.))
        .rounded(theme::radius_md())
        .overflow_hidden()
        .bg(theme::surface_raised())
        .border_1()
        .border_color(theme::border_subtle())
        // Lift on hover — the whole tile is clickable in the grid.
        .hover(|d| {
            d.bg(theme::surface_overlay())
                .border_color(theme::border_strong())
        })
        // Status accent rail — the "border" leg of badge+color+border.
        .child(div().w(px(3.)).h_full().bg(scolor))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .justify_between()
                .overflow_hidden()
                .px_3()
                .py(px(10.))
                // Title + meta, grouped at the top so the tile reads as one block.
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(7.))
                        // Title row: status badge + weighted title. Right padding
                        // reserves room for the absolute "adopt" overlay (grid_home)
                        // so long titles truncate before colliding with it.
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(7.))
                                // Clear the absolute "adopt"/"governed" overlay (grid_home).
                                .pr(px(66.))
                                .child(
                                    div()
                                        .text_color(scolor)
                                        .text_size(theme::text_sm())
                                        .child(status.badge().to_string()),
                                )
                                // ⚠ attention overlay — the loud "did not complete / is
                                // stuck" signal layered over the resting status badge.
                                .children(warn.map(|a| {
                                    div()
                                        .flex_shrink_0()
                                        .text_color(theme::attention_color(a))
                                        .text_size(theme::text_sm())
                                        .child(a.glyph().to_string())
                                }))
                                // Custom-color dot (only when the operator set one).
                                .children(dot.map(|fill| {
                                    div().size(px(8.)).rounded_full().bg(fill).flex_shrink_0()
                                }))
                                .child(
                                    // Single line, ellipsized — never wraps (a wrapped
                                    // title would push the path off a short tile).
                                    div()
                                        .flex_1()
                                        .truncate()
                                        .text_color(theme::text_primary())
                                        .text_size(theme::text_md())
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(name),
                                ),
                        )
                        // Phase chip + mode.
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .child(chip(s.phase.label(), theme::phase_color(s.phase)))
                                // Backend badge — only for non-Claude (Claude is the
                                // implicit default, kept un-badged to avoid clutter).
                                .children(
                                    (agent != AgentKind::ClaudeCode)
                                        .then(|| chip("AGY", theme::accent())),
                                )
                                .child(
                                    div()
                                        .text_color(theme::text_muted())
                                        .text_size(theme::text_sm())
                                        // Derived from phase so it never drifts from the real mode.
                                        .child(s.phase.mode_label()),
                                ),
                        ),
                )
                // Attached path (home-abbreviated, clipped) in mono, anchored at the
                // foot so it reads as the tile's address.
                .child(
                    div()
                        .truncate()
                        .font_family(theme::mono_font())
                        .text_color(theme::text_muted())
                        .text_size(theme::text_xs())
                        .child(
                            s.attached_path
                                .as_deref()
                                .map(abbreviate_home)
                                .unwrap_or_else(|| "—".to_string()),
                        ),
                ),
        )
}

/// Replace a leading `$HOME` with `~` for a compact, readable path.
fn abbreviate_home(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => match path.strip_prefix(&home) {
            Some(rest) => format!("~{rest}"),
            None => path.to_string(),
        },
        _ => path.to_string(),
    }
}

/// A small rounded pill: colored text on a translucent tint of the same color.
fn chip(label: &str, color: Hsla) -> impl IntoElement {
    div()
        .px(px(7.))
        .py(px(2.))
        .rounded(theme::radius_sm())
        .bg(theme::tint(color, 0.16))
        .text_color(color)
        .text_size(theme::text_xs())
        .font_weight(FontWeight::MEDIUM)
        .child(label.to_string())
}
