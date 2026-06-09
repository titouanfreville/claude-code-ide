---
stepsCompleted: ['step-01-init', 'step-02-discovery', 'step-02b-vision', 'step-02c-executive-summary', 'step-03-success', 'step-04-journeys', 'step-05-domain', 'step-06-innovation', 'step-07-project-type', 'step-08-scoping', 'step-09-functional', 'step-10-nonfunctional', 'step-11-polish', 'step-12-complete']
inputDocuments:
  - .bmad-output/planning/product-brief-moonlightcode.md
  - .bmad-output/brainstorming/brainstorming-session-2026-06-01-191535.md
workflowType: 'prd'
classification:
  projectType: 'Native desktop application (developer-tool IDE) with embedded local MCP server'
  domain: 'Developer tooling / AI-agent orchestration'
  complexity: 'high'
  projectContext: 'greenfield'
---

# Product Requirements Document - MoonlightCode

**Author:** Titouan
**Date:** 2026-06-01

## Executive Summary

MoonlightCode is a personal, AI-centric desktop IDE for orchestrating fleets of Claude Code sessions. For developers who now *run agents that write code* rather than writing it themselves, the bottleneck has shifted from editing to **attention and control across many parallel agents**. Existing session monitors (e.g. CodeIsland) can *show* state but not let the user *act* on it, forcing constant context-switching to find which session is blocked, what changed, and whether it's safe to commit.

MoonlightCode replaces the code editor at the center of the IDE with the Claude Code sessions themselves. It is a **workflow state machine** — each session carries an explicit phase (Plan → Auto-Implement → Test → Review → Commit), auto-reverts to **Plan** when work is declared done, and toggles between **Auto** and **Plan** on a single key with phase-gated permissions. Claude becomes a **first-class actor inside the IDE** via a built-in local MCP server under graduated trust tiers, able to run tests with coverage, query the database, fire HTTP requests, drive the debugger, and submit its own diffs to a review queue — all auditable and revertible. Token economy is native: RTK optimization plus an active fleet-level spend governor and an always-on HUD (context %, token burn, rate-limit headroom).

The product's primary goals are concrete and personal: **never miss a blocked session, lower token spend, and act on everything from one cockpit instead of watching from the sidelines.** Because complexity lives in technical feasibility rather than domain rules, a de-risking spike (proving a control surface into Claude Code and reliable phase/blocked detection) gates the heavier build.

### What Makes This Special

MoonlightCode is a **cockpit, not a dashboard**. Three things distinguish it: (1) it is a **workflow state machine** where phase-awareness and auto-revert-to-Plan keep a human gate between implement and commit — a control model nothing else offers; (2) **Claude is an actor, not a subject** — the MCP server makes control bidirectional, so the agent operates the IDE's tools under trust tiers the user sets; (3) **token economy is first-class** — RTK and an OMC-style HUD are native, not bolted on. The signature primitive is **rejection-as-feedback**: every gate (a rejected hunk, a failed test, a denied danger-zone action) emits structured corrective feedback back into the session, closing the human→agent steering loop that pure monitors lack. The honest moat is fit and execution — the tool is shaped precisely around one power-user's real loop and integrates the exact tools that loop depends on (Claude Code, RTK, oh-my-claudecode, bmad, sheik).

## Project Classification

- **Project Type:** Native desktop application — a developer-tool IDE (Rust-leaning, Tauri-class shell; **no Java**) with an embedded local MCP server and OS/process integration (terminal multiplexing, notifications).
- **Domain:** Developer tooling / AI-agent orchestration — a novel niche; no regulatory/compliance burden (personal, local tool).
- **Complexity:** High — driven by technical novelty (control surface into Claude Code, real-time multi-session state detection, MCP actor under trust tiers, concurrency + autonomy + safety), not business rules.
- **Project Context:** Greenfield (repository currently contains only planning artifacts).

## Success Criteria

