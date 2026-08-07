//! Per-session file-change ledger: which files an *agent* wrote, what they looked
//! like before it did, and the operator's review comments on the result.
//!
//! The ledger is what makes review answer "what did **this session** change?"
//! instead of git's "what is dirty in this tree?" — the working tree can't tell an
//! agent's edit from the operator's, and loses a change entirely once it is
//! committed. A touch is recorded when a session is *about to* run a file-writing
//! tool, so the file still on disk at that moment is the [`Baseline`].

use serde::{Deserialize, Serialize};

use crate::ids::{SessionId, Timestamp};

/// The tool a touch came from.
///
/// The first four declare their target path, so the write is attributed directly
/// and its pre-image read before the tool runs. [`Shell`](Self::Shell) is inferred
/// instead: a `Bash` command names no file, so the write is found by comparing the
/// workspace before and after it ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeTool {
    Edit,
    Write,
    MultiEdit,
    NotebookEdit,
    /// A write observed around a `Bash` command — a redirect, `sed -i`, a
    /// formatter, a code generator.
    Shell,
}

impl ChangeTool {
    /// Whether the write was attributed by watching the workspace rather than by
    /// the tool declaring its target. The review surface says so, because the
    /// pre-image for these comes from VCS rather than from the file itself.
    pub fn is_inferred(self) -> bool {
        matches!(self, ChangeTool::Shell)
    }
}

/// Why a file's pre-image could not be kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BaselineGap {
    /// Past the capture size cap — snapshotting it would bloat the store and the
    /// diff would be unreadable anyway.
    TooLarge,
    /// Not UTF-8 text, so there is nothing to diff line-wise.
    Binary,
    /// Existed but could not be read (permissions, a race with the agent).
    Unreadable,
}

/// A file's content the first time a session touched it — the "before" side of the
/// review. Captured once per (session, file): later touches diff against the same
/// baseline, so the panes show the session's *whole* effect on the file rather than
/// just its last edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Baseline {
    /// The file existed; this is what it held before the session's first write.
    /// Read from disk while the writing tool was still blocked, so it is exact.
    Content(String),
    /// The file's content at `HEAD`. Used for a write we only inferred (a shell
    /// command), where no one read the file before it changed: VCS is then the best
    /// available "before". Exact when the file was committed-clean at that point,
    /// and otherwise includes whatever was already uncommitted — which is why it is
    /// a distinct variant rather than passed off as [`Content`](Self::Content).
    FromHead(String),
    /// The agent created this file — the "before" side is empty.
    Created,
    /// No pre-image is available; the review surface says so instead of showing a
    /// misleading empty pane.
    Unavailable { reason: BaselineGap },
}

impl Baseline {
    /// The text to show as the "before" side. A created file reads as empty; a gap
    /// has no text at all (the caller renders the reason instead).
    pub fn text(&self) -> Option<&str> {
        match self {
            Baseline::Content(text) | Baseline::FromHead(text) => Some(text),
            Baseline::Created => Some(""),
            Baseline::Unavailable { .. } => None,
        }
    }

    /// Whether the agent created the file (no prior content on disk).
    pub fn is_created(&self) -> bool {
        matches!(self, Baseline::Created)
    }

    /// Whether the "before" side came from VCS rather than from the file itself —
    /// the review surface labels the pane accordingly, since anything already
    /// uncommitted is inside such a diff.
    pub fn is_from_head(&self) -> bool {
        matches!(self, Baseline::FromHead(_))
    }
}

/// One observed write, as reported by the hook before the tool runs. The write side
/// of the ledger; [`TouchedFile`] is what comes back out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTouch {
    pub session_id: SessionId,
    /// Absolute path — the hook's `cwd` resolves relative tool inputs.
    pub path: String,
    pub at: Timestamp,
    pub tool: ChangeTool,
    /// Captured only on the first touch of this file by this session; the store
    /// ignores it afterwards.
    pub baseline: Baseline,
}

