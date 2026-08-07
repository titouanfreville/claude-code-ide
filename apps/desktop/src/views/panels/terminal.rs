//! Terminal panel — the operator's own manual ANSI terminal (not Claude-related).
//!
//! Renders the [`Emulator`]'s grid as monospace styled runs, forwards keystrokes
//! to the PTY, resizes the grid to the panel bounds, and "follows focus": when the
//! operator focuses a different session, the shell `cd`s into that project root
//! (UX spec: follow focused session). A lightweight timer polls the emulator's
//! dirty flag and re-renders on change (≤1s, off the engine path — NFR4).

use std::path::PathBuf;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    actions, canvas, div, px, App, Bounds, ClipboardEntry, ClipboardItem, Context, DispatchPhase,
    Entity, EventEmitter, FocusHandle, Focusable, Hsla, Image, ImageFormat, KeyBinding,
    KeyDownEvent, Keystroke, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    SharedString, Task, Window,
};
use gpui_component::dock::{Panel, PanelEvent};

actions!(moonlight_terminal, [SendTab, SendShiftTab]);

/// Key context for a focused terminal. Deeper in the dispatch tree than
/// gpui-component `Root`'s `"Root"` context, so the Tab/Shift+Tab bindings below
/// **override** Root's focus-traversal — letting a focused terminal pass Tab to the
/// child (shell completion, Claude Code's autofill) and Shift+Tab as back-tab
/// (Claude Code's permission-mode cycle) instead of moving window focus.
const TERMINAL_CONTEXT: &str = "MoonlightTerminal";

/// Register the terminal's Tab/Shift+Tab key bindings. Call once at startup.
pub fn init_keybindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", SendTab, Some(TERMINAL_CONTEXT)),
        KeyBinding::new("shift-tab", SendShiftTab, Some(TERMINAL_CONTEXT)),
    ]);
}

use alacritty_terminal::index::{Column, Point as TermPoint, Side};
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::viewport_to_point;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor};

use crate::term::emulator::Emulator;
use crate::views::project_space::ProjectSpace;
use crate::views::theme;

const FONT_SIZE: f32 = 13.0;
/// Cell height — a chosen line height rather than a measured one: the renderer
/// sets it explicitly, so the grid's rows are exactly this tall by construction.
const CELL_H: f32 = 17.0;
/// Fallback cell width (Menlo @ 13px), used only until the real advance width of
/// the resolved monospace font is measured — see [`TerminalPanel::cell_width`].
/// Mapping pixels to grid cells with the wrong advance makes clicks and selections
/// drift further the further right you go, so it must match the font in use.
const CELL_W_FALLBACK: f32 = 7.8;
/// Redraw cadence — fast enough to feel live, cheap when idle (only notifies on
/// an actual dirty grid).
const POLL_INTERVAL: Duration = Duration::from_millis(33);
/// A compose keystroke arriving after this long without typing is treated as
/// starting a *fresh* message — the only window in which an armed injection
/// (see [`TerminalPanel::arm_injection`]) may fire. Shorter gaps mean the
/// operator is mid-draft, where injecting would corrupt their input.
const COMPOSE_GAP: Duration = Duration::from_secs(10);

/// One resolved cell ready to render (colors already mapped to the theme).
struct RCell {
    ch: char,
    fg: Hsla,
    bg: Hsla,
    /// Part of the active mouse selection (rendered with the selection background).
    selected: bool,
    /// The URL this cell belongs to, if any (OSC8 hyperlink or an auto-detected
    /// plain URL) — rendered underlined and opened on ⌘-click.
    link: Option<SharedString>,
}

/// A contiguous run of link cells on one grid line, used to resolve a ⌘-click
/// position back to its URL. Lines are absolute grid lines (negative = scrollback).
struct LinkSpan {
    line: i32,
    cols: std::ops::Range<usize>,
    url: SharedString,
}

/// One terminal tab: its own PTY-backed shell.
struct TermTab {
    /// `None` if the PTY failed to spawn; `error` then explains why.
    emulator: Option<Emulator>,
    error: Option<String>,
    /// Last project root this tab `cd`'d into (avoids duplicate `cd`s).
    current_root: PathBuf,
    /// Short tab-strip label.
    label: String,
}

/// Spawn a fresh terminal tab rooted at `root`, optionally running `command`.
fn spawn_tab(root: PathBuf, command: Option<&str>, label: String) -> TermTab {
    let (emulator, error) = match Emulator::spawn(
        Some(root.clone()),
        80,
        24,
        CELL_W_FALLBACK as u16,
        CELL_H as u16,
    ) {
        Ok(emu) => {
            if let Some(cmd) = command {
                emu.write_str(&format!("{cmd}\n"));
            }
            (Some(emu), None)
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to spawn terminal");
            (None, Some(e.to_string()))
        }
    };
    TermTab {
        emulator,
        error,
        current_root: root,
        label,
    }
}

pub struct TerminalPanel {
    /// One shell per tab; always at least one.
    tabs: Vec<TermTab>,
    /// Index of the visible/active tab.
    active: usize,
    /// Shared focus, kept so new tabs spawn at the current project root.
    focus: Option<Entity<ProjectSpace>>,
    /// When this terminal is **pinned** to a single space (no `focus`), the root new
    /// `＋` tabs should open in. `None` for a focus-following terminal (which reads the
    /// root off `focus` instead). See [`Self::new_pinned`] and [`Self::spawn_root`].
    pinned_root: Option<PathBuf>,
    /// Whether to draw the tab strip. Hidden when embedded as another view's
    /// content region (e.g. a managed session's terminal) so it reads as plain
    /// terminal output, not a standalone terminal panel.
    chrome: bool,
    focus_handle: FocusHandle,
    /// Bounds of the grid content area (reported by the canvas sizer each frame),
    /// used to map mouse pixel positions to grid cells.
    grid_bounds: Option<Bounds<Pixels>>,
    /// Link runs in the current frame, for resolving ⌘-clicks to URLs.
    links: Vec<LinkSpan>,
    /// A left-button drag (selection) is in progress.
    dragging: bool,
    /// The pointer moved since the drag began — distinguishes a select from a
    /// bare click (a bare click clears any selection instead of leaving an
    /// empty one that would swallow the next ⌘C).
    drag_moved: bool,
    /// The current drag started as a plain single click (`Simple` selection), so
    /// a release without movement should clear it. Double/triple clicks select a
    /// word/line and must persist even without a drag.
    drag_clearable: bool,
    /// While a selection drag runs past the grid edge, the direction to keep
    /// scrolling each poll tick: `+1` up into history (pointer above the top),
    /// `-1` toward the live bottom (pointer below), `0` when inside the grid.
    /// Driven from the poll loop so it keeps scrolling while the pointer is held
    /// still — fixes selecting/copying text taller than the visible window.
    autoscroll: i32,
    /// Last drag pointer position (window space), so the poll-driven auto-scroll
    /// can re-extend the selection to the edge column.
    drag_pos: Option<gpui::Point<Pixels>>,
    /// A left press was forwarded to the PTY because the child app enabled mouse
    /// reporting — subsequent move/release for this button go to the PTY too
    /// (instead of driving a local text selection).
    mouse_reported: bool,
    /// A one-shot command armed to be written to the PTY immediately before the
    /// next keystroke that starts a fresh message (see [`Self::arm_injection`]).
    auto_inject: Option<String>,
    /// When the operator last typed/pasted into the grid — the typing-lull clock
    /// that gates [`Self::auto_inject`] (only a fresh message may trigger it).
    last_typed_at: Option<std::time::Instant>,
    /// Advance width of one grid cell, measured from the resolved monospace font
    /// the first time this panel renders — see [`Self::cell_width`].
    cell_w: Option<f32>,
    _poll: Option<Task<()>>,
}

impl TerminalPanel {
    pub fn new(focus: Option<Entity<ProjectSpace>>, cx: &mut Context<Self>) -> Self {
        let root = focus
            .as_ref()
            .map(|f| f.read(cx).root())
            .unwrap_or_else(default_root);
        Self::with_first_tab(root, focus, None, cx)
    }

    /// Spawn a terminal pinned to an explicit `root`, immediately running `command`,
    /// and **not** following focus — for a terminal embedded as one session's content
    /// region (e.g. `claude --session-id <id>` for a new session, `claude --resume
    /// <id>` for an observed one), which must stay in its own repo even when the
    /// operator focuses a different session.
    pub fn new_running_in(root: PathBuf, command: &str, cx: &mut Context<Self>) -> Self {
        Self::with_first_tab(root, None, Some(command), cx)
    }

