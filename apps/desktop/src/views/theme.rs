//! MoonlightCode design tokens — the single source of truth for the look.
//!
//! ## Direction: a calm, refined dark cockpit ("Moonlight")
//!
//! Cool deep-slate surfaces with **deliberate elevation steps** (sunken → base →
//! raised → overlay) give depth without borders shouting. Text is a clear
//! three-step neutral hierarchy tuned for readability on the dark field. The only
//! saturated colors are the **status traffic-lights** and **phase accents** — so a
//! session that needs you genuinely pops against an otherwise quiet surface. One
//! soft moonlight-blue accent carries interaction (selection, focus, primary
//! actions). Nothing hardcodes a color or a size: every surface reads a token here,
//! and spacing/size come from the scale below (no scattered `px(11.)` magic).
//!
//! [`install`] pushes this same palette into `gpui-component`'s global `Theme`, so
//! the chrome it draws (dock tab strips, title bar, scrollbars, the code editor,
//! popovers) matches the hand-painted panels instead of falling back to its default
//! light theme.

use std::sync::OnceLock;

use gpui::{point, px, rems, rgb, App, BoxShadow, Hsla, Pixels};
use gpui_component::highlighter::HighlightTheme;
use gpui_component::text::TextViewStyle;
use moonlight_domain::phase::Phase;
use moonlight_domain::session::{AttentionKind, SessionStatus};

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

// ── Surfaces — a cool dark field with four deliberate elevation steps ────────
// Each step is a small, even lift in lightness so stacked surfaces read as depth
// rather than as boxes. `sunken` is the window/gutter backdrop; `base` is a panel;
// `raised` is a card/tile on a panel; `overlay` is a popover / hovered-raised row.

/// Deepest backdrop — window chrome, tab-strip gutter, behind everything.
pub fn surface_sunken() -> Hsla {
    c(0x0f1116)
}
/// The night floor — one step *below* sunken. Reserved for the status bar, so the
/// instrument cluster reads as the darkest, calmest strip of the window.
pub fn surface_void() -> Hsla {
    c(0x0b0d12)
}
/// Default panel surface.
pub fn surface_base() -> Hsla {
    c(0x15171d)
}
/// A card/tile sitting on a panel (session tiles, inset boxes).
pub fn surface_raised() -> Hsla {
    c(0x1d2027)
}
/// Floating surface — popovers, menus, the lifted state of a raised element.
pub fn surface_overlay() -> Hsla {
    c(0x252934)
}

/// Hairline separator between regions (low contrast by design).
pub fn border_subtle() -> Hsla {
    c(0x2b303a)
}
/// A more present border — focused inset, scrollbar thumb, emphasis edges.
pub fn border_strong() -> Hsla {
    c(0x3a404d)
}

// ── Text — a three-step neutral hierarchy, lifted for legible dark-mode text ──

/// Primary reading text and titles.
pub fn text_primary() -> Hsla {
    c(0xedeff3)
}
/// Supporting text — values, secondary labels (clearly readable, not shouting).
pub fn text_secondary() -> Hsla {
    c(0xb7bdc9)
}
/// Muted text — metadata, captions, inactive labels. Kept above the contrast
/// floor so paths and hints stay readable, not ghosted.
pub fn text_muted() -> Hsla {
    c(0x868d9c)
}

// ── Accent — one soft moonlight-blue, carrying interaction ───────────────────

/// The interaction accent (selection, focus ring, primary action).
pub fn accent() -> Hsla {
    c(0x7d90f0)
}
/// Brighter accent for hover/active.
pub fn accent_hover() -> Hsla {
    c(0x94a4f5)
}
/// Readable text/icon color when placed *on* a filled accent surface.
pub fn on_accent() -> Hsla {
    c(0x0f1116)
}

/// Translucent tint of a token color, for chip/badge/selection backgrounds.
pub fn tint(color: Hsla, alpha: f32) -> Hsla {
    Hsla { a: alpha, ..color }
}

