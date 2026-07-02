# Five usability fixes — plan

## STATUS (2026-06-23): all five implemented, `cargo test` = 372 pass, clippy clean (new code)
- **#4** done — `header_collapsed: true` default.
- **#5** done — `PhaseVerbExecutor` now holds the `SessionPolicyView`; `request_phase`
  reports the resulting from→to phase, every other verb result carries a current-phase
  footer. +3 tests.
- **#1** done — reset defers the tab-close to the next tick (`spawn_in`) so the
  successor tab opens first; the panel never goes empty. **Needs an operator click-test.**
- **#3** done — `Space.trust` persisted; `maybe_prompt_project_trust` asks on first
  launch; monitor applies the tier on first sighting; the Trust selector persists back.
  **Needs an operator open-a-new-project test (the prompt is GUI).**
- **#2** done — `Space.auto_resume` persisted + a per-project toggle pill in the session
  header. Two triggers, both gated on the project opt-in:
  - **stall on restart** — a *restored* session (armed via `arm_restore_resume`) that
    detection flags `Incomplete` gets a one-shot continue-prompt into its live terminal
    (`maybe_auto_resume`). Deliberately NOT fired for live mid-session stalls.
  - **process exit (follow-up, done 2026-06-25)** — a managed session whose CC process
    fully exits (PTY closed) is relaunched in place via `claude --resume` + continue
    prompt, for live sessions too. Poll-driven (`check_terminal_exit` every 1.5s on
    `TerminalPanel::has_exited`), capped at `MAX_EXIT_RESUMES = 3` so a CC that dies on
    launch can't loop. **Needs a real-exit test.**

## STATUS (2026-06-25): PTY-exit follow-up landed. `cargo test` (workspace) = 423 pass, clippy clean (new code).

Original plan below.

---


Operator-reported issues (2026-06-23) + decisions captured via AskUserQuestion.

## Decisions
- **Trust (#3):** ask on first open, then persist the choice per-project.
- **Auto-resume (#2):** auto-resume on stall/exit, but **opt-in per project** (default off).
- **Phase (#5):** *accurate responses only* — no per-turn hook; make `request_phase`
  + MCP verb results report the authoritative current phase.

Implementation order: quick wins first (#4, #5), then #1 (needs repro), then #3/#2.

---

## #4 — Session header collapsed by default (TRIVIAL)
- `apps/desktop/src/views/panels/session_monitor.rs:283` `header_collapsed: false` → `true`.
- Toggle/glyph already handled (`collapse_button`, lines 885–893). One-line change.

## #5 — Phase confusion: accurate responses only
Root cause (confirmed): CC learns phase **only at launch** via `--permission-mode`
(`attach_command`, session_monitor.rs:541). 4 phases map to one "auto" mode, so most
phase changes never relaunch and CC is never told. `request_phase`
(`apps/desktop/src/phase_verbs.rs:62`) returns a premature success string built from
the *requested* target, before the operator approves.

Fix:
1. `request_phase` response must read the **live current phase** from engine state and
   report the request as *pending operator approval* (not "moved to X"). e.g.
   `"Requested Plan. Current phase is still Discovery (awaiting operator approval)."`
2. Include `current phase = X` in the response of the other MCP verbs
   (`report_blocked`, `run_status`) so any tool call re-grounds CC.
3. Needs a phase read-handle into the verb layer — verify the control/mcp-server side
   has (or can be given) a snapshot of the session's phase. (Implementation detail to
   confirm: `crates/mcp-server/src/lib.rs`, `phase_verbs.rs`.)

## #1 — Reset button crash ("fatal error closing windows")
`reset_session` (session_monitor.rs:1217–1250): sends `ForgetSession`, then
`deps.center.update(cx, |_,cx| cx.emit(NewManagedSession))`, then
`close_this_tab(self)` (line 1249). The plain self-close pattern works elsewhere
(line 826), so the differentiator is the re-entrant `center.update` emit + forget
**before** tearing down self in the same click listener — likely a GPUI re-entrant
borrow / use-after-teardown.

Plan: **reproduce first** (Test phase / run app, capture the panic), then defer the
successor-launch + self-close out of the click effect cycle (`cx.defer` / `cx.spawn`)
so the panel is removed after the current cycle. Confirm fix by clicking Reset.

## #3 — Trust: ask on open + persist
Today every session hard-codes `TrustTier::Observed` (engine/supervisor.rs:341,704);
trust is not persisted (comment at supervisor.rs:275).

- Add `trust: Option<TrustTier>` to `Space` + `PersistState` (serde default None) in
  `apps/desktop/src/views/project_space.rs`. Accessors `project_trust(root)` /
  `set_project_trust(root, tier)`.
- First time a space is opened (or first managed-session launch under it) with
  `trust == None`: prompt the operator (window dialog) → persist the choice.
- At managed-session launch (where `OpenRequest::NewManagedSession` is handled), resolve
  the project's persisted tier and send `Command::SetTrust { session, tier }` so the PDP
  picks it up. Persisted tier re-applies across restarts.

## #2 — Auto-resume, opt-in per project
- Add `auto_resume: bool` (default false) to `Space` + `PersistState` + a toggle in the
  spaces rail (or session monitor). Accessor `project_auto_resume(root)`.
- Detection already raises `Incomplete` on stall
  (`crates/detection/src/lib.rs:325–332`, `SessionAlert`/`AttentionKind`).
- When a session whose project has auto-resume ON goes `Incomplete` (and is resumable),
  auto-call the resume path (`try_resume`, session_monitor.rs:293, builds
  `attach_command` terminal) and inject a resume prompt via `session_io`/SteerControl.
- Default resume prompt: "You were interrupted before finishing. Re-read the recent
  transcript and continue the task where you left off." (make configurable later).
- PTY-exit-mid-turn detection is still open (per memory) — scope v1 to the `Incomplete`
  stall signal; add PTY-exit as a follow-up trigger.