### User Success (you, the operator)
- **Never miss a blocker:** From the moment a session blocks on input, you're aware within **≤30s** (queue + OS notification). Target: **zero sessions idle-blocked >2 min** unnoticed. *(Primary signal.)*
- **Act from one place:** A blocked or completed session can be reviewed and re-steered (approve/reject hunk → feedback injected) **without leaving MoonlightCode** for a terminal/tab. Target: **0 context-switches** to external tools for the core review/steer loop.
- **Phase discipline:** Sessions reliably stop at **Plan** on "done" — **0 unintended auto-commits** past your gate.
- **Aha moment:** The first time you reject a hunk and watch the session correct itself from that feedback — the loop *closes*.

### Personal Value & Adoption
- **It replaces the old stack:** Within the first month of v1, MoonlightCode is your **default surface** — Claude Code + CodeIsland juggling is retired for daily work.
- **Concurrency lift:** You comfortably steer **≥1.5× the parallel sessions** you manage today without losing track (measured against the baseline week).
- **Loop speed:** Median **Plan→commit cycle time** trends down over 4 weeks of use.

### Technical Success
- **Spike 0 passes:** A proven, non-fragile **control surface** into Claude Code (force Plan, gate permissions, inject MCP actor) and reliable **done/blocked/phase detection** — *gates the rest of the build.*
- **Detection accuracy:** Blocked/done/phase detection is correct **≥95%** of the time (few false "needs you", near-zero missed blockers).
- **Responsiveness:** Session state changes reflected in the UI within **≤1s**; the IDE stays responsive with **≥10 concurrent sessions**.
- **Safety holds:** **100%** of danger-zone actions (force-push, prod DB, etc.) require explicit approval; every autonomous MCP action is audit-logged and revertible.

### Measurable Outcomes
- **Token spend:** Measurable reduction in tokens/cost per session vs. the **baseline week** (instrument current usage *before* building). *(Primary signal.)*
- **Idle-blocked time:** Total idle-blocked session-minutes/day trends toward zero.
- **In-app review coverage:** % of reviews done in-app vs. externally approaches 100% for the core loop.

> **Baseline-first prerequisite:** Instrument one week of current usage (parallel-session count, idle-blocked frequency/duration, token/cost burn) so "lower tokens" and "concurrency lift" are provable, not felt.

## Product Scope

### MVP — Minimum Viable Product (the IDE shell)
Gated by **Spike 0** (control surface + detection). Then: session orchestration (needs-you queue, traffic-light tiles, notifications, attach-path, spawn, central terminal); workflow state machine (phase, auto-revert-to-Plan, Auto↔Plan toggle, phase-gated permissions, rejection-as-feedback); MCP-actor under trust tiers (thin verbs) + audit/revert + danger-zone approvals + GO/NO-GO launch board + file-lock negotiation between sessions; review surface (live diff, cross-session "since I last looked" feed, per-hunk accept/reject→feedback); RTK-native economy + active fleet governor + OMC-style HUD; lightweight continuity (survive restart + "what was I doing").

### Growth Features (Post-MVP / Phase 2)
HTTP collections + Run/Debug configs + DB explorer (in-repo, GitHub-shareable); debugger depth (timeline, rewind, error spotlight, hallucination detector); shared cross-session memory/wiki.

### Vision (Future)
One person conducts a *team* of AI agents like an orchestra — every session visible, every action gated by intent, every token accounted for. The phase-machine + trust-tiered MCP generalize from "my loop" to "any disciplined AI-dev loop," making it natural to hand to a teammate. Ecosystem/marketplace explicitly **out** for now.

## User Journeys

### Journey 1 — Orchestrator (happy path): The morning triage
**Titouan, 9:14 AM.** Six Claude Code sessions ran overnight across three repos. Today, that meant six terminal tabs and a creeping dread of "what did I miss?" **Now**, he opens MoonlightCode to a single grid: four tiles glow green (running), one pulses amber with a 🟡 badge ("needs you — session `auth-refactor` is asking which migration strategy"), one shows blue ("done — 1 diff awaiting review"). He hits the triage hotkey, lands directly in the amber session, types one line, and it's moving again. **Climax:** in 90 seconds he's cleared the queue that used to cost 15 minutes of tab-hunting. **New reality:** he starts the day *in control*, not catching up.
→ *Reveals:* session grid + focus, needs-you queue, traffic-light badges, jump-to-blocker hotkey, central terminal, attach-path context.

