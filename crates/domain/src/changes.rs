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

/// Who wrote a comment.
///
/// A review is a conversation, and a conversation with one voice is just a list of
/// orders. Without this the record cannot say whether a line is the reviewer's
/// objection or the agent's answer to it — so the agent's reply would be delivered
/// back to the agent as new feedback, and the operator would read their own words
/// quoted at them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommentAuthor {
    /// A human reviewer, through any review surface.
    Operator,
    /// The session under review, answering.
    Agent,
}

impl CommentAuthor {
    /// Whether messages from this author are feedback *for* the agent. An agent's own
    /// replies are context, never instructions to itself.
    pub fn is_feedback(self) -> bool {
        matches!(self, CommentAuthor::Operator)
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
    /// Who wrote it — the reviewer, or the session answering them.
    pub author: CommentAuthor,
    /// The comment this one answers, making the two a thread. `None` for a thread
    /// root.
    ///
    /// A reply carries no anchor of its own: it inherits the root's scope, file and
    /// line range, because a reply that could point somewhere else is not a reply.
    /// Enforced where replies are created, so the invariant cannot be written around.
    pub parent_id: Option<String>,
    pub at: Timestamp,
    /// `None` until the review carrying this comment is sent to the session.
    pub sent_at: Option<Timestamp>,
    /// When the operator marked this comment settled. Resolved comments stay in the
    /// record — a review is a conversation, and deleting the half that got fixed
    /// loses why the code looks the way it does — but they leave the diff and are
    /// not delivered again.
    pub resolved_at: Option<Timestamp>,
}

/// The comments that are still waiting on the agent: written by a reviewer, not yet
/// delivered, and not settled.
///
/// An agent's own replies are excluded by construction — delivering them back would
/// have the session answering itself, which is how a review turns into a loop.
pub fn pending_feedback(comments: &[ReviewComment]) -> Vec<&ReviewComment> {
    comments
        .iter()
        .filter(|c| c.author.is_feedback() && c.sent_at.is_none() && !c.is_resolved())
        .collect()
}

/// Group `comments` into threads: each root with its replies in the order written.
///
/// Returned as `(root, replies)` pairs rather than a nested type because that is the
/// whole shape — threads are one level deep (see
/// [`thread_id`](ReviewComment::thread_id)). A reply whose root is absent from the
/// slice is dropped rather than promoted to a root of its own: showing an answer with
/// nothing to answer is worse than not showing it.
pub fn threads(comments: &[ReviewComment]) -> Vec<(&ReviewComment, Vec<&ReviewComment>)> {
    let mut roots: Vec<&ReviewComment> = comments.iter().filter(|c| !c.is_reply()).collect();
    // Oldest first, so a thread reads top to bottom in the order it happened.
    roots.sort_by_key(|c| c.at.as_millis());
    roots
        .into_iter()
        .map(|root| {
            let mut replies: Vec<&ReviewComment> = comments
                .iter()
                .filter(|c| c.parent_id.as_deref() == Some(root.id.as_str()))
                .collect();
            replies.sort_by_key(|c| c.at.as_millis());
            (root, replies)
        })
        .collect()
}

/// Render `comments` as the body of one review message to a session.
///
/// **Widest scope first** — what is true of the whole change frames how to read the
/// notes on individual lines, and a session that reads "land the migration first"
/// after four line comments has already planned the wrong order.
///
/// Threads render as conversations: a thread that has been answered shows both
/// voices, so the agent reads its own last answer and the reviewer's response to it
/// rather than the objection alone, which it has already tried to address once. A
/// thread nobody has answered renders exactly as a lone comment always did — the
/// labels appear only once there is more than one voice to tell apart.
///
/// Only threads **awaiting the agent** appear: the root is unresolved and at least
/// one reviewer message in it is undelivered (see [`pending_feedback`]). Everything
/// else is settled business or has already been said.
///
/// Lives in the domain rather than in a UI so every review surface (the desktop
/// panel, the editor plugin over the control API) delivers the *same* text. Two
/// formatters would mean a session's instructions depended on which window the
/// operator happened to review in.
///
/// Callers that have more to say (automated findings, say) prepend their own
/// section — this owns the comment half and the header only.
pub fn review_message(comments: &[ReviewComment]) -> String {
    let awaiting: Vec<(&ReviewComment, Vec<&ReviewComment>)> = threads(comments)
        .into_iter()
        .filter(|(root, replies)| {
            !root.is_resolved()
                && std::iter::once(*root)
                    .chain(replies.iter().copied())
                    .any(|c| c.author.is_feedback() && c.sent_at.is_none())
        })
        .collect();

    // Counted from the threads actually rendered below, not from `pending_feedback`.
    // The two disagree: a pending reply under a *resolved* root, or an orphan reply
    // whose root is not in this slice (`threads` drops it by design), is pending
    // feedback that never appears in the body. The agent then read "3 comments" above
    // two and went hunting for a third that was never written out.
    let n: usize = awaiting
        .iter()
        .map(|(root, replies)| {
            std::iter::once(*root)
                .chain(replies.iter().copied())
                .filter(|c| c.author.is_feedback() && c.sent_at.is_none())
                .count()
        })
        .sum();
    let mut out = format!(
        "Review of your changes ({n} comment{}):\n",
        if n == 1 { "" } else { "s" }
    );
    let by_scope = |want: CommentScope| awaiting.iter().filter(move |(root, _)| root.scope == want);
    for (root, replies) in by_scope(CommentScope::Review)
        .chain(by_scope(CommentScope::File))
        .chain(by_scope(CommentScope::Line))
    {
        out.push_str(&format!("\n{}\n", root.anchor()));
        if replies.is_empty() {
            // One voice — no labels to disambiguate, so this stays byte-identical to
            // how a single comment has always been delivered.
            for line in root.body.lines() {
                out.push_str(&format!("  {line}\n"));
            }
            continue;
        }
        for message in std::iter::once(*root).chain(replies.iter().copied()) {
            let who = match message.author {
                CommentAuthor::Operator => "reviewer",
                CommentAuthor::Agent => "you",
            };
            out.push_str(&format!("  {who}:\n"));
            for line in message.body.lines() {
                out.push_str(&format!("    {line}\n"));
            }
        }
    }
    out
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

    /// Whether this is an answer to another comment rather than a thread root.
    pub fn is_reply(&self) -> bool {
        self.parent_id.is_some()
    }

    /// The id of the thread this comment belongs to — its parent's, or its own.
    ///
    /// Threads are one level deep on purpose: a reply to a reply still belongs to the
    /// same conversation about the same line, and letting it nest produces a tree
    /// nobody can render in a diff gutter.
    pub fn thread_id(&self) -> &str {
        self.parent_id.as_deref().unwrap_or(&self.id)
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

    fn scoped_comment(scope: CommentScope, path: &str, line: u32, body: &str) -> ReviewComment {
        ReviewComment {
            id: format!("{path}:{line}:{body}"),
            session_id: crate::ids::SessionId::new("s1"),
            scope,
            path: path.to_string(),
            side: DiffSide::After,
            start_line: line,
            end_line: line,
            body: body.to_string(),
            anchor_text: None,
            author: CommentAuthor::Operator,
            parent_id: None,
            at: crate::ids::Timestamp::from_millis(0),
            sent_at: None,
            resolved_at: None,
        }
    }

    /// A reply is an answer to one comment, not a new note floating at the same line.
    fn reply_to(root: &ReviewComment, author: CommentAuthor, body: &str, at: i64) -> ReviewComment {
        ReviewComment {
            id: format!("{}-reply-{at}", root.id),
            parent_id: Some(root.id.clone()),
            author,
            body: body.to_string(),
            at: crate::ids::Timestamp::from_millis(at),
            ..root.clone()
        }
    }

    /// The point of threads: the agent reads the exchange, not just the objection it
    /// already tried to answer once.
    #[test]
    fn an_answered_thread_delivers_both_voices_in_order() {
        let root = scoped_comment(CommentScope::Line, "a.rs", 12, "this leaks a handle");
        let answer = reply_to(
            &root,
            CommentAuthor::Agent,
            "closed it in the drop impl",
            10,
        );
        let back = ReviewComment {
            sent_at: None,
            ..reply_to(
                &root,
                CommentAuthor::Operator,
                "the error path still returns early",
                20,
            )
        };
        // The root was already delivered; what is pending is the reviewer's follow-up.
        let root = ReviewComment {
            sent_at: Some(crate::ids::Timestamp::from_millis(5)),
            ..root
        };
        let msg = review_message(&[root, answer, back]);
        assert!(
            msg.contains("  reviewer:\n    this leaks a handle\n")
                && msg.contains("  you:\n    closed it in the drop impl\n")
                && msg.contains("  reviewer:\n    the error path still returns early\n"),
            "got:\n{msg}"
        );
        // One pending reviewer message, not three comments.
        assert!(
            msg.starts_with("Review of your changes (1 comment):"),
            "got:\n{msg}"
        );
        let you = msg.find("closed it in the drop impl").unwrap();
        let follow_up = msg.find("the error path still returns early").unwrap();
        assert!(you < follow_up, "the exchange must read in order:\n{msg}");
    }

    /// An agent answering must not hand itself its own words back as new feedback.
    #[test]
    fn a_thread_the_agent_answered_last_is_not_redelivered() {
        let root = ReviewComment {
            sent_at: Some(crate::ids::Timestamp::from_millis(5)),
            ..scoped_comment(CommentScope::Line, "a.rs", 12, "this leaks a handle")
        };
        let answer = reply_to(
            &root,
            CommentAuthor::Agent,
            "closed it in the drop impl",
            10,
        );
        assert_eq!(
            review_message(&[root, answer]),
            "Review of your changes (0 comments):\n"
        );
    }

    /// Resolved means settled — the thread stops being delivered, however much
    /// conversation it holds.
    #[test]
    fn a_resolved_thread_is_not_delivered_even_with_pending_replies() {
        let root = ReviewComment {
            resolved_at: Some(crate::ids::Timestamp::from_millis(30)),
            ..scoped_comment(CommentScope::Line, "a.rs", 12, "this leaks a handle")
        };
        let late = reply_to(&root, CommentAuthor::Operator, "one more thing", 40);
        let msg = review_message(&[root, late]);
        assert!(!msg.contains("one more thing"), "got:\n{msg}");
    }

    /// Delivery for an unanswered comment is unchanged — no labels appear until there
    /// are two voices to tell apart.
    #[test]
    fn an_unanswered_thread_reads_exactly_as_a_lone_comment_always_did() {
        let msg = review_message(&[scoped_comment(
            CommentScope::Line,
            "a.rs",
            3,
            "first\nsecond",
        )]);
        assert!(msg.ends_with("a.rs:3\n  first\n  second\n"), "got:\n{msg}");
    }

    /// A reply with no root in the slice is dropped: an answer to nothing is noise.
    #[test]
    fn threads_drop_an_orphan_reply() {
        let root = scoped_comment(CommentScope::Line, "a.rs", 12, "rooted");
        let orphan = ReviewComment {
            parent_id: Some("gone".to_string()),
            ..scoped_comment(CommentScope::Line, "a.rs", 12, "orphan")
        };
        let all = [root, orphan];
        let grouped = threads(&all);
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].0.body, "rooted");
        assert!(grouped[0].1.is_empty());
    }

