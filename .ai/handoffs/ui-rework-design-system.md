# Handoff — Global UI rework (design system + readability)

**Date:** 2026-06-03 · **Lane:** UI (`apps/desktop/src/views/**`) · **Status:** build pending verify

Operator asked for a *global UI rework — clean and readable, minimal/refined*. This
pass establishes a real design system and unifies the chrome. Coordinated with the
parallel UI-lane work on the approval keystone (plan_review footer / main.rs) —
touched additively, nothing in the engine/approval logic changed.

## What changed (and why)

### 1. `views/theme.rs` — rewritten into a token system (foundation)
- **Four-step surface elevation**: `surface_sunken / base / raised / overlay` (+
  `border_subtle / border_strong`). Depth via lightness steps, not loud borders.
- **Three-step text hierarchy**, lifted for dark-mode legibility: `text_primary /
  secondary / muted` (muted raised above the contrast floor so paths/hints read).
- **One accent** (`accent` + `accent_hover` + `on_accent`) carries interaction.
  Status traffic-lights + phase colors are unchanged — they stay the only saturated
  hues so "needs you" genuinely pops.
- **Type scale** as fns returning `Pixels`: `text_2xs(10) … text_xl(17)`. **Radius
  scale**: `radius_sm/md/lg`. **Fonts**: `ui_font()` (`.SystemUIFont`), `mono_font()`
  (`Menlo`). → no more scattered `px(11.)`/`px(8.)` magic in views.
- All previous pub fns kept (status_color, phase_color, git_*, terminal_*, ansi_*),
  so nothing breaks; values for surfaces/text/accent were refined.

### 2. `views/theme::install(cx)` + call in `main.rs` — THE key fix
gpui-component initializes in **light** mode, so the dock chrome (tab strips, title
bar, scrollbars, code editor, popovers) was rendering *light* around our dark panels
— the biggest "undesigned" tell. `install()` switches its global `Theme` to **dark**
and overrides the brand-critical `ThemeColor` fields (surfaces, text, accent, tabs,
title bar, sidebar, lists, scrollbars, semantic status) + fonts/radii to our tokens.
Called once right after `gpui_component::init(cx)` in `main.rs`.

### 3. Panels refined to the new tokens + clearer hierarchy
- **`session_tile.rs`** — weighted (MEDIUM) title leads; quiet phase/mode meta;
  path in **mono** at the foot (reads as an address); hover lifts to `surface_overlay`
  + `border_strong`. Tokens throughout.
- **`grid_home.rs`** — top bar now reads as chrome (`surface_sunken`, ☾ wordmark,
  SEMIBOLD); **needs badge** has a calm green "✓ all clear" when 0, amber "N need you"
  otherwise; **New session** is now a solid-accent primary button; fleet area scrolls;
  adopt chip token-ified.
- **`session_monitor.rs`** — bigger SEMIBOLD title; facts in a quiet bordered card;
  `field()` gained a `mono` flag (Path renders mono).
- **`plan_review.rs`** (G3 readability) — header refined; **plan body now renders
  lightweight markdown** (`plan_line()`): headings (`#/##/###`) weighted+sized,
  bullets get a real • glyph + hanging indent, ``` fences become a faint divider,
  blank lines become real space. NOTE: this is a *leading-token* renderer, not full
  markdown — inline `**bold**`/backticks are still literal. It partially serves
  **T14**; a full markdown widget can replace `plan_line()` later. I did **not** touch
  the approve/reject footer or `apply_event` (your keystone work).

### Not touched (inherit the refresh for free via tokens)
`terminal.rs`, `file_tree.rs`, `code_editor.rs` — they already read `theme::*` for
colors, so the refined palette + dark gpui-component chrome restyle them without edits.
Terminal `MONO_FONT`/cell metrics deliberately left alone (alignment is tuned).

## Coordination notes
- `theme.rs` is now the single source for sizes/radii/fonts too — please pull sizes
  from `theme::text_*()` / `theme::radius_*()` rather than literals in new UI.
- If you add gpui-component components (Button, Input, etc.), they now match
  automatically via `install()` — no per-call theming needed.
- One `main.rs` insert (the `theme::install(cx)` line after `gpui_component::init`)
  had to go in via a guarded shell insert because the file was being written
  concurrently; it's idempotent and verified present.

## Verify
`export PATH="$HOME/.cargo/bin:$PATH" && cargo check -p moonlight-desktop` then run
the app (RustRover ▶ "Run moonlight"). Expect: fully dark, coherent chrome; tiles
with clear title/meta/path hierarchy; readable plan documents.