/// A phosphor glow — a soft, offset-free bloom in `color` around the element.
/// Used for "lit" instruments (live status dots, gauge fills, the unread bell)
/// so active state reads as light, not just hue. Static by design: no pulse
/// animation, an always-visible bar must not force continuous repaints.
pub fn glow(color: Hsla) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: tint(color, 0.55),
        offset: point(px(0.), px(0.)),
        blur_radius: px(6.),
        spread_radius: px(0.),
        inset: false,
    }]
}

/// The deep drop shadow under floating noir surfaces (the notifications popover).
pub fn overlay_shadow() -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: tint(c(0x000000), 0.55),
        offset: point(px(0.), px(4.)),
        blur_radius: px(20.),
        spread_radius: px(0.),
        inset: false,
    }]
}

/// An opaque surface nudged `amount` (0..1) toward `accent` — adopts the accent's
/// hue and a fraction of its saturation/lightness while staying on `base`. Used to
/// wash a session's panel/header in its chosen color without losing legibility.
pub fn blend(base: Hsla, accent: Hsla, amount: f32) -> Hsla {
    Hsla {
        h: accent.h,
        s: base.s + (accent.s - base.s) * amount,
        l: base.l + (accent.l - base.l) * amount,
        a: 1.0,
    }
}

// ── Type scale (px) — name the sizes so views stop hardcoding magic numbers ──

/// 10px — dense overlays (adopt chip, tiny captions).
pub fn text_2xs() -> Pixels {
    px(10.)
}
/// 11px — captions, status letters, tree icons.
pub fn text_xs() -> Pixels {
    px(11.)
}
/// 12px — secondary labels, chips, metadata.
pub fn text_sm() -> Pixels {
    px(12.)
}
/// 13px — default body / row text.
pub fn text_base() -> Pixels {
    px(13.)
}
/// 14px — tile titles, emphasized rows.
pub fn text_md() -> Pixels {
    px(14.)
}
/// 15px — panel headers.
pub fn text_lg() -> Pixels {
    px(15.)
}
/// 17px — the product wordmark / hero header.
pub fn text_xl() -> Pixels {
    px(17.)
}

// ── Radius scale (px) ────────────────────────────────────────────────────────

/// 5px — chips, pills, small controls.
pub fn radius_sm() -> Pixels {
    px(5.)
}
/// 8px — cards, tiles, inset panels.
pub fn radius_md() -> Pixels {
    px(8.)
}
/// 11px — large containers, dialogs.
pub fn radius_lg() -> Pixels {
    px(11.)
}

// ── Fonts ────────────────────────────────────────────────────────────────────

/// UI font — the native system font (SF Pro on macOS): the refined, correct
/// choice for native IDE chrome. Set once on the gpui-component theme.
pub fn ui_font() -> &'static str {
    ".SystemUIFont"
}
/// Monospace candidates, best first, per platform. The first one the system
/// actually has wins — asking for a font that isn't installed silently lands on a
/// proportional fallback, which makes a terminal grid stop aligning.
#[cfg(target_os = "macos")]
const MONO_CANDIDATES: &[&str] = &["Menlo", "SF Mono", "Monaco", "Courier New"];
#[cfg(target_os = "windows")]
const MONO_CANDIDATES: &[&str] = &["Cascadia Mono", "Consolas", "Courier New"];
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const MONO_CANDIDATES: &[&str] = &[
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Noto Sans Mono",
    "Ubuntu Mono",
    "monospace",
];

/// Resolved once at [`install`]; `mono_font` reads it thereafter.
static MONO_FONT: OnceLock<String> = OnceLock::new();

/// Monospace font for code-adjacent UI (paths, session ids, the terminal grid).
///
/// Resolved against the fonts actually present (see [`MONO_CANDIDATES`]). Before
/// [`install`] runs — and if none of the candidates exist — this is the platform's
/// first choice, which is what the generic `"monospace"` alias is for on Linux.
pub fn mono_font() -> &'static str {
    MONO_FONT
        .get()
        .map(String::as_str)
        .unwrap_or(MONO_CANDIDATES[0])
}