### Journey 2 — Reviewer: Rejection-as-feedback closes the loop
**11:40 AM.** A session declares `payment-webhook` done and **auto-reverts to Plan** — it doesn't barrel into commit. The blue "done" badge appears. Titouan opens the review surface: an IntelliJ-style diff, and a "since I last looked" feed. One hunk adds a retry without backoff. **Instead of fixing it himself or typing a vague correction**, he rejects that hunk — and the rejection becomes *structured feedback injected back into the session*: "add exponential backoff, cap 3 retries." The session re-plans. **Climax:** he steered the agent with a gesture, not a paragraph. **Resolution:** he approves the rest, the manual commit gate stages it, he commits.
→ *Reveals:* auto-revert-to-Plan, review queue, per-hunk accept/reject → feedback injection, "since I last looked" feed, commit gate.

### Journey 3 — Debugger / edge case: A session goes off the rails
**2:05 PM.** Session `search-index` errors mid-Auto — and worse, it's about to run a `DROP TABLE` against what looks like a real DB. **Danger-zone approval** halts it cold and pings Titouan. The audit log shows the last three actions; the error spotlight surfaces the failing command. He denies the action (→ feedback: "use the sandbox replica, never prod"), one-click reverts the half-applied change, and flips the session to **Plan** to rethink. **Climax:** a potential disaster became a 30-second course-correction. **Resolution:** trust, because nothing dangerous happens without him.
→ *Reveals:* danger-zone approvals, audit log + one-click revert, error spotlight, phase-gated permissions, sandbox guardrail, denial-as-feedback.

### Journey 4 — Claude-as-Actor (non-human): Autonomy under a leash
**Throughout the day.** A session in **Auto** at trust-tier-2 needs to verify its change. **Before**, Titouan would paste test output back and forth. **Now** Claude calls `run_with_coverage` via MCP, reads the RTK-compressed result, sees a gap, calls `query_db` to confirm a schema assumption, fires an `http_request` against the dev endpoint, then calls `open_review` to submit its diff to Titouan's queue — and waits. Every action streams to the HUD and audit log; the fleet governor down-shifts a low-priority session when rate-limit headroom dips. **Climax:** the agent did a full verify-and-submit cycle autonomously, and Titouan watched it happen in one pane. **Resolution:** he supervises a worker, he doesn't operate it.
→ *Reveals:* MCP-actor verbs, trust tiers, RTK compression on tool output, fleet token governor, HUD streaming, open_review hand-off.

### Journey Requirements Summary
The journeys converge on these capability areas:
- **Orchestration & awareness:** grid/focus, needs-you queue, traffic-light status, notifications, hotkey triage, central terminal, attach-path.
- **Workflow control:** explicit phases, auto-revert-to-Plan, Auto↔Plan toggle, phase-gated permissions, commit gate.
- **Steering:** rejection/denial/failure → structured feedback injection (the signature primitive).
- **Review:** live diff, cross-session "since I last looked" feed, per-hunk accept/reject.
- **Autonomy + safety:** MCP-actor verbs, trust tiers, danger-zone approvals, audit log, one-click revert, sandbox guardrails.
- **Economy & observability:** RTK compression, fleet governor, OMC-style HUD.
- **Continuity:** survive restart, "what was I doing" summaries (so any journey resumes after a crash).

## Domain-Specific Requirements