    /// Spawn a terminal **pinned** to `root` and **not** following focus — for a
    /// per-space dock terminal. Each space owns its own pinned terminal so switching
    /// spaces shows that space's shells (rooted at it) instead of `cd`-ing one shared
    /// shell, which would let spaces stomp on each other. New `＋` tabs open at `root`.
    pub fn new_pinned(root: PathBuf, cx: &mut Context<Self>) -> Self {
        Self::with_first_tab(root, None, None, cx)
    }

    fn with_first_tab(
        root: PathBuf,
        focus: Option<Entity<ProjectSpace>>,
        command: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Self {
        let label = command
            .map(|_| "claude".to_string())
            .unwrap_or_else(|| "term 1".to_string());
        // A terminal with no `focus` is pinned to its `root` (per-space dock terminal
        // or embedded session): remember it so new tabs open there. A focus-following
        // terminal reads its root off `focus`, so it stays `None`.
        let pinned_root = focus.is_none().then(|| root.clone());
        let tab = spawn_tab(root, command, label);

        // Re-render whenever the *active* tab's grid changes.
        let poll = cx.spawn(async move |weak, cx| loop {
            cx.background_executor().timer(POLL_INTERVAL).await;
            let keep = weak
                .update(cx, |this, cx| {
                    // `drive_autoscroll` first: it may scroll the grid (marking it
                    // dirty), and a held-still drag past the edge only advances here.
                    let scrolled = this.drive_autoscroll();
                    if this.active_dirty() || scrolled {
                        cx.notify();
                    }
                })
                .is_ok();
            if !keep {
                break; // view dropped
            }
        });

        // Follow focus: `cd` the *active* tab into the newly-focused session's root.
        if let Some(focus) = focus.as_ref() {
            cx.observe(focus, |this, focus, cx| {
                let new_root = focus.read(cx).root();
                if let Some(tab) = this.tabs.get_mut(this.active) {
                    if new_root != tab.current_root {
                        tab.current_root = new_root.clone();
                        if let Some(emu) = &tab.emulator {
                            emu.write_str(&cd_command(&new_root));
                        }
                        cx.notify();
                    }
                }
            })
            .detach();
        }

        Self {
            tabs: vec![tab],
            active: 0,
            focus,
            pinned_root,
            chrome: true,
            focus_handle: cx.focus_handle(),
            grid_bounds: None,
            links: Vec::new(),
            dragging: false,
            drag_moved: false,
            drag_clearable: true,
            autoscroll: 0,
            drag_pos: None,
            mouse_reported: false,
            auto_inject: None,
            last_typed_at: None,
            cell_w: None,
            _poll: Some(poll),
        }
    }

    /// Hide the tab strip — for use when this terminal is embedded as another
    /// view's content region (e.g. a managed session's live terminal).
    pub fn embedded(mut self) -> Self {
        self.chrome = false;
        self
    }

    fn active_tab(&self) -> Option<&TermTab> {
        self.tabs.get(self.active)
    }

    /// The width of one grid cell: the monospace font's actual advance, measured
    /// once, falling back to the Menlo figure until then.
    fn cell_width(&self) -> f32 {
        self.cell_w.unwrap_or(CELL_W_FALLBACK)
    }

    /// Measure the resolved monospace font's advance width, once. The grid maps
    /// pixels to cells with it, so a font whose advance isn't Menlo's would
    /// otherwise misplace every click past the first column.
    fn measure_cell(&mut self, window: &Window) {
        if self.cell_w.is_some() {
            return;
        }
        let font = gpui::font(theme::mono_font());
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&font);
        if let Ok(advance) = text_system.em_advance(font_id, px(FONT_SIZE)) {
            let advance = f32::from(advance);
            if advance > 0.0 {
                self.cell_w = Some(advance);
            }
        }
    }

    /// Write raw text to the active tab's PTY — used to drive Claude Code's TUI
    /// prompts from the cockpit (e.g. Enter to accept a plan's continuation).
    /// No-op if the PTY failed to spawn.
    pub fn send_text(&self, text: &str) {
        if let Some(emu) = self.active_tab().and_then(|t| t.emulator.as_ref()) {
            emu.write_str(text);
        }
    }

    /// Submit a **multi-line** message to the child as one prompt: the text is
    /// wrapped as a bracketed paste (when the child asked for that mode) and then
    /// submitted with a single CR.
    ///
    /// Sending the same text through [`send_text`](Self::send_text) would submit it
    /// line by line — each `\n` reads as Enter in Claude Code's prompt, so an
    /// N-line message becomes N truncated turns. Everything the cockpit composes
    /// from structured input (a batched review, a rejection reason) goes through
    /// here.
    pub fn send_paste(&self, text: &str) {
        let Some(emu) = self.active_tab().and_then(|t| t.emulator.as_ref()) else {
            return;
        };
        emu.scroll_to_bottom();
        if emu.bracketed_paste() {
            emu.write_str(&format!("\x1b[200~{text}\x1b[201~"));
        } else {
            // Without bracketed paste a bare `\n` IS an Enter: the child would
            // submit the message one line at a time and keep only a fragment. The
            // xterm meta+Enter sequence is a literal newline to Claude Code — the
            // same thing `encode_key` sends for Shift+Enter — so multi-line text
            // arrives as one prompt whatever mode the child is in.
            emu.write_str(&text.replace('\n', "\x1b\r"));
        }
        emu.write_str("\r");
    }

    /// Arm a one-shot injection: `text` is written to the PTY immediately before
    /// the next keystroke that starts a **fresh** message (a printable, non-digit
    /// key or a ⌘V paste, after a [`COMPOSE_GAP`] typing lull), so the injected
    /// command runs first and the operator's message is handled after it. Used by
    /// auto-compact to run `/compact` ahead of the next prompt. Re-arming
    /// replaces the pending text; the injection survives until fired or disarmed.
    pub fn arm_injection(&mut self, text: String) {
        self.auto_inject = Some(text);
    }

    /// Clear any armed injection (manual compact, usage dropped back down, or the
    /// terminal is being replaced).
    pub fn disarm_injection(&mut self) {
        self.auto_inject = None;
    }

    /// Consume the armed injection if `ks` begins composing a fresh message —
    /// a compose event (see [`is_compose_event`]) arriving after a typing lull of
    /// [`COMPOSE_GAP`]. Stamps the typing clock for every compose event, so a
    /// mid-draft arm waits for the *next* message instead of corrupting this one.
    fn injection_for(&mut self, ks: &Keystroke) -> Option<String> {
        if !is_compose_event(ks) {
            return None;
        }
        let fresh = self
            .last_typed_at
            .is_none_or(|t| t.elapsed() >= COMPOSE_GAP);
        self.last_typed_at = Some(std::time::Instant::now());
        if fresh {
            self.auto_inject.take()
        } else {
            None
        }
    }

    /// Whether the active tab's child process has exited (the PTY closed). `false` when
    /// there's no emulator (spawn failed) — an unstarted terminal is not a *stopped*
    /// session, so callers (e.g. auto-resume) don't treat it as one. Used to detect a
    /// managed Claude Code session whose process fully ended, vs one merely idle.
    pub fn has_exited(&self) -> bool {
        self.active_tab()
            .and_then(|t| t.emulator.as_ref())
            .map(Emulator::has_exited)
            .unwrap_or(false)
    }

    /// The active tab's visible screen text, or empty when there is no live terminal.
    /// Used to detect Claude Code's interactive prompts before injecting a response.
    pub fn visible_text(&self) -> String {
        self.active_tab()
            .and_then(|t| t.emulator.as_ref())
            .map(Emulator::visible_text)
            .unwrap_or_default()
    }

    /// Forward raw bytes to the active tab's PTY, clearing any selection and snapping
    /// to the live bottom first (mirrors keyboard input). Used by the Tab/Shift+Tab
    /// key-bound actions, which deliver keys that Root's focus-nav would otherwise eat.
    fn send_to_pty(&self, bytes: &[u8]) {
        if let Some(emu) = self.active_tab().and_then(|t| t.emulator.as_ref()) {
            emu.clear_selection();
            emu.scroll_to_bottom();
            emu.write(bytes.to_vec());
        }
    }

    /// Take-and-clear the active tab's dirty flag.
    fn active_dirty(&self) -> bool {
        self.active_tab()
            .and_then(|t| t.emulator.as_ref())
            .is_some_and(Emulator::take_dirty)
    }

