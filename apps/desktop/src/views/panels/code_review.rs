//! Code-review surface — the end-of-work gate (G4 + G5), the **per-hunk
//! rejection-as-feedback** primitive (FR18/FR23), and the **line-anchored comment**
//! loop over what a session changed (FR22).
//!
//! Opens as a center tab when `EngineEvent::ReviewReady` fires (a session declared
//! it is done), and can be opened on demand at any point while a session works.
//!
//! Two bases, because they answer different questions:
//!
//! * **Session changes** (default) — the files *this agent* wrote, from the
//!   [`SessionChangeStore`] ledger the hook's control server fills. Each file is
//!   shown as two panes: its content when the session first touched it, against
//!   what is on disk now (aligned by [`crate::pair_diff`]). Selecting lines in
//!   either pane and commenting builds a review; sending it delivers every pending
//!   comment as one message via `Command::SubmitReview`.
//! * **All changes vs HEAD** — the working tree, via the `git` module, split into
//!   hunks the operator can **Accept** or **Reject**. This base catches writes that
//!   never went through the file tools (a `sed` inside Bash), and it is the only one
//!   whose hunks are valid patches, so selective staging lives here.
//!
//! Both feedback paths (a rejected hunk, a submitted review) reach the session the
//! same way: the engine can't touch the embedded PTY, so it queues through the
//! `SteerControl` port and the cockpit's drainer writes it into the terminal, with
//! the engine recording the audit. A session with no live terminal has its feedback
//! captured but not pushed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command as ProcCommand;

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    anchored, canvas, deferred, div, px, App, Context, Entity, EventEmitter, FocusHandle,
    Focusable, KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, SharedString,
    WeakEntity, Window,
};
use gpui_component::dock::{Panel, PanelEvent, PanelInfo, PanelState, TabPanel};
use gpui_component::input::{Input, InputEvent, InputState};

use super::CloseTab;

use moonlight_domain::changes::{
    Baseline, BaselineGap, CommentAuthor, CommentScope, DiffSide, ReviewComment, TouchedPath,
};
use moonlight_domain::ids::{SessionId, Timestamp};
use moonlight_domain::ports::store::SessionChangeStore;
use moonlight_domain::review::{Feedback, FeedbackOrigin, ReviewHunk};
use moonlight_engine::Command;
use tokio::sync::mpsc::UnboundedSender;

use crate::git::diff::{
    changed_files, file_diff, parse_hunks, restage_file, split_file_diff, ChangedFile,
};
use crate::git::status::GitFileStatus;
use crate::pair_diff::{self, DiffRow, RowKind};
use crate::path_tree;
use crate::views::theme;

gpui::actions!(moonlight_review, [ToggleSearch, CloseSearch]);

/// Key context for a focused review tab.
const REVIEW_CONTEXT: &str = "CodeReview";

/// Bind the review surface's keys. Called once from `init_shell`.
pub fn init_keybindings(cx: &mut App) {
    cx.bind_keys(vec![
        // `secondary-` is ⌘ on macOS, Ctrl elsewhere — the same Find chord the Run
        // window uses, since this is the same gesture on a different body of text.
        KeyBinding::new("secondary-f", ToggleSearch, Some(REVIEW_CONTEXT)),
        KeyBinding::new("escape", CloseSearch, Some(REVIEW_CONTEXT)),
    ]);
}

/// Feedback used when the operator rejects a hunk without typing a reason.
const DEFAULT_HUNK_REASON: &str = "Reconsider this change — it was rejected in review.";

/// Cap on the "after" side read off disk, matching the ledger's own capture cap —
/// past this a two-pane diff is unreadable anyway.
const MAX_CURRENT_BYTES: u64 = 2 * 1024 * 1024;

/// The operator's verdict on a single hunk (visual state for the review pass).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HunkVerdict {
    Accepted,
    Rejected,
}

/// What the diff is computed *against*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffBase {
    /// The session's own ledger: every file this agent wrote, diffed from the
    /// pre-image captured the first time it touched them. Answers "what did this
    /// session change?" — including changes already committed, and excluding the
    /// operator's own edits.
    Session,
    /// The working tree against `HEAD`. Answers "what is dirty here?" — the only
    /// base that catches writes made outside the file tools (a `sed` in Bash), and
    /// the only one whose hunks are valid patches to stage.
    GitHead,
}

impl DiffBase {
    fn label(self) -> &'static str {
        match self {
            DiffBase::Session => "Session changes",
            DiffBase::GitHead => "All changes vs HEAD",
        }
    }
}

/// Row height and gutter width of the diff panes. One rhythm: the fold band
/// aligns its controls to the same gutter, so the eye tracks a single vertical
/// edge down the pane.
const ROW_H: f32 = 17.0;
const GUTTER_W: f32 = 52.0;

/// A finding plus what the operator did with it.
///
/// The note is the point: a finding is a claim, and what the session needs is the
/// claim *plus* the operator's decision about it. Sending findings alone would just
/// forward a robot's opinion; sending the note alone loses what it is about.
struct FindingState {
    finding: crate::review_findings::Finding,
    /// The operator's reply, delivered with the finding.
    note: Option<String>,
    /// Dropped from this pass — wrong, or not worth the session's time.
    dismissed: bool,
}

/// Unchanged rows kept either side of every change when folding. Enough to see
/// what a change sits between, without scrolling past the file.
const FOLD_CONTEXT: usize = 3;

/// How many identifiers a symbol menu offers. A dense line can hold thirty; a menu
/// that long is a wall, not a shortcut.
const SYMBOL_MENU_MAX: usize = 8;

/// How many unchanged lines one click reveals. GitHub's diff uses 20; enough to
/// answer "what is just above this?" without dumping a whole file.
const EXPAND_STEP: usize = 20;

/// One line of the rendered two-pane view: either a diff row, or the marker
/// standing in for the still-hidden part of a collapsed run.
enum ViewRow {
    /// Index into [`CodeReviewPanel::rows`].
    Row(usize),
    /// A saved comment, rendered as a bubble under the last line it covers.
    Comment(usize),
    /// A review finding, rendered under the line it points at.
    Finding(usize),
    /// The composer, rendered under the last line of the current selection.
    Composer,
    Fold {
        /// The run's start row — the key into [`CodeReviewPanel::expanded`].
        run: usize,
        /// The rows still hidden behind this marker.
        hidden: std::ops::Range<usize>,
    },
}

/// How much of a folded run has been revealed, from each end.
///
/// Two counters rather than a bool so a run can be opened from the top, the bottom
/// or both — reading a change usually means wanting a little more of what comes
/// *before* it, not the entire gap.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Revealed {
    top: usize,
    bottom: usize,
}

/// What a "nothing changed on net" verdict was computed from.
///
/// The verdict costs a pre-image read plus a file read, so it is not something to
/// redo every two seconds for every file. These three facts are what would have to
/// move for the answer to change — the session writing again, or anyone at all
/// touching the file on disk, which is why it is not keyed on the touch count alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    touches: u32,
    /// `None` when the file is not on disk — which for a created file is itself the
    /// answer.
    len: Option<u64>,
    mtime: Option<std::time::SystemTime>,
}

/// A line range the operator has selected in one pane, as row indices into the
/// paired rows (not line numbers — a filler row has no line number).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Selection {
    side: DiffSide,
    anchor: usize,
    head: usize,
}

impl Selection {
    fn range(&self) -> std::ops::RangeInclusive<usize> {
        self.anchor.min(self.head)..=self.anchor.max(self.head)
    }

    fn contains(&self, row: usize) -> bool {
        self.range().contains(&row)
    }
}

pub struct CodeReviewPanel {
    session: SessionId,
    /// Repo root of the session under review (its `attached_path`), if known.
    root: Option<PathBuf>,
    /// The agent's "what this covers" summary (latest assistant prose), if observed
    /// before review opened (T4). Shown as a band above the diff.
    summary: Option<String>,
    /// Sends the operator's verdict to the engine; `None` for static/test views.
    commands: Option<UnboundedSender<Command>>,
    /// Why the diff couldn't be loaded (not a repo, git missing) or a delivery note.
    error: Option<String>,
    files: Vec<ChangedFile>,
    selected: Option<usize>,
    /// Unified diff of the selected file (raw; colorized at render time). Kept for
    /// the no-textual-hunks fallback.
    diff: String,
    /// The selected file's diff split into reviewable hunks.
    hunks: Vec<ReviewHunk>,
    /// The selected file's diff preamble (`diff --git`/`---`/`+++`), kept so accepted
    /// hunks can be reassembled into a valid patch and staged ([`restage_file`]).
    diff_header: String,
    /// Per-hunk verdicts for the current file (index → verdict), reset on file switch.
    decisions: HashMap<usize, HunkVerdict>,
    /// When the operator is rejecting hunk `idx`, the reason field they fill in.
    reject_input: Option<(usize, Entity<InputState>)>,
    /// Output of the last full code review, if any.
    review_output: Option<String>,
    review_running: bool,
    /// When the running review started, so the panel can say how long it has been
    /// going. A lane fan-out takes minutes, and a button that only changes its label
    /// is indistinguishable from one that did nothing — which is exactly how a
    /// working review gets reported as broken.
    review_started: Option<std::time::Instant>,
    /// How long the last finished review took, kept for the result band.
    review_took: Option<std::time::Duration>,
    /// Review lanes the operator is missing, surfaced instead of running a review
    /// that would silently cover less. Empty once everything is installed.
    missing_lanes: Vec<&'static crate::review_skills::Lane>,
    /// What the last review found, and the operator's response to each. Transient:
    /// a finding is only true of the code as of that run, and approving the change
    /// closes the pass (see [`Self::approve`]).
    findings: Vec<FindingState>,
    /// What that review covered — read before its findings, since a failed lane is
    /// the difference between "nothing found" and "nothing looked".
    coverage: Option<crate::review_findings::Coverage>,
    /// The finding whose note is being written, if any.
    note_for: Option<(usize, Entity<InputState>)>,
    /// Which base the diff is computed against; defaults to [`DiffBase::Session`]
    /// when the ledger has anything for this session.
    base: DiffBase,
    /// The files this session wrote (the [`DiffBase::Session`] file list).
    ledger: Vec<TouchedPath>,
    /// Ledger files that have ended up back where they started — created then
    /// deleted, edited then edited back. Absolute paths, matching the ledger.
    ///
    /// The ledger records that a file was *written*, which is not the same as it
    /// having *changed*. A session that adds a helper and then removes it again
    /// leaves a row behind with nothing in it to read, and a review list padded with
    /// those is one the operator stops reading carefully.
    unchanged: std::collections::HashSet<String>,
    /// What each verdict in [`unchanged`](Self::unchanged) was computed from, so the
    /// comparison — which reads a whole pre-image and a whole file — only runs again
    /// when something actually moved.
    checked: HashMap<String, Stamp>,
    /// Whether the files with nothing in them are listed anyway.
    show_unchanged: bool,
    /// Paired before/after rows for the selected file — the two-pane view.
    rows: Vec<DiffRow>,
    /// Why the two panes can't be shown (binary, too large, unreadable).
    rows_gap: Option<String>,
    /// The operator's current line selection, if any.
    selection: Option<Selection>,
    /// Whether a press-drag is in flight, so moving over rows extends the selection.
    dragging: bool,
    /// Files hidden from this review — a mirror of the store's list, kept here so
    /// every row render doesn't hit SQLite.
    ///
    /// Scoped to the *session's pass*, not to the project: "ignore for this review"
    /// is a decision about the work being read now, and it dies when the pass is
    /// approved or cleared. A generated file worth skipping today may be the whole
    /// point of tomorrow's review. It does survive closing the tab, because
    /// re-hiding a dozen generated files after a restart is the kind of chore that
    /// makes people stop hiding them at all.
    ignored: std::collections::HashSet<String>,
    /// An open right-click menu: the file it targets and where to draw it.
    ignore_menu: Option<(String, gpui::Point<Pixels>)>,
    /// The path whose revert has been clicked once and awaits confirmation.
    /// Restoring a pre-image overwrites the working file, so it never happens on a
    /// single click.
    revert_armed: Option<String>,
    /// Directories the operator collapsed in the file tree, by full prefix.
    /// Survives a file switch — it describes the tree, not the shown file.
    collapsed_dirs: std::collections::HashSet<String>,
    /// How much of each folded run has been revealed, keyed by the run's start row.
    /// Cleared on file switch — the indices are file-local.
    expanded: HashMap<usize, Revealed>,
    /// Open comment composer for [`selection`](Self::selection).
    comment_input: Option<Entity<InputState>>,
    /// What the open composer is writing about — a line range, the shown file, or
    /// the review as a whole.
    compose_scope: CommentScope,
    /// Whether settled comments are shown. Off by default: a resolved comment has
    /// done its job, and leaving it on the diff is how a review stops being readable.
    show_resolved: bool,
    /// Whether Approve has been asked once and is waiting on an answer about the
    /// comments it would destroy.
    approve_confirm: bool,
    /// The direct message box — say something to the session without leaving the
    /// review. Built on first render (an [`InputState`] needs a window, which the
    /// constructor has no access to) and dropped after sending, which is how it
    /// comes back empty.
    message_input: Option<Entity<InputState>>,
    /// Keeps the message box's Enter subscription alive.
    message_sub: Option<gpui::Subscription>,
    /// Whether the last thing that happened here was a message going out.
    message_sent: bool,
    /// Keeps the composer's Enter subscription alive while it is open.
    comment_enter: Option<gpui::Subscription>,
    /// The id of the comment being edited, when the composer is correcting one
    /// rather than writing a new one.
    editing: Option<String>,
    /// The find bar over the shown diff, when open.
    search: Option<Entity<InputState>>,
    /// Keeps the find bar's Enter/Change subscription alive.
    search_sub: Option<gpui::Subscription>,
    /// What to match, lowercased once rather than per row.
    query: String,
    /// Which match the operator is standing on.
    current_match: usize,
    /// Width of one monospace character, measured once. It is what turns a click's
    /// x into a column, and therefore into the word under the pointer.
    cell_w: Option<f32>,
    /// Bounds of the two-pane scroll region, reported back by a zero-height canvas
    /// each frame — the same trick the terminal uses to map pixels to grid cells.
    panes_bounds: Option<gpui::Bounds<Pixels>>,
    /// An open symbol menu: where to draw it, and the identifiers it offers.
    symbol_menu: Option<(gpui::Point<Pixels>, Vec<String>)>,
    /// This session's persisted review comments (sent and unsent).
    comments: Vec<ReviewComment>,
    /// Writes comments and reads the change ledger; `None` for static/test views.
    changes: Option<Arc<dyn SessionChangeStore>>,
    focus_handle: FocusHandle,
    /// The tab panel this review lives in (captured in [`Panel::on_added_to`]), so
    /// the tab bar's "×" can close this tab — see [`super::tab_title`].
    tab_panel: Option<WeakEntity<TabPanel>>,
    /// Open right-click tab menu, rendered from the panel body (see [`super::TabMenuHost`]).
    tab_menu: Option<super::TabMenu>,
}

impl CodeReviewPanel {
    pub fn new(
        session: SessionId,
        root: Option<PathBuf>,
        summary: Option<String>,
        commands: Option<UnboundedSender<Command>>,
        changes: Option<Arc<dyn SessionChangeStore>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut panel = Self {
            session,
            root,
            summary,
            commands,
            error: None,
            files: Vec::new(),
            selected: None,
            diff: String::new(),
            hunks: Vec::new(),
            diff_header: String::new(),
            decisions: HashMap::new(),
            reject_input: None,
            review_output: None,
            review_running: false,
            review_started: None,
            review_took: None,
            missing_lanes: Vec::new(),
            findings: Vec::new(),
            coverage: None,
            note_for: None,
            base: DiffBase::Session,
            ledger: Vec::new(),
            unchanged: std::collections::HashSet::new(),
            checked: HashMap::new(),
            show_unchanged: false,
            rows: Vec::new(),
            rows_gap: None,
            selection: None,
            dragging: false,
            ignored: std::collections::HashSet::new(),
            ignore_menu: None,
            revert_armed: None,
            collapsed_dirs: std::collections::HashSet::new(),
            expanded: HashMap::new(),
            comment_input: None,
            compose_scope: CommentScope::Line,
            show_resolved: false,
            approve_confirm: false,
            message_input: None,
            message_sub: None,
            message_sent: false,
            comment_enter: None,
            editing: None,
            search: None,
            search_sub: None,
            query: String::new(),
            current_match: 0,
            cell_w: None,
            panes_bounds: None,
            symbol_menu: None,
            comments: Vec::new(),
            changes,
            focus_handle: cx.focus_handle(),
            tab_panel: None,
            tab_menu: None,
        };
        panel.load_comments();
        panel.load_ignored();
        panel.reload(cx);
        // 2s ledger poll (the Commit tool's cadence): the agent keeps writing while
        // this tab is open, so a file it touches after the tab opened has to appear
        // on its own. One indexed query per tick; the file list only rebuilds when
        // the touched set actually changed.
        cx.spawn(async move |this, cx| loop {
            let alive = this.update(cx, |panel: &mut Self, cx| panel.poll_ledger(cx));
            if alive.is_err() {
                break; // panel dropped
            }
            cx.background_executor()
                .timer(std::time::Duration::from_secs(2))
                .await;
        })
        .detach();
        panel
    }

    /// Refresh the ledger, rebuilding the view only when the touched set changed.
    /// Leaves the operator's selection and open composer alone on a no-op tick.
    fn poll_ledger(&mut self, cx: &mut Context<Self>) {
        // A running review has no other clock. Repainting on the tick is what turns
        // "Running…" into "Running… 4m 12s", which is the difference between a slow
        // review and one the operator concludes did nothing.
        if self.review_running {
            cx.notify();
        }
        if self.base != DiffBase::Session {
            return;
        }
        // Before the composer guard below, and not gated on the ledger changing: a
        // file can come back to its pre-image without the ledger moving at all — the
        // panel's own Revert does exactly that, and so does the operator with an
        // editor. The stamp is what notices.
        self.recheck_unchanged(cx);
        // Never refresh out from under someone who is writing a comment. The agent
        // may well be editing the very file under review — rebuilding its rows would
        // discard the composer (and the selection it is anchored to) mid-sentence.
        // The diff can be a few seconds stale; typed text cannot be un-lost.
        if self.comment_input.is_some() {
            return;
        }
        let before: Vec<(String, u32)> = self
            .ledger
            .iter()
            .map(|f| (f.path.clone(), f.touches))
            .collect();
        self.load_ledger();
        let after: Vec<(String, u32)> = self
            .ledger
            .iter()
            .map(|f| (f.path.clone(), f.touches))
            .collect();
        if before == after {
            return;
        }
        // The list is ordered by last touch, so the shown file's index moves as the
        // agent works — follow it by path rather than by position.
        let showing = self.selected.and_then(|i| before.get(i).cloned());
        let found = showing
            .as_ref()
            .and_then(|(path, _)| after.iter().position(|(p, _)| p == path));
        match (showing, found) {
            // Same file, same touch count: only *other* files changed. Re-point the
            // index and leave the panes — and the operator's in-progress selection
            // and comment — exactly as they are.
            (Some((_, was)), Some(idx)) if after[idx].1 == was => {
                self.selected = Some(idx);
            }
            // The shown file was written again: its content moved under us, so the
            // row indices a selection refers to are no longer meaningful.
            (Some(_), Some(idx)) => self.select(idx, cx),
            // The shown file is gone from the ledger (or nothing was shown yet).
            _ if !self.ledger.is_empty() => self.select(0, cx),
            _ => {}
        }
        cx.notify();
    }

    /// Re-read this session's persisted comments (they outlive the tab).
    fn load_comments(&mut self) {
        let Some(changes) = &self.changes else {
            return;
        };
        match changes.comments(&self.session) {
            Ok(comments) => self.comments = comments,
            Err(err) => tracing::warn!(error = %err, "loading review comments failed"),
        }
    }

    /// Re-read what this session's review is hiding (it outlives the tab).
    fn load_ignored(&mut self) {
        let Some(changes) = &self.changes else {
            return;
        };
        match changes.ignored_paths(&self.session) {
            Ok(paths) => self.ignored = paths.into_iter().collect(),
            Err(err) => tracing::warn!(error = %err, "loading the review's ignore list failed"),
        }
    }

    /// Comments not yet delivered to the session — what "Send review" would carry.
    ///
    /// A resolved comment is settled business and does not go in the batch, whether
    /// or not it was ever sent: the operator closed it, and re-raising it would have
    /// the session act on something already dealt with.
    fn unsent(&self) -> Vec<&ReviewComment> {
        pending_comments(&self.comments)
    }

