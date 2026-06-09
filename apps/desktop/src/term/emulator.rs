//! `Emulator` — owns one PTY-backed terminal off the UI thread.
//!
//! Wraps `alacritty_terminal`: a child shell on a PTY, an `EventLoop` thread that
//! reads the PTY and feeds the VTE parser into a shared [`Term`] grid, and a small
//! [`EventProxy`] that flips a "dirty" flag so the GPUI view knows to re-render.
//! The view reads the grid under the [`FairMutex`] and writes key input back
//! through the `EventLoopSender`. Nothing here touches GPUI — it is pure terminal
//! infrastructure (NFR4: the engine/UI thread is never blocked on PTY I/O).

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::tty;

/// Grid dimensions for `Term` (the upstream `TermSize` is a test-only helper, so
/// we provide our own `Dimensions`). Scrollback history is configured separately
/// via `Config::scrolling_history`, so `total_lines == screen_lines` here.
struct GridSize {
    cols: usize,
    lines: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// Minimum sensible grid so construction/resize never produces a zero dimension.
const MIN_COLS: usize = 2;
const MIN_LINES: usize = 1;

/// Shared terminal type alias — the grid the view renders and the I/O thread fills.
pub type SharedTerm = Arc<FairMutex<Term<EventProxy>>>;

/// Which mouse events the child app wants reported, and in what encoding.
/// Returned by [`Emulator::mouse_mode`]; see the DECSET modes 1000/1002/1003
/// (report level) and 1006 (SGR encoding).
#[derive(Clone, Copy, Debug, Default)]
pub struct MouseMode {
    /// Report button press/release (DECSET 1000).
    pub click: bool,
    /// Also report motion while a button is held — drag (DECSET 1002).
    pub drag: bool,
    /// Report all motion, button or not (DECSET 1003).
    pub motion: bool,
    /// Encode events as SGR (`ESC[<…M/m`, DECSET 1006) rather than the legacy
    /// X10 `ESC[M` byte triple.
    pub sgr: bool,
}

impl MouseMode {
    /// Whether the app wants *any* mouse reporting — if false, the view keeps the
    /// mouse for local text selection.
    pub fn reporting(&self) -> bool {
        self.click || self.drag || self.motion
    }
}

/// Receives terminal events from the parser/IO thread. Cloned into both the
/// `Term` and the `EventLoop`. Cheap to clone (all `Arc`).
#[derive(Clone)]
pub struct EventProxy {
    dirty: Arc<AtomicBool>,
    exited: Arc<AtomicBool>,
    title: Arc<Mutex<Option<String>>>,
    /// Set once after the event loop is built, so PTY-write replies (cursor/DSR
    /// query responses the parser emits) can be sent back into the PTY.
    sender: Arc<OnceLock<EventLoopSender>>,
}

impl EventProxy {
    fn new() -> Self {
        Self {
            dirty: Arc::new(AtomicBool::new(true)),
            exited: Arc::new(AtomicBool::new(false)),
            title: Arc::new(Mutex::new(None)),
            sender: Arc::new(OnceLock::new()),
        }
    }

    fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Release);
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::Wakeup | Event::MouseCursorDirty | Event::CursorBlinkingChange | Event::Bell => {
                self.mark_dirty()
            }
            // The parser wants bytes written back to the PTY (e.g. query replies).
            Event::PtyWrite(text) => {
                if let Some(sender) = self.sender.get() {
                    let _ = sender.send(Msg::Input(Cow::Owned(text.into_bytes())));
                }
            }
            Event::Title(title) => {
                if let Ok(mut slot) = self.title.lock() {
                    *slot = Some(title);
                }
                self.mark_dirty();
            }
            Event::ResetTitle => {
                if let Ok(mut slot) = self.title.lock() {
                    *slot = None;
                }
                self.mark_dirty();
            }
            Event::Exit | Event::ChildExit(_) => {
                self.exited.store(true, Ordering::Release);
                self.mark_dirty();
            }
            // Clipboard / color / text-area-size requests are not wired in this
            // slice; ignoring them is safe (the shell simply gets no reply).
            _ => {}
        }
    }
}