    /// The root a new tab should open in: the current project focus (follow-focus
    /// terminals), else this terminal's pinned root (per-space / embedded), else cwd.
    fn spawn_root(&self, cx: &App) -> PathBuf {
        if let Some(focus) = self.focus.as_ref() {
            return focus.read(cx).root();
        }
        if let Some(root) = self.pinned_root.as_ref() {
            return root.clone();
        }
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
    }

    /// Open a new shell tab and make it active.
    fn add_tab(&mut self, cx: &mut Context<Self>) {
        let root = self.spawn_root(cx);
        let label = format!("term {}", self.tabs.len() + 1);
        self.tabs.push(spawn_tab(root, None, label));
        self.active = self.tabs.len() - 1;
        cx.notify();
    }

    /// Close tab `i` (keeps at least one tab; dropping it shuts its PTY down).
    fn close_tab(&mut self, i: usize, cx: &mut Context<Self>) {
        if self.tabs.len() <= 1 || i >= self.tabs.len() {
            return;
        }
        self.tabs.remove(i);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        } else if self.active > i {
            self.active -= 1;
        }
        cx.notify();
    }

    fn activate(&mut self, i: usize, cx: &mut Context<Self>) {
        if i < self.tabs.len() {
            self.active = i;
            cx.notify();
        }
    }

    /// Map current panel bounds to a grid size and resize the active PTY. Also
    /// records the content bounds so mouse positions can be mapped to grid cells.
    fn sync_size(&mut self, bounds: Bounds<Pixels>) {
        self.grid_bounds = Some(bounds);
        // Read the cell width before borrowing the emulator mutably.
        let cell_w = self.cell_width();
        let Some(emu) = self
            .tabs
            .get_mut(self.active)
            .and_then(|t| t.emulator.as_mut())
        else {
            return;
        };
        let cols = (f32::from(bounds.size.width) / cell_w).floor().max(2.0) as usize;
        let rows = (f32::from(bounds.size.height) / CELL_H).floor().max(1.0) as usize;
        emu.resize(cols, rows, cell_w as u16, CELL_H as u16);
    }

    /// Map a window-space mouse position to a **viewport** cell: 0-based column and
    /// line within the visible grid (both clamped into range), plus which side of
    /// the cell the pointer landed on. `None` before the first frame reports the
    /// grid bounds. Used both for selection (→ absolute point via [`Self::pixel_to_point`])
    /// and for mouse-event reporting (which uses viewport coordinates directly).
    fn pixel_to_cell(&self, pos: gpui::Point<Pixels>) -> Option<(usize, usize, Side)> {
        let b = self.grid_bounds?;
        let rel_x = (f32::from(pos.x) - f32::from(b.origin.x)).max(0.0);
        let rel_y = (f32::from(pos.y) - f32::from(b.origin.y)).max(0.0);
        let cell_w = self.cell_width();
        let cols = (f32::from(b.size.width) / cell_w).floor().max(1.0);
        let rows = (f32::from(b.size.height) / CELL_H).floor().max(1.0);
        let col = (rel_x / cell_w).floor().clamp(0.0, cols - 1.0) as usize;
        let line = (rel_y / CELL_H).floor().clamp(0.0, rows - 1.0) as usize;
        let side = if (rel_x / cell_w).fract() < 0.5 {
            Side::Left
        } else {
            Side::Right
        };
        Some((col, line, side))
    }

    /// Map a window-space mouse position to an absolute grid point (accounting for
    /// scrollback via `display_offset`) plus which side of the cell it landed on.
    /// `None` before the first frame reports the grid bounds.
    fn pixel_to_point(
        &self,
        pos: gpui::Point<Pixels>,
        display_offset: usize,
    ) -> Option<(TermPoint, Side)> {
        let (col, line, side) = self.pixel_to_cell(pos)?;
        let point = viewport_to_point(display_offset, TermPoint::new(line, Column(col)));
        Some((point, side))
    }

    /// Continue a selection drag that has run past the grid edge: scroll the
    /// viewport one line in [`Self::autoscroll`]'s direction and re-extend the
    /// selection to the (clamped) edge cell. Called every poll tick so the scroll
    /// keeps going while the pointer is held still beyond the window. Returns
    /// whether it scrolled (so the caller can `notify`).
    fn drive_autoscroll(&mut self) -> bool {
        if !self.dragging || self.autoscroll == 0 {
            return false;
        }
        let Some(pos) = self.drag_pos else {
            return false;
        };
        let dir = self.autoscroll;
        let scrolled = {
            let Some(emu) = self.active_tab().and_then(|t| t.emulator.as_ref()) else {
                return false;
            };
            emu.scroll(dir);
            if let Some((point, side)) = self.pixel_to_point(pos, emu.display_offset()) {
                emu.update_selection(point, side);
            }
            true
        };
        if scrolled {
            self.drag_moved = true;
        }
        scrolled
    }

    /// Forward a mouse event to the active PTY *iff* the child app has enabled
    /// mouse reporting (and, for a drag, asked for motion). Returns whether the
    /// event was reported — `false` leaves the view free to handle it locally
    /// (text selection / scrollback). `button`: 0 left, 1 middle, 2 right, 64/65
    /// wheel up/down.
    fn report_mouse(
        &self,
        pos: gpui::Point<Pixels>,
        button: u8,
        kind: MouseKind,
        mods: &gpui::Modifiers,
    ) -> bool {
        let Some(emu) = self.active_tab().and_then(|t| t.emulator.as_ref()) else {
            return false;
        };
        let mode = emu.mouse_mode();
        if !mode.reporting() {
            return false;
        }
        if kind == MouseKind::Drag && !(mode.drag || mode.motion) {
            return false;
        }
        let Some((col, line, _side)) = self.pixel_to_cell(pos) else {
            return false;
        };
        if let Some(bytes) = encode_mouse(mode, button, col, line, kind, mods) {
            emu.write(bytes);
        }
        true
    }

    /// The URL covering `point` in the current frame's link runs, if any.
    fn link_at(&self, point: TermPoint) -> Option<SharedString> {
        let col = point.column.0;
        self.links
            .iter()
            .find(|s| s.line == point.line.0 && s.cols.contains(&col))
            .map(|s| s.url.clone())
    }
}

/// Snapshot an emulator's visible grid into owned rows (releases the lock fast),
/// returning both the rendered rows and the link runs they contain (for resolving
/// ⌘-clicks to URLs).
fn snapshot(emu: &Emulator) -> (Vec<Vec<RCell>>, Vec<LinkSpan>) {
    // Build the rows under the lock, then release it before scanning for URLs.
    let (mut lines, line_nums) = {
        let term = emu.term().lock();
        let content = term.renderable_content();
        let cursor_point = content.cursor.point;
        let selection = content.selection;

        let mut lines: Vec<Vec<RCell>> = Vec::new();
        let mut line_nums: Vec<i32> = Vec::new();
        let mut current_line: i32 = i32::MIN;
        for indexed in content.display_iter {
            let line = indexed.point.line.0;
            if line != current_line {
                lines.push(Vec::new());
                line_nums.push(line);
                current_line = line;
            }
            let cell = indexed.cell;
            let inverse = cell.flags.contains(Flags::INVERSE);
            let is_cursor = indexed.point == cursor_point;
            let selected = selection
                .as_ref()
                .is_some_and(|r| r.contains(indexed.point));

            let mut fg = ansi_to_hsla(cell.fg, true);
            let mut bg = ansi_to_hsla(cell.bg, false);
            if inverse {
                std::mem::swap(&mut fg, &mut bg);
            }
            if is_cursor {
                // Block cursor: paint the cell in the cursor color.
                bg = theme::terminal_cursor();
                fg = theme::terminal_bg();
            }
            // OSC8 hyperlinks are authoritative; plain URLs are detected below.
            let link = cell
                .hyperlink()
                .map(|h| SharedString::from(h.uri().to_string()));
            if let Some(row) = lines.last_mut() {
                row.push(RCell {
                    ch: cell.c,
                    fg,
                    bg,
                    selected,
                    link,
                });
            }
        }
        (lines, line_nums)
    };

    // Auto-detect plain http(s) URLs per row (OSC8 links already set above win).
    for row in lines.iter_mut() {
        apply_links(row);
    }

    // Collapse consecutive same-URL cells into spans for click resolution.
    let mut spans: Vec<LinkSpan> = Vec::new();
    for (row, &line) in lines.iter().zip(line_nums.iter()) {
        let mut i = 0;
        while i < row.len() {
            let Some(url) = row[i].link.clone() else {
                i += 1;
                continue;
            };
            let start = i;
            while i < row.len() && row[i].link.as_ref() == Some(&url) {
                i += 1;
            }
            spans.push(LinkSpan {
                line,
                cols: start..i,
                url,
            });
        }
    }

    (lines, spans)
}