/// Pick the monospace family from what the text system reports as installed.
fn resolve_mono_font(cx: &App) {
    let available = cx.text_system().all_font_names();
    let picked = MONO_CANDIDATES
        .iter()
        .find(|name| available.iter().any(|have| have == *name))
        .copied()
        // Nothing matched: keep the last candidate (a generic alias on Linux) and
        // let the text system do what it can.
        .unwrap_or_else(|| MONO_CANDIDATES[MONO_CANDIDATES.len() - 1]);
    if picked != MONO_CANDIDATES[0] {
        tracing::info!(font = picked, "monospace font resolved to a fallback");
    }
    let _ = MONO_FONT.set(picked.to_string());
}

// ── Markdown rendering (plan view, assistant messages) ───────────────────────

/// A **dark** [`TextViewStyle`] for rendering markdown on MoonlightCode surfaces.
///
/// `gpui-component`'s default `TextViewStyle` ships a *light* `HighlightTheme`, so
/// fenced code blocks render with light syntax colors on our dark cards — washed
/// out and hard to read (this is the long-standing "T14" plan-render bug). This
/// swaps in the dark highlight theme, marks the style dark, and applies a modest
/// heading ramp so `TextView::markdown(id, md).style(theme::markdown_style())`
/// reads as a proper document on the cockpit's dark field. Used by the plan-review
/// panel and the assistant-message view.
pub fn markdown_style() -> TextViewStyle {
    TextViewStyle {
        is_dark: true,
        highlight_theme: HighlightTheme::default_dark(),
        ..Default::default()
    }
    .paragraph_gap(rems(0.75))
    .heading_font_size(|level, base| match level {
        1 => base * 1.4,
        2 => base * 1.2,
        3 => base * 1.05,
        _ => base,
    })
}

// ── List / tree row chrome ───────────────────────────────────────────────────

/// Selected row fill (a faint accent wash).
pub fn row_selected() -> Hsla {
    tint(accent(), 0.16)
}
/// Hovered row fill.
pub fn row_hover() -> Hsla {
    c(0x222630)
}
/// Muted glyph for tree disclosure triangles + folder/file icons.
pub fn tree_glyph() -> Hsla {
    c(0x7d8593)
}

/// Traffic-light status color (paired with the status badge glyph + tile border).
pub fn status_color(s: SessionStatus) -> Hsla {
    match s {
        SessionStatus::Running => c(0x4da3ff),
        SessionStatus::WaitingInput => c(0xf5a623),
        SessionStatus::Done => c(0x46c46a),
        SessionStatus::Errored => c(0xe5484d),
        SessionStatus::Idle => c(0x6b7280),
        SessionStatus::Paused => c(0x9aa0aa),
    }
}

/// Colour for a session **attention** signal (the ⚠ overlay + the space-tab dot): a
/// "did not finish correctly" alert (Incomplete/Errored) burns the error red, a "needs
/// you" pause (Stuck/NeedsInput) the waiting amber.
pub fn attention_color(k: AttentionKind) -> Hsla {
    match k {
        AttentionKind::Errored | AttentionKind::Incomplete => status_color(SessionStatus::Errored),
        AttentionKind::Stuck | AttentionKind::NeedsInput => {
            status_color(SessionStatus::WaitingInput)
        }
    }
}

/// Phase accent — the tile's appearance teaches the workflow state machine.
pub fn phase_color(p: Phase) -> Hsla {
    match p {
        Phase::Plan => c(0x6e8bff),
        Phase::AutoImplement => c(0x4da3ff),
        Phase::Test => c(0xc792ea),
        Phase::Review => c(0xf5a623),
        Phase::Commit => c(0x46c46a),
    }
}

// ── Git status decoration (file-tree VCS colors) ───────────────────────────
// Pushed toward the established status hues so "changed" reads consistently with
// the traffic-light system, without colliding with the accent.

pub fn git_modified() -> Hsla {
    c(0x4da3ff) // blue — tracked & changed
}
pub fn git_added() -> Hsla {
    c(0x46c46a) // green — staged addition
}
pub fn git_untracked() -> Hsla {
    c(0x6fae5f) // muted green — new, not yet tracked
}
pub fn git_deleted() -> Hsla {
    c(0xe5534d) // red — removed
}
pub fn git_conflict() -> Hsla {
    c(0xf5a623) // amber — needs resolution
}