/// A file this session has written, as the ledger reports it: names and counts,
/// never the pre-image.
///
/// A baseline can be megabytes, so it is fetched one file at a time — only for
/// whatever is actually on screen — via
/// [`SessionChangeStore::baseline`](crate::ports::store::SessionChangeStore::baseline).
/// Listing a session's changes, or counting them for a badge, must not drag every
/// snapshot in the ledger through memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TouchedPath {
    pub path: String,
    pub touches: u32,
    /// The tool that wrote it **most recently**. Informational: for "what does the
    /// before-side mean", read [`from_head`](Self::from_head) instead — a file first
    /// edited through `Edit` and later touched by a shell command keeps its exact
    /// pre-image.
    pub tool: ChangeTool,
    /// Whether the agent created the file (its baseline is [`Baseline::Created`]).
    pub created: bool,
    /// Whether its baseline came from VCS rather than an observed pre-image
    /// ([`Baseline::FromHead`]).
    pub from_head: bool,
    /// Whether the operator has marked this file reviewed **and** the session has
    /// not written it since.
    ///
    /// Derived from a comparison, not stored as a flag: the mark carries the time
    /// it was made, and a later touch advances `last_touch_at` past it. So a file
    /// the agent rewrites comes back unreviewed by construction — nothing has to
    /// remember to clear anything, which is the failure mode that would quietly
    /// hide a change from the operator.
    pub reviewed: bool,
}

/// Which pane a comment is anchored to. A comment on removed code belongs to the
/// before side; everything else to the after side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffSide {
    Before,
    After,
}

/// What a comment is about, and therefore where it can be shown and how it is
/// addressed when the review is delivered.
///
/// Not every remark fits on a line. "This module should not know about the store"
/// is about the file; "land the migration before the panel" is about the change as
/// a whole. Forcing those onto whichever line happened to be selected makes them
/// read as being about that line, which is how a reviewer's actual point gets lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommentScope {
    /// A line range in one pane of one file.
    Line,
    /// The file as a whole.
    File,
    /// The whole review — no file, no line.
    Review,
}

impl CommentScope {
    /// Whether this scope pins the comment to code that can move under it.
    pub fn is_line(self) -> bool {
        matches!(self, CommentScope::Line)
    }
}

/// An operator's note on a reviewed change — a line range, a file, or the review
/// itself (see [`CommentScope`]). Persisted the moment it is written (so closing the
/// tab doesn't lose it) and batched: comments accumulate until the operator sends
/// the review, which delivers them to the session as one message and stamps
/// [`sent_at`](Self::sent_at).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewComment {
    pub id: String,
    pub session_id: SessionId,
    pub scope: CommentScope,
    /// The file the comment is on. Empty for [`CommentScope::Review`].
    pub path: String,
    pub side: DiffSide,
    /// 1-based, inclusive line range in the pane named by [`side`](Self::side).
    /// Both zero for a file- or review-scoped comment.
    pub start_line: u32,
    pub end_line: u32,
    pub body: String,
    /// The anchored line as it read when the comment was written, kept so the
    /// comment can tell whether it is still about the code beneath it.
    ///
    /// A line *number* is not an anchor — insert a line above it and it now names
    /// something else entirely, which is how a review ends up arguing with code
    /// nobody wrote. Comparing the text is what turns that silent drift into a
    /// visible "outdated". `None` for comments written before this was recorded, and
    /// for scopes that aren't pinned to a line.
    pub anchor_text: Option<String>,
    pub at: Timestamp,
    /// `None` until the review carrying this comment is sent to the session.
    pub sent_at: Option<Timestamp>,
    /// When the operator marked this comment settled. Resolved comments stay in the
    /// record — a review is a conversation, and deleting the half that got fixed
    /// loses why the code looks the way it does — but they leave the diff and are
    /// not delivered again.
    pub resolved_at: Option<Timestamp>,
}

impl ReviewComment {
    /// `path:line`, `path:start-end`, `path`, or `Review` — how the comment is
    /// addressed when it is written into the session's prompt.
    pub fn anchor(&self) -> String {
        match self.scope {
            CommentScope::Review => "Review".to_string(),
            CommentScope::File => self.path.clone(),
            CommentScope::Line if self.start_line == self.end_line => {
                format!("{}:{}", self.path, self.start_line)
            }
            CommentScope::Line => {
                format!("{}:{}-{}", self.path, self.start_line, self.end_line)
            }
        }
    }