/// Tag each cell that falls within a detected `http(s)://` URL with that URL,
/// leaving OSC8-tagged cells untouched. Cells map 1:1 to chars in the row text.
fn apply_links(cells: &mut [RCell]) {
    if cells.is_empty() {
        return;
    }
    let text: String = cells.iter().map(|c| c.ch).collect();
    let ranges = find_urls(&text);
    if ranges.is_empty() {
        return;
    }
    let char_starts: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
    for (start, end) in ranges {
        let url = SharedString::from(text[start..end].to_string());
        for (idx, &b) in char_starts.iter().enumerate() {
            if b >= start && b < end && cells[idx].link.is_none() {
                cells[idx].link = Some(url.clone());
            }
        }
    }
}

/// Byte ranges of `http://` / `https://` URLs in `s`. Walks ASCII URL bytes and
/// trims trailing sentence punctuation; safe across multi-byte (non-URL) text.
fn find_urls(s: &str) -> Vec<(usize, usize)> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = s[from..].find("http") {
        let i = from + rel;
        let scheme = if s[i..].starts_with("https://") {
            8
        } else if s[i..].starts_with("http://") {
            7
        } else {
            from = i + 4;
            continue;
        };
        let mut j = i + scheme;
        while j < bytes.len() && is_url_byte(bytes[j]) {
            j += 1;
        }
        let mut end = j;
        while end > i + scheme && is_trailing_punct(bytes[end - 1]) {
            end -= 1;
        }
        if end > i + scheme {
            out.push((i, end));
        }
        from = j.max(i + 4);
    }
    out
}

/// Characters permitted inside a URL (RFC 3986 reserved + unreserved, ASCII).
fn is_url_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'-' | b'.'
                | b'_'
                | b'~'
                | b':'
                | b'/'
                | b'?'
                | b'#'
                | b'['
                | b']'
                | b'@'
                | b'!'
                | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b'%'
        )
}

/// Punctuation that is usually sentence trailing, not part of the URL.
fn is_trailing_punct(b: u8) -> bool {
    matches!(
        b,
        b'.' | b',' | b';' | b':' | b'!' | b'?' | b')' | b'\'' | b'"' | b'>'
    )
}

/// Resolve an alacritty cell color to a theme `Hsla`. Falls back to the default
/// fg/bg for color slots we don't theme explicitly (dim/bright fg, etc.).
fn ansi_to_hsla(color: AnsiColor, is_fg: bool) -> Hsla {
    let default = if is_fg {
        theme::terminal_fg()
    } else {
        theme::terminal_bg()
    };
    match color {
        AnsiColor::Spec(rgb) => theme::from_rgb8(rgb.r, rgb.g, rgb.b),
        AnsiColor::Indexed(i) => theme::ansi_indexed(i),
        AnsiColor::Named(named) => match named {
            NamedColor::Foreground => theme::terminal_fg(),
            NamedColor::Background => theme::terminal_bg(),
            NamedColor::Cursor => theme::terminal_cursor(),
            other => {
                let idx = other as usize;
                if idx < 16 {
                    theme::ansi_base(idx as u8).unwrap_or(default)
                } else {
                    default
                }
            }
        },
    }
}

/// A clipboard chord the panel serves itself instead of forwarding to the PTY.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClipboardChord {
    Copy,
    Paste,
}

/// Classify a keystroke as a copy/paste chord, or `None` to let it reach the child.
///
/// macOS keeps ⌘C/⌘V. Everywhere else GPUI reports ⌘ as `Modifiers::platform`,
/// which is bound to the **Super** key (`MOD_NAME_LOGO`) — nobody pastes with
/// Super, so the terminal conventions apply instead: Ctrl+Shift+C copies and
/// Ctrl+V / Ctrl+Shift+V / Shift+Insert paste. Bare Ctrl+C is deliberately *not*
/// a chord — it has to stay SIGINT, which is how the operator interrupts Claude
/// Code.
fn clipboard_chord(ks: &Keystroke) -> Option<ClipboardChord> {
    let m = &ks.modifiers;
    if cfg!(target_os = "macos") {
        if !m.platform || m.control || m.alt {
            return None;
        }
        return match ks.key.as_str() {
            "c" => Some(ClipboardChord::Copy),
            "v" => Some(ClipboardChord::Paste),
            _ => None,
        };
    }
    // Super chords belong to the window manager, Alt chords to the child.
    if m.platform || m.alt {
        return None;
    }
    match (ks.key.as_str(), m.control, m.shift) {
        ("c", true, true) => Some(ClipboardChord::Copy),
        ("v", true, _) => Some(ClipboardChord::Paste),
        ("insert", false, true) => Some(ClipboardChord::Paste),
        _ => None,
    }
}

/// Whether a click chord means "open the link under the cursor" — ⌘-click on
/// macOS, Ctrl-click elsewhere (the VS Code / gnome-terminal convention).
fn is_open_link_chord(m: &gpui::Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        m.platform
    } else {
        m.control
    }
}

/// What a paste chord should type into the PTY: the clipboard's text, or — when
/// it holds an image — the path of a file the image was spilled to, since Claude
/// Code reads images by path.
///
/// The image branch is deliberately app-side: CC's own `^V` handler shells out to
/// `xclip`/`wl-paste`, which need not be installed, while GPUI's clipboard already
/// decodes image formats itself.
fn clipboard_payload(cx: &mut App) -> Option<String> {
    let item = cx.read_from_clipboard()?;
    let image = item.entries().iter().find_map(|entry| match entry {
        ClipboardEntry::Image(image) => Some(image),
        _ => None,
    });
    if let Some(image) = image {
        return match spill_image(image) {
            // Trailing space so the path doesn't run into whatever is typed next.
            Ok(path) => Some(format!("{} ", path.display())),
            Err(error) => {
                tracing::warn!(error = %error, "clipboard image could not be spilled to a file");
                None
            }
        };
    }
    item.text().filter(|text| !text.is_empty())
}

/// Write a clipboard image to a temp file so a child process can be handed it by
/// path. Named by the image's content hash, so pasting the same screenshot twice
/// reuses one file instead of littering the temp dir.
fn spill_image(image: &Image) -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join("moonlight-clipboard");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!(
        "paste-{:016x}.{}",
        image.id(),
        image_extension(image.format)
    ));
    if !path.exists() {
        std::fs::write(&path, &image.bytes)?;
    }
    Ok(path)
}

/// File extension for a clipboard image format — readers key off the extension,
/// so it has to match the bytes actually written.
fn image_extension(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Webp => "webp",
        ImageFormat::Gif => "gif",
        ImageFormat::Svg => "svg",
        ImageFormat::Bmp => "bmp",
        ImageFormat::Tiff => "tiff",
        ImageFormat::Ico => "ico",
        ImageFormat::Pnm => "pnm",
    }
}

