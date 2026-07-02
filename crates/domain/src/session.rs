//! Session entity and its observable status.

use serde::{Deserialize, Serialize};

use crate::ids::{SessionId, Timestamp};
use crate::phase::Phase;
use crate::trust::TrustTier;

/// At-a-glance lifecycle status, surfaced as the traffic-light on a tile.
/// Always rendered as badge + color + border (never color-only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionStatus {
    /// Actively working (running a turn / tool).
    Running,
    /// Blocked waiting for the operator (input or permission). The "needs you" state.
    WaitingInput,
    /// Declared done; pending review.
    Done,
    /// Errored / failed.
    Errored,
    /// Alive but idle (no activity, not blocked).
    Idle,
    /// Paused by the operator or by a safety halt.
    Paused,
}

impl SessionStatus {
    /// Whether this status should appear in the prioritized "needs-you" queue.
    pub fn needs_attention(self) -> bool {
        matches!(
            self,
            SessionStatus::WaitingInput | SessionStatus::Errored | SessionStatus::Done
        )
    }

    /// Triage priority for the grid: lower sorts first, so blocked → errored →
    /// review-ready float above active and idle ("what needs me" stays on top).
    pub fn triage_rank(self) -> u8 {
        match self {
            SessionStatus::WaitingInput => 0,
            SessionStatus::Errored => 1,
            SessionStatus::Done => 2,
            SessionStatus::Running => 3,
            SessionStatus::Idle => 4,
            SessionStatus::Paused => 5,
        }
    }

    /// Glanceable badge glyph (paired with color + border in the UI).
    pub fn badge(self) -> &'static str {
        match self {
            SessionStatus::Running => "●",
            SessionStatus::WaitingInput => "◐",
            SessionStatus::Done => "✓",
            SessionStatus::Errored => "✕",
            SessionStatus::Idle => "○",
            SessionStatus::Paused => "‖",
        }
    }
}

/// A glanceable "this session wants you" overlay — louder than the resting status
/// traffic-light, surfaced as a ⚠ badge on tiles/tabs and the space-tab attention dot.
///
/// `NeedsInput` / `Errored` are **derived** from [`SessionStatus`] (a resting state the
/// transcript already implies). `Incomplete` and `Stuck` are **raised out-of-band**,
/// because they can't be expressed as a resting status: `Incomplete` is the IDE noticing
/// a session ended abnormally (its CC process died mid-work, or its last turn was cut
/// off without a clean/errored stop — which otherwise silently decays to `Idle`), and
/// `Stuck` is a still-alive agent **self-reporting** over MCP that it is blocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttentionKind {
    /// Blocked waiting for the operator (input / permission) — the "needs you" nudge.
    NeedsInput,
    /// The agent self-reported that it is blocked and needs the operator.
    Stuck,
    /// Ended without completing correctly: the process died mid-work or the last turn
    /// was cut off. The IDE raises this (a dead session can't report itself).
    Incomplete,
    /// Ended on a hook-errored stop.
    Errored,
}

impl AttentionKind {
    /// The attention a session's resting [`SessionStatus`] implies, if any. `Incomplete`
    /// and `Stuck` are never derived here — they are raised out-of-band (IDE detection /
    /// agent self-report) and overlaid on top of this.
    pub fn from_status(status: SessionStatus) -> Option<AttentionKind> {
        match status {
            SessionStatus::WaitingInput => Some(AttentionKind::NeedsInput),
            SessionStatus::Errored => Some(AttentionKind::Errored),
            _ => None,
        }
    }

    /// When several signals apply to one session, the higher severity wins the badge.
    /// A hard "it broke" (Incomplete/Errored) outranks a "needs you" (NeedsInput/Stuck).
    pub fn severity(self) -> u8 {
        match self {
            AttentionKind::NeedsInput => 1,
            AttentionKind::Stuck => 2,
            AttentionKind::Incomplete => 3,
            AttentionKind::Errored => 3,
        }
    }

