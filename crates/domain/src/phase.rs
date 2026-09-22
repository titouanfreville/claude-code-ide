//! The per-session workflow phase state machine
//! (Plan → Auto → Test → Review → Commit).

use serde::{Deserialize, Serialize};

use crate::session::Mode;

/// The **aim** the agent is given while in [`Phase::Plan`] — the one canonical
/// wording, shared by the launch-time system prompt (`--append-system-prompt`) and
/// the phase-entry nudge injected when a running session enters Plan.
///
/// Plan runs CC in `auto` like every other phase, so nothing in CC's own behavior
/// makes the agent produce a plan — this text is what does. Keep it **one line and
/// apostrophe-free**: it rides both a single-quoted shell argument (the inline
/// system-prompt fallback) and a PTY write, neither of which quotes it further.
pub const PLAN_AIM: &str = "Your aim in this phase is to produce a plan. Investigate freely — read, search, and run commands — and keep notes under .ai/ if useful, but do not edit project files. When you know what to do, call the moonlight MCP tool present_plan with the plan as Markdown: it opens in the IDE plan panel and blocks until the operator approves, refines, or rejects it. Approval moves you to the Auto phase, where you implement it.";

/// Explicit workflow phase of a session. Never stringly-typed.
///
/// **Phase is MoonlightCode's authority, decoupled from CC's permission mode.**
/// Every phase runs CC in its `auto` mode — the substrate is uniform — and *our*
/// PDP/hook is what actually allows or denies a tool. So Plan vs Auto vs Commit
/// differ only in our write policy (and, for Plan, in the [`PLAN_AIM`] the agent
/// is steered with).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    /// Investigate and plan: reads/searches/commands run un-prompted (CC `auto`),
    /// file **edits are denied by the PDP**, and the agent is steered by [`PLAN_AIM`]
    /// to finish by proposing a plan through the IDE plan panel (`present_plan`).
    ///
    /// The `Discovery` alias is the **persistence migration**: sessions stored before
    /// Discovery and Plan merged carry the literal `"Discovery"` and must rehydrate
    /// here rather than fail to decode.
    #[serde(alias = "Discovery")]
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
    /// only). Frozen phases (Plan/Commit) deny project writes so the product state
    /// can't change while planning or at the commit gate.
    pub fn allows_writes(self) -> bool {
        matches!(self, Phase::AutoImplement | Phase::Test | Phase::Review)
    }

    /// Whether the agent may write to its **AI-workspace** (plan docs, BMad entries,
    /// handoffs) in this phase. Allowed in every phase *except* the final
    /// [`Phase::Commit`] gate, which freezes the whole tree pending the human commit.
    /// This is the read-only-phase relaxation: Plan freezes *project* files (see
    /// [`Phase::allows_writes`]) yet still lets the agent keep its own notes —
    /// "read-only" means "don't change project state", not "don't write anything".
    pub fn allows_ai_workspace_writes(self) -> bool {
        !matches!(self, Phase::Commit)
    }

    /// The phase a session reverts to when it declares work "done" (FR13).
    pub const fn on_done() -> Self {
        Phase::Plan
    }

    /// The full workflow sequence, in order. Drives the operator's phase stepper
    /// (the UI lets the operator jump to any of these) and [`Phase::next`].
    pub const ALL: [Phase; 5] = [
        Phase::Plan,
        Phase::AutoImplement,
        Phase::Test,
        Phase::Review,
        Phase::Commit,
    ];

    /// The next phase in the workflow progression. The chain is **cyclic**:
    /// `Commit → Plan` starts the next unit of work (commit, then plan afresh).
    /// This is the forward step surfaced to the operator (the "incoming phase");
    /// the operator may jump to any phase regardless, and an explicit operator pick
    /// always wins over auto-advancement.
    pub fn next(self) -> Phase {
        match self {
            Phase::Plan => Phase::AutoImplement,
            Phase::AutoImplement => Phase::Test,
            Phase::Test => Phase::Review,
            Phase::Review => Phase::Commit,
            Phase::Commit => Phase::Plan,
        }
    }

    /// Whether reaching this phase's "done" should **auto-advance** to [`next`]
    /// on CC's own done-signal. Only `AutoImplement` qualifies — CC knows when it
    /// has finished implementing. `Plan` advances via the plan-approval keystone
    /// (the operator approving a `present_plan` proposal), and `Test`/`Review`/
    /// `Commit` are operator-confirmed gates (the operator signals "no more work").
    pub fn auto_advances_on_done(self) -> bool {
        matches!(self, Phase::AutoImplement)
    }

    /// The Claude Code `--permission-mode` string this phase runs CC in — **always
    /// `auto`**. CC's native `plan` mode is no longer used by any phase: Plan gets
    /// its planning *behavior* from [`PLAN_AIM`], not from CC's mode. The per-phase
    /// write policy (Plan and Commit are read-only; Auto/Test/Review write) is
    /// enforced by **our PDP/hook** — see [`Phase::allows_writes`].
    ///
    /// Because this is constant, a phase change never has to relaunch CC.
    pub fn cc_permission_mode(self) -> &'static str {
        "auto"
    }

    /// Parse a tolerant phase token (case-insensitive, trimmed) — the wire shape a
    /// session passes to the `request_phase` MCP verb. Accepts each phase's label plus
    /// the common aliases CC is likely to use (`auto` / `implement` for AutoImplement,
    /// and `discovery` from the pre-merge workflow, which now means Plan).
    /// Returns `None` for anything unrecognized so the caller can refuse with the valid
    /// set. `"next"` is **not** handled here — the executor treats it as "advance to the
    /// next phase" before consulting this parser.
    pub fn from_token(token: &str) -> Option<Phase> {
        match token.trim().to_ascii_lowercase().as_str() {
            "plan" | "discovery" | "discover" => Some(Phase::Plan),
            "auto" | "autoimplement" | "auto-implement" | "implement" => Some(Phase::AutoImplement),
            "test" => Some(Phase::Test),
            "review" => Some(Phase::Review),
            "commit" => Some(Phase::Commit),
            _ => None,
        }
    }

    /// Human-facing short label.
    pub fn label(self) -> &'static str {
        match self {
            Phase::Plan => "Plan",
            Phase::AutoImplement => "Auto",
            Phase::Test => "Test",
            Phase::Review => "Review",
            Phase::Commit => "Commit",
        }
    }

    /// The operator-facing **execution mode** this phase runs in — derived from the
    /// phase so the displayed mode can never drift from policy. Every phase is CC
    /// `auto`; the label says what *our* policy adds on top.
    pub fn mode_label(self) -> &'static str {
        match self {
            Phase::Plan => "Auto · plan (read-only)",
            Phase::Commit => "Auto · commit gate",
            Phase::AutoImplement | Phase::Test | Phase::Review => "Auto",
        }
    }

    /// The binary CC-native [`Mode`] this phase runs in — **always [`Mode::Auto`]**,
    /// since no phase launches CC in its native plan mode any more. [`Mode::Plan`]
    /// survives only as an *observed* state: detection can still see an operator who
    /// shift-tabbed a live session into CC plan mode. Drives the persisted
    /// `Session.mode`.
    pub fn operator_mode(self) -> Mode {
        Mode::Auto
    }
}