// ── Diff — the review surface's two panes ────────────────────────────────────
// Hue lives in the *row wash* and the gutter glyph, never in the code text: a
// line tinted green on green is harder to read than the line it replaced, and the
// point of a review pane is reading code. Changed lines instead get the brighter
// text step, so emphasis comes from contrast and the color says only "what kind
// of change".

/// Wash behind an added line.
pub fn diff_added_bg() -> Hsla {
    tint(git_added(), 0.13)
}
/// Wash behind a removed line.
pub fn diff_removed_bg() -> Hsla {
    tint(git_deleted(), 0.13)
}
/// Wash behind a line that was rewritten in place (the after side of a replace).
pub fn diff_modified_bg() -> Hsla {
    tint(git_modified(), 0.11)
}
/// The void opposite an inserted or removed line. Reads as "nothing here" rather
/// than as content, which is what keeps the two panes legible as one alignment.
pub fn diff_gap_bg() -> Hsla {
    tint(text_muted(), 0.05)
}

// ── Terminal palette ────────────────────────────────────────────────────────
//
// A real ANSI terminal needs the 256-color model: 16 named colors, a 6×6×6
// color cube (16..232), and a grayscale ramp (232..256). The 16 base colors are
// a calm, legible set tuned to the dark surface; cube/ramp are computed.

/// Terminal background — slightly deeper than the panel surface for contrast.
pub fn terminal_bg() -> Hsla {
    c(0x101216)
}
/// Default terminal foreground (when a cell uses the default fg color).
pub fn terminal_fg() -> Hsla {
    c(0xd6d9e0)
}
/// Block cursor fill (drawn under the cell glyph).
pub fn terminal_cursor() -> Hsla {
    accent()
}
/// Background for mouse-selected terminal cells (matches the global selection tint).
pub fn terminal_selection_bg() -> Hsla {
    tint(accent(), 0.30)
}
/// Foreground for recognized/clickable links in terminal output.
pub fn terminal_link() -> Hsla {
    accent_hover()
}

