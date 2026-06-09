//! The per-session workflow phase state machine
//! (Discovery → Plan → Auto → Test → Review → Commit).

use serde::{Deserialize, Serialize};

use crate::session::Mode;

/// Explicit workflow phase of a session. Never stringly-typed.
///
/// **Phase is MoonlightCode's authority, decoupled from CC's permission mode.**
/// CC runs in only two native modes — `plan` (for [`Phase::Plan`], whose *behavior*
/// makes the agent produce a plan) and `auto` (every other phase). The phase then
/// drives our PDP/hook, which is what actually allows or denies a tool. So Discovery
/// vs Auto vs Commit are all CC-`auto` sessions that differ only in *our* write policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    /// Autonomous information-gathering: reads/searches/commands run un-prompted (CC
    /// `auto`), but file **edits are denied by the PDP**. The pre-planning "look
    /// around without touching anything" phase — distinct from `Plan` in that the
    /// agent is *not* steered to produce a plan.
    Discovery,
    /// Read-only planning. Edit/write tools are denied by the PDP, and CC runs in its
    /// native `plan` mode so the agent produces a plan (validated via `ExitPlanMode`).
    Plan,
    /// Autonomous implementation at the session's trust tier (writes allowed).
    AutoImplement,
    /// Running tests / verification (writes allowed).
    Test,
    /// Reviewing pending changes (writes allowed — e.g. addressing review feedback).
    Review,
    /// Awaiting the explicit human commit gate (CC `auto`, but **read-only**).
    Commit,
}

impl Phase {
    /// Whether write/edit actions to **project** files are permissible in this phase
    /// (the PDP also consults trust tier + danger-zone; this is the phase-level gate
    /// only). Frozen phases (Discovery/Plan/Commit) deny project writes so the product
    /// state can't change while looking around, planning, or at the commit gate.
    pub fn allows_writes(self) -> bool {
        matches!(self, Phase::AutoImplement | Phase::Test | Phase::Review)
    }

    /// Whether the agent may write to its **AI-workspace** (plan docs, BMad entries,
    /// handoffs) in this phase. Allowed in every phase *except* the final
    /// [`Phase::Commit`] gate, which freezes the whole tree pending the human commit.
    /// This is the read-only-phase relaxation: Discovery and Plan freeze *project*
    /// files (see [`Phase::allows_writes`]) yet still let the agent keep its own notes
    /// — "read-only" means "don't change project state", not "don't write anything".
    pub fn allows_ai_workspace_writes(self) -> bool {
        !matches!(self, Phase::Commit)
    }

    /// The phase a session reverts to when it declares work "done" (FR13).
    pub const fn on_done() -> Self {
        Phase::Plan
    }

    /// The full workflow sequence, in order. Drives the operator's phase stepper
    /// (the UI lets the operator jump to any of these) and [`Phase::next`].
    pub const ALL: [Phase; 6] = [
        Phase::Discovery,
        Phase::Plan,
        Phase::AutoImplement,
        Phase::Test,
        Phase::Review,
        Phase::Commit,
    ];

    /// The next phase in the workflow progression. The chain is **cyclic**:
    /// `Commit → Discovery` starts the next unit of work (commit, then explore
    /// afresh). This is the forward step surfaced to the operator (the "incoming
    /// phase"); the operator may jump to any phase regardless, and an explicit
    /// operator pick always wins over auto-advancement.
    pub fn next(self) -> Phase {
        match self {
            Phase::Discovery => Phase::Plan,
            Phase::Plan => Phase::AutoImplement,
            Phase::AutoImplement => Phase::Test,
            Phase::Test => Phase::Review,
            Phase::Review => Phase::Commit,
            Phase::Commit => Phase::Discovery,
        }
    }

    /// Whether reaching this phase's "done" should **auto-advance** to [`next`]
    /// on CC's own done-signal. Only `Discovery` and `AutoImplement` qualify —
    /// CC knows when it has finished gathering context / implementing. `Plan`
    /// advances via the plan-approval keystone; `Test`/`Review`/`Commit` are
    /// operator-confirmed gates (the operator signals "no more work").
    pub fn auto_advances_on_done(self) -> bool {
        matches!(self, Phase::Discovery | Phase::AutoImplement)
    }

