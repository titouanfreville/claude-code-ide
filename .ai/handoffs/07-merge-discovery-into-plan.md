# Merge Discovery into Plan — one plan phase, CC `auto` substrate, aim-driven

> **Status:** IMPLEMENTED (2026-08-05) — 503 tests green, clippy 0 errors. Plan approved via `present_plan`.
>
> **Deltas from the plan as written**, all forced by what the code turned out to do:
> - `Command::ApprovePlan` is the **workflow hand-off only**, sent by the plan panel *after* `ApproveAction`. The composition root (`main.rs::route_approval`) swallows a command that resolved a held hook, so a single combined command would never have reached the engine.
> - `SessionMonitor::relaunch_terminal` was **deleted**, not kept: with `cc_permission_mode()` constant its only caller disappeared and it was dead code (auto-resume-after-exit uses `try_resume`).
> - The legacy-`"Discovery"` decode assertion went into the existing `legacy_rows_backfill_to_claude_code…` test in `moonlight-persistence` (the domain crate is dependency-pure — no `serde_json`, not even as a dev-dep).
> - Four engine tests drove a session into `AutoImplement` via `PhaseObserved`; that path is intentionally dead now, so they use `Command::AdvancePhase` instead.
> - The AGY context file (`agy_setup::gemini_md`) got the same treatment as the Claude system prompt — it also hard-coded the six phases.

## Goal

Collapse `Discovery` and `Plan` into a single phase named **Plan** that:
- runs Claude Code in `auto` (full read / search / run freedom, no native plan mode),
- still **denies project-file writes** via our PDP (`.ai/` notes stay allowed),
- carries an explicit **aim**: investigate, then propose a plan through the IDE plan panel (`present_plan`),
- hands off to `Auto` when the operator approves the plan.

Workflow becomes 5 phases, cyclic: `Plan → Auto → Test → Review → Commit → Plan`.

Consequence worth naming up front: since every phase now maps to CC `auto`, **the plan↔auto relaunch boundary disappears entirely** — no more `--resume` restart on a phase change, and no more `/plan` toggle for AGY.

### Locked decisions (operator)

1. Aim delivery = **system prompt + phase-entry nudge** (covers fresh launches and mid-session switches).
2. Plan approval **auto-advances** Plan → Auto (unless the phase is pinned).
3. Native CC plan mode is **kept as a fallback** (gate still holds `ExitPlanMode`); no phase launches CC in `plan`.

---

## 1. Domain — `crates/domain/src/phase.rs`

- Delete the `Discovery` variant. Add `#[serde(alias = "Discovery")]` on `Plan` so **existing sqlite rows** (`phase`/audit `PhaseChanged` are `serde_json` strings via `store::enc`) rehydrate as `Plan` instead of `StoreError::Corrupt`.
- `ALL` → 5 entries; `next()` → `Plan→AutoImplement→Test→Review→Commit→Plan`.
- `allows_writes()` — unchanged (Plan `false`).
- `allows_ai_workspace_writes()` — unchanged (everything except `Commit`).
- `auto_advances_on_done()` → **only** `AutoImplement`. Plan no longer auto-advances on CC's done-signal; the plan-approval keystone is the gate (old `Discovery→Plan` auto-hop is gone with the variant).
- `cc_permission_mode()` → `"auto"` for every phase; rewrite the doc to say the phase substrate is now uniform and the PDP is the *only* differentiator.
- `operator_mode()` → `Mode::Auto` for every phase. `Mode::Plan` survives only as a **detected** CC state (see §2), never as a phase-derived one.
- `from_token()` — keep `"discovery" | "discover"` as accepted aliases mapping to `Plan` (old prompts, stale scripts, and CC habit).
- `mode_label()` → `Plan => "Auto · plan (read-only)"`.
- New `pub const PLAN_AIM: &str` — the one canonical sentence-set describing the Plan aim, consumed by both the system prompt (§3) and the phase-entry nudge (§2). Keep it apostrophe-free and single-line so it is safe in every delivery path.

## 2. Engine — `crates/engine/src/supervisor.rs`

- `spawn_session` (`:341`, `:354`): keep `starting_phase = Phase::Plan`, set `mode: starting_phase.operator_mode()` instead of the hard-coded `Mode::Plan`.
- `apply_phase` (`:382-407`): replace the `control.set_phase(&session, Phase::Plan)` nudge — both adapters return `ControlError::Unavailable`, so it is a no-op today — with an **aim injection** on *entering* Plan: `inject_feedback` with `PLAN_AIM` (the `SteerControl` channel the UI already drains into the PTY). Guard on a real transition (`previous != Plan`) so re-applying Plan doesn't spam the terminal.
- `PhaseObserved` (`:504-537`): **drop the `(_, Phase::Plan) => Some(Phase::AutoImplement)` rule.** CC now reports `auto` while we legitimately hold `Plan`, so that rule would kick every session out of Plan on the first transcript line. Keep `(Phase::Plan, _) => Some(Phase::Plan)` — a manually shift-tabbed native plan still adopts the Plan phase.
- New `Command::ApprovePlan { session }` (engine `lib.rs` + `handle_command`): does `approve_action`, then — if the session is in `Plan` — advances to `AutoImplement`, or publishes `PhaseAdvanceRequested` when `phase_pinned` (same A ≫ B rule as the done-path at `:656-673`). Plain `ApproveAction` keeps its current semantics for danger/code-review holds.
  - *Alternative considered:* inspect `PendingApprovals` for an `ExitPlanMode`/`present_plan` hold inside `approve_action`. Rejected — the panel already knows it is a plan, an explicit command is cheaper and backend-agnostic.