### Integration Constraints (the load-bearing dependency)
- **Claude Code control surface (Risk #1):** The product's core depends on programmatically *controlling* Claude Code — forcing Plan mode, gating permissions, injecting an MCP actor — not merely observing it. This must be established via supported mechanisms (Claude Code **hooks** — `Notification`/`Stop`/`PreToolUse`; the **Agent SDK**; **MCP**) without forking. **Spike 0 must prove this before committing to the build.** If only partial control is achievable, the product degrades gracefully to "observe + notify + assisted-steer."
- **State source of truth:** Session/phase/blocked detection reads from Claude Code's own artifacts (session JSONL under `~/.claude/projects/`, hook events) — the IDE must tolerate format changes and missing/partial data without crashing or showing false state.
- **RTK & oh-my-claudecode:** RTK integrates as a command wrapper (proven model); OMC state/HUD data is consumed where available. Neither is a hard runtime dependency — absence degrades features, doesn't break the IDE.

### Security & Trust Model
- **Graduated trust tiers:** Each session is assigned a trust tier governing which MCP-actor verbs it may call autonomously. Default deny; escalation is explicit.
- **Danger-zone enforcement:** A non-overridable list of actions (force-push, prod DB writes, destructive FS ops, secret exfiltration) **always** requires human approval, regardless of trust tier — aligned with Claude Code's default harness.
- **Secrets handling:** DB connection profiles and HTTP credentials are vaulted separately from in-repo configs; secrets never land in shareable/committed files or in the audit log in plaintext.
- **Audit integrity:** Every autonomous MCP action is logged with enough context to be one-click revertible; the log is append-only within a session's lifetime.

### Technical Constraints
- **Process & sandbox isolation:** Autonomous sessions should run against isolated working copies (worktree/sandbox) so parallel agents don't corrupt a shared checkout; file-lock negotiation between sessions prevents stomping.
- **Real-time performance:** State changes reflected in UI ≤1s; responsive at ≥10 concurrent sessions; the HUD/governor must not throttle the user's own interactions.
- **Local-first & privacy:** All orchestration, logs, and metrics stay on the user's machine; no telemetry leaves the device (session data may include proprietary code).

### Risk Mitigations
- **Detection fragility →** treat Claude Code artifacts as untrusted input; conservative "needs you" heuristics (favor a false-positive nudge over a missed blocker); ≥95% detection accuracy target.
- **Control-surface unavailability →** Spike 0 gate + graceful degradation path defined above.
- **Autonomy blast radius →** sandbox isolation + danger-zone approvals + revertible audit log as defense-in-depth.

## Innovation & Novel Patterns

### Detected Innovation Areas
1. **The IDE as a workflow state machine, not an editor.** Sessions carry an explicit phase (Plan→Auto→Test→Review→Commit) that *gates permissions* and *auto-reverts to Plan on done*. Treating "developer phase" as a first-class, permission-bearing state — applied to AI agents — is the core novel paradigm. Nothing in the current crop of agent-session managers models this.
2. **Bidirectional MCP: Claude as an actor *inside* the IDE.** Most tooling has the human (or IDE) calling the agent. Here the IDE exposes an MCP server so the *agent* drives the IDE's tools (run/coverage, DB, HTTP, debug, open-review) under graduated trust tiers. The IDE is both cockpit and toolbox.
3. **Rejection-as-feedback as a steering primitive.** Every gate (rejected hunk, failed test, denied danger-zone action) emits *structured corrective feedback* into the session — turning approval UI into a control loop rather than a yes/no switch.
4. **Token economy as first-class orchestration.** RTK-native compression plus a *fleet-level governor* that pauses/down-shifts low-trust sessions on low rate-limit headroom — spend control as an orchestration primitive, not a passive meter.

### Market Context & Competitive Landscape
- **Adjacent tools exist** for *watching* or *listing* multiple agent/Claude Code sessions (e.g. CodeIsland for state; kanban-style multi-agent managers). They largely stop at visibility + spawn/kill.
- **The gap MoonlightCode fills:** none (to the builder's knowledge) combine *phase-gated workflow control* + *bidirectional MCP-actor under trust tiers* + *native token economy* in one local, AI-centric IDE. The novelty is the **combination**, not any single feature.
- *(Personal tool — competitive positioning is informational, not a go-to-market concern. A light landscape scan before/after Spike 0 is worthwhile to borrow patterns, not to differentiate commercially.)*

### Validation Approach
- **Spike 0 first** — validates the riskiest novel assumption (can we actually control Claude Code's phase/permissions and inject an MCP actor?). If this fails, the paradigm degrades to "observe + assisted-steer."
- **Dogfooding against a baseline** — the builder is the user; the one-week baseline (idle-blocked time, token burn, parallel-session count) is the empirical test that the novel approach beats the status quo.
- **Thin-verb MCP** — prove one autonomous verb end-to-end (`run_with_coverage`) before building the full set.

### Risk Mitigation
- **Paradigm hinges on control surface** → Spike 0 gate + documented graceful-degradation fallback.
- **Detection of "phase/done/blocked" is novel and fragile** → conservative heuristics, ≥95% accuracy target, treat Claude Code artifacts as untrusted input.
- **Combination complexity** → staged build order (Spike 0 → orchestration slice → workflow+review → autonomy+economy) so each novel piece is validated before the next is layered on.

## Desktop Application Specific Requirements

### Project-Type Overview
MoonlightCode is a **native desktop application** that orchestrates local processes (Claude Code sessions), reads local artifacts, and hosts an embedded local MCP server. It is **developer-tooling** in spirit (extensible panels, in-repo configs) but **desktop-app** in architecture (process management, PTYs, OS integration). Rust-leaning, Tauri-class shell, **no Java**.

### Technical Architecture Considerations
- **Shell:** Tauri-class — Rust core + web-tech UI for the dockable JetBrains-style panels (grid, review diff, HUD, terminal). Rust gives the systems-level process/PTY control the product needs.
- **Core engine (Rust):** session supervisor (spawn/track/attach Claude Code processes), a **phase state machine** per session, an **event bus** (phase transitions, detection events → UI + hooks), and the **detection layer** that tails `~/.claude/projects/*.jsonl` + consumes Claude Code hook events.
- **Embedded local MCP server:** exposes the IDE's actor verbs (`run_with_coverage`, `query_db`, `http_request`, `start_debug`, `open_review`) to sessions, gated by trust tiers.
- **Local persistence:** an embedded store (e.g. SQLite) for continuity (survive restart, "what was I doing"), audit log, baseline metrics, and trust-tier config.

### Platform Support
- **MVP: macOS first** (the builder's platform — darwin). Architecture stays cross-platform-capable (Tauri + Rust) so **Linux** can follow; Windows last/if-ever.
- No mobile, no web-SEO concerns (skipped per project type).

### System Integration
- **Process & PTY management:** spawn, attach, and multiplex Claude Code session terminals (the "central terminal"); tmux interop as a fallback/attach path.
- **Filesystem watching:** live tail of session JSONL + project dirs for detection and the repo heatmap (Phase 2).
- **OS notifications:** native "needs you / done / errored" alerts (with DND/focus batching).
- **Secrets:** OS keychain for DB/HTTP credentials, kept out of in-repo configs and the audit log.
- **Sandbox/worktree:** integrate git worktrees / sandboxed working copies for isolated autonomous sessions.

### Update Strategy
- Personal MVP: simple self-built update (or manual rebuild) is acceptable; no app-store distribution. Revisit signed auto-update only if/when shared with coworkers.

### Offline Capabilities
- **Local-first:** orchestration, UI, review, HUD, persistence, and audit all work fully offline. The only network dependency is the **Claude API itself** (agent execution) — the IDE must clearly surface "session stalled: network/API" vs. "needs you," and remain usable for review/inspection while offline.

### Implementation Considerations
- **Spike 0 is the architectural fork:** it determines whether control happens via hooks, Agent SDK, MCP, or a wrapper — which in turn shapes the supervisor and detection layers. Build it as a throwaway Rust prototype before the shell.
- **Degradation path:** if full control proves impossible, the same engine still powers "observe + notify + assisted-steer."
- **Extensibility (deferred):** plugin-panel API and IDE-integration concerns are Phase-2+/out-of-scope, but the panel architecture should not preclude them.

## Project Scoping & Phased Development

### MVP Strategy & Philosophy
- **MVP Approach: Problem-solving MVP + dogfood-validated.** The MVP exists to prove the distinctive paradigm against the builder's own daily loop — not to impress an audience. "Useful" = it replaces the Claude Code + CodeIsland juggling for real work.
- **Resource Requirements:** Solo builder, spare-time cadence. This is the dominant constraint and shapes everything below: ruthless sequencing, throwaway spikes before commitment, and composing existing building blocks (Claude Code hooks/JSONL, MCP SDK, tmux, RTK, OMC) over building from scratch.
- **Fastest path to validated learning:** Spike 0 (control surface + detection) → a single vertical "watch & act" slice inside the shell that the builder uses daily within weeks, not months.

### Build Order & Dependencies (within the MVP)
1. **Spike 0 — De-risk gate (blocking):** Prove control surface + done/blocked/phase detection. *Everything depends on this.* Output: a go/no-go + the chosen control mechanism (hooks / Agent SDK / MCP / wrapper).
2. **Slice 1 — Watch & Act:** session supervisor + detection → grid, needs-you queue, traffic-light, notifications, central terminal, attach-path. *Depends on Spike 0.* Delivers the #1 value first.
3. **Slice 2 — Workflow + Review:** phase state machine, auto-revert-to-Plan, Auto↔Plan toggle, phase-gated permissions, review surface + "since I last looked", rejection-as-feedback. *Depends on Slice 1 + control surface.*
4. **Slice 3 — Autonomy + Economy:** trust-tiered MCP-actor (thin verbs), audit/revert, danger-zone approvals, RTK economy + fleet governor, HUD, lightweight continuity. *Depends on Slice 1–2 + MCP server.*

### Post-MVP Features
- **Phase 2 (Growth):** HTTP collections + Run/Debug configs + DB explorer (in-repo, GitHub-shareable); debugger depth (timeline, rewind, error spotlight, hallucination detector); shared cross-session memory/wiki; repo heatmap plugin view.
- **Phase 3 (Expansion):** Analyst stats suite (most-used tools, time-per-phase, cycle-time trends); adversarial plan-debate; living repo wiki; Linux/Windows platform support; *eventually* sharing with coworkers (ecosystem/plugin API — currently out of scope).

### Risk Mitigation Strategy
- **Technical risks:** The control surface (Risk #1) is the riskiest assumption → throwaway Spike 0 proves it before any UI; documented graceful degradation ("observe + assisted-steer") if full control is impossible. MCP verbs kept *thin* in v1; debugger integration deferred to Phase 2.
- **"Market" risks (personal-fit risks):** Risk that the built tool doesn't actually beat the status quo → the one-week **baseline** makes this measurable; dogfooding from Slice 1 catches misfit early.
- **Resource risks (the big one — solo, spare time):** Scope is sliced so each slice is independently useful — if momentum stalls after Slice 1, the builder still has a daily-useful "watch & act" tool. Compose-don't-build (hooks, tmux, RTK, OMC, MCP SDK) minimizes net-new infrastructure. No deadline pressure; quality of fit over feature count.

## Functional Requirements

> Capability contract. Tags: **[MVP]** = lean-core MVP; **[P2]** = Phase 2. Actors: **Operator** (the user), **Agent** (a Claude session), **System** (MoonlightCode itself).

### Session Orchestration & Awareness
- **FR1 [MVP]:** Operator can view all active Claude Code sessions as a live grid of tiles showing per-session status, attached path, and current phase.
- **FR2 [MVP]:** Operator can resize tiles and enter a focus mode for a single session.
- **FR3 [MVP]:** System can detect and visually distinguish session states (running, waiting-input, done, errored) via a badge + color applied to the tile.
- **FR4 [MVP]:** Operator can see a prioritized "needs-you" queue of all sessions blocked on input.
- **FR5 [MVP]:** System can notify the Operator via the OS when a session needs input, completes, or errors.
- **FR6 [MVP]:** Operator can jump directly to the session most needing attention via a hotkey.
- **FR7 [MVP]:** Operator can pin sessions to keep them always visible.
- **FR8 [MVP]:** Operator can attach a working directory / repo path to a session, and the System opens that project context alongside it.
- **FR9 [MVP]:** Operator can spawn a new session with a pre-filled prompt, attached path, and selected tool set.
- **FR10 [MVP]:** Operator can interact with any session through a central terminal that multiplexes all sessions.
- **FR11 [P2]:** Operator can view a heatmap of which repo areas each session is currently working in.

### Workflow & Phase Control
- **FR12 [MVP]:** System can track and display an explicit phase per session (Plan → Auto-Implement → Test → Review → Commit).
- **FR13 [MVP]:** System can auto-revert a session to Plan phase when it declares work done.
- **FR14 [MVP]:** Operator can toggle a session between Auto and Plan mode with a single key.
- **FR15 [MVP]:** System can enforce phase-gated permissions (e.g. Plan = read-only; Auto = the session's full trust tier).
- **FR16 [MVP]:** Operator can define and apply a reusable workflow template ("encode your loop") as the default for new sessions.
- **FR17 [MVP]:** System can require an explicit Operator gate before a session commits.

### Steering & Feedback
- **FR18 [MVP]:** Operator can reject a proposed change (hunk) and have the rejection delivered to the session as structured corrective feedback.
- **FR19 [MVP]:** System can convert a denied danger-zone action or a failed gate into structured feedback injected back into the session.
- **FR20 [MVP]:** Operator can re-steer any session by injecting instructions without leaving the IDE.

### Review
- **FR21 [MVP]:** Operator can view a per-session diff of pending changes with gutter-style change markers.
- **FR22 [MVP]:** Operator can review pending changes across all sessions in a single cross-session feed ("since I last looked").
- **FR23 [MVP]:** Operator can accept or reject changes at the per-hunk level.
- **FR24 [P2]:** Operator can see the blast radius (files/tests touched) of a pending change before approving.

### Agent Autonomy (MCP-Actor) & Trust
- **FR25 [MVP]:** Agent can run tests with coverage through the IDE's MCP server and receive a compact result.
- **FR26 [MVP]:** Agent can submit its own diff to the Operator's review queue and await a verdict.
- **FR27 [MVP]:** Agent can query a configured database through the MCP server and receive compact results.
- **FR28 [MVP]:** Agent can issue an HTTP request through the MCP server and read the response.
- **FR29 [P2]:** Agent can drive a debugger (set breakpoints, read stack/variables) through the MCP server.
- **FR30 [MVP]:** Operator can assign each session a trust tier governing which MCP-actor verbs it may invoke autonomously.
- **FR31 [MVP]:** System can require explicit Operator approval for an autonomous action that exceeds the session's trust tier.

### Safety & Audit
- **FR32 [MVP]:** System can block any action on a non-overridable danger-zone list pending explicit Operator approval, regardless of trust tier.
- **FR33 [MVP]:** System can record every autonomous MCP action in an audit log.
- **FR34 [MVP]:** Operator can revert any logged autonomous action with one action.
- **FR35 [MVP]:** System can run autonomous sessions against isolated working copies (worktree/sandbox).
- **FR36 [MVP]:** Operator can freeze a session at a "ready for review" checkpoint before it continues.
- **FR51 [MVP]:** Operator can review a GO/NO-GO board summarizing the planned actions and blast radius of a risky multi-session run before launching it.
- **FR52 [MVP]:** System can mediate file-lock negotiation between concurrent autonomous sessions (acquire/wait/release) so parallel sessions never stomp each other's edits.

### Token Economy & Observability
- **FR37 [MVP]:** System can apply RTK compression to the output of commands sessions run.
- **FR38 [MVP]:** System can display a HUD with context %, token burn, active model, and rate-limit headroom (global and per-session).
- **FR39 [MVP]:** System can display tokens/cost saved (per session and globally).
- **FR40 [MVP]:** System can act as a fleet governor — pausing or down-shifting low-trust sessions when rate-limit headroom drops.
- **FR41 [MVP]:** Operator can configure which metrics the HUD displays.

### Continuity & Persistence
- **FR42 [MVP]:** System can persist session orchestration state so it survives an IDE restart.
- **FR43 [MVP]:** System can generate a "what was I doing" summary for a session when the Operator returns to it.
- **FR44 [P2]:** Agent can read knowledge captured by sibling sessions (shared cross-session memory).

### Baseline & Setup
- **FR45 [MVP]:** System can capture a usage baseline (parallel-session count, idle-blocked time, token/cost burn) for before/after comparison.
- **FR46 [MVP]:** System can detect existing Claude Code sessions, RTK, and oh-my-claudecode on first run and offer to import/integrate.

### Phase 2 Capability Areas (out of MVP, named for completeness)
- **FR47 [P2]:** Operator can manage HTTP request collections stored as in-repo, GitHub-shareable files.
- **FR48 [P2]:** Operator can manage Run/Debug configurations stored as committed files.
- **FR49 [P2]:** Operator can explore a configured database through a DB explorer view.
- **FR50 [P2]:** Operator can replay a session's event timeline and rewind to a prior checkpoint.

## Non-Functional Requirements

### Performance & Responsiveness
- **NFR1:** Session state changes (needs-input, done, errored, phase transition) reflect in the UI within **≤1s** of the underlying event.
- **NFR2:** The IDE remains responsive (no perceptible input lag, <100ms UI interactions) with **≥10 concurrent sessions** running.
- **NFR3:** OS notification for a blocked/completed session fires within **≤30s** of the triggering event.
- **NFR4:** The HUD and fleet governor must never throttle or block the Operator's own UI interactions.

### Reliability & Resilience
- **NFR5:** Detection of session state (blocked/done/phase) is correct **≥95%** of the time, biased toward false-positive nudges over missed blockers (a missed blocker is the worst failure).
- **NFR6:** The IDE tolerates malformed, partial, or schema-changed Claude Code artifacts without crashing or displaying false state (treat as untrusted input).
- **NFR7:** Orchestration state survives an IDE crash/restart with no loss of tracked sessions or the audit log (per FR42).
- **NFR8:** If the control surface into Claude Code is unavailable, the IDE degrades gracefully to observe-and-notify rather than failing.

### Security & Privacy
- **NFR9:** Secrets (DB/HTTP credentials) are stored in the OS keychain, never written to in-repo configs, shareable files, or the audit log in plaintext.
- **NFR10:** Autonomous actions are denied by default; a session may only invoke MCP-actor verbs explicitly granted by its trust tier (per FR30).
- **NFR11:** Danger-zone actions always require explicit Operator approval and cannot be auto-approved by any trust tier (per FR32).
- **NFR12:** All orchestration data, logs, and metrics remain local to the Operator's machine — **zero telemetry** leaves the device (session data may contain proprietary code).
- **NFR13:** Every autonomous action is logged with sufficient context to be reverted (per FR33–34).

### Integration & Compatibility
- **NFR14:** The IDE integrates with Claude Code via its supported mechanisms (hooks / Agent SDK / MCP) without requiring a fork (contingent on Spike 0).
- **NFR15:** RTK and oh-my-claudecode are optional enhancements — their absence degrades features but never breaks the IDE.
- **NFR16:** Runs on **macOS first**; the architecture (Rust + Tauri-class) preserves a path to Linux without a rewrite.
- **NFR17:** Core orchestration, review, HUD, and persistence function fully offline; only agent execution (Claude API) requires network, and network/API stalls are surfaced distinctly from "needs you."

### Usability
- **NFR18:** Calm-by-default — the UI surfaces panels/alerts only when there is something that needs attention; idle sessions don't generate noise.
- **NFR19:** The core triage→review→steer loop is operable from the keyboard (hotkey-first), without leaving the IDE.
- **NFR20:** *(Light note — personal tool)* No formal WCAG target, but respect OS dark/light theme and avoid color-only status signaling (badge + color together, per FR3).