    pub fn is_resolved(&self) -> bool {
        self.resolved_at.is_some()
    }

    /// Whether the code this comment points at has changed since it was written.
    ///
    /// `line_now` is the anchored line as it reads in the diff *now*, or `None` when
    /// that line number is no longer in the diff at all. Trailing whitespace is
    /// ignored: a formatter run should not mark every comment in the file stale.
    ///
    /// A comment with no recorded anchor is never called outdated. It might be —
    /// there is simply no evidence either way, and a false "outdated" badge teaches
    /// the operator to ignore the real ones.
    pub fn outdated_against(&self, line_now: Option<&str>) -> bool {
        let Some(anchor) = self.anchor_text.as_deref() else {
            return false;
        };
        if !self.scope.is_line() {
            return false;
        }
        match line_now {
            None => true,
            Some(now) => now.trim_end() != anchor.trim_end(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_text_distinguishes_created_from_unavailable() {
        assert_eq!(Baseline::Content("a\n".into()).text(), Some("a\n"));
        // A created file has an empty before-pane — that's a real diff, not a gap.
        assert_eq!(Baseline::Created.text(), Some(""));
        assert!(Baseline::Created.is_created());
        // A gap has no text, so the caller must render the reason instead.
        assert_eq!(
            Baseline::Unavailable {
                reason: BaselineGap::Binary
            }
            .text(),
            None
        );
    }

    fn comment() -> ReviewComment {
        ReviewComment {
            id: "c1".into(),
            session_id: SessionId::new("s"),
            scope: CommentScope::Line,
            path: "src/foo.rs".into(),
            side: DiffSide::After,
            start_line: 42,
            end_line: 42,
            body: "no backoff".into(),
            anchor_text: None,
            at: Timestamp::from_millis(0),
            sent_at: None,
            resolved_at: None,
        }
    }

    #[test]
    fn comment_anchor_collapses_single_line_ranges() {
        let mut c = comment();
        assert_eq!(c.anchor(), "src/foo.rs:42");
        c.end_line = 48;
        assert_eq!(c.anchor(), "src/foo.rs:42-48");
    }

    #[test]
    fn a_wider_scope_drops_the_line_numbers_from_the_anchor() {
        // The anchor is what the session is told to look at. Sending it a line
        // number for a comment that was never about a line is a false lead.
        let mut c = comment();
        c.scope = CommentScope::File;
        assert_eq!(c.anchor(), "src/foo.rs");
        c.scope = CommentScope::Review;
        assert_eq!(c.anchor(), "Review");
    }

    #[test]
    fn a_comment_goes_outdated_when_its_line_stops_reading_the_same() {
        let mut c = comment();
        c.anchor_text = Some("    let x = compute();".into());

        assert!(!c.outdated_against(Some("    let x = compute();")));
        // Re-indented or re-wrapped is a different line, and a comment about the old
        // one has to say so rather than sit on the new text as if it still applied.
        assert!(c.outdated_against(Some("    let x = compute(arg);")));
        // Trailing whitespace is not a change anyone means.
        assert!(!c.outdated_against(Some("    let x = compute();   ")));
        // The line is gone from the diff entirely.
        assert!(c.outdated_against(None));
    }

    #[test]
    fn a_comment_with_no_recorded_anchor_is_never_called_outdated() {
        // Written before anchors were kept. "Outdated" would be a guess, and a badge
        // that is sometimes wrong is one the operator learns to ignore.
        let c = comment();
        assert!(!c.outdated_against(Some("something else entirely")));
        assert!(!c.outdated_against(None));
    }

    #[test]
    fn only_line_comments_can_go_outdated() {
        // A remark about the file as a whole doesn't stop applying because one line
        // moved.
        let mut c = comment();
        c.anchor_text = Some("fn old() {}".into());
        c.scope = CommentScope::File;
        assert!(!c.outdated_against(None));
        c.scope = CommentScope::Review;
        assert!(!c.outdated_against(None));
    }
}