/// A live PTY-backed terminal. Drop sends a shutdown to the I/O thread.
pub struct Emulator {
    term: SharedTerm,
    sender: EventLoopSender,
    proxy: EventProxy,
    cols: usize,
    lines: usize,
}

impl Emulator {
    /// Spawn the default shell on a PTY in `cwd`, sized to `cols`×`lines` with the
    /// given cell pixel metrics. The I/O thread runs detached; [`Emulator`]'s
    /// `Drop` shuts it down.
    pub fn spawn(
        cwd: Option<PathBuf>,
        cols: usize,
        lines: usize,
        cell_width: u16,
        cell_height: u16,
    ) -> anyhow::Result<Self> {
        // Advertise a sane TERM/COLORTERM to the child (24-bit color).
        tty::setup_env();

        let cols = cols.max(MIN_COLS);
        let lines = lines.max(MIN_LINES);

        let options = tty::Options {
            working_directory: cwd,
            ..Default::default()
        };
        let window_size = WindowSize {
            num_lines: clamp_u16(lines),
            num_cols: clamp_u16(cols),
            cell_width: cell_width.max(1),
            cell_height: cell_height.max(1),
        };

        let pty = tty::new(&options, window_size, 0)
            .map_err(|e| anyhow::anyhow!("failed to open PTY: {e}"))?;

        let proxy = EventProxy::new();
        let term = Term::new(Config::default(), &GridSize { cols, lines }, proxy.clone());
        let term: SharedTerm = Arc::new(FairMutex::new(term));

        let event_loop = EventLoop::new(
            term.clone(),
            proxy.clone(),
            pty,
            options.drain_on_exit,
            false,
        )
        .map_err(|e| anyhow::anyhow!("failed to start terminal event loop: {e}"))?;
        let sender = event_loop.channel();
        // Now that the sender exists, let the proxy answer PTY-write requests.
        let _ = proxy.sender.set(sender.clone());

        // The I/O thread runs detached; shutdown is signalled on Drop.
        let _io = event_loop.spawn();

        Ok(Self {
            term,
            sender,
            proxy,
            cols,
            lines,
        })
    }

    /// The shared grid, for the view to read under the lock during render.
    pub fn term(&self) -> &SharedTerm {
        &self.term
    }

    /// Take-and-clear the dirty flag: `true` means the grid changed since the last
    /// render and the view should `notify`.
    pub fn take_dirty(&self) -> bool {
        self.proxy.dirty.swap(false, Ordering::AcqRel)
    }

    /// Whether the child shell has exited.
    pub fn has_exited(&self) -> bool {
        self.proxy.exited.load(Ordering::Acquire)
    }

    /// Forward raw bytes (decoded key input) to the PTY.
    pub fn write(&self, bytes: Vec<u8>) {
        if let Err(e) = self.sender.send(Msg::Input(Cow::Owned(bytes))) {
            tracing::warn!(error = %e, "terminal PTY write failed");
        }
    }

    /// Forward a UTF-8 string to the PTY (convenience over [`Emulator::write`]).
    pub fn write_str(&self, text: &str) {
        self.write(text.as_bytes().to_vec());
    }

    /// The current visible screen as plain text (one line per grid row). Mirrors the
    /// renderer's `renderable_content` walk but keeps only the characters — used to
    /// detect an interactive prompt (e.g. Claude Code's post-plan continuation menu)
    /// so it can be driven the moment it appears instead of racing a fixed delay.
    pub fn visible_text(&self) -> String {
        let term = self.term.lock();
        let content = term.renderable_content();
        let mut out = String::new();
        let mut current_line: i32 = i32::MIN;
        for indexed in content.display_iter {
            let line = indexed.point.line.0;
            if line != current_line {
                if current_line != i32::MIN {
                    out.push('\n');
                }
                current_line = line;
            }
            out.push(indexed.cell.c);
        }
        out
    }

    /// Scroll the display through the scrollback by `lines` (positive scrolls *up*
    /// into older history, negative back toward the live bottom). The scrollback
    /// buffer is `Config::scrolling_history` deep (10k lines).
    pub fn scroll(&self, lines: i32) {
        if lines != 0 {
            self.term.lock().scroll_display(Scroll::Delta(lines));
            self.proxy.mark_dirty();
        }
    }