    /// (Re)load the file list for the current base and select the first entry.
    ///
    /// The session ledger is always loaded (it decides whether the Session base is
    /// even offered); the git list only when that base is showing, since it shells
    /// out.
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.diff.clear();
        self.hunks.clear();
        self.diff_header.clear();
        self.decisions.clear();
        self.reject_input = None;
        self.rows.clear();
        self.rows_gap = None;
        self.selection = None;
        self.comment_input = None;
        self.load_ledger();
        // Nothing in the ledger (an unhooked or idle session) → fall back to git so
        // the panel still shows something rather than an empty list.
        if self.base == DiffBase::Session && self.ledger.is_empty() {
            self.base = DiffBase::GitHead;
        }
        match self.base {
            DiffBase::Session => {
                self.error = None;
                self.recheck_unchanged(cx);
                if !self.ledger.is_empty() {
                    self.select(0, cx);
                }
            }
            DiffBase::GitHead => self.reload_git(cx),
        }
    }

    /// Load this session's file-change ledger (the [`DiffBase::Session`] list).
    ///
    /// Names and counts only — this runs on the UI thread every poll tick, and the
    /// pre-images behind it run to hundreds of KB per session. The baseline for the
    /// one file on screen is fetched separately, in [`select_ledger_file`](Self::select_ledger_file).
    fn load_ledger(&mut self) {
        let Some(changes) = &self.changes else {
            self.ledger.clear();
            return;
        };
        match changes.touched_paths(&self.session) {
            Ok(mut files) => {
                // The store answers most-recently-touched first, which is right for a
                // glance but wrong for a list you click: while a session works, rows
                // reorder under the pointer between press and release and the click
                // is lost (or worse, lands on whichever file moved into that slot).
                // Path order is stable, and it is the order the tree wants anyway.
                files.sort_by(|a, b| a.path.cmp(&b.path));
                self.ledger = files;
            }
            Err(err) => {
                tracing::warn!(error = %err, "loading the session change ledger failed");
                self.ledger.clear();
            }
        }
    }

    /// Re-decide which ledger files have nothing left in them, for the files where
    /// that could have changed.
    ///
    /// Stamping is cheap (one `stat` per file) and happens on the tick; the verdict
    /// is expensive (a pre-image out of SQLite plus the whole file off disk) and
    /// happens in the background, only for the files whose stamp moved. So a session
    /// writing one file does not re-read a hundred.
    fn recheck_unchanged(&mut self, cx: &mut Context<Self>) {
        let Some(changes) = self.changes.clone() else {
            return;
        };
        // Files that left the ledger take their verdict with them.
        let live: std::collections::HashSet<&str> =
            self.ledger.iter().map(|f| f.path.as_str()).collect();
        self.checked.retain(|path, _| live.contains(path.as_str()));
        self.unchanged.retain(|path| live.contains(path.as_str()));

        let stale: Vec<(String, Stamp)> = self
            .ledger
            .iter()
            .filter_map(|file| {
                let stamp = stamp_of(&file.path, file.touches);
                (self.checked.get(&file.path) != Some(&stamp)).then(|| (file.path.clone(), stamp))
            })
            .collect();
        if stale.is_empty() {
            return;
        }

        let session = self.session.clone();
        cx.spawn(async move |weak, cx| {
            let verdicts = cx
                .background_executor()
                .spawn(async move {
                    stale
                        .into_iter()
                        .map(|(path, stamp)| {
                            let same = is_back_to_baseline(changes.as_ref(), &session, &path);
                            (path, stamp, same)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = weak.update(cx, |this, cx| {
                for (path, stamp, same) in verdicts {
                    // A file that moved again while we were reading gets stamped
                    // with what we actually looked at, so the next tick notices and
                    // asks again rather than trusting a stale answer.
                    this.checked.insert(path.clone(), stamp);
                    if same {
                        this.unchanged.insert(path);
                    } else {
                        this.unchanged.remove(&path);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Whether this ledger file has nothing left to review. Keyed by absolute path,
    /// the way the ledger and the store both name a file.
    fn is_unchanged(&self, path: &str) -> bool {
        self.unchanged.contains(path)
    }

    /// Load the working-tree change list for the session's repo.
    fn reload_git(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            self.error = Some("no repo path for this session".to_string());
            return;
        };
        match changed_files(&root) {
            Some(files) => {
                self.error = None;
                self.files = files;
                if !self.files.is_empty() {
                    self.select(0, cx);
                }
            }
            None => {
                self.error = Some("not a git repository (or git unavailable)".to_string());
                self.files.clear();
            }
        }
    }

    /// Switch which base the diff is computed against, reloading its file list.
    fn set_base(&mut self, base: DiffBase, cx: &mut Context<Self>) {
        if self.base == base {
            return;
        }
        self.base = base;
        self.files.clear();
        self.reload(cx);
        cx.notify();
    }

    /// The label shown for file `idx` — repo-relative where we can, so the list
    /// stays readable.
    fn file_label(&self, idx: usize) -> String {
        match self.base {
            DiffBase::Session => {
                let path = self.ledger.get(idx).map(|f| f.path.as_str()).unwrap_or("");
                self.root
                    .as_ref()
                    .and_then(|root| path.strip_prefix(&format!("{}/", root.display())))
                    .unwrap_or(path)
                    .to_string()
            }
            DiffBase::GitHead => self.files.get(idx).map(|f| f.display()).unwrap_or_default(),
        }
    }

    /// Show file `idx`. Resets per-file state (hunk indices and row indices are both
    /// file-local, as is the selection).
    fn select(&mut self, idx: usize, cx: &mut Context<Self>) {
        self.selection = None;
        self.dragging = false;
        self.expanded.clear();
        self.comment_input = None;
        match self.base {
            DiffBase::Session => self.select_ledger_file(idx, cx),
            DiffBase::GitHead => self.select_git_file(idx, cx),
        }
    }

    /// Build the two panes for a ledger file: the baseline captured at first touch
    /// on the left, what is on disk now on the right.
    fn select_ledger_file(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(file) = self.ledger.get(idx).cloned() else {
            return;
        };
        self.selected = Some(idx);
        self.rows.clear();
        self.hunks.clear();
        self.diff.clear();

        // The pre-image is fetched here and nowhere else — one file's worth, only
        // when it is about to be shown.
        let baseline = match self
            .changes
            .as_ref()
            .map(|changes| changes.baseline(&self.session, &file.path))
        {
            Some(Ok(Some(baseline))) => baseline,
            Some(Ok(None)) | None => {
                self.rows_gap = Some(
                    "This file left the session's ledger. Switch base to see it against HEAD."
                        .into(),
                );
                cx.notify();
                return;
            }
            Some(Err(err)) => {
                self.rows_gap = Some(format!("Couldn't read this file's previous state: {err}"));
                cx.notify();
                return;
            }
        };
        let Some(before) = baseline.text().map(str::to_string) else {
            self.rows_gap = Some(match &baseline {
                Baseline::Unavailable {
                    reason: BaselineGap::TooLarge,
                } => {
                    "Too large to snapshot when the session first touched it. Switch base to diff it against HEAD.".into()
                }
                Baseline::Unavailable {
                    reason: BaselineGap::Binary,
                } => "Binary file — there is no line diff to show.".into(),
                _ => "The previous state was unreadable. Switch base to diff this against HEAD.".into(),
            });
            cx.notify();
            return;
        };
        match read_current(&file.path) {
            Ok(after) => {
                self.rows = pair_diff::side_by_side(&before, &after);
                self.rows_gap = (!pair_diff::has_changes(&self.rows)).then(|| {
                    "No net change — this file matches what it held before the session touched it."
                        .to_string()
                });
                self.error = None;
            }
            Err(reason) => {
                self.rows_gap = Some(reason);
            }
        }
        cx.notify();
    }

    /// Load and show the git diff of file `idx`, split into hunks.
    fn select_git_file(&mut self, idx: usize, cx: &mut Context<Self>) {
        let (Some(root), Some(file)) = (self.root.clone(), self.files.get(idx).cloned()) else {
            return;
        };
        self.selected = Some(idx);
        self.rows.clear();
        self.rows_gap = None;
        self.diff = file_diff(&root, &file).unwrap_or_else(|| "(diff unavailable)".to_string());
        let (header, _) = split_file_diff(&self.diff);
        self.diff_header = header;
        self.hunks = parse_hunks(&self.diff, &self.session, &file.display());
        self.decisions.clear();
        self.reject_input = None;
        self.error = None;
        cx.notify();
    }

    /// Stage exactly the accepted hunks of the selected file into the git index
    /// (idempotent; recomputed on every accept/reject). Surfaces a git error in the
    /// panel rather than failing silently.
    fn restage_selected(&mut self) {
        let (Some(root), Some(file)) = (
            self.root.clone(),
            self.selected.and_then(|i| self.files.get(i).cloned()),
        ) else {
            return;
        };
        let accepted: Vec<String> = self
            .hunks
            .iter()
            .enumerate()
            .filter(|(i, _)| self.decisions.get(i) == Some(&HunkVerdict::Accepted))
            .map(|(_, h)| h.diff.clone())
            .collect();
        let untracked = file.status == GitFileStatus::Untracked;
        match restage_file(
            &root,
            &file.display(),
            &self.diff_header,
            &accepted,
            untracked,
        ) {
            Ok(()) => self.error = None,
            Err(e) => self.error = Some(format!("staging failed: {e}")),
        }
    }

    /// Mark a hunk accepted (visual only in v1 — selective staging is a follow-on;
    /// the steering loop is reject→feedback, which is what changes agent behavior).
    fn accept_hunk(&mut self, idx: usize, cx: &mut Context<Self>) {
        self.decisions.insert(idx, HunkVerdict::Accepted);
        self.restage_selected();
        cx.notify();
    }

    /// Open the inline reason field for rejecting hunk `idx`.
    fn begin_hunk_reject(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("What's wrong with this hunk? (sent to the agent)")
        });
        input.focus_handle(cx).focus(window, cx);
        self.reject_input = Some((idx, input));
        cx.notify();
    }

    /// Back out of the reason field, restoring the hunk's Accept/Reject controls.
    fn cancel_reject(&mut self, cx: &mut Context<Self>) {
        self.reject_input = None;
        cx.notify();
    }

    /// Send the rejection for hunk `idx`: emit `Command::RejectHunk`, which the
    /// engine delivers through the `SteerControl` port → the UI drainer writes it
    /// into the session's embedded terminal (Option C). The panel optimistically
    /// marks the hunk rejected.
    fn send_hunk_reject(&mut self, idx: usize, cx: &mut Context<Self>) {
        let reason = self
            .reject_input
            .as_ref()
            .filter(|(i, _)| *i == idx)
            .map(|(_, input)| input.read(cx).value().trim().to_string())
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| DEFAULT_HUNK_REASON.to_string());
        let file = self
            .hunks
            .get(idx)
            .map(|h| h.file_path.clone())
            .unwrap_or_default();
        let message = format!("Review feedback — {file}: {reason}");
        self.reject_input = None;
        self.send(Command::RejectHunk {
            feedback: Feedback {
                session_id: self.session.clone(),
                message,
                origin: FeedbackOrigin::HunkRejection,
            },
        });
        self.decisions.insert(idx, HunkVerdict::Rejected);
        // A rejected hunk is not staged — recompute the index from the accepted set
        // (un-stages it if it had been accepted before).
        self.restage_selected();
        cx.notify();
    }

    /// Run the full code review over the session's repo in the background and show
    /// its output (G5). Shells `claude -p "/full-code-review"` in the repo root.
    ///
    /// Refuses to start when a lane is missing: the skill is an orchestrator, and
    /// without its lanes it reviews with whatever it can find and returns a thinner
    /// report without saying so. Better to name what is absent than to hand back a
    /// review that quietly covered less than it claims.
    fn run_full_review(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            self.error =
                Some("No repo path for this session, so there is nothing to review.".into());
            cx.notify();
            return;
        };
        let missing = crate::review_skills::missing();
        if !missing.is_empty() {
            self.missing_lanes = missing;
            cx.notify();
            return;
        }
        if self.review_running {
            return;
        }
        let Some(changes) = self.changes.clone() else {
            self.error = Some("No change ledger for this session.".into());
            cx.notify();
            return;
        };

        // Only the cheap part happens here. Reading the pre-images is a SQLite read
        // plus a JSON decode PER FILE, each up to the capture cap — on a session
        // with a large ledger that is tens of megabytes, and doing it on the render
        // thread froze the window so thoroughly that the click looked ignored.
        let entries: Vec<(String, String)> = self
            .ledger
            .iter()
            .enumerate()
            .map(|(i, file)| (self.file_label(i), file.path.clone()))
            // A file that is back where it started has an empty diff. Handing it to
            // the lanes spends a review on nothing and pads the scope the report is
            // measured against.
            .filter(|(label, path)| !self.is_ignored(label) && !self.is_unchanged(path))
            .collect();
        if entries.is_empty() {
            self.error = Some("This session has no recorded changes to review.".into());
            cx.notify();
            return;
        }

        self.review_running = true;
        self.review_started = Some(std::time::Instant::now());
        self.review_took = None;
        self.review_output = None;
        self.findings.clear();
        self.coverage = None;
        self.note_for = None;
        self.error = None;
        cx.notify();

        let session = self.session.clone();
        cx.spawn(async move |weak, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut files = Vec::new();
                    for (label, path) in entries {
                        match changes.baseline(&session, &path) {
                            Ok(Some(baseline)) => files.push((label, path, baseline)),
                            // A file whose baseline has gone is not reviewable as a
                            // change; skipping it here is reported by the skill's
                            // coverage rather than silently narrowing the review.
                            Ok(None) => continue,
                            Err(err) => return Err(format!("Couldn't read a baseline: {err}")),
                        }
                    }
                    if files.is_empty() {
                        return Err("None of this session's files still have a baseline.".into());
                    }
                    let scope = write_scope(&session, &root, &files)?;
                    Ok(run_full_review_blocking(&root, &scope))
                })
                .await;

            let _ = weak.update(cx, |this, cx| {
                // Cleared on every path, so a failure can never wedge the button.
                this.review_running = false;
                this.review_took = this.review_started.take().map(|at| at.elapsed());
                match result {
                    Err(err) => this.error = Some(err),
                    Ok(output) => match crate::review_findings::parse(&output) {
                        Ok(report) => {
                            this.coverage = Some(report.coverage);
                            this.findings = crate::review_findings::sorted(report.findings)
                                .into_iter()
                                .map(|finding| FindingState {
                                    finding,
                                    note: None,
                                    dismissed: false,
                                })
                                .collect();
                            this.review_output = None;
                        }
                        // Keep the raw output on a parse failure: a review that
                        // didn't emit properly still did work, and the operator
                        // needs to see what it said rather than a bare error.
                        Err(err) => this.review_output = Some(format!("{err}\n\n{output}")),
                    },
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Press on a row: start a selection there, or — with Shift — extend the
    /// existing one. Either way the drag is armed, so moving over further rows keeps
    /// growing the range. Pressing in the other pane starts over, since a comment is
    /// anchored to one side's line numbers.
    fn begin_select(&mut self, side: DiffSide, row: usize, extend: bool, cx: &mut Context<Self>) {
        self.selection = match self.selection {
            Some(sel) if extend && sel.side == side => Some(Selection { head: row, ..sel }),
            _ => Some(Selection {
                side,
                anchor: row,
                head: row,
            }),
        };
        self.dragging = true;
        self.comment_input = None;
        cx.notify();
    }

    /// Grow the in-flight selection to `row` while the button is held.
    fn extend_select(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(sel) = self.selection else {
            return;
        };
        if sel.head == row {
            return;
        }
        // Extends from **either** pane. Dragging is a vertical gesture and the rows
        // are 17px tall, so any slightly diagonal drag crosses into the other
        // column; refusing those made a multi-line selection feel like it kept
        // giving up. The anchor still decides which side's line numbers the comment
        // refers to — only the pointer's column is ignored.
        self.selection = Some(Selection { head: row, ..sel });
        cx.notify();
    }

    /// The lines to render: every changed row plus its context, with long unchanged
    /// runs collapsed into a single marker unless the operator expanded them.
    fn view_rows(&self) -> Vec<ViewRow> {
        let runs = pair_diff::foldable_runs(&self.rows, FOLD_CONTEXT);
        // Where each saved comment hangs, and where the composer goes: under the
        // LAST row its range covers, the way GitHub tucks a thread beneath the code
        // it is about.
        let mut threads: HashMap<usize, Vec<usize>> = HashMap::new();
        for (i, comment) in self.comments.iter().enumerate() {
            if comment.is_resolved() && !self.show_resolved {
                continue;
            }
            if let Some(row) = self.anchor_row(comment) {
                threads.entry(row).or_default().push(i);
            }
        }
        // Only a line comment is composed on the code. A file- or review-scoped one
        // is written in the notes band, where it will be read.
        let composer_row = self
            .comment_input
            .as_ref()
            .filter(|_| self.compose_scope.is_line())
            .and(self.selection)
            .map(|sel| *sel.range().end());
        // Findings hang off the line they name, in the file being shown.
        let shown_file = self.shown_label();
        let mut found: HashMap<usize, Vec<usize>> = HashMap::new();
        for (i, state) in self.findings.iter().enumerate() {
            if state.dismissed || state.finding.file != shown_file {
                continue;
            }
            if let Some(row) = self.row_of_line(state.finding.line) {
                found.entry(row).or_default().push(i);
            }
        }

        let mut out = Vec::new();
        let mut row = 0;
        // Emit a code row plus anything hanging off it.
        let push_row = |out: &mut Vec<ViewRow>, i: usize| {
            out.push(ViewRow::Row(i));
            for finding in found.get(&i).into_iter().flatten() {
                out.push(ViewRow::Finding(*finding));
            }
            for comment in threads.get(&i).into_iter().flatten() {
                out.push(ViewRow::Comment(*comment));
            }
            if composer_row == Some(i) {
                out.push(ViewRow::Composer);
            }
        };
        while row < self.rows.len() {
            // A run holding a finding — or a search match — is never folded. Hiding
            // either would leave the operator looking for something the panel is
            // deliberately not showing.
            let matched = self.matches();
            let Some(run) = runs.iter().find(|run| {
                run.start == row
                    && !found.keys().any(|r| run.contains(r))
                    && !matched.iter().any(|r| run.contains(r))
            }) else {
                push_row(&mut out, row);
                row += 1;
                continue;
            };
            let shown = self.expanded.get(&run.start).copied().unwrap_or_default();
            let hidden = hidden_slice(run.clone(), shown);
            let head_end = hidden.as_ref().map(|h| h.start).unwrap_or(run.end);
            for i in run.start..head_end {
                push_row(&mut out, i);
            }
            if let Some(hidden) = hidden {
                let tail_start = hidden.end;
                out.push(ViewRow::Fold {
                    run: run.start,
                    hidden,
                });
                for i in tail_start..run.end {
                    push_row(&mut out, i);
                }
            }
            row = run.end;
        }
        out
    }

    /// The repo-relative label of the file on screen, or empty when none is.
    /// Comments and findings are addressed by this label, so everything that asks
    /// "is this about what I am looking at?" asks it here.
    fn shown_label(&self) -> String {
        self.selected
            .map(|i| self.file_label(i))
            .unwrap_or_default()
    }

    /// What the last review did, or how long the running one has been going.
    ///
    /// Without this the surface has no way to say "the review finished and found
    /// nothing" — it simply goes quiet, which reads exactly like a review that never
    /// ran. Coverage belongs here too: a lane that failed is the difference between
    /// nothing found and nothing looked.
    fn review_band(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if self.review_running {
            let elapsed = self
                .review_started
                .map(|at| format!(" · {}", fmt_elapsed(at.elapsed())))
                .unwrap_or_default();
            return Some(
                Self::band(theme::accent())
                    .child(
                        div()
                            .text_size(theme::text_xs())
                            .text_color(theme::accent())
                            .child(format!("Full code review running{elapsed}")),
                    )
                    .child(
                        div()
                            .text_size(theme::text_2xs())
                            .text_color(theme::text_muted())
                            .child(
                                "It fans out to review lanes in a separate Claude Code run, so minutes is normal. Findings land on the diff when it returns.",
                            ),
                    )
                    .into_any_element(),
            );
        }

        let coverage = self.coverage.as_ref()?;
        let live = self.findings.iter().filter(|f| !f.dismissed).count();
        let took = self
            .review_took
            .map(|d| format!(" in {}", fmt_elapsed(d)))
            .unwrap_or_default();
        let headline = match live {
            0 => format!("Review finished{took} — no findings"),
            1 => format!("Review finished{took} — 1 finding"),
            n => format!("Review finished{took} — {n} findings"),
        };
        // A lane that failed is louder than the count: a clean-looking review that
        // only half ran is the one failure mode worth interrupting for.
        let failed = !coverage.lanes_failed.is_empty();
        let hue = if failed {
            theme::git_conflict()
        } else if live == 0 {
            theme::git_added()
        } else {
            theme::accent()
        };
        let mut lines = Vec::new();
        if !coverage.lanes_run.is_empty() {
            lines.push(format!("Lanes: {}", coverage.lanes_run.join(", ")));
        }
        if failed {
            lines.push(format!(
                "Failed: {} — this review covered less than it looks like.",
                coverage.lanes_failed.join(", ")
            ));
        }
        if !coverage.excluded.is_empty() {
            lines.push(format!("Excluded: {}", coverage.excluded.join(", ")));
        }
        if !coverage.notes.trim().is_empty() {
            lines.push(coverage.notes.trim().to_string());
        }
        if coverage.lanes_run.is_empty() && !failed {
            lines.push(
                "No lane reported running, so treat this as a review that did not happen."
                    .to_string(),
            );
        }

        Some(
            Self::band(hue)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .text_size(theme::text_xs())
                                .text_color(hue)
                                .child(headline),
                        )
                        .child(Self::pill(
                            ("review-band-dismiss", 0),
                            "Dismiss",
                            theme::text_muted(),
                            move |this, _w, cx| {
                                this.coverage = None;
                                cx.notify();
                            },
                            cx,
                        )),
                )
                .children(lines.into_iter().map(|line| {
                    div()
                        .text_size(theme::text_2xs())
                        .text_color(theme::text_muted())
                        .child(line)
                }))
                .into_any_element(),
        )
    }

    /// The message box: say something to the session from inside the review.
    ///
    /// Comments are a *batch* — they wait for Send, and arrive as one considered
    /// message. That is right for review notes and wrong for everything else: "stop,
    /// you're editing the generated file" has to go now, and walking to the terminal
    /// tab to type it is how a reviewer loses the thread of what they were reading.
    /// So the two live side by side and stay distinct: this line goes immediately and
    /// is recorded as steering, not as a review comment.
    fn message_box(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        // Built here rather than in the constructor: an input needs a window, and a
        // panel is constructed without one.
        if self.message_input.is_none() {
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder(
                    "Say something to this session (Enter to send, Shift+Enter for a new line)",
                )
            });
            self.message_sub = Some(cx.subscribe(
                &input,
                |this, _state, event: &InputEvent, cx| match event {
                    InputEvent::PressEnter { shift, .. } if !shift => this.send_message(cx),
                    // Typing again means the last delivery is old news; the receipt
                    // goes rather than hanging over a message that hasn't gone yet.
                    InputEvent::Change if this.message_sent => {
                        this.message_sent = false;
                        cx.notify();
                    }
                    _ => {}
                },
            ));
            self.message_input = Some(input);
        }
        let input = self.message_input.clone().expect("just built");

        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w(px(0.)).child(Input::new(&input)))
                    .child(Self::pill(
                        ("review-message-send", 0),
                        "Send",
                        theme::accent(),
                        move |this, _w, cx| this.send_message(cx),
                        cx,
                    )),
            )
            .children(self.message_sent.then(|| {
                div()
                    .text_size(theme::text_2xs())
                    .text_color(theme::text_muted())
                    .child(
                        "Sent to the session's prompt. It arrives when the session is ready for input.",
                    )
            }))
            .into_any_element()
    }

    /// Deliver what is in the message box, immediately and on its own.
    fn send_message(&mut self, cx: &mut Context<Self>) {
        let text = self
            .message_input
            .as_ref()
            .map(|input| input.read(cx).value().trim().to_string())
            .unwrap_or_default();
        if text.is_empty() {
            return;
        }
        self.send(Command::Steer {
            session: self.session.clone(),
            message: text,
        });
        // Dropped, not blanked: clearing an input needs the window, and the next
        // frame rebuilds this one empty anyway.
        self.message_input = None;
        self.message_sub = None;
        self.message_sent = true;
        cx.notify();
    }

    /// Comments that have no line to sit on: the ones about the whole review, the
    /// ones about the shown file, and the ones whose line has left the diff.
    ///
    /// The last group is why this exists at all. A comment anchored to a line the
    /// session has since deleted used to render nowhere — silently, so the operator's
    /// own note simply vanished. Somewhere to put it is the difference between a
    /// review you can trust and one that quietly drops things.
    fn notes_band(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let shown = self.shown_label();
        let visible = |c: &ReviewComment| self.show_resolved || !c.is_resolved();
        let mut general = Vec::new();
        let mut on_file = Vec::new();
        let mut drifted = Vec::new();
        let mut resolved = 0usize;
        for (i, c) in self.comments.iter().enumerate() {
            if c.is_resolved() {
                resolved += 1;
            }
            if !visible(c) {
                continue;
            }
            match c.scope {
                CommentScope::Review => general.push(i),
                CommentScope::File if c.path == shown => on_file.push(i),
                // Only meaningful against the panes: the git base has no paired rows
                // to look the line up in, and "not in this diff" would then be true
                // of every comment ever written.
                CommentScope::Line
                    if self.base == DiffBase::Session
                        && !shown.is_empty()
                        && c.path == shown
                        && self.anchor_row(c).is_none() =>
                {
                    drifted.push(i)
                }
                _ => {}
            }
        }
        let composing = self.comment_input.is_some() && !self.compose_scope.is_line();
        if general.is_empty()
            && on_file.is_empty()
            && drifted.is_empty()
            && !composing
            && resolved == 0
        {
            return None;
        }

        let show_resolved = self.show_resolved;
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .text_size(theme::text_2xs())
                    .text_color(theme::text_muted())
                    .child("Notes"),
            )
            .children((resolved > 0).then(|| {
                Self::pill(
                    ("notes-resolved", 0),
                    if show_resolved {
                        format!("{resolved} resolved · hide")
                    } else {
                        format!("{resolved} resolved · show")
                    },
                    theme::git_added(),
                    move |this, _w, cx| {
                        this.show_resolved = !this.show_resolved;
                        cx.notify();
                    },
                    cx,
                )
            }));

        let section = |label: String, hue: gpui::Hsla| {
            div()
                .text_size(theme::text_2xs())
                .text_color(hue)
                .child(label)
        };

        Some(
            Self::band(theme::border_strong())
                .child(header)
                .children(
                    (!general.is_empty())
                        .then(|| section("On this review".to_string(), theme::text_muted())),
                )
                .children(
                    general
                        .into_iter()
                        .map(|i| self.comment_bubble(i, false, cx)),
                )
                .children(
                    (!on_file.is_empty())
                        .then(|| section(format!("On {shown}"), theme::text_muted())),
                )
                .children(
                    on_file
                        .into_iter()
                        .map(|i| self.comment_bubble(i, false, cx)),
                )
                .children((!drifted.is_empty()).then(|| {
                    section(
                        "No longer in this diff — the lines they were on have gone".to_string(),
                        theme::git_conflict(),
                    )
                }))
                .children(
                    drifted
                        .into_iter()
                        .map(|i| self.comment_bubble(i, false, cx)),
                )
                .children(composing.then(|| self.inline_composer(false, cx)))
                .into_any_element(),
        )
    }

    /// The shell every notice in this panel uses: a tinted left edge, so bands stack
    /// without each inventing its own frame.
    fn band(hue: gpui::Hsla) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .rounded(theme::radius_sm())
            .border_l_2()
            .border_color(hue)
            .bg(theme::surface_raised())
            .px_2()
            .py(px(5.))
    }

    /// Findings not visible on the current file: those naming another file, and
    /// those whose line no longer exists in this diff.
    ///
    /// They are listed rather than dropped. A finding the operator never sees is
    /// indistinguishable from one that was never raised.
    fn offscreen_findings(&self) -> (usize, Vec<usize>) {
        let shown = self.shown_label();
        let mut elsewhere = 0;
        let mut unanchored = Vec::new();
        for (i, state) in self.findings.iter().enumerate() {
            if state.dismissed {
                continue;
            }
            if state.finding.file != shown {
                elsewhere += 1;
            } else if self.row_of_line(state.finding.line).is_none() {
                unanchored.push(i);
            }
        }
        (elsewhere, unanchored)
    }

    /// The band naming findings the panes cannot show.
    fn offscreen_band(&self) -> Option<gpui::AnyElement> {
        let (elsewhere, unanchored) = self.offscreen_findings();
        if elsewhere == 0 && unanchored.is_empty() {
            return None;
        }
        let rows: Vec<gpui::AnyElement> = unanchored
            .iter()
            .map(|i| {
                let f = &self.findings[*i].finding;
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .text_size(theme::text_xs())
                    .child(
                        div()
                            .font_family(theme::mono_font())
                            .text_color(theme::text_muted())
                            .child(format!("{}:{}", f.file, f.line)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_color(theme::text_secondary())
                            .child(f.summary.clone()),
                    )
                    .into_any_element()
            })
            .collect();

        Some(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .rounded(theme::radius_sm())
                .border_1()
                .border_color(theme::border_subtle())
                .bg(theme::surface_raised())
                .px_2()
                .py(px(4.))
                .children((elsewhere > 0).then(|| {
                    div()
                        .text_size(theme::text_xs())
                        .text_color(theme::text_muted())
                        .child(if elsewhere == 1 {
                            "1 finding on another file — open it to reply.".to_string()
                        } else {
                            format!("{elsewhere} findings on other files — open them to reply.")
                        })
                }))
                .children((!rows.is_empty()).then(|| {
                    div()
                        .text_size(theme::text_xs())
                        .text_color(theme::git_conflict())
                        .child(
                            "These name lines that are not in this diff — the file changed since the review:",
                        )
                }))
                .children(rows)
                .into_any_element(),
        )
    }

    /// The row carrying `line` on the after side — where a finding points.
    fn row_of_line(&self, line: u32) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| row.right.as_ref().is_some_and(|l| l.number == line))
    }

    /// The row a comment hangs under: the one carrying the last line of its range,
    /// on the side it was anchored to. `None` when that line is no longer in the
    /// diff — the file changed under a comment written earlier.
    fn anchor_row(&self, comment: &ReviewComment) -> Option<usize> {
        if !comment.scope.is_line() || comment.path != self.shown_label() {
            return None;
        }
        self.rows.iter().position(|row| {
            let line = match comment.side {
                DiffSide::Before => row.left.as_ref(),
                DiffSide::After => row.right.as_ref(),
            };
            line.is_some_and(|line| line.number == comment.end_line)
        })
    }

    /// The text of line `number` on `side` of the shown diff, if it is there.
    fn line_text(&self, side: DiffSide, number: u32) -> Option<String> {
        self.rows
            .iter()
            .filter_map(|row| match side {
                DiffSide::Before => row.left.as_ref(),
                DiffSide::After => row.right.as_ref(),
            })
            .find(|line| line.number == number)
            .map(|line| line.text.clone())
    }

    /// The anchored line as the diff reads it *now*, for comparison against what the
    /// comment recorded. `None` when that line number is no longer in the diff.
    fn line_now(&self, comment: &ReviewComment) -> Option<String> {
        self.anchor_row(comment)?;
        self.line_text(comment.side, comment.end_line)
    }

    /// Whether the code under this comment has changed since it was written.
    ///
    /// Only answerable for the file on screen — for anything else there are no rows
    /// to compare against, and "we are not looking at it" must not be reported as
    /// "it went stale".
    fn is_outdated(&self, comment: &ReviewComment) -> bool {
        if !comment.scope.is_line() || comment.path != self.shown_label() {
            return false;
        }
        comment.outdated_against(self.line_now(comment).as_deref())
    }

    /// Settle a comment, or reopen a settled one.
    ///
    /// Settling is the operator's call, never inferred. The session rewriting a line
    /// is evidence the comment was *read*, not that it was answered the way the
    /// operator wanted — so a changed line marks the comment outdated and offers
    /// this, rather than quietly closing it.
    fn toggle_resolved(&mut self, index: usize, cx: &mut Context<Self>) {
        let at = now();
        let Some(comment) = self.comments.get_mut(index) else {
            return;
        };
        comment.resolved_at = comment.resolved_at.is_none().then_some(at);
        let updated = comment.clone();
        if let Some(changes) = &self.changes {
            if let Err(err) = changes.update_comment(&updated) {
                self.error = Some(format!("Couldn't update the comment: {err}"));
            }
        }
        cx.notify();
    }

    /// Reveal [`EXPAND_STEP`] more lines of a folded run, from the top edge (the
    /// lines just *below* what precedes the fold) or the bottom edge.
    fn expand_fold(&mut self, run: usize, from_top: bool, cx: &mut Context<Self>) {
        let shown = self.expanded.entry(run).or_default();
        if from_top {
            shown.top += EXPAND_STEP;
        } else {
            shown.bottom += EXPAND_STEP;
        }
        cx.notify();
    }

    /// Reveal one folded run completely.
    fn expand_fold_fully(&mut self, run: usize, cx: &mut Context<Self>) {
        // The clamp in `view_rows` caps this at the run's real length.
        self.expanded.insert(
            run,
            Revealed {
                top: usize::MAX / 2,
                bottom: 0,
            },
        );
        cx.notify();
    }

    /// Collapse every run again — the way back from "expand all".
    fn collapse_folds(&mut self, cx: &mut Context<Self>) {
        self.expanded.clear();
        cx.notify();
    }

    /// Reveal the whole file, for when the change only makes sense in full context.
    fn expand_all_folds(&mut self, cx: &mut Context<Self>) {
        for run in pair_diff::foldable_runs(&self.rows, FOLD_CONTEXT) {
            self.expanded.insert(
                run.start,
                Revealed {
                    top: usize::MAX / 2,
                    bottom: 0,
                },
            );
        }
        cx.notify();
    }

    /// A review finding, tucked under the line it names.
    ///
    /// Distinct from an operator's comment: it carries a severity, the lanes that
    /// raised it, and — once written — the operator's reply. The reply is what makes
    /// it actionable; a finding forwarded on its own is just a robot's opinion
    /// arriving in someone's prompt.
    fn finding_bubble(&self, index: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(state) = self.findings.get(index) else {
            return div().into_any_element();
        };
        let finding = &state.finding;
        let hue = match finding.severity {
            crate::review_findings::Severity::High => theme::git_deleted(),
            crate::review_findings::Severity::Medium => theme::git_conflict(),
            crate::review_findings::Severity::Low => theme::text_muted(),
        };
        let writing = self.note_for.as_ref().filter(|(i, _)| *i == index);

        let mut card = div()
            .flex_1()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .gap_1()
            .rounded(theme::radius_sm())
            .border_l_2()
            .border_color(hue)
            .bg(theme::surface_base())
            .px_2()
            .py(px(4.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .text_size(theme::text_2xs())
                    .child(
                        div()
                            .px(px(4.))
                            .rounded(theme::radius_sm())
                            .bg(theme::tint(hue, 0.16))
                            .text_color(hue)
                            .child(format!("{} · {}", finding.key, finding.severity.label())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_color(theme::text_muted())
                            .child(finding.owner.join(", ")),
                    )
                    .child(
                        div()
                            .text_color(theme::text_muted())
                            .child(finding.route.label()),
                    ),
            )
            .child(
                div()
                    .text_size(theme::text_sm())
                    .text_color(theme::text_primary())
                    .child(finding.summary.clone()),
            );

        if !finding.detail.is_empty() {
            card = card.child(
                div()
                    .text_size(theme::text_xs())
                    .text_color(theme::text_secondary())
                    .child(finding.detail.clone()),
            );
        }
        if let Some(fix) = &finding.fix {
            card = card.child(
                div()
                    .text_size(theme::text_xs())
                    .text_color(theme::text_muted())
                    .child(format!("Suggested fix: {fix}")),
            );
        }
        if let Some(note) = &state.note {
            card = card.child(
                div()
                    .mt(px(2.))
                    .pl_2()
                    .border_l_2()
                    .border_color(theme::accent())
                    .text_size(theme::text_sm())
                    .text_color(theme::accent())
                    .child(note.clone()),
            );
        }

        card = match writing {
            Some((_, input)) => card.child(Input::new(input)).child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(Self::pill(
                        ("finding-note-save", index),
                        "Add note",
                        theme::accent(),
                        move |this, _w, cx| this.save_note(index, cx),
                        cx,
                    ))
                    .child(Self::pill(
                        ("finding-note-cancel", index),
                        "Cancel",
                        theme::text_muted(),
                        move |this, _w, cx| this.cancel_note(cx),
                        cx,
                    )),
            ),
            None => card.child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(Self::pill(
                        ("finding-note", index),
                        if state.note.is_some() {
                            "Edit note"
                        } else {
                            "Reply"
                        },
                        theme::accent(),
                        move |this, window, cx| this.begin_note(index, window, cx),
                        cx,
                    ))
                    .child(Self::pill(
                        ("finding-dismiss", index),
                        "Dismiss",
                        theme::text_muted(),
                        move |this, _w, cx| this.dismiss_finding(index, cx),
                        cx,
                    )),
            ),
        };

        div()
            .flex()
            .flex_row()
            .w_full()
            .pl(px(GUTTER_W))
            .pr_2()
            .py(px(3.))
            .bg(theme::surface_sunken())
            .child(card)
            .into_any_element()
    }

    /// Open the reply field on a finding.
    fn begin_note(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let existing = self
            .findings
            .get(index)
            .and_then(|state| state.note.clone())
            .unwrap_or_default();
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("What should the session do about this?")
        });
        if !existing.is_empty() {
            input.update(cx, |state, cx| state.set_value(existing, window, cx));
        }
        input.focus_handle(cx).focus(window, cx);
        self.note_for = Some((index, input));
        cx.notify();
    }

    /// Attach the reply. An empty note clears it rather than storing a blank.
    fn save_note(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some((_, input)) = self.note_for.clone() else {
            return;
        };
        let text = input.read(cx).value().trim().to_string();
        if let Some(state) = self.findings.get_mut(index) {
            state.note = (!text.is_empty()).then_some(text);
        }
        self.note_for = None;
        cx.notify();
    }

    fn cancel_note(&mut self, cx: &mut Context<Self>) {
        self.note_for = None;
        cx.notify();
    }

    /// Drop a finding from this pass.
    fn dismiss_finding(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(state) = self.findings.get_mut(index) {
            state.dismissed = true;
        }
        if self.note_for.as_ref().is_some_and(|(i, _)| *i == index) {
            self.note_for = None;
        }
        cx.notify();
    }

    /// A saved comment, tucked under the code it is about.
    ///
    /// Indented past the gutter so the thread reads as hanging off the line rather
    /// than as another line, and tinted by whether it has been delivered yet: a
    /// pending comment is the thing the operator still owes the session.
    /// One saved comment. `inline` is whether it is hanging under a line of code —
    /// on the diff it aligns to the gutter, in the notes band it does not.
    fn comment_bubble(
        &self,
        index: usize,
        inline: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let Some(comment) = self.comments.get(index) else {
            return div().into_any_element();
        };
        let sent = comment.sent_at.is_some();
        let resolved = comment.is_resolved();
        let outdated = self.is_outdated(comment);
        let accent = match (resolved, outdated, sent) {
            (true, _, _) => theme::git_added(),
            (_, true, _) => theme::git_conflict(),
            (_, _, true) => theme::text_muted(),
            _ => theme::accent(),
        };
        let status = match (resolved, sent) {
            (true, _) => "Resolved",
            (_, true) => "Sent",
            _ => "Pending",
        };
        // What the line said when the comment was written. Shown only once it stops
        // matching: that is the moment the comment starts describing code that is no
        // longer there, and the operator needs the old text to judge whether it still
        // stands.
        let was = outdated
            .then(|| comment.anchor_text.clone())
            .flatten()
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());
        let id = comment.id.clone();
        let editing = self
            .editing
            .as_deref()
            .filter(|editing| *editing == comment.id)
            .and(self.comment_input.clone());

        div()
            .flex()
            .flex_row()
            .w_full()
            .when(inline, |d| d.pl(px(GUTTER_W)).bg(theme::surface_sunken()))
            .pr_2()
            .py(px(3.))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .rounded(theme::radius_sm())
                    .border_l_2()
                    .border_color(accent)
                    .bg(theme::surface_base())
                    .px_2()
                    .py(px(4.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .text_size(theme::text_2xs())
                            .child(div().text_color(accent).child(status))
                            // Off the diff, the comment has to say what it is about —
                            // the code it points at is not next to it.
                            .children((!inline).then(|| {
                                div()
                                    .font_family(theme::mono_font())
                                    .text_color(theme::text_muted())
                                    .child(comment.anchor())
                            }))
                            // The badge is the whole point of recording the anchor:
                            // a comment whose line changed underneath it is arguing
                            // with code nobody wrote, and it has to say so on the
                            // diff rather than look current.
                            .children(outdated.then(|| {
                                div()
                                    .id(SharedString::from(format!("comment-stale:{id}")))
                                    .px(px(4.))
                                    .rounded(theme::radius_sm())
                                    .bg(theme::tint(theme::git_conflict(), 0.16))
                                    .text_color(theme::git_conflict())
                                    .child("outdated")
                                    .tooltip(|window, cx| {
                                        gpui_component::tooltip::Tooltip::new(
                                            "The session changed this line after the comment was written",
                                        )
                                        .build(window, cx)
                                    })
                            }))
                            .child(div().flex_1())
                            // A delivered comment is not finished business: it can
                            // have landed against the wrong place, or simply not
                            // have been acted on. Correcting or re-queuing it beats
                            // retyping it.
                            .child(
                                div()
                                    .id(SharedString::from(format!("comment-edit:{id}")))
                                    .px_1()
                                    .cursor_pointer()
                                    .text_color(theme::text_muted())
                                    .hover(|d| d.text_color(theme::accent()))
                                    .child("Edit")
                                    .on_click(cx.listener(move |this, _ev, window, cx| {
                                        this.begin_edit(index, window, cx)
                                    })),
                            )
                            // Settling a comment is what keeps a long review readable:
                            // it leaves the diff without being deleted, so the record
                            // of what was raised survives.
                            .child(
                                div()
                                    .id(SharedString::from(format!("comment-resolve:{id}")))
                                    .px_1()
                                    .cursor_pointer()
                                    .text_color(if resolved {
                                        theme::git_added()
                                    } else {
                                        theme::text_muted()
                                    })
                                    .hover(|d| d.text_color(theme::git_added()))
                                    .child(if resolved { "Reopen" } else { "Resolve" })
                                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                                        this.toggle_resolved(index, cx)
                                    })),
                            )
                            .children((sent && !resolved).then(|| {
                                div()
                                    .id(SharedString::from(format!("comment-resend:{id}")))
                                    .px_1()
                                    .cursor_pointer()
                                    .text_color(theme::text_muted())
                                    .hover(|d| d.text_color(theme::accent()))
                                    .child("Send again")
                                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                                        this.resend_comment(index, cx)
                                    }))
                            }))
                            .children((!sent).then(|| {
                                let id = id.clone();
                                div()
                                    .id(SharedString::from(format!("comment-del:{id}")))
                                    .px_1()
                                    .cursor_pointer()
                                    .text_color(theme::text_muted())
                                    .hover(|d| d.text_color(theme::git_deleted()))
                                    .child("✕")
                                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                                        this.delete_comment(id.clone(), cx)
                                    }))
                            })),
                    )
                    // While this comment is the one being corrected, the composer
                    // takes the body's place — the edit happens where the comment
                    // is, not in a field elsewhere on screen.
                    .child(match editing {
                        Some(input) => div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(Input::new(&input))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap_2()
                                    .child(Self::pill(
                                        ("comment-save-edit", index),
                                        "Save",
                                        theme::accent(),
                                        move |this, _w, cx| this.save_comment(cx),
                                        cx,
                                    ))
                                    .child(Self::pill(
                                        ("comment-cancel-edit", index),
                                        "Cancel",
                                        theme::text_muted(),
                                        move |this, _w, cx| this.cancel_comment(cx),
                                        cx,
                                    )),
                            )
                            .into_any_element(),
                        None => div()
                            .text_size(theme::text_sm())
                            .text_color(if resolved {
                                theme::text_muted()
                            } else {
                                theme::text_secondary()
                            })
                            .child(comment.body.clone())
                            .into_any_element(),
                    })
                    .children(was.map(|text| {
                        div()
                            .flex()
                            .flex_row()
                            .gap_1()
                            .text_size(theme::text_2xs())
                            .child(div().text_color(theme::text_muted()).child("was on"))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .font_family(theme::mono_font())
                                    .text_color(theme::git_deleted())
                                    .child(text),
                            )
                    })),
            )
            .into_any_element()
    }

    /// The composer. `inline` is whether it is opening on the code (a line comment)
    /// or in the notes band (a file- or review-scoped one).
    fn inline_composer(&self, inline: bool, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(input) = self.comment_input.as_ref() else {
            return div().into_any_element();
        };
        // The composer names its own subject. Without it, a file comment and a line
        // comment are the same empty box, and which one you are writing is decided by
        // whichever button you happened to press a moment ago.
        let range = match self.compose_scope {
            CommentScope::Review => "On this review".to_string(),
            CommentScope::File => format!("On {}", self.shown_label()),
            CommentScope::Line => self
                .selected_lines()
                .map(|(_, start, end)| {
                    if start == end {
                        format!("Line {start}")
                    } else {
                        format!("Lines {start}–{end}")
                    }
                })
                .unwrap_or_default(),
        };

        div()
            .flex()
            .flex_row()
            .w_full()
            .when(inline, |d| d.pl(px(GUTTER_W)).bg(theme::surface_sunken()))
            .pr_2()
            .py(px(3.))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .rounded(theme::radius_sm())
                    .border_l_2()
                    .border_color(theme::accent())
                    .bg(theme::surface_base())
                    .px_2()
                    .py(px(5.))
                    .child(
                        div()
                            .text_size(theme::text_2xs())
                            .text_color(theme::text_muted())
                            .child(range),
                    )
                    .child(Input::new(input))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .child(Self::pill(
                                ("comment-save", 0),
                                "Add comment",
                                theme::accent(),
                                move |this, _w, cx| this.save_comment(cx),
                                cx,
                            ))
                            .child(Self::pill(
                                ("comment-cancel", 0),
                                "Cancel",
                                theme::text_muted(),
                                move |this, _w, cx| this.cancel_comment(cx),
                                cx,
                            )),
                    ),
            )
            .into_any_element()
    }

    /// The band standing in for a still-hidden run.
    ///
    /// Its controls sit in the gutter column, on the same vertical edge as the line
    /// numbers, so the band reads as part of the pane rather than as an interruption
    /// laid across it. Three controls, one job each — reveal above, reveal all of
    /// this run, reveal below — and the text stays a label rather than doubling as
    /// a fourth button.
    fn fold_marker(
        &self,
        run: usize,
        hidden: std::ops::Range<usize>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let count = hidden.end - hidden.start;
        // Number the gap by the after-side lines it covers, so it reads against the
        // file being reviewed. Falls back to the before side for a stretch that only
        // exists there.
        let line_of = |row: usize| {
            self.rows
                .get(row)
                .and_then(|r| r.right.as_ref().or(r.left.as_ref()).map(|line| line.number))
        };
        let span = match (line_of(hidden.start), line_of(hidden.end.saturating_sub(1))) {
            (Some(first), Some(last)) => format!(" · {first}–{last}"),
            _ => String::new(),
        };

        let control =
            |id: &'static str, glyph: &'static str, label: &'static str, cx: &mut Context<Self>| {
                let all = id == "fold-all";
                let top = id == "fold-up";
                div()
                    .id((id, run))
                    .w(px(16.))
                    .flex_none()
                    .cursor_pointer()
                    .text_align(gpui::TextAlign::Center)
                    .text_color(theme::text_muted())
                    .hover(|d| d.text_color(theme::accent()))
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(label).build(window, cx)
                    })
                    .child(glyph)
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        if all {
                            this.expand_fold_fully(run, cx)
                        } else {
                            this.expand_fold(run, top, cx)
                        }
                    }))
            };

        div()
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .h(px(ROW_H))
            .bg(theme::surface_sunken())
            .border_t_1()
            .border_b_1()
            .border_color(theme::border_subtle())
            .text_size(theme::text_2xs())
            .text_color(theme::text_muted())
            .child(
                div()
                    .w(px(GUTTER_W))
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .child(control("fold-up", "⌃", "Reveal lines above", cx))
                    .child(control("fold-all", "⇕", "Reveal the whole gap", cx))
                    .child(control("fold-down", "⌄", "Reveal lines below", cx)),
            )
            .child(div().child(format!("{count} unchanged lines{span}")))
            .into_any_element()
    }

    /// Rows whose text contains the query, either side, in view order.
    ///
    /// A plain case-insensitive substring match, which is what finding a symbol in a
    /// diff actually needs — typing `send_paste` should land on it wherever it
    /// appears, in the before side as well as the after.
    fn matches(&self) -> Vec<usize> {
        if self.query.is_empty() {
            return Vec::new();
        }
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                [row.left.as_ref(), row.right.as_ref()]
                    .into_iter()
                    .flatten()
                    .any(|line| line.text.to_lowercase().contains(&self.query))
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Open or close the find bar.
    fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.is_some() {
            self.close_search(cx);
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in this diff"));
        self.search_sub =
            Some(
                cx.subscribe(&input, |this, state, event: &InputEvent, cx| match event {
                    InputEvent::Change => {
                        this.query = state.read(cx).value().to_lowercase();
                        this.current_match = 0;
                        this.select_match(cx);
                    }
                    // Enter walks the matches; Shift+Enter walks back, as in the Run window.
                    InputEvent::PressEnter { shift, .. } => this.step_match(!shift, cx),
                    _ => {}
                }),
            );
        input.focus_handle(cx).focus(window, cx);
        self.search = Some(input);
        cx.notify();
    }

    fn close_search(&mut self, cx: &mut Context<Self>) {
        self.search = None;
        self.search_sub = None;
        self.query.clear();
        self.current_match = 0;
        cx.notify();
    }

    /// Step to the next (or previous) match, wrapping.
    fn step_match(&mut self, forward: bool, cx: &mut Context<Self>) {
        let matches = self.matches();
        if matches.is_empty() {
            return;
        }
        self.current_match = if forward {
            (self.current_match + 1) % matches.len()
        } else {
            (self.current_match + matches.len() - 1) % matches.len()
        };
        self.select_match(cx);
    }

    /// Measure the monospace advance once, so a click's x can become a column.
    fn measure_cell(&mut self, window: &Window) {
        if self.cell_w.is_some() {
            return;
        }
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&gpui::font(theme::mono_font()));
        if let Ok(advance) = text_system.em_advance(font_id, theme::text_sm()) {
            let advance = f32::from(advance);
            if advance > 0.0 {
                self.cell_w = Some(advance);
            }
        }
    }

    /// Which character column of `side`'s code column the pointer is over.
    ///
    /// The two panes split the scroll region evenly (both cells are `flex_1` around
    /// a 1px divider), so one measurement of the whole region locates either one;
    /// the code itself starts a fixed gutter in from the pane's edge.
    fn column_at(&self, side: DiffSide, x: Pixels) -> Option<usize> {
        let bounds = self.panes_bounds?;
        let cell_w = self.cell_w?;
        let pane_w = (f32::from(bounds.size.width) - 1.0) / 2.0;
        let pane_x = f32::from(bounds.origin.x)
            + match side {
                DiffSide::Before => 0.0,
                DiffSide::After => pane_w + 1.0,
            };
        let offset = f32::from(x) - (pane_x + GUTTER_W);
        (offset >= 0.0).then(|| (offset / cell_w) as usize)
    }

    /// Open the symbol menu for a click at `at` on `row`'s `side`.
    ///
    /// The word under the pointer leads, when one resolved; the rest of the line
    /// follows. Offering the whole line matters — the column comes from pixel
    /// arithmetic, so a click that lands a character wide, or between two words,
    /// still gets the operator to the symbol they meant instead of nothing.
    fn open_symbol_menu(
        &mut self,
        side: DiffSide,
        row: usize,
        at: gpui::Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(text) = self.rows.get(row).and_then(|r| {
            match side {
                DiffSide::Before => r.left.as_ref(),
                DiffSide::After => r.right.as_ref(),
            }
            .map(|line| line.text.clone())
        }) else {
            return;
        };
        let under = self
            .column_at(side, at.x)
            .and_then(|col| word_at(&text, col));
        let mut words: Vec<String> = under.into_iter().collect();
        for word in identifiers(&text) {
            if !words.contains(&word) {
                words.push(word);
            }
        }
        words.truncate(SYMBOL_MENU_MAX);
        if words.is_empty() {
            return;
        }
        self.symbol_menu = Some((at, words));
        cx.notify();
    }

    /// Find `word` through the diff: opens the find bar carrying it, and lands on
    /// the first match. The bar stays open so ↑/↓ walk the rest.
    fn find_symbol(&mut self, word: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.symbol_menu = None;
        if self.search.is_none() {
            self.toggle_search(window, cx);
        }
        if let Some(input) = self.search.clone() {
            // Setting the value raises `InputEvent::Change`, which is what refreshes
            // the query and jumps — the same path typing takes.
            input.update(cx, |state, cx| state.set_value(word, window, cx));
        }
        cx.notify();
    }

    /// The menu behind a right-click (or ⌃/⌘-click) on a line of code.
    fn symbol_menu(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let (at, words) = self.symbol_menu.clone()?;
        let dismiss = cx.listener(|this, _ev: &MouseDownEvent, _w, cx| {
            this.symbol_menu = None;
            cx.notify();
        });
        Some(
            deferred(
                anchored().child(
                    div()
                        .occlude()
                        .size_full()
                        .on_mouse_down(MouseButton::Left, dismiss)
                        .child(
                            anchored()
                                .position(at)
                                .snap_to_window_with_margin(px(8.))
                                .child(
                                    div()
                                        .id("symbol-menu")
                                        .flex()
                                        .flex_col()
                                        .gap(px(1.))
                                        .rounded(theme::radius_sm())
                                        .border_1()
                                        .border_color(theme::border_subtle())
                                        .bg(theme::surface_overlay())
                                        .shadow(theme::overlay_shadow())
                                        .px_2()
                                        .py(px(4.))
                                        .child(
                                            div()
                                                .pb(px(2.))
                                                .text_size(theme::text_2xs())
                                                .text_color(theme::text_muted())
                                                .child("Find in this diff"),
                                        )
                                        .children(words.into_iter().enumerate().map(
                                            |(i, word)| {
                                                let query = word.clone();
                                                div()
                                                    .id(("symbol-find", i))
                                                    .cursor_pointer()
                                                    .px_1()
                                                    .py(px(1.))
                                                    .rounded(theme::radius_sm())
                                                    .font_family(theme::mono_font())
                                                    .text_size(theme::text_xs())
                                                    // The word under the pointer leads;
                                                    // the rest of the line is the fallback.
                                                    .text_color(if i == 0 {
                                                        theme::text_primary()
                                                    } else {
                                                        theme::text_secondary()
                                                    })
                                                    .hover(|d| d.bg(theme::row_hover()))
                                                    .child(word)
                                                    .on_click(cx.listener(
                                                        move |this, _ev, window, cx| {
                                                            this.find_symbol(&query, window, cx)
                                                        },
                                                    ))
                                            },
                                        )),
                                ),
                        ),
                ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// Put the selection on the current match, so it is highlighted where the eye
    /// already is — and so it can be commented on without a second gesture.
    fn select_match(&mut self, cx: &mut Context<Self>) {
        let matches = self.matches();
        let Some(row) = matches.get(self.current_match).copied() else {
            cx.notify();
            return;
        };
        let side = match self.rows.get(row).and_then(|r| r.right.as_ref()) {
            Some(_) => DiffSide::After,
            None => DiffSide::Before,
        };
        self.selection = Some(Selection {
            side,
            anchor: row,
            head: row,
        });
        cx.notify();
    }

    /// The find bar.
    fn search_bar(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let input = self.search.as_ref()?;
        let matches = self.matches();
        let count = if self.query.is_empty() {
            String::new()
        } else if matches.is_empty() {
            "no matches".to_string()
        } else {
            format!("{} / {}", self.current_match + 1, matches.len())
        };
        Some(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(div().flex_1().child(Input::new(input)))
                .child(
                    div()
                        .w(px(90.))
                        .text_size(theme::text_xs())
                        .text_color(theme::text_muted())
                        .child(count),
                )
                .child(Self::pill(
                    ("search-prev", 0),
                    "↑",
                    theme::text_muted(),
                    move |this, _w, cx| this.step_match(false, cx),
                    cx,
                ))
                .child(Self::pill(
                    ("search-next", 0),
                    "↓",
                    theme::text_muted(),
                    move |this, _w, cx| this.step_match(true, cx),
                    cx,
                ))
                .child(Self::pill(
                    ("search-close", 0),
                    "Close",
                    theme::text_muted(),
                    move |this, _w, cx| this.close_search(cx),
                    cx,
                ))
                .into_any_element(),
        )
    }

    /// Move the selection to the next (or previous) run of changed rows, wrapping
    /// at the ends. Equal stretches can be long; this is how you get from one change
    /// to the next without scrolling past them.
    fn jump_change(&mut self, forward: bool, cx: &mut Context<Self>) {
        let blocks = pair_diff::change_blocks(&self.rows);
        if blocks.is_empty() {
            return;
        }
        let head = self.selection.map(|s| s.head);
        let block = match (head, forward) {
            (Some(row), true) => blocks.iter().find(|b| b.start > row).or(blocks.first()),
            (Some(row), false) => blocks.iter().rev().find(|b| b.end <= row).or(blocks.last()),
            (None, true) => blocks.first(),
            (None, false) => blocks.last(),
        };
        let Some(block) = block.cloned() else {
            return;
        };
        // Anchor on the side that actually has lines here: a pure deletion exists
        // only on the left, so selecting the right pane would select nothing.
        let side = match self.rows.get(block.start).and_then(|r| r.right.as_ref()) {
            Some(_) => DiffSide::After,
            None => DiffSide::Before,
        };
        self.selection = Some(Selection {
            side,
            anchor: block.start,
            head: block.end.saturating_sub(1),
        });
        self.comment_input = None;
        cx.notify();
    }

    /// The 1-based line range the current selection covers on its own side, or
    /// `None` when the selection is entirely filler rows (a gap opposite an
    /// insertion has no lines to comment on).
    fn selected_lines(&self) -> Option<(DiffSide, u32, u32)> {
        let sel = self.selection?;
        let numbers: Vec<u32> = self
            .rows
            .get(sel.range())?
            .iter()
            .filter_map(|row| match sel.side {
                DiffSide::Before => row.left.as_ref(),
                DiffSide::After => row.right.as_ref(),
            })
            .map(|line| line.number)
            .collect();
        let first = *numbers.first()?;
        let last = *numbers.last()?;
        Some((sel.side, first, last))
    }

    /// Open the composer for the current selection.
    fn begin_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_lines().is_none() {
            return;
        }
        self.compose_scope = CommentScope::Line;
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("What should change here? (Enter to add)")
        });
        // Enter adds the comment; Shift+Enter is left to the field so a note can run
        // to a second line.
        self.comment_enter = Some(
            cx.subscribe(&input, |this, _state, event: &InputEvent, cx| {
                if let InputEvent::PressEnter { shift, .. } = event {
                    if !shift {
                        this.save_comment(cx);
                    }
                }
            }),
        );
        input.focus_handle(cx).focus(window, cx);
        self.comment_input = Some(input);
        cx.notify();
    }

    /// Open the composer for something wider than a line — the shown file, or the
    /// review itself.
    ///
    /// Not every remark is about a line, and pinning one to whichever line happened
    /// to be selected makes it read as being about that line. "This module should
    /// not know about the store" belongs to the file; "land the migration first"
    /// belongs to the change.
    fn begin_scoped_comment(
        &mut self,
        scope: CommentScope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if scope == CommentScope::File && self.selected.is_none() {
            return;
        }
        self.close_composer();
        self.compose_scope = scope;
        let placeholder = match scope {
            CommentScope::File => "What is true of this whole file? (Enter to add)",
            CommentScope::Review => "What is true of this change as a whole? (Enter to add)",
            CommentScope::Line => "What should change here? (Enter to add)",
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        self.comment_enter = Some(
            cx.subscribe(&input, |this, _state, event: &InputEvent, cx| {
                if let InputEvent::PressEnter { shift, .. } = event {
                    if !shift {
                        this.save_comment(cx);
                    }
                }
            }),
        );
        input.focus_handle(cx).focus(window, cx);
        self.comment_input = Some(input);
        cx.notify();
    }

    /// Open the composer on an existing comment.
    fn begin_edit(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(comment) = self.comments.get(index).cloned() else {
            return;
        };
        // The composer opens where the comment lives, so it has to know which of the
        // two places that is.
        self.compose_scope = comment.scope;
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("What should change here? (Enter to save)")
        });
        input.update(cx, |state, cx| {
            state.set_value(comment.body.clone(), window, cx)
        });
        self.comment_enter = Some(
            cx.subscribe(&input, |this, _state, event: &InputEvent, cx| {
                if let InputEvent::PressEnter { shift, .. } = event {
                    if !shift {
                        this.save_comment(cx);
                    }
                }
            }),
        );
        input.focus_handle(cx).focus(window, cx);
        self.comment_input = Some(input);
        self.editing = Some(comment.id);
        cx.notify();
    }

    /// Put a delivered comment back in the queue, unchanged.
    ///
    /// Sending is not always the end of it: a comment can land against the wrong
    /// place, or the session can simply not act on it. Re-queuing beats retyping.
    fn resend_comment(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(comment) = self.comments.get_mut(index) else {
            return;
        };
        comment.sent_at = None;
        let updated = comment.clone();
        if let Some(changes) = &self.changes {
            if let Err(err) = changes.update_comment(&updated) {
                self.error = Some(format!("Couldn't re-queue the comment: {err}"));
            }
        }
        cx.notify();
    }

    /// Persist the composed comment against the selected line range. Saved
    /// immediately — a review survives closing the tab, and is only *delivered*
    /// when the operator sends it.
    ///
    /// When [`editing`](Self::editing) names a comment, this rewrites that one and
    /// clears its delivery stamp: a corrected comment has not been sent, whatever
    /// its previous version was.
    fn save_comment(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.editing.clone() {
            let body = self
                .comment_input
                .as_ref()
                .map(|input| input.read(cx).value().trim().to_string())
                .unwrap_or_default();
            // Re-anchor while editing: the operator is looking at the code right now,
            // so whatever the line says is what this comment is about. A corrected
            // comment that came back still flagged outdated would be reporting the
            // state from before the correction. Read before the mutable borrow —
            // finding the row is a read of the same panel.
            let re_anchored = self
                .comments
                .iter()
                .find(|c| c.id == id)
                .filter(|c| c.scope.is_line() && c.path == self.shown_label())
                .and_then(|c| self.line_text(c.side, c.end_line));
            if let Some(comment) = self.comments.iter_mut().find(|c| c.id == id) {
                if body.is_empty() {
                    // An emptied comment is a deletion in disguise; treat it as one
                    // rather than sending the session a blank instruction.
                    let id = comment.id.clone();
                    self.close_composer();
                    self.delete_comment(id, cx);
                    return;
                }
                comment.body = body;
                comment.sent_at = None;
                // Only when the line was actually found: an edit made with the file
                // closed, or on a line that has since gone, must not erase the anchor
                // and leave the comment looking freshly current.
                if let Some(text) = re_anchored {
                    comment.anchor_text = Some(text);
                }
                let updated = comment.clone();
                if let Some(changes) = &self.changes {
                    if let Err(err) = changes.update_comment(&updated) {
                        self.error = Some(format!("Couldn't save the comment: {err}"));
                    }
                }
            }
            self.close_composer();
            cx.notify();
            return;
        }
        let Some(input) = self.comment_input.clone() else {
            return;
        };
        // What the comment is pinned to, per scope. A file comment still carries its
        // file; a review comment carries neither file nor line, and says so with an
        // empty path rather than a plausible-looking one.
        let (path, side, start, end, anchor_text) = match self.compose_scope {
            CommentScope::Line => {
                let (Some((side, start, end)), Some(idx)) = (self.selected_lines(), self.selected)
                else {
                    return;
                };
                // The readable, repo-relative label — this is what the agent is told
                // to look at, so it must match how it refers to the file itself.
                (
                    self.file_label(idx),
                    side,
                    start,
                    end,
                    self.line_text(side, end),
                )
            }
            CommentScope::File => {
                let Some(idx) = self.selected else {
                    return;
                };
                (self.file_label(idx), DiffSide::After, 0, 0, None)
            }
            CommentScope::Review => (String::new(), DiffSide::After, 0, 0, None),
        };
        let body = input.read(cx).value().trim().to_string();
        if body.is_empty() {
            self.close_composer();
            cx.notify();
            return;
        }
        let at = now();
        let comment = ReviewComment {
            // Time-ordered and unique per session: one comment per (session, instant,
            // anchor), which the operator can't produce twice.
            id: format!("{}-{}-{}", self.session.as_str(), at.as_millis(), start),
            session_id: self.session.clone(),
            scope: self.compose_scope,
            path,
            side,
            start_line: start,
            end_line: end,
            body,
            anchor_text,
            // This panel is the operator's, and it writes thread roots: replying to a
            // comment is an editor-surface gesture today (see the VSCode review
            // extension), so nothing here starts mid-conversation.
            author: CommentAuthor::Operator,
            parent_id: None,
            at,
            sent_at: None,
            resolved_at: None,
        };
        if let Some(changes) = &self.changes {
            if let Err(err) = changes.add_comment(&comment) {
                self.error = Some(format!("saving the comment failed: {err}"));
                cx.notify();
                return;
            }
        }
        self.comments.push(comment);
        self.close_composer();
        self.selection = None;
        cx.notify();
    }

    /// Tear the composer down, whichever job it was doing.
    fn close_composer(&mut self) {
        self.comment_input = None;
        self.comment_enter = None;
        self.editing = None;
        self.compose_scope = CommentScope::Line;
    }

    fn cancel_comment(&mut self, cx: &mut Context<Self>) {
        self.close_composer();
        cx.notify();
    }

    fn delete_comment(&mut self, id: String, cx: &mut Context<Self>) {
        if let Some(changes) = &self.changes {
            if let Err(err) = changes.delete_comment(&id) {
                self.error = Some(format!("deleting the comment failed: {err}"));
                cx.notify();
                return;
            }
        }
        self.comments.retain(|c| c.id != id);
        cx.notify();
    }

    /// Deliver every unsent comment as **one** message, then stamp them sent. One
    /// injection and one audit entry: the agent reads a review, not a stream of
    /// interruptions mid-turn.
    fn send_review(&mut self, cx: &mut Context<Self>) {
        // Exactly what the button counted — a batch assembled by a second, subtly
        // different filter is one that sends comments the operator was never told
        // about.
        let unsent: Vec<ReviewComment> = self.unsent().into_iter().cloned().collect();
        let kept: Vec<&FindingState> = self
            .findings
            .iter()
            .filter(|state| !state.dismissed)
            .collect();
        if unsent.is_empty() && kept.is_empty() {
            return;
        }
        let message = review_message(&unsent, &kept);
        self.send(Command::SubmitReview {
            session: self.session.clone(),
            message,
        });

        let at = now();
        let ids: Vec<String> = unsent.iter().map(|c| c.id.clone()).collect();
        if let Some(changes) = &self.changes {
            if let Err(err) = changes.mark_comments_sent(&ids, at) {
                self.error = Some(format!("marking the review sent failed: {err}"));
            }
        }
        // Stamp only what went. A resolved comment stayed out of the batch and must
        // not come back looking delivered.
        for comment in self.comments.iter_mut() {
            if ids.contains(&comment.id) {
                comment.sent_at = Some(at);
            }
        }
        // Delivered findings leave the pass: they are now the session's to fix, and
        // whether they were fixed is a question for the next review run, not for a
        // stale bubble sitting on a line that may no longer exist.
        self.findings.clear();
        self.note_for = None;
        cx.notify();
    }

    /// What is missing before a full code review can run at full strength, and the
    /// consent to install it.
    ///
    /// Shown instead of the review rather than alongside it: the operator asked for
    /// every lane, and running a subset without saying so is the failure mode this
    /// whole check exists to prevent.
    fn missing_lanes_band(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if self.missing_lanes.is_empty() {
            return None;
        }
        let lanes: Vec<gpui::AnyElement> = self
            .missing_lanes
            .iter()
            .enumerate()
            .map(|(i, lane)| {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .py(px(2.))
                    .child(
                        div()
                            .w(px(150.))
                            .flex_none()
                            .font_family(theme::mono_font())
                            .text_size(theme::text_xs())
                            .text_color(theme::text_primary())
                            .child(lane.skill),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(theme::text_xs())
                            .text_color(theme::text_muted())
                            .child(lane.finds),
                    )
                    .children(crate::review_skills::installable(lane).then(|| {
                        Self::pill(
                            ("lane-install", i),
                            "Install",
                            theme::accent(),
                            move |this, _w, cx| this.install_lane(i, cx),
                            cx,
                        )
                    }))
                    .into_any_element()
            })
            .collect();

        Some(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .rounded(theme::radius_md())
                .border_1()
                .border_color(theme::tint(theme::git_conflict(), 0.5))
                .bg(theme::surface_raised())
                .px_3()
                .py_2()
                .child(
                    div()
                        .text_size(theme::text_sm())
                        .text_color(theme::text_primary())
                        .child("The full code review runs several lanes. These are not installed:"),
                )
                .children(lanes)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .pt_1()
                        .child(
                            div()
                                .flex_1()
                                .text_size(theme::text_2xs())
                                .text_color(theme::text_muted())
                                .child(
                                    "Without them the review still runs, but covers less than it reports.",
                                ),
                        )
                        .child(Self::pill(
                            ("lane-recheck", 0),
                            "Check again",
                            theme::text_muted(),
                            move |this, _w, cx| this.recheck_lanes(cx),
                            cx,
                        ))
                        .child(Self::pill(
                            ("lane-anyway", 0),
                            "Review without them",
                            theme::git_conflict(),
                            move |this, _w, cx| this.review_without_lanes(cx),
                            cx,
                        )),
                )
                .into_any_element(),
        )
    }

    /// Install one missing lane, on the operator's explicit ask, then re-check.
    ///
    /// A skill MoonlightCode ships is written straight into the skills directory
    /// (never over an existing one); a plugin is installed by its own command, run
    /// off the UI thread since it reaches the network.
    fn install_lane(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(lane) = self.missing_lanes.get(index).copied() else {
            return;
        };
        if crate::review_skills::bundled(lane).is_some() {
            let report = match std::env::var_os("HOME") {
                Some(home) => {
                    crate::review_skills::install_bundled(lane, std::path::Path::new(&home))
                        .unwrap_or_else(|err| err)
                }
                None => "No home directory to install into.".to_string(),
            };
            self.review_output = Some(report);
            self.missing_lanes = crate::review_skills::missing();
            cx.notify();
            return;
        }
        let Some(command) = crate::review_skills::install_command(lane) else {
            return;
        };
        self.review_output = Some(format!("$ {command}"));
        cx.spawn(async move |weak, cx| {
            let output = cx
                .background_executor()
                .spawn(async move { run_install_blocking(&command) })
                .await;
            let _ = weak.update(cx, |this, cx| {
                this.review_output = Some(output);
                this.missing_lanes = crate::review_skills::missing();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Re-run the check, for a lane installed outside the cockpit.
    fn recheck_lanes(&mut self, cx: &mut Context<Self>) {
        self.missing_lanes = crate::review_skills::missing();
        if self.missing_lanes.is_empty() {
            self.review_output = Some("Every review lane is installed.".into());
        }
        cx.notify();
    }

    /// Proceed with a knowingly-thinner review.
    fn review_without_lanes(&mut self, cx: &mut Context<Self>) {
        self.missing_lanes.clear();
        self.run_full_review(cx);
    }

    fn send(&mut self, command: Command) {
        if let Some(tx) = &self.commands {
            let _ = tx.send(command);
        }
    }

    /// Everything a review pass accumulated, cleared when the pass ends.
    fn clear_pass(&mut self) {
        self.findings.clear();
        self.coverage = None;
        self.note_for = None;
        self.comments.retain(|comment| comment.sent_at.is_none());
        self.ignored.clear();
        self.ignore_menu = None;
        self.selection = None;
        self.comment_input = None;
        self.comment_enter = None;
    }

    /// Approve, unless there are comments the session has never seen.
    ///
    /// Approving drops the ledger, and the ledger is where the comments live — so a
    /// review typed but not sent would be destroyed by the click that says the work
    /// is fine. The operator gets told the number and chooses; nothing here decides
    /// for them.
    fn approve(&mut self, cx: &mut Context<Self>) {
        let pending = self.unsent().len();
        if pending > 0 && !self.approve_confirm {
            self.approve_confirm = true;
            cx.notify();
            return;
        }
        self.approve_confirm = false;
        self.approve_now(cx);
    }

    /// Send the pending comments, then approve — the way out of the warning that
    /// loses nothing.
    fn send_then_approve(&mut self, cx: &mut Context<Self>) {
        self.send_review(cx);
        self.approve_confirm = false;
        self.approve_now(cx);
    }

    /// The warning itself: what approving would destroy, and the three ways out.
    fn approve_confirm_band(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.approve_confirm {
            return None;
        }
        let pending = self.unsent().len();
        if pending == 0 {
            return None;
        }
        Some(
            Self::band(theme::git_conflict())
                .child(
                    div()
                        .text_size(theme::text_xs())
                        .text_color(theme::git_conflict())
                        .child(if pending == 1 {
                            "1 comment has never been sent. Approving deletes it along with the ledger."
                                .to_string()
                        } else {
                            format!(
                                "{pending} comments have never been sent. Approving deletes them along with the ledger."
                            )
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .child(Self::pill(
                            ("approve-send-first", 0),
                            "Send them, then approve",
                            theme::accent(),
                            move |this, _w, cx| this.send_then_approve(cx),
                            cx,
                        ))
                        .child(Self::pill(
                            ("approve-discard", 0),
                            "Approve anyway",
                            theme::git_deleted(),
                            move |this, _w, cx| this.approve(cx),
                            cx,
                        ))
                        .child(Self::pill(
                            ("approve-cancel", 0),
                            "Cancel",
                            theme::text_muted(),
                            move |this, _w, cx| {
                                this.approve_confirm = false;
                                cx.notify();
                            },
                            cx,
                        )),
                )
                .into_any_element(),
        )
    }

    fn approve_now(&mut self, cx: &mut Context<Self>) {
        // Resolve any held approval / resume the session, then advance the workflow
        // Review → Commit — approving the review *is* the operator confirmation for
        // this gate (returns the session to auto mode).
        self.send(Command::ApproveAction {
            session: self.session.clone(),
        });
        self.send(Command::AdvancePhase {
            session: self.session.clone(),
        });
        // Approving ends the pass, and the pass includes the pile of modifications
        // itself. Clearing only the panel's own state left the ledger holding every
        // file of a change that has just been accepted, so the next review opened on
        // work already signed off. The ledger is the record of what is OUTSTANDING;
        // once accepted, nothing is.
        if let Some(changes) = &self.changes {
            if let Err(err) = changes.forget_session_changes(&self.session) {
                tracing::warn!(error = %err, "clearing the session's changes failed");
                self.error = Some(format!("Couldn't clear this session's changes: {err}"));
            }
        }
        self.clear_pass();
        self.reload(cx);
        cx.notify();
    }
}

/// Run an install command and capture what it said, so a failure is visible rather
/// than leaving the operator to wonder why the lane is still missing.
fn run_install_blocking(command: &str) -> String {
    let mut parts = command.split_whitespace();
    let Some(program) = parts.next() else {
        return "Nothing to run.".to_string();
    };
    match ProcCommand::new(program).args(parts).output() {
        Ok(out) => {
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            if !out.status.success() {
                text.push_str(&String::from_utf8_lossy(&out.stderr));
                text.push_str(&format!("\n[{command} exited with {}]", out.status));
            }
            if text.trim().is_empty() {
                format!("{command} finished.")
            } else {
                text
            }
        }
        Err(e) => format!("Couldn't run `{command}`: {e}"),
    }
}

/// Write the scope file the review skill reads: the files this session wrote, each
/// with its pre-image on disk and how that pre-image was obtained.
///
/// Handing the scope over is the point of owning the skill. A review left to
/// discover its own scope reaches for `git diff HEAD`, which answers "what is dirty
/// in this tree" — the operator's own edits included, and the session's committed
/// work excluded.
fn write_scope(
    session: &SessionId,
    root: &Path,
    files: &[(String, String, Baseline)],
) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join(format!("moonlight-review-{}", session.as_str()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't create {}: {e}", dir.display()))?;

    let mut entries = Vec::new();
    for (label, path, baseline) in files {
        // The pre-image goes to a file the lanes can diff against. A created file
        // has none by definition; an uncapturable one is flagged so the skill can
        // say the file was reviewed whole rather than as a change.
        let (baseline_path, provenance) = match baseline {
            Baseline::Content(text) | Baseline::FromHead(text) => {
                let safe = label.replace('/', "__");
                let file = dir.join(format!("{safe}.before"));
                std::fs::write(&file, text)
                    .map_err(|e| format!("Couldn't write {}: {e}", file.display()))?;
                let provenance = if matches!(baseline, Baseline::FromHead(_)) {
                    "head"
                } else {
                    "observed"
                };
                (Some(file.to_string_lossy().into_owned()), provenance)
            }
            Baseline::Created => (None, "created"),
            Baseline::Unavailable { .. } => (None, "unavailable"),
        };
        entries.push(serde_json::json!({
            "path": label,
            "absolute": path,
            "baseline": baseline_path,
            "provenance": provenance,
        }));
    }

    let scope = serde_json::json!({
        "session": session.as_str(),
        "root": root.display().to_string(),
        "base": "session",
        "files": entries,
    });
    let file = dir.join("scope.json");
    std::fs::write(
        &file,
        serde_json::to_string_pretty(&scope).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("Couldn't write {}: {e}", file.display()))?;
    Ok(file)
}

/// Blocking review invocation (run off the UI thread). Captures stdout+stderr so a
/// failure surfaces in the panel rather than vanishing.
fn run_full_review_blocking(root: &PathBuf, scope: &Path) -> String {
    match ProcCommand::new("claude")
        .arg("-p")
        .arg(format!(
            "/{} --scope {}",
            crate::review_skills::ENTRY_SKILL,
            scope.display()
        ))
        .current_dir(root)
        .output()
    {
        Ok(out) => {
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            if !out.status.success() {
                let err = String::from_utf8_lossy(&out.stderr);
                text.push_str(&format!(
                    "\n[full code review exited with {}]\n{err}",
                    out.status
                ));
            }
            if text.trim().is_empty() {
                "The full code review produced no output.".to_string()
            } else {
                text
            }
        }
        Err(e) => format!("Couldn't launch the full code review (`claude -p`): {e}"),
    }
}

/// Render a batch of comments as the message the session receives. Anchored by
/// `path:line`, so the agent can go straight to what each note is about.
fn review_message(comments: &[ReviewComment], findings: &[&FindingState]) -> String {
    let mut parts = Vec::new();
    if !findings.is_empty() {
        parts.push(format!(
            "{} finding{}",
            findings.len(),
            if findings.len() == 1 { "" } else { "s" }
        ));
    }
    if !comments.is_empty() {
        parts.push(format!(
            "{} comment{}",
            comments.len(),
            if comments.len() == 1 { "" } else { "s" }
        ));
    }
    let mut out = format!("Review of your changes ({}):\n", parts.join(", "));

    // Findings first, worst-first as they were sorted: they carry a severity the
    // session should weigh, and the operator's note is the decision about them.
    for state in findings {
        let f = &state.finding;
        out.push_str(&format!(
            "\n{}:{} — [{}] {}\n",
            f.file,
            f.line,
            f.severity.label(),
            f.summary
        ));
        if !f.detail.is_empty() {
            for line in f.detail.lines() {
                out.push_str(&format!("  {line}\n"));
            }
        }
        // The note is what the session is being asked to act on, so it is labelled
        // as the operator's rather than blurring into the reviewer's claim.
        if let Some(note) = &state.note {
            for line in note.lines() {
                out.push_str(&format!("  → {line}\n"));
            }
        }
    }

    // The comment half is rendered by `moonlight_domain::changes::review_message`, so
    // this panel and the editor plugin (which reaches the same tracker over the
    // control API) deliver identical text. Its own header line is dropped here — the
    // findings count above already framed the message.
    if !comments.is_empty() {
        let shared = moonlight_domain::changes::review_message(comments);
        let body = shared.split_once('\n').map(|(_, rest)| rest).unwrap_or("");
        out.push_str(body);
    }
    out
}

/// The "after" side: the file as it stands on disk right now. Errors come back as
/// the notice the panel shows in place of the panes.
fn read_current(path: &str) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|_| {
        "This file is gone from disk. The session deleted it after writing.".to_string()
    })?;
    if meta.len() > MAX_CURRENT_BYTES {
        return Err("Too large to diff here. Open it in an editor tab instead.".to_string());
    }
    let bytes = std::fs::read(path).map_err(|e| format!("Couldn't read this file: {e}"))?;
    String::from_utf8(bytes).map_err(|_| "Binary file — there is no line diff to show.".to_string())
}

/// What the current base can and can't show, said plainly — the Session base is
/// blind to writes that didn't go through the file tools (a `sed` inside Bash), and
/// only the git base can stage.
fn base_hint(base: DiffBase) -> &'static str {
    match base {
        DiffBase::Session => {
            "Drag or shift-click lines to select, then comment. Send review delivers \
             every comment as one message."
        }
        DiffBase::GitHead => "Reject a hunk to steer the agent; Approve advances Review → Commit.",
    }
}

/// The part of a folded `run` still hidden, given how much has been revealed from
/// each end, or `None` once the whole run is visible.
///
/// Revealing from both edges can meet in the middle, so the two counts are clamped
/// against the run's length — an unclamped subtraction would wrap and produce a
/// backwards range.
fn hidden_slice(run: std::ops::Range<usize>, shown: Revealed) -> Option<std::ops::Range<usize>> {
    let len = run.end.saturating_sub(run.start);
    let top = shown.top.min(len);
    let bottom = shown.bottom.min(len - top);
    (top + bottom < len).then(|| (run.start + top)..(run.end - bottom))
}

/// Whether `label` is hidden by anything in `ignored` — itself, or a directory
/// above it.
///
/// The boundary matters: ignoring `src` must not also hide `src2/main.rs`, so a
/// prefix only counts when the next character is a separator.
fn is_ignored_by(ignored: &std::collections::HashSet<String>, label: &str) -> bool {
    ignored.iter().any(|prefix| {
        label == prefix
            || label
                .strip_prefix(prefix.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// What counts as part of an identifier. Deliberately language-agnostic: the panes
/// hold whatever the session wrote, and a lexer per language is a research project
/// where a word boundary is enough to find `send_paste` in a diff.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The identifier straddling character column `col` of `text`, if any.
///
/// Columns come from pixel arithmetic over a monospace advance, so this is exact
/// only as far as that measurement is. Landing on whitespace returns `None` rather
/// than guessing at a neighbour — the caller offers the line's other identifiers
/// instead, which recovers gracefully from a click between words.
fn word_at(text: &str, col: usize) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    if !chars.get(col).copied().is_some_and(is_word_char) {
        return None;
    }
    let start = chars[..col]
        .iter()
        .rposition(|c| !is_word_char(*c))
        .map(|i| i + 1)
        .unwrap_or(0);
    let end = chars[col..]
        .iter()
        .position(|c| !is_word_char(*c))
        .map(|i| col + i)
        .unwrap_or(chars.len());
    Some(chars[start..end].iter().collect())
}

/// The identifiers on a line, in the order they appear, without repeats.
///
/// Bare numbers and single characters are dropped: searching the diff for `1` or
/// `i` finds everything, which is the same as finding nothing.
fn identifiers(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in text.split(|c: char| !is_word_char(c)) {
        if word.chars().count() < 2 || word.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if !out.iter().any(|seen| seen == word) {
            out.push(word.to_string());
        }
    }
    out
}

/// The tail of a path, for naming a right-click target without spending the width
/// of a full repo-relative path on it.
/// Stat a ledger file into the three facts a no-net-change verdict depends on.
fn stamp_of(path: &str, touches: u32) -> Stamp {
    let meta = std::fs::metadata(path).ok();
    Stamp {
        touches,
        len: meta.as_ref().map(|m| m.len()),
        mtime: meta.and_then(|m| m.modified().ok()),
    }
}

/// Whether this file is byte-for-byte what it was before the session first touched
/// it — the two panes of its diff would be identical, so there is nothing to read.
///
/// Only ever answered from evidence. A pre-image that could not be captured
/// (`Unavailable`) means we do not know what the file used to look like, and a file
/// we cannot compare stays in the review: hiding a real change is a far worse
/// failure than listing an empty one.
fn is_back_to_baseline(changes: &dyn SessionChangeStore, session: &SessionId, path: &str) -> bool {
    match changes.baseline(session, path) {
        // Created and then deleted: the session's whole effect on this path is gone.
        Ok(Some(Baseline::Created)) => !Path::new(path).exists(),
        Ok(Some(Baseline::Content(before))) | Ok(Some(Baseline::FromHead(before))) => {
            // Bytes, not lines: a file that differs only in its trailing newline is
            // still a change, and the diff would show it.
            std::fs::read(path).is_ok_and(|now| now == before.as_bytes())
        }
        _ => false,
    }
}

/// Comments the session has never seen and the operator has not settled.
///
/// One definition, three readers: what "Send review" counts, what it actually
/// carries, and what Approve warns is about to be destroyed. Three filters that
/// drifted apart would mean a button that counts two comments, sends three, and
/// warns about none.
fn pending_comments(comments: &[ReviewComment]) -> Vec<&ReviewComment> {
    // The shared rule, not a second copy of it: a session can now answer a comment
    // from the editor surface, and those replies land in this same store. Filtering
    // on "unsent and unresolved" alone would put the agent's own words back in the
    // batch delivered *to* it — the session answering itself, forever.
    moonlight_domain::changes::pending_feedback(comments)
}

/// A duration as a reviewer reads it: `12s`, `4m 12s`, `1h 04m`. Seconds stop being
/// interesting once a review has been running for an hour.
fn fmt_elapsed(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    match (secs / 3600, (secs % 3600) / 60, secs % 60) {
        (0, 0, s) => format!("{s}s"),
        (0, m, s) => format!("{m}m {s:02}s"),
        (h, m, _) => format!("{h}h {m:02}m"),
    }
}

fn short_target(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((_, name)) => name.to_string(),
        None => path.to_string(),
    }
}

/// 0 / 1 for the two panes, so each row's two cells get distinct element ids.
fn side_index(side: DiffSide) -> usize {
    match side {
        DiffSide::Before => 0,
        DiffSide::After => 1,
    }
}

/// Wall-clock now as epoch millis (the domain has no clock).
fn now() -> Timestamp {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Timestamp::from_millis(ms)
}

/// First 8 chars of the session id — enough to identify a tab.
fn short_id(id: &SessionId) -> String {
    let s = id.as_str();
    s.get(..8).unwrap_or(s).to_string()
}

/// Color a diff line by its leading marker (added / removed / hunk header).
fn diff_line_color(line: &str) -> gpui::Hsla {
    if line.starts_with("@@") {
        theme::accent()
    } else if line.starts_with('+') {
        theme::status_color(moonlight_domain::session::SessionStatus::Running)
    } else if line.starts_with('-') {
        theme::status_color(moonlight_domain::session::SessionStatus::Errored)
    } else {
        theme::text_secondary()
    }
}

/// Render one diff line (blank lines keep a space so rows don't collapse).
fn diff_line(line: &str) -> impl IntoElement {
    div()
        .font_family(theme::mono_font())
        .text_size(px(12.))
        .text_color(diff_line_color(line))
        .child(if line.is_empty() {
            " ".to_string()
        } else {
            line.to_string()
        })
}

impl Focusable for CodeReviewPanel {
    /// Route focus into whichever text field is open, so an activated tab types
    /// into it (same reason as [`CodeEditorPanel`](super::code_editor)). Without
    /// this the dock focuses the panel root on activation and the comment /
    /// rejection fields render but never receive a keystroke.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.comment_input
            .as_ref()
            .or_else(|| self.reject_input.as_ref().map(|(_, input)| input))
            .map(|input| input.read(cx).focus_handle(cx))
            .unwrap_or_else(|| self.focus_handle.clone())
    }
}

impl super::TabMenuHost for CodeReviewPanel {
    fn tab_menu_slot(&mut self) -> &mut Option<super::TabMenu> {
        &mut self.tab_menu
    }
}

impl EventEmitter<PanelEvent> for CodeReviewPanel {}

impl Panel for CodeReviewPanel {
    fn panel_name(&self) -> &'static str {
        "CodeReview"
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        super::tab_title(
            format!("Review · {}", short_id(&self.session)),
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

    /// Persist the session id + repo root so a rehydrated tab can reload the diff.
    fn dump(&self, _cx: &App) -> PanelState {
        let mut state = PanelState::new(self);
        state.info = PanelInfo::panel(serde_json::json!({
            "session": self.session.as_str(),
            "root": self.root.as_ref().map(|p| p.to_string_lossy().into_owned()),
            "summary": self.summary,
        }));
        state
    }
}

impl CodeReviewPanel {
    fn file_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // (marker, label, stable key) per file, indexed the same as the underlying
        // list so a tree row can point back at it. The key identifies the ROW ACROSS
        // FRAMES — an index would not: the list can change under the pointer between
        // press and release, and GPUI would match the release to whatever file had
        // since moved into that slot.
        let entries: Vec<(String, String, String)> = match self.base {
            DiffBase::Session => self
                .ledger
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    // Keyed on the *baseline*, not on the last tool to write the
                    // file: a file first edited through `Edit` and later touched by
                    // a shell command still has its exact pre-image, and the marker
                    // has to agree with what the pane says it is showing.
                    // `+` created, `~` before-side came from HEAD, else write count.
                    let marker = if f.created {
                        "+".to_string()
                    } else if f.from_head {
                        "~".to_string()
                    } else {
                        f.touches.to_string()
                    };
                    (marker, self.file_label(i), f.path.clone())
                })
                .collect(),
            DiffBase::GitHead => self
                .files
                .iter()
                .map(|f| {
                    let label = f.display();
                    (f.status.letter().to_string(), label.clone(), label)
                })
                .collect(),
        };

        let paths: Vec<(usize, String)> = entries
            .iter()
            .enumerate()
            .filter(|(i, (_, label, key))| {
                if self.is_ignored(label) {
                    return false;
                }
                // A file with nothing left in it drops out — but never the one being
                // read. Yanking the open file out of the tree the moment its last
                // change is undone would leave the panes showing a file the list
                // says isn't there.
                self.show_unchanged || self.selected == Some(*i) || !self.is_unchanged(key)
            })
            .map(|(i, (_, label, _))| (i, label.clone()))
            .collect();
        let rows = path_tree::rows(&paths, &|dir| self.collapsed_dirs.contains(dir));

        div()
            .id("review-files")
            .w(px(240.))
            .flex_none()
            .overflow_y_scroll()
            .border_r_1()
            .border_color(theme::border_subtle())
            .flex()
            .flex_col()
            .children(rows.into_iter().map(|row| {
                // One indent step per level, leaving the first column for the caret
                // (directories) or the status marker (files).
                let indent = px(6. + row.depth as f32 * 12.);
                match row.kind {
                    path_tree::TreeKind::Dir { path, collapsed } => {
                        let menu_path = path.clone();
                        div()
                            .id(SharedString::from(format!("review-dir:{path}")))
                            .cursor_pointer()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .pl(indent)
                            .pr_2()
                            .py(px(2.))
                            .hover(|d| d.bg(theme::row_hover()))
                            .child(
                                div()
                                    .w(px(10.))
                                    .text_size(px(9.))
                                    .text_color(theme::text_muted())
                                    .child(if collapsed { "▸" } else { "▾" }),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(theme::text_muted())
                                    .child(row.label),
                            )
                            .on_click(cx.listener(move |this, _ev, _window, cx| {
                                this.toggle_dir(path.clone(), cx)
                            }))
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                                    this.ignore_menu = Some((menu_path.clone(), ev.position));
                                    cx.notify();
                                }),
                            )
                    }
                    path_tree::TreeKind::File { index } => {
                        let selected = self.selected == Some(index);
                        let (marker, label_for_menu, key) = entries[index].clone();
                        // A live sign-off, i.e. one the session hasn't invalidated by
                        // writing the file again. Only the ledger records touches, so
                        // only the session base can offer it.
                        let reviewed = self.base == DiffBase::Session
                            && self.ledger.get(index).is_some_and(|f| f.reviewed);
                        // Status here, control at the top of the diff itself: the
                        // tree answers "what is left to read", and signing a file
                        // off is something you do having just read it.
                        let check = reviewed.then(|| {
                            div()
                                .flex_none()
                                .text_size(px(10.))
                                .text_color(theme::git_added())
                                .child("✓")
                        });
                        div()
                            .id(SharedString::from(format!("review-file:{key}")))
                            .cursor_pointer()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .pl(indent)
                            .pr_2()
                            .py(px(3.))
                            .when(selected, |d| d.bg(theme::tint(theme::accent(), 0.12)))
                            .hover(|d| d.bg(theme::row_hover()))
                            .child(
                                div()
                                    .w(px(12.))
                                    .flex_none()
                                    .text_size(px(11.))
                                    .text_color(theme::text_muted())
                                    .child(marker),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_size(px(12.))
                                    // Reviewed files recede rather than disappear —
                                    // they are still part of what Approve accepts.
                                    .text_color(match (selected, reviewed) {
                                        (true, _) => theme::text_primary(),
                                        (false, true) => theme::text_muted(),
                                        (false, false) => theme::text_secondary(),
                                    })
                                    .child(row.label),
                            )
                            .children(check)
                            .on_click(
                                cx.listener(move |this, _ev, _window, cx| this.select(index, cx)),
                            )
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                                    this.ignore_menu = Some((label_for_menu.clone(), ev.position));
                                    cx.notify();
                                }),
                            )
                    }
                }
            }))
    }

    /// Hide a file from this review, and drop any selection that pointed at it.
    fn ignore_path(&mut self, path: String, cx: &mut Context<Self>) {
        if let Some(changes) = &self.changes {
            if let Err(err) = changes.set_ignored(&self.session, &path, true) {
                tracing::warn!(error = %err, "persisting an ignored review path failed");
            }
        }
        self.ignored.insert(path);
        self.ignore_menu = None;
        // The shown file may be the one just hidden; fall to the first that is left
        // so the panes never keep displaying something the tree no longer lists.
        let still_listed = self
            .selected
            .and_then(|i| self.ledger.get(i).map(|_| self.file_label(i)))
            .is_some_and(|label| !self.is_ignored(&label));
        if !still_listed {
            let next = (0..self.ledger.len()).find(|i| !self.is_ignored(&self.file_label(*i)));
            match next {
                Some(i) => self.select(i, cx),
                None => {
                    self.selected = None;
                    self.rows.clear();
                    self.rows_gap = Some("Every file is ignored in this review.".into());
                }
            }
        }
        cx.notify();
    }

    /// Whether `label` is hidden from this review — itself, or by an ignored
    /// directory above it. Ignoring a folder has to hide what is *under* it, which
    /// an exact-match set cannot express.
    fn is_ignored(&self, label: &str) -> bool {
        is_ignored_by(&self.ignored, label)
    }

    /// Put a file back to the state the session found it in.
    ///
    /// The ledger's pre-image is the only record of that state — for a file the
    /// session both created and committed, VCS cannot help — so this is the one
    /// action here that writes to the operator's tree. It takes two clicks, and it
    /// leaves the ledger entry alone: the file is now identical to its baseline, so
    /// the diff simply shows no change, and re-running the agent can change it again.
    fn revert_file(&mut self, label: String, cx: &mut Context<Self>) {
        if self.revert_armed.as_deref() != Some(label.as_str()) {
            self.revert_armed = Some(label);
            cx.notify();
            return;
        }
        self.revert_armed = None;
        self.ignore_menu = None;

        let Some(index) = (0..self.ledger.len()).find(|i| self.file_label(*i) == label) else {
            return;
        };
        let path = self.ledger[index].path.clone();
        let baseline = self
            .changes
            .as_ref()
            .and_then(|changes| changes.baseline(&self.session, &path).ok().flatten());

        let outcome = match baseline {
            Some(Baseline::Content(text)) | Some(Baseline::FromHead(text)) => {
                std::fs::write(&path, text)
                    .map(|()| format!("Reverted {label} to its state before this session."))
                    .map_err(|e| format!("Couldn't write {path}: {e}"))
            }
            // The session created it, so the state before is "absent".
            Some(Baseline::Created) => std::fs::remove_file(&path)
                .map(|()| format!("Deleted {label} — the session created it."))
                .map_err(|e| format!("Couldn't delete {path}: {e}")),
            Some(Baseline::Unavailable { .. }) => Err(format!(
                "No pre-image was captured for {label}, so there is nothing to restore."
            )),
            None => Err(format!("{label} is no longer in this session's ledger.")),
        };
        match outcome {
            Ok(note) => {
                self.error = None;
                self.review_output = Some(note);
                self.select(index, cx);
            }
            Err(err) => self.error = Some(err),
        }
        cx.notify();
    }

    /// Bring every ignored file back.
    fn clear_ignored(&mut self, cx: &mut Context<Self>) {
        if let Some(changes) = &self.changes {
            if let Err(err) = changes.clear_ignored(&self.session) {
                tracing::warn!(error = %err, "clearing the review's ignore list failed");
            }
        }
        self.ignored.clear();
        cx.notify();
    }

    /// Sign off on a file, or take that back — GitHub's "viewed" checkbox.
    ///
    /// What is stored is *when* it was marked, so the mark stops counting the moment
    /// the session writes the file again. A reviewed file is dimmed, not hidden: it
    /// is still part of the change, and hiding it would make the tree disagree with
    /// what Approve is about to accept.
    fn toggle_reviewed(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(file) = self.ledger.get(index) else {
            return;
        };
        let path = file.path.clone();
        let at = (!file.reviewed).then(now);
        if let Some(changes) = &self.changes {
            if let Err(err) = changes.mark_reviewed(&self.session, &path, at) {
                tracing::warn!(error = %err, "marking a file reviewed failed");
                self.error = Some(format!("Couldn't mark {path} reviewed: {err}"));
                cx.notify();
                return;
            }
        }
        if let Some(file) = self.ledger.get_mut(index) {
            file.reviewed = at.is_some();
        }
        cx.notify();
    }

    /// How many of the listed (non-ignored) files carry a live review mark, over how
    /// many there are.
    fn reviewed_count(&self) -> (usize, usize) {
        // Counts what the operator can actually see and sign off. A file with nothing
        // in it sitting in the denominator makes "12/14 reviewed" unreachable.
        let listed = (0..self.ledger.len()).filter(|i| {
            !self.is_ignored(&self.file_label(*i)) && !self.is_unchanged(&self.ledger[*i].path)
        });
        let mut total = 0;
        let mut done = 0;
        for i in listed {
            total += 1;
            done += usize::from(self.ledger[i].reviewed);
        }
        (done, total)
    }

    /// The one-item menu behind a right-click on a file.
    fn ignore_menu(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let (target, at) = self.ignore_menu.clone()?;
        // Name what will disappear: a folder takes everything under it with it, so
        // "Ignore in this review" alone would understate the action.
        let label = format!("Ignore {} in this review", short_target(&target));
        // Revert applies to a file, not a directory: reverting a folder would
        // overwrite many files from one click.
        let revert = (self.base == DiffBase::Session
            && (0..self.ledger.len()).any(|i| self.file_label(i) == target))
        .then(|| target.clone());
        let dismiss = cx.listener(|this, _ev: &MouseDownEvent, _w, cx| {
            this.ignore_menu = None;
            cx.notify();
        });
        Some(
            deferred(
                anchored().child(
                    div()
                        .occlude()
                        .size_full()
                        .on_mouse_down(MouseButton::Left, dismiss)
                        .child(
                            anchored()
                                .position(at)
                                .snap_to_window_with_margin(px(8.))
                                .child(
                                    div()
                                        .id("ignore-menu")
                                        .flex()
                                        .flex_col()
                                        .gap(px(2.))
                                        .rounded(theme::radius_sm())
                                        .border_1()
                                        .border_color(theme::border_subtle())
                                        .bg(theme::surface_overlay())
                                        .shadow(theme::overlay_shadow())
                                        .px_3()
                                        .py(px(5.))
                                        .text_size(theme::text_sm())
                                        .text_color(theme::text_secondary())
                                        .hover(|d| d.text_color(theme::text_primary()))
                                        .child(
                                            div()
                                                .id("ignore-item")
                                                .cursor_pointer()
                                                .hover(|d| d.text_color(theme::text_primary()))
                                                .child(label)
                                                .on_click(cx.listener(
                                                    move |this, _ev, _window, cx| {
                                                        this.ignore_path(target.clone(), cx)
                                                    },
                                                )),
                                        )
                                        .children(revert.map(|target| {
                                            let armed = self.revert_armed.as_deref()
                                                == Some(target.as_str());
                                            div()
                                                .id("revert-item")
                                                .cursor_pointer()
                                                .text_color(if armed {
                                                    theme::git_deleted()
                                                } else {
                                                    theme::text_secondary()
                                                })
                                                .hover(|d| d.text_color(theme::git_deleted()))
                                                .child(if armed {
                                                    "Really revert? This overwrites the file"
                                                        .to_string()
                                                } else {
                                                    "Revert to the state before this session"
                                                        .to_string()
                                                })
                                                .on_click(cx.listener(
                                                    move |this, _ev, _window, cx| {
                                                        this.revert_file(target.clone(), cx)
                                                    },
                                                ))
                                        })),
                                ),
                        ),
                ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// Collapse or reveal one directory in the file tree.
    fn toggle_dir(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.collapsed_dirs.remove(&path) {
            self.collapsed_dirs.insert(path);
        }
        cx.notify();
    }

    /// One side of one row: the gutter line number and the line itself, tinted by
    /// what happened to it and highlighted while selected.
    fn pane_cell(
        &self,
        row_idx: usize,
        side: DiffSide,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let Some(row) = self.rows.get(row_idx) else {
            return div().into_any_element();
        };
        let line = match side {
            DiffSide::Before => row.left.as_ref(),
            DiffSide::After => row.right.as_ref(),
        };
        let selected = self
            .selection
            .is_some_and(|s| s.side == side && s.contains(row_idx));

        // A filler row is the *absence* of a line on this side. It gets the gap
        // wash so the eye reads the two panes as one alignment rather than as two
        // lists that happen to sit side by side.
        let Some(line) = line else {
            return div()
                .h(px(ROW_H))
                .bg(theme::diff_gap_bg())
                .into_any_element();
        };

        // Hue in the wash and the glyph; the code itself keeps reading contrast,
        // stepped up for changed lines so emphasis comes from contrast, not color.
        let (glyph, wash) = match (row.kind, side) {
            (RowKind::Equal, _) => (None, None),
            (RowKind::Insert, _) => (
                Some(("+", theme::git_added())),
                Some(theme::diff_added_bg()),
            ),
            (RowKind::Delete, _) => (
                Some(("−", theme::git_deleted())),
                Some(theme::diff_removed_bg()),
            ),
            (RowKind::Replace, DiffSide::Before) => (
                Some(("−", theme::git_deleted())),
                Some(theme::diff_removed_bg()),
            ),
            (RowKind::Replace, DiffSide::After) => (
                Some(("~", theme::git_modified())),
                Some(theme::diff_modified_bg()),
            ),
        };
        let code_color = if row.kind.is_change() {
            theme::text_primary()
        } else {
            theme::text_secondary()
        };

        // Is this the line a new comment would hang under?
        let opens_comment = self.comment_input.is_none()
            && self.selection.is_some_and(|sel| {
                sel.side == side && *sel.range().end() == row_idx && self.selected_lines().is_some()
            });

        let number = line.number;
        let text = line.text.clone();
        let mut cell = div()
            .id(("diff-row", row_idx * 2 + side_index(side)))
            .cursor_pointer()
            .flex()
            .flex_row()
            .items_center()
            .h(px(ROW_H))
            // Line number: right-aligned so the digits form a clean edge against
            // the change glyph, whatever the file's length.
            .child(
                div()
                    .w(px(GUTTER_W - 14.))
                    .flex_none()
                    .pr_1()
                    .text_align(gpui::TextAlign::Right)
                    .font_family(theme::mono_font())
                    .text_size(theme::text_2xs())
                    .text_color(theme::text_muted())
                    .child(number.to_string()),
            )
            // The change glyph — the only place the hue appears in a row.
            .child(
                div()
                    .w(px(14.))
                    .flex_none()
                    .font_family(theme::mono_font())
                    .text_size(theme::text_2xs())
                    .text_color(glyph.map(|(_, color)| color).unwrap_or(theme::text_muted()))
                    .child(glyph.map(|(mark, _)| mark).unwrap_or(" ")),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .font_family(theme::mono_font())
                    .text_size(theme::text_sm())
                    .text_color(code_color)
                    .child(if text.is_empty() {
                        " ".to_string()
                    } else {
                        text
                    }),
            )
            // The comment affordance appears on the LAST row of a selection, on the
            // side it is anchored to — one control, on the line the thread will hang
            // under, rather than a button parked elsewhere on screen.
            .children(opens_comment.then(|| {
                div()
                    .id(("comment-open", row_idx))
                    .flex_none()
                    .px_1()
                    .cursor_pointer()
                    .text_size(theme::text_2xs())
                    .text_color(theme::accent())
                    .child("💬 Comment")
                    .on_click(
                        cx.listener(move |this, _ev, window, cx| this.begin_comment(window, cx)),
                    )
            }))
            // Press-drag-release selects a range, and shift-click extends one —
            // both, because a reviewer reaches for whichever is closer to hand.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                    // ⌃/⌘-click asks about the word under the pointer instead of
                    // selecting the line — the gesture every editor uses to chase a
                    // symbol, doing here what this surface can honestly do: find it
                    // everywhere in the diff.
                    if ev.modifiers.control || ev.modifiers.platform {
                        this.open_symbol_menu(side, row_idx, ev.position, cx);
                        return;
                    }
                    this.begin_select(side, row_idx, ev.modifiers.shift, cx)
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                    this.open_symbol_menu(side, row_idx, ev.position, cx)
                }),
            )
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _window, cx| {
                // The button coming up outside any row would otherwise leave the
                // drag armed; the event carries what's held, so it self-heals.
                if ev.pressed_button != Some(MouseButton::Left) {
                    this.dragging = false;
                } else if this.dragging {
                    this.extend_select(row_idx, cx);
                }
            }));

        if selected {
            cell = cell.bg(theme::row_selected());
        } else if let Some(wash) = wash {
            cell = cell.bg(wash);
        }
        cell.into_any_element()
    }

    /// The two panes, scrolled as one so the rows stay opposite each other.
    fn side_by_side_view(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if let Some(note) = &self.rows_gap {
            return div()
                .flex_1()
                .p_3()
                .text_size(px(12.))
                .text_color(theme::text_muted())
                .child(note.clone())
                .into_any_element();
        }

        // The left pane means different things depending on where the pre-image came
        // from, and saying so is the difference between an exact diff and one that
        // may carry uncommitted work the session didn't do.
        // The signature of this surface: it knows *how* it knows the before side.
        // Nothing else in a diff tool can say whether the previous state was read
        // before the agent wrote, recovered from VCS afterwards, or never existed —
        // and the difference decides how much to trust what is on the left.
        let shown = self.selected.and_then(|i| self.ledger.get(i));
        let provenance = match shown {
            Some(file) if file.created => ("new file", theme::git_added()),
            Some(file) if file.from_head => ("from HEAD", theme::git_modified()),
            _ => ("as first touched", theme::text_muted()),
        };
        // Whether this file has anything foldable, and whether any of it is open —
        // the toggle is pointless on a file that is one big change.
        let runs = pair_diff::foldable_runs(&self.rows, FOLD_CONTEXT);
        let folds = !runs.is_empty();
        let expanded_any = runs
            .iter()
            .any(|run| self.expanded.contains_key(&run.start));

        // A pane header: its name, and — on the before side — the tag saying where
        // that content came from.
        let header = |label: &'static str, tag: Option<(&'static str, gpui::Hsla)>| {
            div()
                .flex_1()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_2()
                .py(px(3.))
                .text_size(theme::text_xs())
                .text_color(theme::text_muted())
                .bg(theme::surface_sunken())
                .child(label)
                .children(tag.map(|(text, color)| {
                    div()
                        .px(px(5.))
                        .rounded(theme::radius_sm())
                        .bg(theme::tint(color, 0.16))
                        .text_color(color)
                        .child(text)
                }))
        };

        // Only the changed rows and their context are built: a long file is mostly
        // unchanged, and laying out thousands of identical row pairs every frame is
        // both unreadable and the bulk of the work.
        let rows: Vec<gpui::AnyElement> = self
            .view_rows()
            .into_iter()
            .map(|view| match view {
                ViewRow::Fold { run, hidden } => self.fold_marker(run, hidden, cx),
                ViewRow::Comment(i) => self.comment_bubble(i, true, cx),
                ViewRow::Finding(i) => self.finding_bubble(i, cx),
                ViewRow::Composer => self.inline_composer(true, cx),
                ViewRow::Row(i) => div()
                    .flex()
                    .flex_row()
                    .w_full()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .child(self.pane_cell(i, DiffSide::Before, cx)),
                    )
                    .child(
                        div()
                            .w(px(1.))
                            .flex_none()
                            .h(px(17.))
                            .bg(theme::border_subtle()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .child(self.pane_cell(i, DiffSide::After, cx)),
                    )
                    .into_any_element(),
            })
            .collect();

        div()
            .flex_1()
            .flex()
            .flex_col()
            .min_h(px(0.))
            .children(self.file_banner(cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .child(header("Before", Some(provenance)))
                    .child(header("After · on disk now", None))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_none()
                            .gap_1()
                            .px_2()
                            .children(folds.then(|| {
                                Self::pill(
                                    ("fold-toggle", 0),
                                    if expanded_any {
                                        "⤡ Collapse unchanged"
                                    } else {
                                        "⤢ Expand all"
                                    },
                                    theme::text_muted(),
                                    move |this, _w, cx| {
                                        if expanded_any {
                                            this.collapse_folds(cx)
                                        } else {
                                            this.expand_all_folds(cx)
                                        }
                                    },
                                    cx,
                                )
                            }))
                            .child(Self::pill(
                                ("jump-prev", 0),
                                "↑ Prev change",
                                theme::text_muted(),
                                move |this, _w, cx| this.jump_change(false, cx),
                                cx,
                            ))
                            .child(Self::pill(
                                ("jump-next", 0),
                                "↓ Next change",
                                theme::text_muted(),
                                move |this, _w, cx| this.jump_change(true, cx),
                                cx,
                            )),
                    ),
            )
            .child(
                div()
                    .id("review-panes")
                    .flex_1()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    // Zero-height, so it reports where the panes are without
                    // standing between the pointer and a line of code.
                    .child(canvas(
                        {
                            let panel = cx.entity().downgrade();
                            move |bounds, _window, app| {
                                let _ = panel.update(app, |this: &mut Self, _cx| {
                                    this.panes_bounds = Some(bounds)
                                });
                            }
                        },
                        |_bounds, _state, _window, _app| {},
                    ))
                    .children(rows),
            )
            .into_any_element()
    }

    /// The strip at the top of the diff: which file is open, and the control that
    /// signs it off.
    ///
    /// The checkbox belongs here rather than in the file tree because marking a file
    /// reviewed is something you do *having just read it* — the gesture wants to be
    /// where your eyes already are, at the head of what you read. The tree keeps the
    /// tick as a status marker so "what is left" stays answerable at a glance.
    fn file_banner(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if self.base != DiffBase::Session {
            return None;
        }
        let index = self.selected?;
        let file = self.ledger.get(index)?;
        let reviewed = file.reviewed;
        let settled = self.is_unchanged(&file.path);
        let label = self.file_label(index);
        Some(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_2()
                .py(px(4.))
                .border_b_1()
                .border_color(theme::border_subtle())
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .font_family(theme::mono_font())
                        .text_size(theme::text_xs())
                        .text_color(theme::text_secondary())
                        .child(label),
                )
                // Why this one has no diff, said where the empty panes are — otherwise
                // it reads as a rendering failure.
                .children(settled.then(|| {
                    div()
                        .flex_none()
                        .px(px(5.))
                        .rounded(theme::radius_sm())
                        .bg(theme::tint(theme::text_muted(), 0.16))
                        .text_size(theme::text_2xs())
                        .text_color(theme::text_muted())
                        .child("back to how it started")
                }))
                // A remark about the file as a whole starts here, at the head of the
                // file, rather than being forced onto whichever line was selected.
                .child(Self::pill(
                    ("comment-on-file", index),
                    "＋ Comment on file",
                    theme::text_muted(),
                    move |this, window, cx| {
                        this.begin_scoped_comment(CommentScope::File, window, cx)
                    },
                    cx,
                ))
                .child(
                    div()
                        .id("review-viewed")
                        .flex_none()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1p5()
                        .cursor_pointer()
                        .px_2()
                        .py(px(2.))
                        .rounded(theme::radius_sm())
                        .bg(if reviewed {
                            theme::tint(theme::git_added(), 0.16)
                        } else {
                            theme::tint(theme::text_muted(), 0.10)
                        })
                        .child(
                            div()
                                .size(px(12.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(3.))
                                .border_1()
                                .border_color(if reviewed {
                                    theme::git_added()
                                } else {
                                    theme::border_strong()
                                })
                                .text_size(px(9.))
                                .text_color(theme::git_added())
                                .child(if reviewed { "✓" } else { "" }),
                        )
                        .child(
                            div()
                                .text_size(theme::text_xs())
                                .text_color(if reviewed {
                                    theme::git_added()
                                } else {
                                    theme::text_muted()
                                })
                                .child("Viewed"),
                        )
                        .tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(if reviewed {
                                "Marked reviewed — clears itself if the session writes this file again"
                            } else {
                                "Mark this file reviewed"
                            })
                            .build(window, cx)
                        })
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.toggle_reviewed(index, cx)
                        })),
                )
                .into_any_element(),
        )
    }

    /// The base selector — which question the diff is answering.
    fn base_selector(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = |base: DiffBase, idx: usize, cx: &mut Context<Self>| {
            let active = self.base == base;
            div()
                .id(("review-base", idx))
                .cursor_pointer()
                .px_2()
                .py(px(2.))
                .rounded(px(6.))
                .text_size(px(11.))
                .when(active, |d| {
                    d.bg(theme::tint(theme::accent(), 0.16))
                        .text_color(theme::accent())
                })
                .when(!active, |d| d.text_color(theme::text_muted()))
                .child(base.label())
                .on_click(cx.listener(move |this, _ev, _window, cx| this.set_base(base, cx)))
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(tab(DiffBase::Session, 0, cx))
            .child(tab(DiffBase::GitHead, 1, cx))
    }

    /// A small pill button.
    fn pill(
        id: (&'static str, usize),
        // Owned, so a pill can carry a computed label (a count, a filename) without
        // leaking a `&'static str` on every frame to satisfy the signature.
        label: impl Into<SharedString>,
        color: gpui::Hsla,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(id)
            .cursor_pointer()
            .px_2()
            .py(px(2.))
            .rounded(px(6.))
            .bg(theme::tint(color, 0.16))
            .text_color(color)
            .text_size(px(11.))
            .child(label.into())
            .on_click(cx.listener(move |this, _ev, window, cx| on_click(this, window, cx)))
    }

    /// One hunk: header (index + verdict / controls), the colorized diff body, and —
    /// while rejecting — the inline reason field.
    fn hunk_card(&self, i: usize, hunk: &ReviewHunk, cx: &mut Context<Self>) -> gpui::AnyElement {
        let verdict = self.decisions.get(&i).copied();
        let rejecting = self.reject_input.as_ref().is_some_and(|(idx, _)| *idx == i);

        let controls = match verdict {
            Some(HunkVerdict::Accepted) => div()
                .text_size(px(11.))
                .text_color(theme::status_color(
                    moonlight_domain::session::SessionStatus::Running,
                ))
                .child("✓ accepted · staged")
                .into_any_element(),
            Some(HunkVerdict::Rejected) => div()
                .text_size(px(11.))
                .text_color(theme::status_color(
                    moonlight_domain::session::SessionStatus::Errored,
                ))
                .child("✕ rejected → feedback sent")
                .into_any_element(),
            None if rejecting => div().into_any_element(),
            None => div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(Self::pill(
                    ("hunk-accept", i),
                    "✓ Accept",
                    theme::status_color(moonlight_domain::session::SessionStatus::Running),
                    move |this, _w, cx| this.accept_hunk(i, cx),
                    cx,
                ))
                .child(Self::pill(
                    ("hunk-reject", i),
                    "✕ Reject",
                    theme::status_color(moonlight_domain::session::SessionStatus::Errored),
                    move |this, window, cx| this.begin_hunk_reject(i, window, cx),
                    cx,
                ))
                .into_any_element(),
        };

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .mb_1()
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(theme::text_muted())
                    .child(format!("Hunk {}", i + 1)),
            )
            .child(div().flex_1())
            .child(controls);

        let body = div()
            .flex()
            .flex_col()
            .children(hunk.diff.lines().map(diff_line));

        let mut card = div()
            .id(("hunk", i))
            .rounded(px(8.))
            .border_1()
            .border_color(theme::border_subtle())
            .bg(theme::surface_base())
            .p_2()
            .flex()
            .flex_col()
            .child(header)
            .child(body);

        if rejecting {
            if let Some((_, input)) = self.reject_input.as_ref() {
                let send = Self::pill(
                    ("hunk-reject-send", i),
                    "✕ Send rejection",
                    theme::status_color(moonlight_domain::session::SessionStatus::Errored),
                    move |this, _w, cx| this.send_hunk_reject(i, cx),
                    cx,
                );
                let cancel = Self::pill(
                    ("hunk-reject-cancel", i),
                    "Cancel",
                    theme::text_muted(),
                    move |this, _w, cx| this.cancel_reject(cx),
                    cx,
                );
                card = card.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .mt_2()
                        .child(Input::new(input))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .child(send)
                                .child(cancel),
                        ),
                );
            }
        }

        card.into_any_element()
    }

    /// The hunk list (or the raw-diff fallback when a file has no textual hunks).
    fn hunk_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div()
            .id("review-hunks")
            .flex_1()
            .overflow_y_scroll()
            .bg(theme::surface_raised())
            .p_2()
            .flex()
            .flex_col()
            .gap_2();

        if self.hunks.is_empty() {
            let text = if self.diff.trim().is_empty() {
                "(no textual changes for this file)".to_string()
            } else {
                self.diff.clone()
            };
            col = col.child(
                div()
                    .p_1()
                    .flex()
                    .flex_col()
                    .children(text.lines().map(diff_line)),
            );
        } else {
            col = col.children(
                self.hunks
                    .iter()
                    .enumerate()
                    .map(|(i, h)| self.hunk_card(i, h, cx))
                    .collect::<Vec<_>>(),
            );
        }
        col
    }

    /// "What this covers" band — the agent's latest summary (T4), above the diff.
    fn summary_band(summary: &str) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .rounded(px(8.))
            .border_1()
            .border_color(theme::border_subtle())
            .bg(theme::surface_raised())
            .px_3()
            .py_2()
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(theme::text_muted())
                    .child("What this covers"),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme::text_secondary())
                    .child(summary.to_string()),
            )
    }

    fn action_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // Delivering the review is the surface's terminal action, so it sits with
        // the other verdicts rather than beside the code. Absent until there is
        // something to send.
        let pending = self.unsent().len();
        let send = (pending > 0).then(|| {
            div()
                .id("review-send")
                .cursor_pointer()
                .px_3()
                .py(px(5.))
                .rounded(theme::radius_md())
                .bg(theme::tint(theme::accent(), 0.16))
                .text_color(theme::accent())
                .text_size(theme::text_sm())
                .child(if pending == 1 {
                    "Send review · 1 comment".to_string()
                } else {
                    format!("Send review · {pending} comments")
                })
                .on_click(cx.listener(|this, _ev, _window, cx| this.send_review(cx)))
        });

        let run_label = if self.review_running {
            "Running full code review…"
        } else {
            "Run full code review"
        };
        let run = div()
            .id("run-full-review")
            .cursor_pointer()
            .px_3()
            .py(px(5.))
            .rounded(px(8.))
            .bg(theme::tint(theme::accent(), 0.16))
            .text_color(theme::accent())
            .text_size(px(12.))
            .child(run_label)
            .on_click(cx.listener(|this, _ev, _window, cx| this.run_full_review(cx)));

        let approve = div()
            .id("review-approve")
            .cursor_pointer()
            .px_3()
            .py(px(5.))
            .rounded(px(8.))
            .bg(theme::tint(
                theme::status_color(moonlight_domain::session::SessionStatus::Running),
                0.16,
            ))
            .text_color(theme::status_color(
                moonlight_domain::session::SessionStatus::Running,
            ))
            .text_size(px(12.))
            .child("✓ Approve")
            .on_click(cx.listener(|this, _ev, _window, cx| this.approve(cx)));

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(run)
            // A comment about the change as a whole — pace, ordering, direction —
            // belongs beside the verdicts, not on a line of code it isn't about.
            .child(Self::pill(
                ("comment-general", 0),
                "＋ General comment",
                theme::text_muted(),
                move |this, window, cx| this.begin_scoped_comment(CommentScope::Review, window, cx),
                cx,
            ))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(theme::text_muted())
                    .child(base_hint(self.base)),
            )
            .child(div().flex_1())
            .children(send)
            .child(approve)
    }
}

