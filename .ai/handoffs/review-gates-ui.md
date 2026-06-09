# Handoff — Plan-review & Code-review UI (the two acceptance gates)

Operator design (2026-06): acceptance has **two gates**:
1. **Plan review** — before the agent acts, show the plan it intends to follow.
2. **Code review** — once done, show a diff linked to a short "what this covers".

The **data/engine side is done** (control/detection/engine — see below). What remains
is the **UI surfacing**, which lives in your area (`apps/desktop/src/views`, `CenterRequests`,
the `git` module). This note hands you the contract.

## Gate 1 — Plan review (data side DONE)

- Detection parses the agent's `ExitPlanMode` tool call from the transcript and emits
  `DetectionEvent::PlanProposed { session, plan }` (deduped — once per distinct plan).
- The supervisor republishes it as **`EngineEvent::PlanProposed { session, plan }`** on the bus.
- `plan` is markdown (CC's plan text).

**UI to build:**
- Subscribe to the bus for `EngineEvent::PlanProposed` (the `Workspace` already has bus
  access via `ShellDeps`; the grid's `FleetModel::apply_event` currently *ignores* it).
- Open a **"Plan review" center tab** (via `CenterRequests` — consider an
  `OpenRequest::PlanReview { session, plan }` variant, or a dedicated panel) rendering the
  plan markdown, labelled with the session.
- Note: the classifier now treats `ExitPlanMode` as **Safe** (it must not be gated, or a
  plan-mode session is trapped). So plan-review is **display-first**; CC's own native
  ExitPlanMode prompt still does the accept. True in-app approve/reject routing is later
  (the hook is synchronous + fail-open, so it can't block waiting for a human — needs a
  separate channel or pre-set policy; see the control-adapter design doc §10b).

## Gate 2 — Code review (data side: partial)

- The supervisor already emits **`EngineEvent::ReviewReady { session }`** when a session
  goes done (FR13 auto-revert path).
- **UI to build:** on `ReviewReady`, open a **"Code review" center tab**:
  - a **diff view** (your `git` module — `git diff` of the session's repo/worktree), and
  - a **short summary** of what it covers. Source for the summary: the session's final
    assistant message text (last non-tool assistant block in the transcript). If you want
    that surfaced as an engine fact too, ping me and I'll add `EngineEvent::SummaryObserved`
    (detection can parse it the same way it parses the plan).

## Contract notes
- `EngineEvent` is exhaustively matched in `FleetModel::apply_event` (grid) — I added
  `PlanProposed` to its ignore arm, so it compiles; the plan tab is opened by a *separate*
  bus subscriber, not the grid model.
- Don't gate on `EngineEvent` ordering; treat each as an idempotent fact.

## Tests already covering the data side
- `jsonl::extracts_plan_from_exit_plan_mode`, detection fold dedup, supervisor pass-through.