/// Encode a keystroke into the bytes a PTY expects, or `None` to let GPUI handle
/// it (e.g. ⌘-shortcuts, unmapped chords).
fn encode_key(ks: &Keystroke) -> Option<Vec<u8>> {
    let m = &ks.modifiers;
    // Leave platform (⌘ / Super) chords to the app's keybindings.
    if m.platform {
        return None;
    }

    // Modifier+Enter inserts a soft newline instead of submitting. Claude Code
    // (like most readline-style TUIs) treats the xterm "meta + Enter" sequence
    // ESC-CR as a literal newline, while a bare CR submits the prompt. Map
    // Ctrl/Shift-Enter to ESC-CR so the operator can compose multi-line prompts
    // (Alt-Enter already produces ESC-CR via the meta-prefix path below).
    if ks.key == "enter" && (m.control || m.shift) {
        return Some(b"\x1b\r".to_vec());
    }

    // Tab / Shift+Tab are delivered via the dedicated `SendTab`/`SendShiftTab`
    // key-bound actions (in the `MoonlightTerminal` context, which overrides
    // gpui-component Root's focus-traversal binding). Returning `None` here keeps
    // the PTY from also receiving them through this path (no double-send).
    if ks.key == "tab" {
        return None;
    }

    // Ctrl + letter → control byte (C0). ctrl-a = 0x01 … ctrl-z = 0x1a.
    if m.control && !m.alt {
        if let Some(byte) = ctrl_byte(&ks.key) {
            return Some(vec![byte]);
        }
    }

    // A modified cursor/navigation key (Alt/Shift/Ctrl + arrow, Home/End, etc.)
    // uses the xterm CSI form `ESC[1;<mod><final>` (e.g. Alt+Left → `ESC[1;3D`
    // for word-back) instead of a bare ESC-prefixed arrow, which Claude Code's
    // readline and most shells don't read as a word jump. Unmodified keys fall
    // through to `base` unchanged.
    if let Some(seq) = encode_modified_nav(&ks.key, m) {
        return Some(seq);
    }

    let base: Vec<u8> = match ks.key.as_str() {
        "enter" => b"\r".to_vec(),
        "backspace" => vec![0x7f],
        "escape" => vec![0x1b],
        "up" => b"\x1b[A".to_vec(),
        "down" => b"\x1b[B".to_vec(),
        "right" => b"\x1b[C".to_vec(),
        "left" => b"\x1b[D".to_vec(),
        "home" => b"\x1b[H".to_vec(),
        "end" => b"\x1b[F".to_vec(),
        "pageup" => b"\x1b[5~".to_vec(),
        "pagedown" => b"\x1b[6~".to_vec(),
        "delete" => b"\x1b[3~".to_vec(),
        "space" => b" ".to_vec(),
        _ => ks.key_char.as_ref()?.clone().into_bytes(),
    };

    // Alt/Meta prefixes the sequence with ESC (xterm convention).
    if m.alt {
        let mut bytes = vec![0x1b];
        bytes.extend(base);
        Some(bytes)
    } else {
        Some(base)
    }
}

/// The xterm modifier parameter for a modified key: `1 + shift + alt·2 + ctrl·4`
/// (platform/⌘ is handled earlier). `None` when no modifier is held, so the
/// caller emits the bare, unmodified sequence instead.
fn csi_modifier(m: &gpui::Modifiers) -> Option<u8> {
    let mut bits = 0u8;
    if m.shift {
        bits += 1;
    }
    if m.alt {
        bits += 2;
    }
    if m.control {
        bits += 4;
    }
    (bits != 0).then_some(1 + bits)
}

/// Encode a **modified** cursor/navigation key as the xterm CSI form. Returns
/// `None` for non-navigation keys *or* when no modifier is held (the unmodified
/// sequence is emitted by `encode_key`'s `base` table, unchanged). Letter-final
/// keys (arrows, Home/End) use `ESC[1;<mod><final>`; the editing keys
/// (PageUp/Down, Delete) use the tilde form `ESC[<n>;<mod>~`.
fn encode_modified_nav(key: &str, m: &gpui::Modifiers) -> Option<Vec<u8>> {
    let v = csi_modifier(m)?;
    // (number/param, final byte) — `~` marks the tilde editing keys.
    let (lead, tail) = match key {
        "up" => ("1", "A"),
        "down" => ("1", "B"),
        "right" => ("1", "C"),
        "left" => ("1", "D"),
        "home" => ("1", "H"),
        "end" => ("1", "F"),
        "pageup" => ("5", "~"),
        "pagedown" => ("6", "~"),
        "delete" => ("3", "~"),
        _ => return None,
    };
    Some(format!("\x1b[{lead};{v}{tail}").into_bytes())
}

/// A mouse event to report to the child app.
#[derive(Clone, Copy, PartialEq)]
enum MouseKind {
    Press,
    Release,
    /// Motion with a button held (drag) — sets the +32 motion bit.
    Drag,
}

/// Encode a mouse event into the bytes a mouse-reporting child expects, given its
/// negotiated [`MouseMode`]. `button` is the low button code (0 left, 1 middle,
/// 2 right, 64/65 wheel up/down); `col`/`line` are 0-based viewport coordinates.
/// Returns SGR (`ESC[<b;x;yM/m`) when the app asked for it (DECSET 1006), else the
/// legacy X10 `ESC[Mbxy` triple (dropped when a coordinate exceeds the 223 cap the
/// legacy form can carry).
fn encode_mouse(
    mode: crate::term::emulator::MouseMode,
    button: u8,
    col: usize,
    line: usize,
    kind: MouseKind,
    mods: &gpui::Modifiers,
) -> Option<Vec<u8>> {
    let mut cb = button;
    if kind == MouseKind::Drag {
        cb += 32; // motion bit
    }
    if mods.shift {
        cb += 4;
    }
    if mods.alt {
        cb += 8;
    }
    if mods.control {
        cb += 16;
    }
    let x = col + 1; // mouse coordinates are 1-based
    let y = line + 1;
    if mode.sgr {
        let final_byte = if kind == MouseKind::Release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{cb};{x};{y}{final_byte}").into_bytes());
    }
    // Legacy X10: release reports button 3 (all up); fields are offset by 32 and
    // cannot exceed 255, so a position past 223 columns/lines is undeliverable.
    let legacy_cb = if kind == MouseKind::Release {
        (cb & !0b11) | 0b11
    } else {
        cb
    };
    let bx = 32u8.checked_add(u8::try_from(x).ok()?)?;
    let by = 32u8.checked_add(u8::try_from(y).ok()?)?;
    let bcb = 32u8.checked_add(legacy_cb)?;
    Some(vec![0x1b, b'[', b'M', bcb, bx, by])
}

/// Whether a keystroke starts/continues **composing a message**: a paste, or a
/// printable character with no chord — excluding digits, which drive Claude Code's
/// numbered menus (never inject a command in front of a menu pick). Gates the armed
/// injection (see [`TerminalPanel::arm_injection`]).
fn is_compose_event(ks: &Keystroke) -> bool {
    let m = &ks.modifiers;
    // A paste drops a draft into the prompt; the copy chord changes nothing.
    if let Some(chord) = clipboard_chord(ks) {
        return chord == ClipboardChord::Paste;
    }
    if m.platform || m.control || m.alt {
        return false;
    }
    ks.key_char
        .as_ref()
        .and_then(|s| s.chars().next())
        .is_some_and(|c| !c.is_control() && !c.is_ascii_digit())
}

/// The C0 control byte for a `ctrl-<letter>` chord, if `key` is a single a–z.
fn ctrl_byte(key: &str) -> Option<u8> {
    let mut chars = key.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    if c.is_ascii_alphabetic() {
        Some((c.to_ascii_lowercase() as u8 - b'a') + 1)
    } else {
        None
    }
}

/// The fallback root when no project is focused: the process working directory.
fn default_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

/// A `cd` command into `root`, single-quoted so spaces/specials are safe.
fn cd_command(root: &std::path::Path) -> String {
    let p = root.to_string_lossy();
    let escaped = p.replace('\'', "'\\''");
    format!(" cd '{escaped}'\r")
}

impl Focusable for TerminalPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for TerminalPanel {}

impl Panel for TerminalPanel {
    fn panel_name(&self) -> &'static str {
        "Terminal"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from("Terminal")
    }

    /// The terminal renders its own background; no extra panel padding.
    fn inner_padding(&self, _cx: &App) -> bool {
        false
    }
}

