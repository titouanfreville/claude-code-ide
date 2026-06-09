//! In-app notification store — the data behind the status bar's 🔔.
//!
//! A small UI-local [`Entity`](gpui::Entity) the [`Workspace`](super::workspace) fills
//! by folding "needs-you" facts off the engine bus (review ready, approval/plan
//! proposed, pinned-advance requested, a session errored). The
//! [`status_bar`](super::panels::status_bar) reads it for the unread badge and the
//! popover list. Kept gpui-free (pure data + logic) so it is unit-testable; the
//! status bar maps each [`NotificationKind`] to an icon + color. It never crosses the
//! engine bus and is not persisted (a session-local inbox; prepared for heavy use —
//! navigation-on-click and persistence are follow-ups).

use moonlight_domain::ids::SessionId;

/// Category of a notification — drives its glyph + color in the popover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationKind {
    /// A session reached the review gate.
    Review,
    /// A plan / action awaits the operator's approval.
    Approval,
    /// A pinned session wants to advance its workflow phase.
    Advance,
    /// A session errored.
    Error,
    /// A session changed workflow phase (a phase ended / the next began).
    Phase,
    /// A session is blocked waiting for the operator (CC needs your input).
    Input,
    /// A session's context crossed the auto-compact threshold — `/compact` is
    /// armed to run before the operator's next message.
    Compact,
}

/// One inbox entry.
#[derive(Debug, Clone)]
pub struct Notification {
    pub id: u64,
    pub kind: NotificationKind,
    pub text: String,
    /// The session it concerns, if any — drives click-to-navigate from the popover.
    pub session: Option<SessionId>,
    pub read: bool,
}

/// Newest-first inbox with an unread count, capped so it can't grow unbounded.
#[derive(Default)]
pub struct Notifications {
    items: Vec<Notification>,
    next_id: u64,
}

/// Keep at most this many notifications (drops the oldest).
const CAP: usize = 100;

impl Notifications {
    /// Append a fresh (unread) notification at the front. Returns its id.
    pub fn push(
        &mut self,
        kind: NotificationKind,
        text: impl Into<String>,
        session: Option<SessionId>,
    ) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.items.insert(
            0,
            Notification {
                id,
                kind,
                text: text.into(),
                session,
                read: false,
            },
        );
        self.items.truncate(CAP);
        id
    }

    /// All entries, newest first.
    pub fn items(&self) -> &[Notification] {
        &self.items
    }

    /// How many are unread (the bell badge count).
    pub fn unread(&self) -> usize {
        self.items.iter().filter(|n| !n.read).count()
    }

    /// Mark a single entry read (clicked).
    pub fn mark_read(&mut self, id: u64) {
        if let Some(n) = self.items.iter_mut().find(|n| n.id == id) {
            n.read = true;
        }
    }

    /// Mark every entry read.
    pub fn mark_all_read(&mut self) {
        for n in &mut self.items {
            n.read = true;
        }
    }

    /// Drop every entry.
    pub fn clear(&mut self) {
        self.items.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_is_newest_first_with_unique_ids_and_unread() {
        let mut n = Notifications::default();
        let a = n.push(NotificationKind::Review, "first", None);
        let b = n.push(NotificationKind::Error, "second", None);
        assert_ne!(a, b);
        assert_eq!(n.items()[0].text, "second"); // newest first
        assert_eq!(n.items()[1].text, "first");
        assert_eq!(n.unread(), 2);
    }

    #[test]
    fn mark_read_and_mark_all_and_clear() {
        let mut n = Notifications::default();
        let a = n.push(NotificationKind::Review, "a", None);
        let _b = n.push(NotificationKind::Approval, "b", None);
        n.mark_read(a);
        assert_eq!(n.unread(), 1);
        n.mark_all_read();
        assert_eq!(n.unread(), 0);
        assert_eq!(n.items().len(), 2);
        n.clear();
        assert!(n.items().is_empty());
    }

    #[test]
    fn capped_at_one_hundred_dropping_oldest() {
        let mut n = Notifications::default();
        for i in 0..(CAP + 10) {
            n.push(NotificationKind::Review, format!("n{i}"), None);
        }
        assert_eq!(n.items().len(), CAP);
        // The newest survive; the oldest ("n0") was dropped.
        assert_eq!(n.items()[0].text, format!("n{}", CAP + 9));
        assert!(!n.items().iter().any(|x| x.text == "n0"));
    }
}