impl Render for CodeReviewPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Needed before a click can be turned into a column, and free after the
        // first frame.
        self.measure_cell(window);
        let file_count = match self.base {
            DiffBase::Session => self.ledger.len(),
            DiffBase::GitHead => self.files.len(),
        };
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(div().text_size(px(15.)).child("Code review"))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme::text_muted())
                    .child(format!(
                        "session {} · {file_count} file(s)",
                        short_id(&self.session),
                    )),
            )
            .child(div().flex_1())
            .children({
                // Progress through the pass, and the one number that says whether
                // Approve is premature.
                let (done, total) = self.reviewed_count();
                (self.base == DiffBase::Session && done > 0).then(|| {
                    div()
                        .text_size(px(11.))
                        .text_color(if done == total {
                            theme::git_added()
                        } else {
                            theme::text_muted()
                        })
                        .child(format!("{done}/{total} reviewed"))
                })
            })
            // Never a silent drop. The session did write these files, and an operator
            // who remembers it touching one has to be able to find out where it went.
            .children({
                let n = self
                    .ledger
                    .iter()
                    .filter(|f| self.is_unchanged(&f.path))
                    .count();
                let showing = self.show_unchanged;
                (self.base == DiffBase::Session && n > 0).then(|| {
                    Self::pill(
                        ("unchanged-toggle", 0),
                        if showing {
                            format!("{n} unchanged · hide")
                        } else {
                            format!("{n} unchanged · show")
                        },
                        theme::text_muted(),
                        move |this, _w, cx| {
                            this.show_unchanged = !this.show_unchanged;
                            cx.notify();
                        },
                        cx,
                    )
                })
            })
            .children((!self.ignored.is_empty()).then(|| {
                let n = self.ignored.len();
                Self::pill(
                    ("ignored-restore", 0),
                    if n == 1 {
                        "1 ignored · restore".to_string()
                    } else {
                        format!("{n} ignored · restore")
                    },
                    theme::text_muted(),
                    move |this, _w, cx| this.clear_ignored(cx),
                    cx,
                )
            }))
            .child(self.base_selector(cx));

        // Session base → two panes + the comment rail; git base → the hunk list with
        // its accept/reject staging (whose patches are only valid against the index).
        let center = match self.base {
            DiffBase::Session => self.side_by_side_view(cx),
            DiffBase::GitHead => self.hunk_list(cx).into_any_element(),
        };
        let body = match &self.error {
            Some(err) if file_count == 0 => div()
                .flex_1()
                .p_3()
                .text_size(px(12.))
                .text_color(theme::text_muted())
                .child(err.clone())
                .into_any_element(),
            _ => div()
                .flex_1()
                .flex()
                .flex_row()
                .min_h(px(0.))
                .border_1()
                .border_color(theme::border_subtle())
                .rounded(px(8.))
                .overflow_hidden()
                .child(self.file_list(cx))
                .child(center)
                .into_any_element(),
        };

        let mut root = div()
            .track_focus(&self.focus_handle)
            .key_context(REVIEW_CONTEXT)
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                super::close_this_tab(&this.tab_panel, cx.entity(), window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ToggleSearch, window, cx| this.toggle_search(window, cx)),
            )
            .on_action(cx.listener(|this, _: &CloseSearch, _window, cx| this.close_search(cx)))
            .flex()
            .flex_col()
            .gap_3()
            .size_full()
            .bg(theme::surface_base())
            .text_color(theme::text_primary())
            .p_4()
            .child(header)
            .children(self.search_bar(cx))
            .children(self.missing_lanes_band(cx))
            .children(self.review_band(cx))
            .children(self.notes_band(cx))
            .children(self.offscreen_band())
            .children(self.summary.as_ref().map(|s| Self::summary_band(s)))
            .child(body);

        // A delivery note (e.g. "no live terminal") shown while files are present.
        if let (Some(err), false) = (&self.error, file_count == 0) {
            root = root.child(
                div()
                    .text_size(px(11.))
                    .text_color(theme::status_color(
                        moonlight_domain::session::SessionStatus::Errored,
                    ))
                    .child(err.clone()),
            );
        }

        root = root.children(self.approve_confirm_band(cx));
        root = root.child(self.message_box(window, cx));
        root = root.child(self.action_bar(cx));

        if let Some(output) = &self.review_output {
            root = root.child(
                div()
                    .id("review-output")
                    .max_h(px(220.))
                    .overflow_y_scroll()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(theme::border_subtle())
                    .bg(theme::surface_raised())
                    .p_3()
                    .flex()
                    .flex_col()
                    .children(output.lines().map(|line| {
                        div()
                            .text_size(px(11.))
                            .text_color(theme::text_secondary())
                            .child(if line.is_empty() {
                                " ".to_string()
                            } else {
                                line.to_string()
                            })
                    })),
            );
        }

        let dismiss = cx.listener(|this, _: &MouseDownEvent, _w, cx| {
            this.tab_menu = None;
            cx.notify();
        });
        root = root.children(self.ignore_menu(cx));
        root = root.children(self.symbol_menu(cx));

        root.children(super::tab_menu_overlay(
            self.tab_menu.as_ref(),
            dismiss,
            window,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(path: &str, start: u32, end: u32, body: &str) -> ReviewComment {
        ReviewComment {
            id: format!("{path}-{start}"),
            session_id: SessionId::new("s1"),
            scope: CommentScope::Line,
            path: path.into(),
            side: DiffSide::After,
            start_line: start,
            end_line: end,
            body: body.into(),
            anchor_text: None,
            author: CommentAuthor::Operator,
            parent_id: None,
            at: Timestamp::from_millis(0),
            sent_at: None,
            resolved_at: None,
        }
    }

    fn scoped(scope: CommentScope, path: &str, body: &str) -> ReviewComment {
        ReviewComment {
            scope,
            path: path.into(),
            start_line: 0,
            end_line: 0,
            ..comment(path, 0, 0, body)
        }
    }

    #[test]
    fn a_review_reads_as_one_anchored_message() {
        let message = review_message(
            &[
                comment("src/foo.rs", 42, 48, "This retry has no backoff."),
                comment("src/bar.rs", 10, 10, "unwrap() here can panic."),
            ],
            &[],
        );
        assert_eq!(
            message,
            "Review of your changes (2 comments):\n\
             \n\
             src/foo.rs:42-48\n\
             \x20 This retry has no backoff.\n\
             \n\
             src/bar.rs:10\n\
             \x20 unwrap() here can panic.\n"
        );
    }

    #[test]
    fn a_review_leads_with_what_is_true_of_the_whole_change() {
        // Order is meaning here. A session told "land the migration first" only after
        // four line comments has already planned the work in the wrong order.
        let message = review_message(
            &[
                comment("src/foo.rs", 42, 42, "No backoff."),
                scoped(CommentScope::Review, "", "Land the migration first."),
                scoped(
                    CommentScope::File,
                    "src/foo.rs",
                    "This module knows too much.",
                ),
            ],
            &[],
        );
        assert_eq!(
            message,
            "Review of your changes (3 comments):\n\
             \n\
             Review\n\
             \x20 Land the migration first.\n\
             \n\
             src/foo.rs\n\
             \x20 This module knows too much.\n\
             \n\
             src/foo.rs:42\n\
             \x20 No backoff.\n"
        );
    }

    #[test]
    fn approval_is_warned_about_exactly_what_sending_would_carry() {
        // This is the count Approve warns with and the batch Send delivers. If the
        // two ever disagree, the warning is either crying wolf or silently letting a
        // comment be destroyed.
        let mut sent = comment("src/a.rs", 1, 1, "already delivered");
        sent.sent_at = Some(Timestamp::from_millis(5));
        let mut settled = comment("src/b.rs", 2, 2, "dealt with");
        settled.resolved_at = Some(Timestamp::from_millis(6));
        // Settled *and* never sent: the operator closed it themselves, so it is not
        // pending — approving over it destroys nothing they still wanted said.
        let live = comment("src/c.rs", 3, 3, "still needs saying");

        let all = [sent, settled, live];
        let pending = pending_comments(&all);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].path, "src/c.rs");
    }

    /// A store plus a scratch directory, for the one rule here that has to be true
    /// of real files: whether a touched file still holds a change.
    struct Scratch {
        store: moonlight_persistence::Store,
        dir: PathBuf,
        session: SessionId,
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("ml-net-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            Self {
                store: moonlight_persistence::Store::open_in_memory().expect("store"),
                dir,
                session: SessionId::new("s1"),
            }
        }

        /// Record a touch of `name` with the given pre-image, and leave `now` on disk
        /// (`None` deletes it).
        fn touched(&self, name: &str, baseline: Baseline, now: Option<&str>) -> String {
            let path = self.dir.join(name).to_string_lossy().into_owned();
            self.store
                .record_touch(&moonlight_domain::changes::FileTouch {
                    session_id: self.session.clone(),
                    path: path.clone(),
                    at: Timestamp::from_millis(1),
                    tool: moonlight_domain::changes::ChangeTool::Edit,
                    baseline,
                })
                .expect("record");
            match now {
                Some(text) => std::fs::write(&path, text).expect("write"),
                None => {
                    let _ = std::fs::remove_file(&path);
                }
            }
            path
        }

        fn settled(&self, path: &str) -> bool {
            is_back_to_baseline(&self.store, &self.session, path)
        }
    }

    #[test]
    fn a_file_put_back_where_it_started_has_nothing_left_to_review() {
        let s = Scratch::new("restored");
        let before = "fn main() {}\n";

        let restored = s.touched("a.rs", Baseline::Content(before.into()), Some(before));
        assert!(s.settled(&restored), "edited and edited back");

        let still_changed = s.touched(
            "b.rs",
            Baseline::Content(before.into()),
            Some("fn main() { work() }\n"),
        );
        assert!(!s.settled(&still_changed));

        // A trailing newline is a change — the diff would show it, so the list must.
        let newline = s.touched(
            "c.rs",
            Baseline::Content(before.into()),
            Some("fn main() {}"),
        );
        assert!(!s.settled(&newline));

        // Deleting a file that existed before is the biggest change there is.
        let deleted = s.touched("d.rs", Baseline::Content(before.into()), None);
        assert!(!s.settled(&deleted));
    }

    #[test]
    fn a_file_created_and_then_deleted_leaves_nothing_behind() {
        let s = Scratch::new("created");
        let gone = s.touched("new.rs", Baseline::Created, None);
        assert!(s.settled(&gone), "created then removed");

        let kept = s.touched("kept.rs", Baseline::Created, Some("anything\n"));
        assert!(!s.settled(&kept), "a new file is entirely a change");
    }

    #[test]
    fn a_file_whose_pre_image_was_never_captured_stays_in_the_review() {
        // We do not know what it looked like, so we cannot claim it is unchanged.
        // Listing an empty file wastes a glance; hiding a real change loses it.
        let s = Scratch::new("unknown");
        let opaque = s.touched(
            "big.bin",
            Baseline::Unavailable {
                reason: BaselineGap::TooLarge,
            },
            Some("whatever"),
        );
        assert!(!s.settled(&opaque));
    }

    #[test]
    fn elapsed_time_reads_the_way_someone_waiting_reads_it() {
        use std::time::Duration;
        assert_eq!(fmt_elapsed(Duration::from_secs(9)), "9s");
        assert_eq!(fmt_elapsed(Duration::from_secs(75)), "1m 15s");
        assert_eq!(fmt_elapsed(Duration::from_secs(600)), "10m 00s");
        // Past an hour the seconds stop being the interesting digit.
        assert_eq!(fmt_elapsed(Duration::from_secs(3900)), "1h 05m");
    }

    fn finding_state(file: &str, line: u32, summary: &str, note: Option<&str>) -> FindingState {
        FindingState {
            finding: crate::review_findings::Finding {
                key: "H1".into(),
                severity: crate::review_findings::Severity::High,
                route: crate::review_findings::Route::Decision,
                owner: vec!["blind-hunter".into()],
                file: file.into(),
                line,
                summary: summary.into(),
                detail: "Reachable from the HTTP handler.".into(),
                fix: None,
            },
            note: note.map(str::to_string),
            dismissed: false,
        }
    }

    #[test]
    fn a_finding_reaches_the_session_with_the_operators_decision() {
        // The whole loop: the reviewer's claim tells the session WHAT, the
        // operator's note tells it WHICH WAY. Either alone is not actionable.
        let state = finding_state(
            "src/y.rs",
            128,
            "Concurrent moves can retract a win",
            Some("take the game lock, don't serialise at the handler"),
        );
        let message = review_message(&[], &[&state]);

        assert!(message.contains("src/y.rs:128 — [high] Concurrent moves can retract a win"));
        assert!(message.contains("  Reachable from the HTTP handler."));
        assert!(
            message.contains("  → take the game lock, don't serialise at the handler"),
            "the operator's note is marked as theirs: {message}"
        );
        assert!(
            message.starts_with("Review of your changes (1 finding):"),
            "{message}"
        );
    }

    #[test]
    fn findings_and_comments_travel_together() {
        let state = finding_state("src/y.rs", 128, "summary", None);
        let message = review_message(&[comment("a.rs", 3, 3, "my own note")], &[&state]);
        assert!(
            message.starts_with("Review of your changes (1 finding, 1 comment):"),
            "{message}"
        );
        assert!(message.contains("src/y.rs:128"));
        assert!(message.contains("a.rs:3"));
    }

    #[test]
    fn a_single_comment_is_not_pluralized() {
        let message = review_message(&[comment("a.rs", 1, 1, "no")], &[]);
        assert!(message.starts_with("Review of your changes (1 comment):"));
    }

    #[test]
    fn a_multi_line_comment_body_stays_indented_under_its_anchor() {
        // A review is inherently multi-line, so delivery has to carry it as ONE
        // prompt — see `TerminalPanel::send_paste`, which rewrites newlines into the
        // child's literal-newline sequence when bracketed paste isn't available.
        // Sent raw, every `\n` is an Enter and the session receives a fragment.
        let message = review_message(&[comment("a.rs", 3, 3, "first\nsecond")], &[]);
        assert!(
            message.ends_with("a.rs:3\n  first\n  second\n"),
            "{message}"
        );
        assert!(
            message.matches('\n').count() > 3,
            "carries newlines that delivery must not submit on: {message:?}"
        );
    }

    #[test]
    fn a_selection_reports_the_line_range_of_its_own_side() {
        // Rows: one equal, then an insert (which has no line on the before side).
        let rows = pair_diff::side_by_side("a\n", "a\nb\nc\n");
        let selection = Selection {
            side: DiffSide::After,
            anchor: 1,
            head: 2,
        };
        let numbers: Vec<u32> = rows[selection.range()]
            .iter()
            .filter_map(|r| r.right.as_ref())
            .map(|l| l.number)
            .collect();
        assert_eq!(numbers, vec![2, 3]);

        // The same rows carry no before-side lines, so a comment there has nothing
        // to anchor to.
        let before: Vec<u32> = rows[selection.range()]
            .iter()
            .filter_map(|r| r.left.as_ref())
            .map(|l| l.number)
            .collect();
        assert!(before.is_empty());
    }

    #[test]
    fn revealing_a_fold_shrinks_it_from_the_chosen_edge() {
        let run = 10..40; // 30 hidden lines
        assert_eq!(hidden_slice(run.clone(), Revealed::default()), Some(10..40));
        assert_eq!(
            hidden_slice(run.clone(), Revealed { top: 20, bottom: 0 }),
            Some(30..40),
            "revealing from the top eats into the front"
        );
        assert_eq!(
            hidden_slice(run.clone(), Revealed { top: 0, bottom: 20 }),
            Some(10..20),
            "and from the bottom, into the back"
        );
        assert_eq!(
            hidden_slice(run.clone(), Revealed { top: 20, bottom: 5 }),
            Some(30..35),
            "both edges at once"
        );
    }

    #[test]
    fn a_fully_revealed_fold_disappears() {
        let run = 10..40;
        assert_eq!(
            hidden_slice(run.clone(), Revealed { top: 30, bottom: 0 }),
            None
        );
        // The two edges meeting exactly in the middle also closes it.
        assert_eq!(
            hidden_slice(
                run.clone(),
                Revealed {
                    top: 15,
                    bottom: 15
                }
            ),
            None
        );
    }

    #[test]
    fn over_revealing_never_produces_a_backwards_range() {
        // "Expand all" uses a deliberately huge count, and repeated clicks on both
        // chevrons can overshoot — neither may underflow the subtraction.
        let run = 10..40;
        assert_eq!(
            hidden_slice(
                run.clone(),
                Revealed {
                    top: usize::MAX / 2,
                    bottom: 0
                }
            ),
            None
        );
        assert_eq!(
            hidden_slice(
                run.clone(),
                Revealed {
                    top: 999,
                    bottom: 999
                }
            ),
            None
        );
        // An empty run is degenerate but must not panic.
        assert_eq!(hidden_slice(5..5, Revealed { top: 3, bottom: 3 }), None);
    }

    #[test]
    fn ignoring_a_folder_hides_what_is_under_it_and_nothing_else() {
        let ignored: std::collections::HashSet<String> =
            ["crates/domain".to_string(), "notes.md".to_string()]
                .into_iter()
                .collect();

        assert!(is_ignored_by(&ignored, "crates/domain/src/changes.rs"));
        assert!(
            is_ignored_by(&ignored, "crates/domain"),
            "the folder itself"
        );
        assert!(is_ignored_by(&ignored, "notes.md"), "a single file");

        // The boundary case: a sibling that merely starts with the same characters
        // must survive, or ignoring `src` would silently swallow `src2/`.
        assert!(!is_ignored_by(&ignored, "crates/domain-extra/x.rs"));
        assert!(!is_ignored_by(&ignored, "crates/engine/src/lib.rs"));
        assert!(!is_ignored_by(&ignored, "notes.md.bak"));
        // A parent of an ignored path is not itself ignored.
        assert!(!is_ignored_by(&ignored, "crates"));
    }

    #[test]
    fn nothing_is_ignored_by_an_empty_set() {
        let empty = std::collections::HashSet::new();
        assert!(!is_ignored_by(&empty, "anything/at/all.rs"));
    }

    #[test]
    fn a_click_lands_on_the_whole_identifier_it_touches() {
        let line = "    this.send_paste(&text)?;";
        // Anywhere inside the word gives the same word — the point of the gesture,
        // since a pointer lands mid-token far more often than on its first char.
        for col in [9, 13, 18] {
            assert_eq!(
                word_at(line, col).as_deref(),
                Some("send_paste"),
                "col {col}"
            );
        }
        assert_eq!(word_at(line, 5).as_deref(), Some("this"));
        assert_eq!(word_at(line, 21).as_deref(), Some("text"));
        // Punctuation and indentation are not identifiers: guessing at a neighbour
        // would send the operator hunting for a symbol they never clicked.
        assert_eq!(word_at(line, 0), None, "leading whitespace");
        assert_eq!(word_at(line, 8), None, "the dot");
        assert_eq!(word_at(line, 999), None, "past the end of the line");
    }

    #[test]
    fn word_columns_are_characters_not_bytes() {
        // A comment in the diff can be any language; counting bytes would slide the
        // column right by the width of every multi-byte character before it.
        let line = "// é accentué: parse_value(x)";
        assert_eq!(word_at(line, 18).as_deref(), Some("parse_value"));
    }

    #[test]
    fn the_menu_offers_the_lines_symbols_without_the_noise() {
        assert_eq!(
            identifiers("    let x = parse_value(input, 42) + parse_value(other);"),
            vec!["let", "parse_value", "input", "other"],
            "no repeats, no bare numbers, no single letters"
        );
        assert!(identifiers("  }); // ...").is_empty());
    }

    /// Two rows, one containing `send_paste`.
    fn searchable_rows() -> Vec<DiffRow> {
        pair_diff::side_by_side(
            "fn send_text(&self) {}\nlet x = 1;\n",
            "fn send_paste(&self) {}\nlet x = 1;\n",
        )
    }

    #[test]
    fn a_query_matches_either_side_of_the_diff() {
        let rows = searchable_rows();
        let hits: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                [row.left.as_ref(), row.right.as_ref()]
                    .into_iter()
                    .flatten()
                    .any(|line| line.text.to_lowercase().contains("send_paste"))
            })
            .map(|(i, _)| i)
            .collect();
        assert_eq!(hits, vec![0], "matches the after side");

        // …and the before side, which is where a symbol being REMOVED lives — the
        // case a match against only the new text would miss entirely.
        let removed: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                [row.left.as_ref(), row.right.as_ref()]
                    .into_iter()
                    .flatten()
                    .any(|line| line.text.to_lowercase().contains("send_text"))
            })
            .map(|(i, _)| i)
            .collect();
        assert_eq!(removed, vec![0]);
    }

    #[test]
    fn folding_never_hides_a_match() {
        // 20 identical rows with one change: everything outside the change folds by
        // default, so a match inside a folded run must keep that run open or the
        // operator hunts for something the panel is deliberately not showing.
        let before: String = (0..20).map(|i| format!("line {i}\n")).collect();
        let after = before.replace("line 10", "CHANGED");
        let rows = pair_diff::side_by_side(&before, &after);
        let runs = pair_diff::foldable_runs(&rows, FOLD_CONTEXT);

        let needle = rows
            .iter()
            .position(|r| {
                r.right
                    .as_ref()
                    .is_some_and(|l| l.text.contains("line 2") && !l.text.contains("line 20"))
            })
            .expect("a row far from the change");
        assert!(
            runs.iter().any(|run| run.contains(&needle)),
            "the row is inside a fold to begin with"
        );

        // The view builder skips any run containing a match, which is what keeps it
        // visible.
        let kept: Vec<_> = runs
            .iter()
            .filter(|run| !run.contains(&needle))
            .cloned()
            .collect();
        assert!(
            !kept.iter().any(|run| run.contains(&needle)),
            "no surviving fold hides the match"
        );
    }

    #[test]
    fn selection_range_is_order_independent() {
        // Dragging upward must select the same rows as dragging downward.
        let up = Selection {
            side: DiffSide::After,
            anchor: 7,
            head: 3,
        };
        assert_eq!(up.range(), 3..=7);
        assert!(up.contains(5));
        assert!(!up.contains(8));
    }
}
