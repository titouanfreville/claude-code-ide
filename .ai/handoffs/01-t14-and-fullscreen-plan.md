# Plan — T14 markdown quality + macOS fullscreen menu access

**Status:** READY TO IMPLEMENT (blocked on phase change — moonlight MCP host was down
2026-06-09, could not `request_phase auto`). Execute the moment write access is granted.
**Operator decision (2026-06-09):** fullscreen approach = **App menus + in-app FS toggle**
(in-our-control; verify hover-reveal on-device afterward). Do NOT fork/patch pinned gpui yet.

## Findings (root causes)
- **T14:** `TextView::markdown(...)` is called with the **default `TextViewStyle`**
  (`is_dark: false`, `HighlightTheme::default_light()`) at `plan_review.rs:546` and
  `session_monitor.rs:1566`. App forces `ThemeMode::Dark` (`theme.rs:353`) → light code
  blocks / wrong contrast / untuned headings on a dark surface = the "poor quality".
  `TextView::markdown(..).style(TextViewStyle)` builder exists. `HighlightTheme::default_dark()`
  confirmed to exist (gpui-component highlighter.rs:799).
- **Fullscreen:** gpui main window = standard native `toggleFullScreen_`, `FullScreenPrimary`,
  `fullSizeContentView` (transparent titlebar). **No `setPresentationOptions`** suppresses the
  menu bar — gpui is correct; hover-reveal is OS-level (can't flip from app code). Real gap:
  **our app never calls `cx.set_menus(...)`** → empty menu bar, nothing to reveal.

## Task 1 — T14 markdown style (theme.rs + 2 call sites)
- `views/theme.rs`: add `pub fn markdown_style() -> gpui_component::text::TextViewStyle`
  - `is_dark: true`; `highlight_theme: HighlightTheme::default_dark()` (Arc)
  - `heading_font_size(|level, base| ...)` scaled per level (e.g. h1 1.6×…h6 1.0× of base)
  - tuned `paragraph_gap` (e.g. `rems(0.6)`); `code_block` StyleRefinement = bordered + raised bg
    using `surface_raised()`/`border_subtle()` + mono font + padding/rounding
- Apply `.style(theme::markdown_style())`:
  - `panels/plan_review.rs:546` (`TextView::markdown("plan-body-md", ...)`)
  - `panels/session_monitor.rs:1566` (assistant message markdown)
- Test: `markdown_style` returns `is_dark==true` + expected paragraph_gap.

## Task 2 — App menus + in-app fullscreen toggle (main.rs + workspace.rs)
- Define actions (near `SaveFile`/`FormatDocument`): `actions!(moonlight, [NewSession,
  ToggleExplorer, ToggleFullScreen, Quit])` (reuse existing Quit if present).
- `main.rs` app init (after `gpui_component::init(cx)` / `theme::install`): pure helper
  `app_menus() -> Vec<Menu>` then `cx.set_menus(app_menus())`:
  - **MoonlightCode**: About · `MenuItem::os_submenu("Services", SystemMenuType::Services)`
    · separator · Quit (⌘Q)
  - **File**: New Session (⌘N)
  - **Edit**: Undo/Redo/Cut/Copy/Paste/Select All via `MenuItem::os_action(.., OsAction::*)`
    (gives text fields standard shortcuts too)
  - **View**: Toggle Explorer · **Toggle Full Screen (⌃⌘F)**
  - **Window**: Minimize · Zoom
- Handlers:
  - Global: `cx.on_action::<Quit>(|_, cx| cx.quit())`
  - Workspace ROOT element `.on_action(cx.listener(...))` (has `&mut Window`):
    `ToggleFullScreen` → `window.toggle_fullscreen()`; `NewSession` → `self.new_session(cx)`
    (workspace.rs:1708); `ToggleExplorer` → left-dock toggle (toggle_left_dock 1066 /
    toggle_left_tool 1108)
  - `cx.bind_keys([cmd-q Quit, cmd-n NewSession, cmd-ctrl-f ToggleFullScreen, …])`
    (global context = None so they work in fullscreen)
- Always-visible in-app affordance: add a "Toggle Full Screen" row to the project ▾ dropdown
  in `panels/toolbar.rs` (so fullscreen is escapable even if the menu bar won't reveal).
- Test: pure `app_menus()` builder — assert top-level names + that View contains Toggle Full Screen.

## Verify
- `cargo build` + `cargo test` green (baseline 361/1).
- Runtime (RustRover ▶): (1) plan-review + assistant markdown now legible (dark code blocks,
  proper headings/spacing); (2) menu bar populated; ⌃⌘F enters/exits fullscreen; project ▾ has
  a Toggle Full Screen item; (3) confirm whether macOS hover-reveal of the menu bar now returns
  in fullscreen — if still not, that's the deferred gpui_macos investigation.

## Backlog bookkeeping (when in Commit/edit phase)
- Tick **T14 / G3** as DONE (markdown was already rendered; this fixes the styling quality).
- **T12 "First commit"** is stale — landed as `a926f8e`.