    /// Glanceable warning glyph (paired with color + label in the UI, never glyph-only).
    pub fn glyph(self) -> &'static str {
        match self {
            AttentionKind::NeedsInput => "◐",
            AttentionKind::Stuck => "⚠",
            AttentionKind::Incomplete => "⚠",
            AttentionKind::Errored => "✕",
        }
    }

    /// Short human label (tooltip / fleet text).
    pub fn label(self) -> &'static str {
        match self {
            AttentionKind::NeedsInput => "needs input",
            AttentionKind::Stuck => "agent blocked",
            AttentionKind::Incomplete => "did not complete",
            AttentionKind::Errored => "errored",
        }
    }

    /// Whether this is a "did not finish correctly" alert (the operator should look) —
    /// as opposed to a normal, expected "needs you" pause.
    pub fn is_warning(self) -> bool {
        matches!(
            self,
            AttentionKind::Incomplete | AttentionKind::Errored | AttentionKind::Stuck
        )
    }
}

/// Operator-controlled execution mode, toggled with one key (FR14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    /// Human-gated: the session plans and waits for approval to act (CC `plan`).
    Plan,
    /// Autonomous within the session's trust tier (CC `auto`). The binary CC-native
    /// counterpart to `Plan` — the richer workflow distinctions (Discovery's
    /// no-edit posture, Test/Review/Commit) live on [`Phase`], not here.
    Auto,
}

/// A single supervised Claude Code session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    /// Claude-generated title (from the transcript `ai-title`), if known.
    pub title: Option<String>,
    pub status: SessionStatus,
    pub phase: Phase,
    pub mode: Mode,
    pub trust_tier: TrustTier,
    /// Attached working directory / repo path, if the operator set one.
    pub attached_path: Option<String>,
    /// Whether the operator pinned this session to stay visible.
    pub pinned: bool,
    /// Whether the operator adopted this session into MoonlightCode governance.
    /// Unadopted sessions are observed only — the PDP never gates their tools
    /// (day-one safety: nothing is denied until the operator opts in).
    pub adopted: bool,
    /// Whether the operator paused this session (safety halt). A paused, adopted
    /// session has every tool denied by the gate until resumed.
    pub paused: bool,
    /// Whether the operator **pinned** the workflow phase (manually picked it via
    /// the stepper). A pinned phase never auto-advances and is never clobbered by
    /// detection — the operator's pick wins (A ≫ B). Cleared when the operator
    /// approves an advance (back to auto) or explicitly unlocks.
    pub phase_pinned: bool,
    /// Whether the operator masked this session from the default fleet view (a
    /// soft-hide for "not relevant anymore" sessions). Hidden sessions are filtered
    /// out of the grid unless "show hidden" is on; the transcript and governance are
    /// untouched. Persists only for managed sessions (the store is managed-only).
    pub hidden: bool,
    /// Last time we observed activity from this session.
    pub last_activity: Timestamp,
}

impl Session {
    pub fn label(&self) -> &str {
        self.title.as_deref().unwrap_or_else(|| self.id.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attention_derives_from_status_for_resting_states_only() {
        assert_eq!(
            AttentionKind::from_status(SessionStatus::WaitingInput),
            Some(AttentionKind::NeedsInput)
        );
        assert_eq!(
            AttentionKind::from_status(SessionStatus::Errored),
            Some(AttentionKind::Errored)
        );
        // Running/Done/Idle/Paused imply no attention overlay on their own.
        for s in [
            SessionStatus::Running,
            SessionStatus::Done,
            SessionStatus::Idle,
            SessionStatus::Paused,
        ] {
            assert_eq!(AttentionKind::from_status(s), None, "{s:?}");
        }
    }

    #[test]
    fn warnings_outrank_a_needs_you_nudge() {
        // A hard "it broke" must win the badge over a normal "needs you" pause.
        assert!(AttentionKind::Incomplete.severity() > AttentionKind::NeedsInput.severity());
        assert!(AttentionKind::Errored.severity() > AttentionKind::Stuck.severity());
        assert!(AttentionKind::Stuck.severity() > AttentionKind::NeedsInput.severity());

        // `is_warning` flags the "did not finish correctly" trio, not the plain nudge.
        assert!(AttentionKind::Incomplete.is_warning());
        assert!(AttentionKind::Errored.is_warning());
        assert!(AttentionKind::Stuck.is_warning());
        assert!(!AttentionKind::NeedsInput.is_warning());
    }
}