## 3. Prompt / aim delivery — `apps/desktop/src/views/mcp_host.rs`

- Turn `IDE_CONTEXT` from a `const` into a builder that interpolates `PLAN_AIM`, and rewrite it for **five** phases: Plan (auto, project edits denied, `.ai/` allowed, *aim: investigate then call `present_plan`; approval moves you to Auto*), Auto/Test/Review (writes allowed), Commit (frozen).
- Update `IDE_CONTEXT_SHORT` the same way — must stay one line, apostrophe-free (test at `:213` enforces this and the `< 400` char flag budget; the long form still rides the `session-context.txt` file).
- Keep the existing test assertions on `request_phase`, add ones on `present_plan` / the plan aim appearing in both forms.

## 4. Plan approval path — `apps/desktop/src/views/panels/plan_review.rs`

- `approve` (`:151`) sends the new `Command::ApprovePlan`. `refine` (`:207`) keeps plain `ApproveAction` (it is a re-plan, not a hand-off).
- `select_native_option` / `is_continuation_menu` stay as-is: they already no-op when no native continuation menu appears, which is now the common case (`present_plan` path).
- Native `ExitPlanMode` keeps holding in `crates/control/src/gate.rs` — unchanged.

## 5. UI cleanups

- `views/grid_home.rs:528-535` — drop the **Discovery** segment from the new-session picker (`Plan | Auto`); `new_session_phase` default stays `Plan`.
- `views/panels/session_monitor.rs:2290-2332` (`request_phase`) — remove the native-mode-boundary block: the Claude `relaunch_terminal` arm (now unreachable — `cc_permission_mode()` is constant) and the AGY `/plan` toggle (AGY gets the aim nudge instead). `relaunch_terminal` itself stays; it is still used by auto-resume-after-exit. Update the doc comment.
- `views/theme.rs:265` — drop the `Phase::Discovery` color arm.
- Doc/comment fixes naming the six phases or Discovery: `views/center_requests.rs:47`, `phase_verbs.rs` (valid-target list is `Phase::ALL`-derived, so it follows automatically), `session_monitor.rs:1677`, `supervisor.rs:388,495,651-653`.
- Fixture `Mode::Plan` → `Mode::Auto`: `seed.rs:53,80`, `views/space_sessions.rs:133`, `views/project_space.rs:619`, `persistence/store.rs:325`.

## 6. Tests

- `domain/phase.rs`: rewrite the six-phase tests — every phase is `"auto"`; only `AutoImplement` auto-advances; the 5-phase cycle; `from_token("discovery") == Some(Plan)`; **new**: `serde_json::from_str::<Phase>("\"Discovery\"") == Plan` (the migration contract).
- `crates/trust/src/lib.rs:204-392`: replace `Phase::Discovery` cases with `Phase::Plan`; the read-only-phase loops become `[Plan, Commit]`.
- `crates/engine/src/supervisor.rs`: drop the Discovery auto-advance test; add (a) Plan does **not** auto-advance on `Done`, (b) `PhaseObserved(auto)` leaves a `Plan` session in `Plan`, (c) `ApprovePlan` in Plan → `AutoImplement`, and pinned → `PhaseAdvanceRequested`.
- `crates/mcp-server/src/lib.rs:671-694` and `crates/control/src/gate.rs:297,522`, `apps/desktop/src/phase_verbs.rs:381,417`: swap the Discovery fixtures for Plan.
- `crates/persistence`: round-trip test that a row written with the literal `"Discovery"` string loads as `Phase::Plan`.

## 7. Docs

- `AGENTS.md` / `.ai/handoffs/00-status-and-tasks.md`: record the 5-phase workflow, the uniform `auto` substrate, and the new plan keystone (`present_plan` → approve → Auto). Do **not** rewrite historical handoff entries; append a dated note.

---

## Verification

1. `rtk cargo clippy` + `rtk cargo test` across the workspace (green, no `Discovery` references left outside intentional aliases: `rg -n 'Discovery'`).
2. Runtime, one managed Claude session: launch → phase reads **Plan / Auto · plan (read-only)**; the terminal shows the aim nudge; a `Write` to a project file is **denied**; a write under `.ai/` is **allowed**; ask for a plan → `present_plan` opens the plan panel → **Approve** → phase flips to **Auto** and the same project write now succeeds — with **no session relaunch** at any point.
3. Legacy state: open the app against the existing sqlite store and confirm a session previously persisted in `Discovery` rehydrates as `Plan` (no `Corrupt` in the log).
4. Fallback: manually shift-tab a session into CC's native plan mode → `ExitPlanMode` still lands in the plan panel.

## Risks / accepted trade-offs

- **No more aim-free exploration phase.** Every read-only phase now steers toward producing a plan. That is the point of the request; if a pure "look around, no plan" mode is later wanted, it should be a per-session toggle, not a phase.
- **`Session.mode` becomes effectively constant `Auto`** (only detection can ever set `Plan`). It stays persisted and displayed via `mode_label()`, but it is no longer a dial — worth deleting in a follow-up if it stays dead.
- **The nudge lands in the PTY as a user turn.** If a session is mid-turn when the operator switches to Plan, the aim text queues behind the current turn rather than steering it immediately.