impl TerminalPanel {
    /// The tab strip: a chip per shell (active highlighted, × to close) + ＋ new.
    fn render_tab_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.active;
        let closable = self.tabs.len() > 1;
        let chips: Vec<_> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| {
                let label = tab.label.clone();
                let is_active = i == active;
                div()
                    .id(("term-tab", i))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py(px(2.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(is_active, |d| d.bg(theme::surface_raised()))
                    .text_size(px(11.))
                    .text_color(if is_active {
                        theme::text_primary()
                    } else {
                        theme::text_muted()
                    })
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.activate(i, cx)))
                    .child(label)
                    .when(closable, |d| {
                        d.child(
                            div()
                                .id(("term-close", i))
                                .px(px(2.))
                                .text_color(theme::text_muted())
                                .on_click(cx.listener(move |this, _ev, _w, cx| {
                                    cx.stop_propagation();
                                    this.close_tab(i, cx);
                                }))
                                .child("×"),
                        )
                    })
            })
            .collect();

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .w_full()
            .bg(theme::surface_base())
            .border_b_1()
            .border_color(theme::border_subtle())
            .px_1()
            .py(px(1.))
            .children(chips)
            .child(
                div()
                    .id("term-add")
                    .px_2()
                    .py(px(2.))
                    .cursor_pointer()
                    .text_size(px(12.))
                    .text_color(theme::text_muted())
                    .child("＋")
                    .on_click(cx.listener(|this, _ev, _w, cx| this.add_tab(cx))),
            )
            // Far right: the uniform tool-window hide ✕ (collapses the bottom dock).
            .child(div().flex_1())
            .child(super::tool_hide_button(
                "terminal-hide",
                crate::views::chrome_requests::ChromeRequest::HideBottomDock,
            ))
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // First frame: learn the monospace font's real advance width, so pixel →
        // cell mapping matches whatever font this platform actually resolved.
        self.measure_cell(window);

        // Active-tab content (grid lines, link runs, exited flag, or a spawn error).
        let (lines, links, exited, err) = match self.active_tab() {
            Some(tab) => match &tab.emulator {
                Some(emu) => {
                    let (lines, links) = snapshot(emu);
                    (lines, links, emu.has_exited(), None)
                }
                None => (Vec::new(), Vec::new(), false, tab.error.clone()),
            },
            None => (Vec::new(), Vec::new(), false, None),
        };
        // Keep this frame's link runs so ⌘-clicks can resolve to a URL.
        self.links = links;

        let weak_size = cx.entity().downgrade();
        let weak_move = cx.entity().downgrade();
        let weak_up = cx.entity().downgrade();
        // Canvas overlay: prepaint reports the content bounds back so the active
        // grid sizes to the panel; paint registers frame-scoped *global* mouse
        // handlers that drive drag-selection. Element-level `on_mouse_move` only
        // fires while the pointer is over the hitbox, which is too weak to track
        // a drag — so we follow Zed's terminal and use `window.on_mouse_event`,
        // which keeps extending the selection even past the grid edge.
        let sizer = canvas(
            move |bounds, _window, app| {
                let _ = weak_size.update(app, |this, _cx| this.sync_size(bounds));
            },
            move |_bounds, _state, window, _app| {
                let weak = weak_move.clone();
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, app| {
                    if phase != DispatchPhase::Bubble
                        || e.pressed_button != Some(MouseButton::Left)
                        || app.has_active_drag()
                    {
                        return;
                    }
                    let _ = weak.update(app, |this, cx| {
                        if !this.focus_handle.is_focused(window) {
                            return;
                        }
                        // The child grabbed the mouse on press: forward the drag too.
                        if this.mouse_reported {
                            this.report_mouse(e.position, 0, MouseKind::Drag, &e.modifiers);
                            return;
                        }
                        if !this.dragging {
                            return;
                        }
                        // Track the pointer and arm edge auto-scroll: above the top
                        // scrolls up into history, below the bottom toward the live
                        // view; the poll tick keeps it going while held still.
                        this.drag_pos = Some(e.position);
                        this.autoscroll = match this.grid_bounds {
                            Some(b) if e.position.y < b.origin.y => 1,
                            Some(b) if e.position.y > b.origin.y + b.size.height => -1,
                            _ => 0,
                        };
                        let updated = {
                            let Some(emu) = this.active_tab().and_then(|t| t.emulator.as_ref())
                            else {
                                return;
                            };
                            let Some((point, side)) =
                                this.pixel_to_point(e.position, emu.display_offset())
                            else {
                                return;
                            };
                            emu.update_selection(point, side);
                            true
                        };
                        if updated {
                            this.drag_moved = true;
                            cx.notify();
                        }
                    });
                });

                let weak = weak_up.clone();
                window.on_mouse_event(move |e: &MouseUpEvent, phase, _window, app| {
                    if phase != DispatchPhase::Bubble || e.button != MouseButton::Left {
                        return;
                    }
                    let _ = weak.update(app, |this, cx| {
                        // The child grabbed the mouse: forward the release and stop.
                        if this.mouse_reported {
                            this.report_mouse(e.position, 0, MouseKind::Release, &e.modifiers);
                            this.mouse_reported = false;
                            return;
                        }
                        // A plain single click that never moved clears the empty
                        // selection so it can't swallow the next ⌘C. A double/triple
                        // click selected a word/line and is kept.
                        if this.dragging && !this.drag_moved && this.drag_clearable {
                            if let Some(emu) = this.active_tab().and_then(|t| t.emulator.as_ref()) {
                                emu.clear_selection();
                            }
                            cx.notify();
                        }
                        this.dragging = false;
                        this.autoscroll = 0;
                    });
                });
            },
        )
        .absolute()
        .size_full();

        div()
            .track_focus(&self.focus_handle)
            // The terminal's own key context (see [`TERMINAL_CONTEXT`]) so the
            // Tab/Shift+Tab bindings below win over Root's focus traversal.
            .key_context(TERMINAL_CONTEXT)
            // Tab → child (shell completion / Claude Code autofill); Shift+Tab →
            // back-tab (`\x1b[Z`, Claude Code's permission-mode cycle).
            .on_action(cx.listener(|this, _: &SendTab, _window, _cx| {
                this.send_to_pty(b"\t");
            }))
            .on_action(cx.listener(|this, _: &SendShiftTab, _window, _cx| {
                this.send_to_pty(b"\x1b[Z");
            }))
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::terminal_bg())
            .text_color(theme::terminal_fg())
            .font_family(theme::mono_font())
            .text_size(px(FONT_SIZE))
            // Pin the visual line height to the cell height the PTY is sized against,
            // so rendered rows match the grid exactly (no drift / trailing gap).
            .line_height(px(CELL_H))
            // Left button: ⌘-click opens a link under the cursor; a plain click
            // focuses and begins a text selection.
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus_handle, cx);
                    let started = {
                        let Some(emu) = this.active_tab().and_then(|t| t.emulator.as_ref()) else {
                            return;
                        };
                        let Some((point, side)) =
                            this.pixel_to_point(ev.position, emu.display_offset())
                        else {
                            return;
                        };
                        if is_open_link_chord(&ev.modifiers) {
                            if let Some(url) = this.link_at(point) {
                                cx.open_url(&url);
                            }
                            return;
                        }
                        // When the child app grabs the mouse (vim, htop, Claude
                        // Code's prompt), forward the press so it can act on it —
                        // e.g. position its caret. Shift overrides this to allow a
                        // manual text selection, as in iTerm/Terminal.app.
                        if !ev.modifiers.shift
                            && this.report_mouse(ev.position, 0, MouseKind::Press, &ev.modifiers)
                        {
                            this.mouse_reported = true;
                            return;
                        }
                        // Repeated clicks widen the granularity, like modern
                        // editors: single = cell, double = word, triple = line.
                        let ty = match ev.click_count {
                            0 | 1 => SelectionType::Simple,
                            2 => SelectionType::Semantic,
                            _ => SelectionType::Lines,
                        };
                        emu.start_selection(ty, point, side);
                        ty
                    };
                    this.dragging = true;
                    this.drag_moved = false;
                    this.autoscroll = 0;
                    this.drag_pos = Some(ev.position);
                    // Only a plain single click should vanish on release; a
                    // double/triple click already selected a word/line.
                    this.drag_clearable = started == SelectionType::Simple;
                    cx.notify();
                }),
            )
            // Drag-extend and drag-release are handled globally via
            // `window.on_mouse_event` registered in the canvas paint closure above.
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, cx| {
                // Armed auto-injection (e.g. auto-compact's `/compact`): a keystroke
                // that starts a fresh message submits the injected command first, so
                // the operator's message is processed after it (their keys land in
                // CC's input box while the command runs).
                let inject = this.injection_for(&ev.keystroke);
                let Some(emu) = this.active_tab().and_then(|t| t.emulator.as_ref()) else {
                    return;
                };
                if let Some(text) = &inject {
                    emu.scroll_to_bottom();
                    emu.write_str(text);
                }
                // The copy/paste chords are served here (see [`clipboard_chord`]);
                // everything else falls through to the PTY or the app's keybindings.
                match clipboard_chord(&ev.keystroke) {
                    Some(ClipboardChord::Copy) => {
                        if let Some(text) = emu.selection_to_string() {
                            if !text.is_empty() {
                                cx.write_to_clipboard(ClipboardItem::new_string(text));
                            }
                        }
                        return;
                    }
                    Some(ClipboardChord::Paste) => {
                        if let Some(text) = clipboard_payload(cx) {
                            let payload = if emu.bracketed_paste() {
                                format!("\x1b[200~{text}\x1b[201~")
                            } else {
                                text
                            };
                            emu.scroll_to_bottom();
                            emu.write_str(&payload);
                        }
                        return;
                    }
                    None => {}
                }
                if let Some(bytes) = encode_key(&ev.keystroke) {
                    // Typing clears any selection and snaps back to the live view.
                    emu.clear_selection();
                    emu.scroll_to_bottom();
                    emu.write(bytes);
                }
            }))
            // Mouse wheel scrolls the scrollback history. Convert the delta to whole
            // lines; positive (wheel up) scrolls into older output.
            .on_scroll_wheel(
                cx.listener(|this, ev: &gpui::ScrollWheelEvent, _window, _cx| {
                    let dy = f32::from(ev.delta.pixel_delta(px(CELL_H)).y);
                    let lines = (dy / CELL_H).round() as i32;
                    if lines == 0 {
                        return;
                    }
                    // When the child app reports mouse events, the wheel drives it
                    // (button 64 up / 65 down) instead of the local scrollback.
                    let reporting = this
                        .active_tab()
                        .and_then(|t| t.emulator.as_ref())
                        .is_some_and(|emu| emu.mouse_mode().reporting());
                    if reporting {
                        let button = if lines > 0 { 64 } else { 65 };
                        for _ in 0..lines.unsigned_abs() {
                            this.report_mouse(ev.position, button, MouseKind::Press, &ev.modifiers);
                        }
                    } else if let Some(emu) = this.active_tab().and_then(|t| t.emulator.as_ref()) {
                        emu.scroll(lines);
                    }
                }),
            )
            .when(self.chrome, |d| d.child(self.render_tab_strip(cx)))
            // Grid area (fills the rest, beneath the tab strip).
            .child(
                div()
                    .relative()
                    .flex_1()
                    .w_full()
                    .overflow_hidden()
                    .child(sizer)
                    .when_some(err, |d, err| {
                        d.child(
                            div()
                                .p_3()
                                .text_color(theme::status_color(
                                    moonlight_domain::session::SessionStatus::Errored,
                                ))
                                .child(format!("terminal unavailable: {err}")),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .size_full()
                            .children(lines.into_iter().map(render_line)),
                    )
                    // When the child shell exits, make it visible (the grid otherwise
                    // just freezes on its last frame).
                    .when(exited, |d| {
                        d.child(
                            div()
                                .text_color(theme::text_muted())
                                .text_size(px(11.))
                                .child("[process exited — ＋ opens a new shell]"),
                        )
                    }),
            )
    }
}