/// Build an `Hsla` from 8-bit sRGB channels (terminal colors arrive as `u8` rgb).
pub fn from_rgb8(r: u8, g: u8, b: u8) -> Hsla {
    let hex = (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
    c(hex)
}

/// The 16 base ANSI colors (indices 0..16): normal 0..8 then bright 8..16.
/// Returns `None` for out-of-range indices.
pub fn ansi_base(index: u8) -> Option<Hsla> {
    let hex = match index {
        0 => 0x2a2e37,  // black (lifted off pure black so it reads on terminal_bg)
        1 => 0xe5484d,  // red
        2 => 0x46c46a,  // green
        3 => 0xf5a623,  // yellow
        4 => 0x6e8bff,  // blue
        5 => 0xc792ea,  // magenta
        6 => 0x2db7b0,  // cyan (product teal)
        7 => 0xc3c7d1,  // white
        8 => 0x4b515e,  // bright black (grey)
        9 => 0xff6b6f,  // bright red
        10 => 0x6fe39a, // bright green
        11 => 0xffc24b, // bright yellow
        12 => 0x8aa2ff, // bright blue
        13 => 0xdcb0f5, // bright magenta
        14 => 0x4fd6ce, // bright cyan
        15 => 0xf0f2f6, // bright white
        _ => return None,
    };
    Some(c(hex))
}

/// Resolve any 256-color index to an `Hsla`: 0..16 named, 16..232 cube,
/// 232..256 grayscale ramp (the standard xterm-256 layout).
pub fn ansi_indexed(index: u8) -> Hsla {
    if let Some(base) = ansi_base(index) {
        return base;
    }
    if (16..=231).contains(&index) {
        // 6×6×6 cube. Each axis level maps 0->0, then 95,135,175,215,255.
        let i = index - 16;
        let to_channel = |level: u8| -> u8 {
            if level == 0 {
                0
            } else {
                55 + level * 40
            }
        };
        let r = to_channel(i / 36);
        let g = to_channel((i / 6) % 6);
        let b = to_channel(i % 6);
        return from_rgb8(r, g, b);
    }
    // Grayscale ramp 232..=255: 24 steps from 8 to 238.
    let level = 8u16 + u16::from(index - 232) * 10;
    let v = level.min(255) as u8;
    from_rgb8(v, v, v)
}

/// Configure `gpui-component`'s global `Theme` to match these tokens.
///
/// gpui-component initializes in **light** mode, so without this the dock chrome
/// (tab strips, title bar, scrollbars, code editor, popovers) renders light around
/// our dark panels. We switch it to dark — which gives every unset field a
/// coherent dark default — then override the brand-critical surfaces, text,
/// accent, fonts and radii so the whole window reads as one designed surface.
///
/// Call once, immediately after `gpui_component::init(cx)`.
pub fn install(cx: &mut App) {
    use gpui_component::{Theme, ThemeMode};

    // Must precede any `mono_font()` read: the terminal grid and every mono surface
    // are sized from whichever family this picks.
    resolve_mono_font(cx);

    // Start from gpui-component's built-in dark config (coherent defaults for the
    // many fields we don't touch), then brand it.
    Theme::change(ThemeMode::Dark, None, cx);

    let theme = Theme::global_mut(cx);
    theme.font_family = ui_font().into();
    theme.mono_font_family = mono_font().into();
    theme.font_size = px(14.);
    theme.radius = radius_md();
    theme.radius_lg = radius_lg();
    // Toasts rise from the **bottom-right** — the status bar's 🔔 corner — clearing
    // the 30px status bar, so they read as coming from the notification area.
    theme.notification.placement = gpui::Anchor::BottomRight;
    theme.notification.margins.bottom = px(40.);

    let p = &mut theme.colors;

    // Core surfaces & text.
    p.background = surface_base();
    p.foreground = text_primary();
    p.border = border_subtle();
    p.muted = surface_raised();
    p.muted_foreground = text_muted();

    // Accent / interaction.
    p.accent = surface_overlay(); // hover bg for menu/list items
    p.accent_foreground = text_primary();
    p.primary = accent();
    p.primary_foreground = on_accent();
    p.primary_hover = accent_hover();
    p.primary_active = accent_hover();
    p.secondary = surface_raised();
    p.secondary_foreground = text_secondary();
    p.secondary_hover = surface_overlay();
    p.ring = accent();
    p.caret = accent();
    p.selection = tint(accent(), 0.30);
    p.link = accent();
    p.link_hover = accent_hover();

    // Popovers / inputs.
    p.popover = surface_overlay();
    p.popover_foreground = text_primary();
    p.input = border_subtle();

    // Left rail (file tree) chrome.
    p.sidebar = surface_raised();
    p.sidebar_foreground = text_primary();
    p.sidebar_border = border_subtle();
    p.sidebar_accent = surface_overlay();
    p.sidebar_accent_foreground = text_primary();

    // Dock tab strip — inactive tabs recede into the sunken gutter; the active tab
    // matches the panel surface so it reads as the current surface.
    p.tab_bar = surface_sunken();
    p.tab = surface_sunken();
    p.tab_active = surface_base();
    p.tab_active_foreground = text_primary();
    p.tab_foreground = text_muted();

    // Window title bar.
    p.title_bar = surface_sunken();
    p.title_bar_border = border_subtle();

    // Lists / tables.
    p.list = surface_base();
    p.list_active = row_selected();
    p.list_active_border = tint(accent(), 0.45);
    p.list_hover = row_hover();
    p.list_head = surface_raised();

    // Scrollbars — quiet track, thumb on the strong border.
    p.scrollbar = tint(surface_sunken(), 0.0);
    p.scrollbar_thumb = border_strong();
    p.scrollbar_thumb_hover = text_muted();

    // Semantic status (so gpui-component badges/alerts speak our traffic-lights).
    p.danger = status_color(SessionStatus::Errored);
    p.danger_foreground = text_primary();
    p.warning = status_color(SessionStatus::WaitingInput);
    p.warning_foreground = surface_sunken();
    p.success = status_color(SessionStatus::Done);
    p.success_foreground = surface_sunken();
    p.info = accent();
    p.info_foreground = on_accent();
}