    /// The Claude Code `--permission-mode` string this phase runs CC in. Only two
    /// are ever used: `plan` for [`Phase::Plan`] (CC's native plan *behavior*), and
    /// `auto` for everything else. The richer per-phase write policy (Discovery and
    /// Commit are read-only; Auto/Test/Review write) is enforced by **our PDP/hook**,
    /// not by CC's mode — see [`Phase::allows_writes`].
    pub fn cc_permission_mode(self) -> &'static str {
        match self {
            Phase::Plan => "plan",
            Phase::Discovery
            | Phase::AutoImplement
            | Phase::Test
            | Phase::Review
            | Phase::Commit => "auto",
        }
    }

    /// Parse a tolerant phase token (case-insensitive, trimmed) — the wire shape a
    /// session passes to the `request_phase` MCP verb. Accepts each phase's label plus
    /// the common aliases CC is likely to use (`auto` / `implement` for AutoImplement).
    /// Returns `None` for anything unrecognized so the caller can refuse with the valid
    /// set. `"next"` is **not** handled here — the executor treats it as "advance to the
    /// next phase" before consulting this parser.
    pub fn from_token(token: &str) -> Option<Phase> {
        match token.trim().to_ascii_lowercase().as_str() {
            "discovery" | "discover" => Some(Phase::Discovery),
            "plan" => Some(Phase::Plan),
            "auto" | "autoimplement" | "auto-implement" | "implement" => {
                Some(Phase::AutoImplement)
            }
            "test" => Some(Phase::Test),
            "review" => Some(Phase::Review),
            "commit" => Some(Phase::Commit),
            _ => None,
        }
    }

    /// Human-facing short label.
    pub fn label(self) -> &'static str {
        match self {
            Phase::Discovery => "Discovery",
            Phase::Plan => "Plan",
            Phase::AutoImplement => "Auto",
            Phase::Test => "Test",
            Phase::Review => "Review",
            Phase::Commit => "Commit",
        }
    }

    /// The operator-facing **execution mode** this phase runs in — derived from the
    /// phase so the displayed mode can never drift from policy. `Plan` gates;
    /// `Discovery`/`Commit` are auto-but-read-only; everything else is full Auto.
    pub fn mode_label(self) -> &'static str {
        match self {
            Phase::Plan => "Plan-gated",
            Phase::Discovery => "Auto · read-only",
            Phase::Commit => "Auto · commit gate",
            Phase::AutoImplement | Phase::Test | Phase::Review => "Auto",
        }
    }

    /// The binary CC-native [`Mode`] this phase runs in: `Plan` only for
    /// [`Phase::Plan`] (CC's native plan mode), `Auto` for every other phase
    /// (Discovery/AutoImplement/Test/Review/Commit all run CC in `auto`; their
    /// differences are enforced by our PDP, not CC's mode). Drives the persisted
    /// `Session.mode` and the relaunch-on-plan-boundary decision.
    pub fn operator_mode(self) -> Mode {
        match self {
            Phase::Plan => Mode::Plan,
            Phase::Discovery
            | Phase::AutoImplement
            | Phase::Test
            | Phase::Review
            | Phase::Commit => Mode::Auto,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cc_runs_plan_for_plan_and_auto_for_everything_else() {
        assert_eq!(Phase::Plan.cc_permission_mode(), "plan");
        // Discovery and Commit are read-only, but CC still runs `auto` — the PDP,
        // not CC's mode, enforces read-only (decoupled posture).
        assert_eq!(Phase::Discovery.cc_permission_mode(), "auto");
        assert_eq!(Phase::AutoImplement.cc_permission_mode(), "auto");
        assert_eq!(Phase::Test.cc_permission_mode(), "auto");
        assert_eq!(Phase::Review.cc_permission_mode(), "auto");
        assert_eq!(Phase::Commit.cc_permission_mode(), "auto");
    }

    #[test]
    fn writes_allowed_in_implement_test_review_only() {
        assert!(Phase::AutoImplement.allows_writes());
        assert!(Phase::Test.allows_writes());
        assert!(Phase::Review.allows_writes());
        // Read-only phases: Discovery (look, don't touch), Plan, and the Commit gate.
        assert!(!Phase::Discovery.allows_writes());
        assert!(!Phase::Plan.allows_writes());
        assert!(!Phase::Commit.allows_writes());
    }

    #[test]
    fn ai_workspace_writes_allowed_everywhere_except_commit() {
        // Discovery/Plan freeze project files but still let the agent write plans,
        // BMad entries, handoffs — "read-only" protects project *state*, not notes.
        assert!(Phase::Discovery.allows_ai_workspace_writes());
        assert!(Phase::Plan.allows_ai_workspace_writes());
        // Write phases trivially allow AI-workspace writes too.
        assert!(Phase::AutoImplement.allows_ai_workspace_writes());
        assert!(Phase::Test.allows_ai_workspace_writes());
        assert!(Phase::Review.allows_ai_workspace_writes());
        // The Commit gate is the one fully-frozen phase: nothing is written, not even
        // AI scratch, until the human commits.
        assert!(!Phase::Commit.allows_ai_workspace_writes());
    }

    #[test]
    fn next_walks_the_workflow_and_loops_commit_to_discovery() {
        assert_eq!(Phase::Discovery.next(), Phase::Plan);
        assert_eq!(Phase::Plan.next(), Phase::AutoImplement);
        assert_eq!(Phase::AutoImplement.next(), Phase::Test);
        assert_eq!(Phase::Test.next(), Phase::Review);
        assert_eq!(Phase::Review.next(), Phase::Commit);
        // Cyclic: Commit loops back to Discovery to start the next unit of work.
        assert_eq!(Phase::Commit.next(), Phase::Discovery);
    }

    #[test]
    fn all_lists_every_phase_in_workflow_order() {
        assert_eq!(Phase::ALL.len(), 6);
        assert_eq!(Phase::ALL[0], Phase::Discovery);
        assert_eq!(*Phase::ALL.last().unwrap(), Phase::Commit);
        // ALL is exactly the linear chain produced by next().
        for w in Phase::ALL.windows(2) {
            assert_eq!(w[0].next(), w[1]);
        }
    }

    #[test]
    fn only_discovery_and_autoimplement_auto_advance_on_done() {
        assert!(Phase::Discovery.auto_advances_on_done());
        assert!(Phase::AutoImplement.auto_advances_on_done());
        // Plan (keystone), Test/Review/Commit (operator-confirmed) do not.
        assert!(!Phase::Plan.auto_advances_on_done());
        assert!(!Phase::Test.auto_advances_on_done());
        assert!(!Phase::Review.auto_advances_on_done());
        assert!(!Phase::Commit.auto_advances_on_done());
    }

    #[test]
    fn from_token_parses_labels_and_aliases_case_insensitively() {
        assert_eq!(Phase::from_token("discovery"), Some(Phase::Discovery));
        assert_eq!(Phase::from_token("Plan"), Some(Phase::Plan));
        // `auto` is the label; `autoimplement`/`implement` are accepted aliases.
        assert_eq!(Phase::from_token("auto"), Some(Phase::AutoImplement));
        assert_eq!(Phase::from_token("AutoImplement"), Some(Phase::AutoImplement));
        assert_eq!(Phase::from_token("  implement "), Some(Phase::AutoImplement));
        assert_eq!(Phase::from_token("test"), Some(Phase::Test));
        assert_eq!(Phase::from_token("REVIEW"), Some(Phase::Review));
        assert_eq!(Phase::from_token("commit"), Some(Phase::Commit));
        // Unknown / the executor-handled "next" sentinel parse to None.
        assert_eq!(Phase::from_token("next"), None);
        assert_eq!(Phase::from_token("nonsense"), None);
        // Every label round-trips through from_token.
        for p in Phase::ALL {
            assert_eq!(Phase::from_token(p.label()), Some(p));
        }
    }

    #[test]
    fn operator_mode_is_plan_only_for_plan() {
        assert_eq!(Phase::Plan.operator_mode(), Mode::Plan);
        // Every non-plan phase — including Discovery — is CC-native `Auto`.
        assert_eq!(Phase::Discovery.operator_mode(), Mode::Auto);
        assert_eq!(Phase::AutoImplement.operator_mode(), Mode::Auto);
        assert_eq!(Phase::Test.operator_mode(), Mode::Auto);
        assert_eq!(Phase::Review.operator_mode(), Mode::Auto);
        assert_eq!(Phase::Commit.operator_mode(), Mode::Auto);
    }
}
