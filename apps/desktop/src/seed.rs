//! Mock fleet for unit tests that build a `GridHome` directly. The live app no
//! longer uses this — it is fed by the real `JsonlDetectionSource`. Test-only.

use moonlight_domain::ids::{SessionId, Timestamp};
use moonlight_domain::phase::Phase;
use moonlight_domain::session::{Mode, Session, SessionStatus};
use moonlight_domain::trust::TrustTier;

/// Placeholder fleet — exercises every status/phase so the traffic-light palette
/// and phase chips are all visible at once.
pub fn mock_sessions() -> Vec<Session> {
    let t = Timestamp::from_millis(0);
    let mk = |id: &str,
              title: &str,
              status: SessionStatus,
              phase: Phase,
              mode: Mode,
              tier: TrustTier,
              path: Option<&str>|
     -> Session {
        Session {
            id: SessionId::new(id),
            title: Some(title.to_string()),
            status,
            phase,
            mode,
            trust_tier: tier,
            attached_path: path.map(str::to_string),
            pinned: false,
            adopted: false,
            paused: false,
            phase_pinned: false,
            hidden: false,
            last_activity: t,
        }
    };

    vec![
        mk(
            "s1",
            "Refactor auth module",
            SessionStatus::Running,
            Phase::AutoImplement,
            Mode::Auto,
            TrustTier::Standard,
            Some("~/code/api"),
        ),
        mk(
            "s2",
            "Add billing webhook",
            SessionStatus::WaitingInput,
            Phase::Plan,
            Mode::Auto,
            TrustTier::ReadOnly,
            Some("~/code/billing"),
        ),
        mk(
            "s3",
            "Migrate DB schema",
            SessionStatus::Done,
            Phase::Review,
            Mode::Auto,
            TrustTier::Standard,
            Some("~/code/db"),
        ),
        mk(
            "s4",
            "Fix flaky e2e",
            SessionStatus::Errored,
            Phase::Test,
            Mode::Auto,
            TrustTier::Trusted,
            Some("~/code/web"),
        ),
        mk(
            "s5",
            "Draft release notes",
            SessionStatus::Idle,
            Phase::Plan,
            Mode::Auto,
            TrustTier::Observed,
            None,
        ),
        mk(
            "s6",
            "Bump dependencies",
            SessionStatus::Paused,
            Phase::Commit,
            Mode::Auto,
            TrustTier::Standard,
            Some("~/code/infra"),
        ),
    ]
}