    /// Snap the display back to the live bottom (e.g. when the operator types).
    pub fn scroll_to_bottom(&self) {
        self.term.lock().scroll_display(Scroll::Bottom);
        self.proxy.mark_dirty();
    }

    /// How far the viewport is scrolled into history (0 = live bottom). Needed to
    /// map a viewport mouse position back to an absolute grid point.
    pub fn display_offset(&self) -> usize {
        self.term.lock().grid().display_offset()
    }

    /// Begin a selection anchored at `point`. `ty` chooses the granularity:
    /// [`SelectionType::Simple`] for cell-by-cell, [`SelectionType::Semantic`]
    /// for word (double-click), [`SelectionType::Lines`] for the whole line
    /// (triple-click). Semantic/Lines selections expand immediately, so a bare
    /// double/triple click already selects the word/line without dragging.
    pub fn start_selection(&self, ty: SelectionType, point: Point, side: Side) {
        {
            let mut term = self.term.lock();
            term.selection = Some(Selection::new(ty, point, side));
        }
        self.proxy.mark_dirty();
    }

    /// Extend the in-progress selection to `point` (during a drag).
    pub fn update_selection(&self, point: Point, side: Side) {
        {
            let mut term = self.term.lock();
            if let Some(sel) = term.selection.as_mut() {
                sel.update(point, side);
            }
        }
        self.proxy.mark_dirty();
    }

    /// Drop any active selection (e.g. on a bare click or a keystroke).
    pub fn clear_selection(&self) {
        {
            let mut term = self.term.lock();
            term.selection = None;
        }
        self.proxy.mark_dirty();
    }

    /// The current selection rendered to text, for copying. `None` when nothing
    /// is selected; may be empty for a zero-width selection.
    pub fn selection_to_string(&self) -> Option<String> {
        self.term.lock().selection_to_string()
    }

    /// Whether the child has enabled bracketed paste (DECSET 2004) — pastes must
    /// then be wrapped in `ESC[200~ … ESC[201~` so the app treats them literally.
    pub fn bracketed_paste(&self) -> bool {
        self.term.lock().mode().contains(TermMode::BRACKETED_PASTE)
    }

    /// The child's mouse-reporting state: which kinds of mouse events it wants
    /// forwarded as escape sequences, and whether to encode them in SGR. When
    /// [`MouseMode::reporting`] is false the view handles the mouse locally
    /// (text selection); otherwise clicks/drags/wheel are sent to the PTY so a
    /// TUI (vim, htop, Claude Code's prompt) can act on them — e.g. position its
    /// caret on a click.
    pub fn mouse_mode(&self) -> MouseMode {
        let term = self.term.lock();
        let m = term.mode();
        MouseMode {
            click: m.contains(TermMode::MOUSE_REPORT_CLICK),
            drag: m.contains(TermMode::MOUSE_DRAG),
            motion: m.contains(TermMode::MOUSE_MOTION),
            sgr: m.contains(TermMode::SGR_MOUSE),
        }
    }

    /// Resize the grid + PTY to new cell dimensions. No-op if unchanged.
    pub fn resize(&mut self, cols: usize, lines: usize, cell_width: u16, cell_height: u16) {
        let cols = cols.max(MIN_COLS);
        let lines = lines.max(MIN_LINES);
        if cols == self.cols && lines == self.lines {
            return;
        }
        self.cols = cols;
        self.lines = lines;

        self.term.lock().resize(GridSize { cols, lines });
        let window_size = WindowSize {
            num_lines: clamp_u16(lines),
            num_cols: clamp_u16(cols),
            cell_width: cell_width.max(1),
            cell_height: cell_height.max(1),
        };
        if let Err(e) = self.sender.send(Msg::Resize(window_size)) {
            tracing::warn!(error = %e, "terminal PTY resize failed");
        }
        self.proxy.mark_dirty();
    }
}

impl Drop for Emulator {
    fn drop(&mut self) {
        // Ask the I/O thread to stop; ignore errors (it may already be gone).
        let _ = self.sender.send(Msg::Shutdown);
    }
}

fn clamp_u16(v: usize) -> u16 {
    v.min(u16::MAX as usize) as u16
}