    /// Widest scope first: a review-wide instruction has to frame the line notes,
    /// not trail them.
    #[test]
    fn review_message_orders_widest_scope_first() {
        let msg = review_message(&[
            scoped_comment(CommentScope::Line, "a.rs", 12, "rename this"),
            scoped_comment(CommentScope::Review, "", 0, "land the migration first"),
            scoped_comment(CommentScope::File, "a.rs", 0, "this module knows too much"),
        ]);
        let review = msg.find("land the migration first").unwrap();
        let file = msg.find("this module knows too much").unwrap();
        let line = msg.find("rename this").unwrap();
        assert!(review < file && file < line, "got:\n{msg}");
        assert!(
            msg.starts_with("Review of your changes (3 comments):"),
            "got:\n{msg}"
        );
    }

    #[test]
    fn review_message_anchors_each_comment() {
        let msg = review_message(&[scoped_comment(
            CommentScope::Line,
            "a.rs",
            12,
            "rename this",
        )]);
        assert!(msg.contains("a.rs:12"), "got:\n{msg}");
        assert!(
            msg.contains("  rename this"),
            "body must be indented under its anchor:\n{msg}"
        );
    }

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
            author: CommentAuthor::Operator,
            parent_id: None,
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

    /// The header used to count `pending_feedback`, which includes comments the body
    /// never renders — a pending reply under a resolved root among them.
    #[test]
    fn the_header_counts_only_what_the_body_renders() {
        let root = ReviewComment {
            id: "root".into(),
            resolved_at: Some(Timestamp::from_millis(1)),
            ..comment()
        };
        let reply = ReviewComment {
            id: "reply".into(),
            body: "a pending follow-up".into(),
            parent_id: Some(root.id.clone()),
            ..comment()
        };
        let comments = vec![root, reply];

        // One pending operator comment exists, but its thread is resolved, so nothing
        // is rendered — and the header must agree.
        assert_eq!(pending_feedback(&comments).len(), 1);
        let message = review_message(&comments);
        assert!(message.contains("(0 comments)"), "{message}");
        assert!(!message.contains("a pending follow-up"), "{message}");
    }
}
