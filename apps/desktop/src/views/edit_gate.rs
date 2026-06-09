//! Editability gate — decides when the operator may edit an open file.
//!
//! MoonlightCode is a supervision cockpit: you mostly review what agents do. So a
//! file you've opened is **read-only exactly when the agent is modifying it** —
//! i.e. the focused session is in [`Phase::AutoImplement`] *and* the file lives
//! under that session's root (`attached_path`). In every other case (Plan / Test /
//! Review / Commit, files outside the session's root, or no focused session) the
//! file is editable and saveable.
//!
//! "Under the session's root" is a proxy for true worktree/lock ownership (FR52),
//! which the engine doesn't expose yet — it errs toward read-only only for files
//! the active Auto session plausibly owns.
//!
//! This is a shared GPUI [`Entity`] in `ShellDeps`: it seeds from the focused
//! session (see `grid_home::open_session`) and stays live by folding the focused
//! session's `PhaseTransitioned` facts off the engine bus. Editor tabs `cx.observe`
//! it and flip their read-only rendering as the phase changes.

use std::path::{Path, PathBuf};

use gpui::{Context, Task};

use moonlight_domain::ids::SessionId;
use moonlight_domain::phase::Phase;
use moonlight_engine::EngineEvent;
use tokio::sync::broadcast;

use super::project_space::expand_home;

pub struct EditGate {
    focused_id: Option<SessionId>,
    /// Absolute root of the focused session (its `attached_path`), if known.
    focused_root: Option<PathBuf>,
    focused_phase: Option<Phase>,
    /// Holds the bus subscription alive for the gate's lifetime.
    _subscription: Option<Task<()>>,
}

impl EditGate {
    /// Build a gate kept live by folding the focused session's phase off the bus.
    pub fn new(rx: broadcast::Receiver<EngineEvent>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.spawn(async move |weak, cx| {
            let mut rx = rx;
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        let applied = weak.update(cx, |this, cx| {
                            if this.apply_event(&event) {
                                cx.notify();
                            }
                        });
                        if applied.is_err() {
                            break; // gate dropped
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        Self {
            focused_id: None,
            focused_root: None,
            focused_phase: None,
            _subscription: Some(subscription),
        }
    }

    /// Seed the gate from the focused session (called when a tile is focused).
    /// `attached_path` is the session's home-abbreviated root, if any.
    pub fn set_focus(&mut self, id: SessionId, attached_path: Option<&str>, phase: Phase) {
        self.focused_id = Some(id);
        self.focused_root = attached_path.map(expand_home);
        self.focused_phase = Some(phase);
    }

    /// Whether the operator may edit `path` right now.
    pub fn editable_for(&self, path: &Path) -> bool {
        if self.focused_phase != Some(Phase::AutoImplement) {
            return true;
        }
        // Auto phase: lock only files under the focused session's root.
        match &self.focused_root {
            Some(root) => !path.starts_with(root),
            None => true,
        }
    }

    /// Fold an event into the gate; returns `true` if it changed the focused
    /// session's phase/root (so observers only re-render when relevant).
    fn apply_event(&mut self, event: &EngineEvent) -> bool {
        let Some(focused) = self.focused_id.as_ref() else {
            return false;
        };
        match event {
            EngineEvent::PhaseTransitioned { session, phase } if session == focused => {
                self.focused_phase = Some(*phase);
                true
            }
            EngineEvent::SessionUpserted { session } if &session.id == focused => {
                self.focused_phase = Some(session.phase);
                self.focused_root = session.attached_path.as_deref().map(expand_home);
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a gate with fixed focus and no live subscription (pure-logic tests).
    fn gated(root: Option<&str>, phase: Phase) -> EditGate {
        EditGate {
            focused_id: Some(SessionId::new("s1")),
            focused_root: root.map(expand_home),
            focused_phase: Some(phase),
            _subscription: None,
        }
    }

    #[test]
    fn auto_locks_files_under_the_session_root() {
        let gate = gated(Some("/repo"), Phase::AutoImplement);
        assert!(!gate.editable_for(Path::new("/repo/src/main.rs")));
    }

    #[test]
    fn auto_leaves_files_outside_the_root_editable() {
        let gate = gated(Some("/repo"), Phase::AutoImplement);
        assert!(gate.editable_for(Path::new("/elsewhere/notes.md")));
    }

    #[test]
    fn non_auto_phases_are_always_editable() {
        for phase in [Phase::Plan, Phase::Test, Phase::Review, Phase::Commit] {
            let gate = gated(Some("/repo"), phase);
            assert!(
                gate.editable_for(Path::new("/repo/src/main.rs")),
                "phase {phase:?} should be editable"
            );
        }
    }

    #[test]
    fn no_focused_session_is_editable() {
        let gate = EditGate {
            focused_id: None,
            focused_root: None,
            focused_phase: None,
            _subscription: None,
        };
        assert!(gate.editable_for(Path::new("/repo/src/main.rs")));
    }

    #[test]
    fn auto_without_known_root_does_not_lock() {
        let gate = gated(None, Phase::AutoImplement);
        assert!(gate.editable_for(Path::new("/anything")));
    }
}