/// Render one grid line as a row of color runs (consecutive same-style cells
/// merged into a single span for fewer elements). Selected cells take the
/// selection background; link cells are underlined in the link color.
fn render_line(cells: Vec<RCell>) -> impl IntoElement {
    // A run merges cells sharing fg, effective bg, and link target.
    let mut runs: Vec<(String, Hsla, Hsla, Option<SharedString>)> = Vec::new();
    for cell in cells {
        let bg = if cell.selected {
            theme::terminal_selection_bg()
        } else {
            cell.bg
        };
        match runs.last_mut() {
            Some((text, fg, b, link)) if *fg == cell.fg && *b == bg && *link == cell.link => {
                text.push(cell.ch)
            }
            _ => runs.push((cell.ch.to_string(), cell.fg, bg, cell.link)),
        }
    }

    div()
        .flex()
        .flex_row()
        .children(runs.into_iter().map(|(text, fg, bg, link)| {
            let run = div().bg(bg);
            if link.is_some() {
                run.text_color(theme::terminal_link())
                    .underline()
                    .cursor_pointer()
                    .child(SharedString::from(text))
            } else {
                run.text_color(fg).child(SharedString::from(text))
            }
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::vte::ansi::Rgb;
    use gpui::Keystroke;

    /// Build a row of plain (unstyled, unselected, unlinked) cells from text.
    fn row(text: &str) -> Vec<RCell> {
        text.chars()
            .map(|ch| RCell {
                ch,
                fg: theme::terminal_fg(),
                bg: theme::terminal_bg(),
                selected: false,
                link: None,
            })
            .collect()
    }

    #[test]
    fn find_urls_detects_http_and_https() {
        let s = "see http://a.co and https://b.io/x done";
        let found: Vec<&str> = find_urls(s).iter().map(|&(a, b)| &s[a..b]).collect();
        assert_eq!(found, vec!["http://a.co", "https://b.io/x"]);
    }

    #[test]
    fn find_urls_trims_trailing_punctuation() {
        let s = "go to https://example.com/path).";
        let found: Vec<&str> = find_urls(s).iter().map(|&(a, b)| &s[a..b]).collect();
        assert_eq!(found, vec!["https://example.com/path"]);
    }

    #[test]
    fn find_urls_ignores_bare_scheme_word() {
        assert!(find_urls("the http thing and https stuff").is_empty());
    }

    #[test]
    fn find_urls_safe_with_multibyte_text() {
        // A non-ASCII glyph before the URL must not break byte indexing.
        let s = "▶ https://x.io ◀";
        let found: Vec<&str> = find_urls(s).iter().map(|&(a, b)| &s[a..b]).collect();
        assert_eq!(found, vec!["https://x.io"]);
    }

    #[test]
    fn apply_links_tags_url_cells_only() {
        let mut cells = row("x https://a.io y");
        apply_links(&mut cells);
        let linked: String = cells
            .iter()
            .filter(|c| c.link.is_some())
            .map(|c| c.ch)
            .collect();
        assert_eq!(linked, "https://a.io");
        // Surrounding text stays unlinked.
        assert!(cells[0].link.is_none());
        assert!(cells.last().unwrap().link.is_none());
    }

    #[test]
    fn apply_links_preserves_existing_osc8_link() {
        let mut cells = row("https://plain.io");
        // Pretend the first cell already carries an OSC8 link.
        cells[0].link = Some(SharedString::from("https://osc8.example"));
        apply_links(&mut cells);
        // OSC8 wins on its cell; the rest get the auto-detected URL.
        assert_eq!(cells[0].link.as_deref(), Some("https://osc8.example"));
        assert_eq!(cells[1].link.as_deref(), Some("https://plain.io"));
    }

    #[test]
    fn ansi_spec_color_maps_to_rgb() {
        let c = ansi_to_hsla(AnsiColor::Spec(Rgb { r: 255, g: 0, b: 0 }), true);
        assert_eq!(c, theme::from_rgb8(255, 0, 0));
    }

    #[test]
    fn ansi_named_default_fg_bg() {
        assert_eq!(
            ansi_to_hsla(AnsiColor::Named(NamedColor::Foreground), true),
            theme::terminal_fg()
        );
        assert_eq!(
            ansi_to_hsla(AnsiColor::Named(NamedColor::Background), false),
            theme::terminal_bg()
        );
    }

    #[test]
    fn ansi_named_base_palette() {
        // Red (index 1) resolves to the theme's ANSI red.
        assert_eq!(
            ansi_to_hsla(AnsiColor::Named(NamedColor::Red), true),
            theme::ansi_base(1).unwrap()
        );
    }

    #[test]
    fn ctrl_c_encodes_to_etx() {
        let ks = Keystroke {
            modifiers: gpui::Modifiers {
                control: true,
                ..Default::default()
            },
            key: "c".into(),
            key_char: None,
        };
        assert_eq!(encode_key(&ks), Some(vec![3]));
    }

    #[test]
    fn enter_encodes_to_carriage_return() {
        let ks = Keystroke {
            modifiers: gpui::Modifiers::default(),
            key: "enter".into(),
            key_char: None,
        };
        assert_eq!(encode_key(&ks), Some(b"\r".to_vec()));
    }

    #[test]
    fn ctrl_enter_inserts_soft_newline() {
        // Ctrl+Enter → ESC-CR (meta-enter), the soft newline Claude Code expects;
        // a bare Enter still submits (see `enter_encodes_to_carriage_return`).
        for modifiers in [
            gpui::Modifiers {
                control: true,
                ..Default::default()
            },
            gpui::Modifiers {
                shift: true,
                ..Default::default()
            },
        ] {
            let ks = Keystroke {
                modifiers,
                key: "enter".into(),
                key_char: None,
            };
            assert_eq!(encode_key(&ks), Some(b"\x1b\r".to_vec()));
        }
    }

    #[test]
    fn alt_arrows_encode_word_nav() {
        let alt = gpui::Modifiers {
            alt: true,
            ..Default::default()
        };
        // Alt+Left / Alt+Right → word-back / word-forward (xterm modifier 3).
        assert_eq!(
            encode_key(&ks("left", None, alt)),
            Some(b"\x1b[1;3D".to_vec())
        );
        assert_eq!(
            encode_key(&ks("right", None, alt)),
            Some(b"\x1b[1;3C".to_vec())
        );
    }

    #[test]
    fn modified_nav_uses_csi_form() {
        // Shift → mod 2, Ctrl → mod 5; Home/End take the letter-final form.
        let shift = gpui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let ctrl = gpui::Modifiers {
            control: true,
            ..Default::default()
        };
        assert_eq!(
            encode_key(&ks("up", None, shift)),
            Some(b"\x1b[1;2A".to_vec())
        );
        assert_eq!(
            encode_key(&ks("home", None, ctrl)),
            Some(b"\x1b[1;5H".to_vec())
        );
        // Tilde editing keys carry the modifier before the `~`.
        assert_eq!(
            encode_key(&ks("delete", None, ctrl)),
            Some(b"\x1b[3;5~".to_vec())
        );
    }

    #[test]
    fn unmodified_arrows_unchanged() {
        let none = gpui::Modifiers::default();
        // No modifier → the bare sequence (regression guard for the base table).
        assert_eq!(
            encode_key(&ks("left", None, none)),
            Some(b"\x1b[D".to_vec())
        );
        assert_eq!(
            encode_key(&ks("home", None, none)),
            Some(b"\x1b[H".to_vec())
        );
        assert_eq!(
            encode_key(&ks("delete", None, none)),
            Some(b"\x1b[3~".to_vec())
        );
    }

    #[test]
    fn platform_chord_is_passed_through() {
        let ks = Keystroke {
            modifiers: gpui::Modifiers {
                platform: true,
                ..Default::default()
            },
            key: "c".into(),
            key_char: Some("c".into()),
        };
        assert_eq!(encode_key(&ks), None);
    }

    #[test]
    fn cd_command_quotes_path() {
        let cmd = cd_command(std::path::Path::new("/tmp/my project"));
        assert_eq!(cmd, " cd '/tmp/my project'\r");
    }

    /// Build a bare keystroke with `key` and an optional typed character.
    fn ks(key: &str, key_char: Option<&str>, modifiers: gpui::Modifiers) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.into(),
            key_char: key_char.map(Into::into),
        }
    }

    fn mode(sgr: bool) -> crate::term::emulator::MouseMode {
        crate::term::emulator::MouseMode {
            click: true,
            drag: true,
            motion: false,
            sgr,
        }
    }

    #[test]
    fn encode_mouse_sgr_press_release_and_drag() {
        let none = gpui::Modifiers::default();
        // Left press at viewport cell (col 0, line 0) → 1-based coords, button 0.
        assert_eq!(
            encode_mouse(mode(true), 0, 0, 0, MouseKind::Press, &none),
            Some(b"\x1b[<0;1;1M".to_vec())
        );
        // Release uses the lowercase final byte.
        assert_eq!(
            encode_mouse(mode(true), 0, 4, 2, MouseKind::Release, &none),
            Some(b"\x1b[<0;5;3m".to_vec())
        );
        // A drag sets the +32 motion bit.
        assert_eq!(
            encode_mouse(mode(true), 0, 0, 0, MouseKind::Drag, &none),
            Some(b"\x1b[<32;1;1M".to_vec())
        );
    }

    #[test]
    fn encode_mouse_modifiers_and_legacy() {
        // Shift(+4) on an SGR left press.
        let shift = gpui::Modifiers {
            shift: true,
            ..Default::default()
        };
        assert_eq!(
            encode_mouse(mode(true), 0, 0, 0, MouseKind::Press, &shift),
            Some(b"\x1b[<4;1;1M".to_vec())
        );
        // Legacy X10: ESC [ M then (32+cb)(32+x)(32+y); release reports button 3.
        let none = gpui::Modifiers::default();
        assert_eq!(
            encode_mouse(mode(false), 0, 0, 0, MouseKind::Press, &none),
            Some(vec![0x1b, b'[', b'M', 32, 33, 33])
        );
        assert_eq!(
            encode_mouse(mode(false), 0, 0, 0, MouseKind::Release, &none),
            Some(vec![0x1b, b'[', b'M', 35, 33, 33])
        );
        // Legacy can't carry a coordinate past the 223-column cap.
        assert_eq!(
            encode_mouse(mode(false), 0, 300, 0, MouseKind::Press, &none),
            None
        );
    }

    #[test]
    fn compose_event_accepts_letters_and_paste_only() {
        let none = gpui::Modifiers::default();
        // A typed letter starts a message…
        assert!(is_compose_event(&ks("h", Some("h"), none)));
        // …a digit could be a menu pick (plan continuation) — never inject there…
        assert!(!is_compose_event(&ks("1", Some("1"), none)));
        // …nor on submit/navigation keys or chords.
        assert!(!is_compose_event(&ks("enter", None, none)));
        assert!(!is_compose_event(&ks(
            "c",
            Some("c"),
            gpui::Modifiers {
                control: true,
                ..Default::default()
            }
        )));
        // A paste drops in a draft → compose; the copy chord is not.
        assert!(is_compose_event(&ks("v", Some("v"), paste_mods())));
        assert!(!is_compose_event(&ks("c", Some("c"), copy_mods())));
    }

    /// The host's paste modifiers — ⌘ on macOS, Ctrl elsewhere.
    fn paste_mods() -> gpui::Modifiers {
        if cfg!(target_os = "macos") {
            gpui::Modifiers {
                platform: true,
                ..Default::default()
            }
        } else {
            gpui::Modifiers {
                control: true,
                ..Default::default()
            }
        }
    }

    /// The host's copy modifiers — ⌘ on macOS, Ctrl+Shift elsewhere (bare Ctrl+C
    /// is reserved for SIGINT).
    fn copy_mods() -> gpui::Modifiers {
        if cfg!(target_os = "macos") {
            gpui::Modifiers {
                platform: true,
                ..Default::default()
            }
        } else {
            gpui::Modifiers {
                control: true,
                shift: true,
                ..Default::default()
            }
        }
    }

    #[test]
    fn host_copy_and_paste_chords_are_recognized() {
        assert_eq!(
            clipboard_chord(&ks("v", Some("v"), paste_mods())),
            Some(ClipboardChord::Paste)
        );
        assert_eq!(
            clipboard_chord(&ks("c", Some("c"), copy_mods())),
            Some(ClipboardChord::Copy)
        );
    }

    /// The whole point of the Linux chords: interrupting Claude Code still works,
    /// and Super chords stay with the window manager rather than pasting.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn ctrl_c_stays_sigint_and_super_is_not_a_chord() {
        let ctrl = gpui::Modifiers {
            control: true,
            ..Default::default()
        };
        assert_eq!(clipboard_chord(&ks("c", Some("c"), ctrl)), None);
        // …and it reaches the PTY as the interrupt byte.
        assert_eq!(encode_key(&ks("c", Some("c"), ctrl)), Some(vec![0x03]));

        let sup = gpui::Modifiers {
            platform: true,
            ..Default::default()
        };
        assert_eq!(clipboard_chord(&ks("v", Some("v"), sup)), None);
    }

    /// Shift+Insert is the X11 paste chord that predates Ctrl+Shift+V.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn shift_insert_and_ctrl_shift_v_also_paste() {
        let shift = gpui::Modifiers {
            shift: true,
            ..Default::default()
        };
        assert_eq!(
            clipboard_chord(&ks("insert", None, shift)),
            Some(ClipboardChord::Paste)
        );
        let ctrl_shift = gpui::Modifiers {
            control: true,
            shift: true,
            ..Default::default()
        };
        assert_eq!(
            clipboard_chord(&ks("v", Some("v"), ctrl_shift)),
            Some(ClipboardChord::Paste)
        );
    }

    #[test]
    fn spilled_image_keeps_its_format_extension() {
        let image = Image::from_bytes(ImageFormat::Png, b"\x89PNG\r\n\x1a\n".to_vec());
        let path = spill_image(&image).expect("spill");
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("png"));
        assert_eq!(std::fs::read(&path).expect("read back"), image.bytes);
        // Same bytes → same file, so repeated pastes don't litter the temp dir.
        assert_eq!(spill_image(&image).expect("spill again"), path);
        std::fs::remove_file(&path).ok();
    }
}