/// Which MoonlightCode MCP verbs a session can actually reach.
///
/// Not cosmetic: [`turn_brief`] names the verb the agent should call, and naming one
/// that is not wired hands it an instruction it can only fail. A VSCode agent-panel
/// session gets no `--mcp-config`, so it has none of them — the brief has to say
/// "the operator changes phase" instead of "call `request_phase`".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verbs {
    /// The `moonlight` MCP server is wired into this session.
    Available,
    /// No MCP server — phase changes come from the operator's IDE, not from the agent.
    Unavailable,
}

/// The per-turn briefing for a governed session: which phase it is in, what that
/// phase allows, and how to get out of it.
///
/// Injected by the `UserPromptSubmit` hook, which is handed the session id and so can
/// read the **live** phase. That is the whole point of it existing alongside the
/// launch-time system prompt ([`PLAN_AIM`] and the IDE context built on it): a
/// launch-time prompt is a snapshot, and a session that starts in Plan and moves to
/// Auto keeps being told it is planning for the rest of its life. This is re-derived
/// every turn, so it cannot go stale.
///
/// Deliberately terse — it rides every single prompt, so it spends context budget on
/// every turn of every governed session. It states the phase, the two write rules the
/// PDP actually enforces, and the one way out. Anything else belongs in the denial
/// reason, which is only paid for when something is actually blocked.
pub fn turn_brief(phase: Phase, verbs: Verbs) -> String {
    let mut brief = format!(
        "[MoonlightCode] Workflow phase: {}. Project files: {}. Notes under .ai/ (and \
         other AI-workspace dirs): {}.",
        phase.label(),
        if phase.allows_writes() {
            "writable"
        } else {
            "READ-ONLY — edits are denied by the IDE policy, not by you"
        },
        if phase.allows_ai_workspace_writes() {
            "writable"
        } else {
            "read-only (the commit gate freezes the whole tree)"
        },
    );

    // Plan is the one phase whose *aim* differs, not just its permissions. CC runs
    // `auto` in every phase, so nothing about being in Plan makes the agent plan —
    // saying so is what does (the same reason [`PLAN_AIM`] exists).
    if phase == Phase::Plan {
        brief.push_str(match verbs {
            Verbs::Available => {
                " Your aim here is to produce a plan: investigate freely, \
                 then call the moonlight MCP tool present_plan with it as Markdown for the \
                 operator to approve."
            }
            Verbs::Unavailable => {
                " Your aim here is to produce a plan: investigate freely, \
                 then present it and stop — the operator approves it and moves you on."
            }
        });
    }

    brief.push_str(match verbs {
        Verbs::Available => {
            " You cannot change phase yourself: call the moonlight MCP \
             tool request_phase (the operator approves or denies), and never assume the \
             phase changed until the result confirms it."
        }
        // Nothing to call, so do not invent a verb. Asking in prose is the honest
        // remaining move, and the operator has the phase control in their IDE.
        Verbs::Unavailable => {
            " You cannot change phase yourself and have no tool to ask \
             with — say what you need in your reply; the operator changes it from the IDE."
        }
    });

    brief
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_phase_runs_cc_in_auto() {
        // The substrate is uniform — which is why a phase change never relaunches CC.
        for p in Phase::ALL {
            assert_eq!(p.cc_permission_mode(), "auto", "{p:?}");
        }
    }

    #[test]
    fn writes_allowed_in_implement_test_review_only() {
        assert!(Phase::AutoImplement.allows_writes());
        assert!(Phase::Test.allows_writes());
        assert!(Phase::Review.allows_writes());
        // Read-only phases: Plan (investigate + propose, don't touch) and the Commit gate.
        assert!(!Phase::Plan.allows_writes());
        assert!(!Phase::Commit.allows_writes());
    }

    #[test]
    fn ai_workspace_writes_allowed_everywhere_except_commit() {
        // Plan freezes project files but still lets the agent write plans, BMad
        // entries, handoffs — "read-only" protects project *state*, not notes.
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
    fn next_walks_the_workflow_and_loops_commit_to_plan() {
        assert_eq!(Phase::Plan.next(), Phase::AutoImplement);
        assert_eq!(Phase::AutoImplement.next(), Phase::Test);
        assert_eq!(Phase::Test.next(), Phase::Review);
        assert_eq!(Phase::Review.next(), Phase::Commit);
        // Cyclic: Commit loops back to Plan to start the next unit of work.
        assert_eq!(Phase::Commit.next(), Phase::Plan);
    }

    #[test]
    fn all_lists_every_phase_in_workflow_order() {
        assert_eq!(Phase::ALL.len(), 5);
        assert_eq!(Phase::ALL[0], Phase::Plan);
        assert_eq!(*Phase::ALL.last().unwrap(), Phase::Commit);
        // ALL is exactly the linear chain produced by next().
        for w in Phase::ALL.windows(2) {
            assert_eq!(w[0].next(), w[1]);
        }
    }

    #[test]
    fn only_autoimplement_auto_advances_on_done() {
        assert!(Phase::AutoImplement.auto_advances_on_done());
        // Plan holds for the plan-approval keystone; Test/Review/Commit are
        // operator-confirmed gates.
        assert!(!Phase::Plan.auto_advances_on_done());
        assert!(!Phase::Test.auto_advances_on_done());
        assert!(!Phase::Review.auto_advances_on_done());
        assert!(!Phase::Commit.auto_advances_on_done());
    }

    #[test]
    fn from_token_parses_labels_and_aliases_case_insensitively() {
        assert_eq!(Phase::from_token("Plan"), Some(Phase::Plan));
        // `auto` is the label; `autoimplement`/`implement` are accepted aliases.
        assert_eq!(Phase::from_token("auto"), Some(Phase::AutoImplement));
        assert_eq!(
            Phase::from_token("AutoImplement"),
            Some(Phase::AutoImplement)
        );
        assert_eq!(
            Phase::from_token("  implement "),
            Some(Phase::AutoImplement)
        );
        assert_eq!(Phase::from_token("test"), Some(Phase::Test));
        assert_eq!(Phase::from_token("REVIEW"), Some(Phase::Review));
        assert_eq!(Phase::from_token("commit"), Some(Phase::Commit));
        // The pre-merge Discovery token still resolves — to its successor, Plan.
        assert_eq!(Phase::from_token("discovery"), Some(Phase::Plan));
        assert_eq!(Phase::from_token("Discover"), Some(Phase::Plan));
        // Unknown / the executor-handled "next" sentinel parse to None.
        assert_eq!(Phase::from_token("next"), None);
        assert_eq!(Phase::from_token("nonsense"), None);
        // Every label round-trips through from_token.
        for p in Phase::ALL {
            assert_eq!(Phase::from_token(p.label()), Some(p));
        }
    }

    // The serde side of the migration (a persisted `"Discovery"` decoding as Plan) is
    // covered in `moonlight-persistence`, which owns the enc/dec pair — this crate is
    // dependency-pure and has no serde_json.

    #[test]
    fn operator_mode_is_auto_for_every_phase() {
        // No phase launches CC in native plan mode any more; Mode::Plan is now only
        // ever *observed* from a session the operator shift-tabbed into plan mode.
        for p in Phase::ALL {
            assert_eq!(p.operator_mode(), Mode::Auto, "{p:?}");
        }
    }

    #[test]
    fn plan_aim_is_safe_to_deliver_inline() {
        // Rides a single-quoted shell arg and a PTY write — no apostrophes, one line.
        assert!(!PLAN_AIM.contains('\''), "{PLAN_AIM}");
        assert!(!PLAN_AIM.contains('\n'), "{PLAN_AIM}");
        // It must name the verb that actually opens the plan panel.
        assert!(PLAN_AIM.contains("present_plan"), "{PLAN_AIM}");
    }
}

#[cfg(test)]
mod turn_brief_tests {
    use super::*;

    #[test]
    fn frozen_phases_say_project_files_are_read_only() {
        for phase in [Phase::Plan, Phase::Commit] {
            let brief = turn_brief(phase, Verbs::Available);
            assert!(brief.contains("READ-ONLY"), "{phase:?}: {brief}");
        }
        for phase in [Phase::AutoImplement, Phase::Test, Phase::Review] {
            let brief = turn_brief(phase, Verbs::Available);
            assert!(
                brief.contains("Project files: writable"),
                "{phase:?}: {brief}"
            );
        }
    }

    #[test]
    fn commit_is_the_only_phase_that_freezes_ai_workspace_notes() {
        // Mirrors `allows_ai_workspace_writes` — the brief must not tell a Plan-phase
        // agent it cannot keep notes, since the PDP lets it.
        assert!(turn_brief(Phase::Plan, Verbs::Available)
            .contains(".ai/ (and other AI-workspace dirs): writable"));
        assert!(turn_brief(Phase::Commit, Verbs::Available)
            .contains("the commit gate freezes the whole tree"));
    }

    #[test]
    fn only_plan_states_an_aim() {
        assert!(turn_brief(Phase::Plan, Verbs::Available).contains("aim here is to produce a plan"));
        for phase in [
            Phase::AutoImplement,
            Phase::Test,
            Phase::Review,
            Phase::Commit,
        ] {
            assert!(
                !turn_brief(phase, Verbs::Available).contains("aim here"),
                "{phase:?}"
            );
        }
    }

    /// The reason `Verbs` exists: naming a tool a session cannot reach hands it an
    /// instruction it can only fail. No brief may mention an MCP verb without them.
    #[test]
    fn no_mcp_verb_is_named_when_none_are_wired() {
        for phase in Phase::ALL {
            let brief = turn_brief(phase, Verbs::Unavailable);
            for verb in ["present_plan", "request_phase", "phase_status", "MCP"] {
                assert!(!brief.contains(verb), "{phase:?} names {verb}: {brief}");
            }
        }
    }

    /// It rides every prompt of every governed session, so its cost is paid per turn.
    #[test]
    fn stays_small_enough_to_pay_for_every_turn() {
        for phase in Phase::ALL {
            for verbs in [Verbs::Available, Verbs::Unavailable] {
                let len = turn_brief(phase, verbs).len();
                assert!(len < 600, "{phase:?}/{verbs:?} is {len} bytes");
            }
        }
    }
}
